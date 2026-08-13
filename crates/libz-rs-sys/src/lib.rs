//! `libz-rs-sys` — the C ABI facade over the safe `zlib-rs` core.
//!
//! This crate is what the drop-in replacement for the C `libz` is built from —
//! see "Why the `cdylib` is not the shipped library" below for the packaging step
//! that stands between `cargo build` and an installable library. It contributes no
//! compression logic of its own: every algorithm lives in [`zlib_rs`], which is
//! compiled under `#![forbid(unsafe_code)]` and has an empty `[dependencies]`
//! table. What this crate contributes is the *boundary* — the place where a raw
//! pointer handed over by a C caller is validated and turned into a safe Rust
//! slice exactly once, before anything in the core is reached.
//!
//! That division is the whole safety argument of the port. Unsafety cannot be
//! eliminated at an FFI edge, because the edge is where the type system stops;
//! it can only be *relocated* to a surface small enough to audit exhaustively.
//! This crate is that surface, and it is the only crate in the workspace
//! permitted to write `unsafe`.
//!
//! This file is the barrel. It declares the eleven-module tree, installs the
//! crate-wide lint attributes that make the safety claim checkable by the
//! compiler rather than by review, and re-exports the ABI types and the public
//! constant surface for Rust consumers. It defines no entry point and adds no
//! symbol of its own to the dynamic symbol table.
//!
//! # ★ The artifact matrix — one table, and it is the only one
//!
//! `Cargo.toml` declares `[lib] name = "z"` with
//! `crate-type = ["cdylib", "staticlib", "rlib"]`, so one crate defines the exported
//! C surface in exactly one place. Three artifacts come out of it and a fourth is
//! packaged from one of them. They are **not** interchangeable, every difference below
//! was measured with `nm`/`readelf` rather than reasoned about, and this table is the
//! single statement of it that the rest of the workspace refers back to:
//!
//! | Artifact | Produced by | Exports | Installable |
//! |---|---|---|---|
//! | `libz.a` (`staticlib`) | `cargo build -p libz-rs-sys --features libz-compat` | **all 95** functions `zlib.h` declares, plus `inflate_table` as a hidden global | **YES** — this is the static library, and `infcover` links it |
//! | `libz.so` (`cdylib`) | the same command | **93** of the 95, and nothing else; **no** version nodes | no — see below |
//! | *(the `rlib`)* | the same command | nothing; it is a Rust library | n/a — `zlib-rs-differential`, `fuzz/` and `tests/` depend on it |
//! | `libz.so.1.3.2.1-motley` | `make rust`, or the `CMake` equivalent, **relinked from `libz.a`** | **95** functions and the **16** zlib version nodes, internals hidden | **YES** — this is the shared library, and it is the only one |
//!
//! ★ Only the last row carries the versioned names `libz.so.1` and
//! `libz.so.1.3.2.1-motley`, and that is deliberate. `build.rs` used to stage those
//! two names beside cargo's `libz.so` so that a consumer linking against
//! `target/<profile>` could not fall through to the system libz; it no longer does,
//! because a build script runs BEFORE the link and therefore cannot tell a
//! `cargo check` from a `cargo build` — `cargo check`, any failed build and
//! `cargo clean -p` each left the two names behind as DANGLING links, which the
//! loader skips exactly as if they were absent — and because cargo puts that
//! directory first on the library search path of every build script it launches,
//! which put this incomplete library under the `as`, `ar`, `nm` and `rustc` that
//! were building it. Both were measured; the second reached a hard
//! `as: symbol lookup error: undefined symbol: deflate` once a
//! `--no-default-features` build had left a zero-export `libz.so` at the end of
//! that link. So job 4 of `build.rs` now prunes those names and writes a
//! `README-cargo-artifacts.txt` in their place, and the chain exists once, in what
//! `make rust` stages.
//!
//! The consequence is worth stating plainly rather than discovering: a C program
//! linked `-L target/release -lz` still builds, and then binds
//! `/lib/…/libz.so.1` at run time, because nothing in that directory answers to
//! the SONAME. Point consumers, `LD_LIBRARY_PATH` and `pkg-config` at what
//! `make rust` stages, and check with `ldd` — `make rust-test` does exactly that
//! and refuses to infer the binding from a passing run.
//!
//! The archive is complete because `build.rs` compiles this crate's one shipped C
//! translation unit — `csrc/gzprintf_shim.c`, which defines the variadic `gzprintf`
//! and the `va_list`-taking `gzvprintf` — and emits `-l static=` for the archive it
//! is collected into, which rustc merges into `libz.a`. Every other export, including
//! `inflate_table`, is Rust. Measured: `ar t` lists the one object, and `nm` reports
//! `T gzprintf`, `T gzvprintf` and `T inflate_table` in the archive.
//!
//! ★ Every row of that table is a gate rather than a recorded measurement, and
//! the row about the *complete* artifact is the one worth naming here.
//! `tests/symbol_parity.rs::cargo_staticlib_defines_the_whole_contract` requires
//! all 95 contract functions to be global text symbols in cargo's `libz.a`, and
//! names `gzprintf`/`gzvprintf` explicitly, so a shim that stopped being
//! compiled fails immediately instead of surfacing as a missing export from the
//! packaged library much later. `cargo_cdylib_matches_its_measured_shape` pins
//! the second row the same way, and the packaged row is held to full parity by
//! the rest of that file, by `make rust-symbols` and by the `symbols` job of
//! `.github/workflows/rust.yml`.
//!
//! ★ The packaged row's gates are **armed**, not merely present. Every one of
//! them inspects a file `cargo test` does not build, so each skips when
//! `target/dropin` is empty -- right for a developer, and vacuous for CI, where a
//! job could stage the drop-in, run the suite, print `SKIP:` for all nine gates
//! and be recorded green. `ZLIB_RS_REQUIRE_PACKAGED=1` turns each of those skips
//! into a failure that names what went unverified, and the `symbols` job sets it
//! and additionally fails on any `SKIP:` line in the output:
//!
//! ```text
//! make rust CARGO=cargo                                    # stage target/dropin
//! ZLIB_RS_REQUIRE_PACKAGED=1 cargo test --locked --release \
//!   -p libz-rs-sys --test symbol_parity -- --nocapture      # 16 gates, zero skips
//! ```
//!
//! Two of those gates are worth naming because they are what keeps the second row
//! from being read as a shipping artifact.
//! `versioned_symlink_chain_present` asserts the **topology** the C build
//! produces -- the versioned file real, `libz.so.1` and `libz.so` symlinks whose
//! recorded target is that bare name -- rather than only that three names resolve
//! to one object, which a `cp -L` copy or three independent files would also
//! satisfy. And `compile_flags_bit_27_agrees_with_the_shipping_artifacts` ties
//! `zlibCompileFlags()`'s "gzprintf is present" claim to the symbol tables of
//! `libz.a` and the packaged library, because the `cdylib` is the one artifact
//! where that bit and the dynamic table disagree: the shim's objects are in the
//! archive, and rustc gives a C-contributed symbol no dynamic entry, so the bare
//! `cdylib` reports the pair available while exporting neither. `src/util.rs`
//! documents that at the constant; cargo compiles all three artifacts from one
//! rustc invocation -- measured: one `--crate-name z` command carrying
//! `--crate-type` three times -- so no `cfg` could report a different word per
//! artifact even in principle.
//!
//! ## ★ Why the `cdylib` is not the shipped library, and cannot be made into one
//!
//! It is tempting to read the second row as "`cargo build` gives you the drop-in".
//! It does not, the gap is exactly two exports and sixteen version nodes, and the
//! cause is one mechanism rather than three: **rustc always hands the `cdylib` link a
//! version script of its own** — an anonymous tag listing this crate's `#[no_mangle]`
//! items under `global:`, and `local: *`. Three consequences follow, each measured on
//! the toolchain this workspace pins:
//!
//! * **`zlib.map` cannot be added alongside it.** `ld.bfd` refuses outright —
//!   *"anonymous version tag cannot be combined with other version tags"* — and
//!   `rust-lld` warns *"attempt to reassign symbol … to version"* and ignores the
//!   second script, producing a library with **zero** version nodes. So the 16
//!   version nodes cannot be attached here by any argument.
//! * **A symbol the C shims define gets no dynamic entry.** Pulling the shim objects
//!   in with `+whole-archive` contributes their code, and `local: *` then hides the
//!   names; `-Wl,--export-dynamic-symbol=gzprintf` does not override a version script
//!   (the name is absent from `.dynsym` under both linkers). So `gzprintf` and
//!   `gzvprintf` cannot be exported here either, which is why `build.rs` emits
//!   `-l static=` with cargo's default `-whole-archive`: the objects go into the
//!   archive that needs them and stay out of the cdylib that could not use them.
//! * **The crate's three internal helpers WOULD be visible, and are not.** All three
//!   are in `zlib.map`'s `local:` block — `inflate_table` by name, and
//!   `_zlib_rs_gzprintf_begin`/`_zlib_rs_gzprintf_commit` through its `_*` pattern —
//!   and rustc's script, which lists every `#[no_mangle]` item under `global:`, exports
//!   them regardless. This one consequence IS fixable, and is fixed: the `.hidden`
//!   directives further down this file give all three ELF `STV_HIDDEN` visibility,
//!   which no version script can undo and which leaves static linking untouched. The
//!   cdylib went from 96 dynamic globals to 93 when they were added.
//!
//! Measured totals for the `cdylib`, so the arithmetic is checkable: **93** dynamic
//! globals, being the 95 public functions less `gzprintf` and `gzvprintf`, with no
//! internal helper among them; and **0** version nodes. The remaining gap to the
//! contract is therefore two functions and sixteen version nodes, and both halves are
//! structural — a rustc-linked `cdylib` can carry neither.
//!
//! None of that is a defect in the packaging and none of it is fixable inside
//! `cargo`: it is what a rustc-linked `cdylib` is. The installable shared object is
//! therefore produced by **one** documented step from the complete archive, and that
//! step is the only place a `libz.so.*` fit to install comes from:
//!
//! ```text
//! make rust                # relink libz.a under zlib.map: SONAME, version nodes, symlink chain
//! make rust-test           # link and run the UNMODIFIED example.c, minigzip.c and infcover.c
//! make rust-symbols        # diff the staged tables against a built C libz
//! ```
//!
//! `CMake` performs the same relink under its own `ZLIB_BUILD_RUST` option — same
//! archive, same `zlib.map`, same SONAME, same chain — and additionally installs
//! the result, so the two build systems reach one artifact by one recipe rather
//! than two. `.github/workflows/rust.yml` exercises both: its `symbols`,
//! `dropin` and `cmake-rust` jobs each start from `libz.a` and each assert the
//! packaged library with `ldd` rather than inferring the binding from a test
//! that passed.
//!
//! `./configure` is not a prerequisite for those three: `Makefile.in` composes the
//! shared-object link from its own `RUSTLDSHARED`/`RUSTSHAREDFLAG` defaults and names
//! the staged library from `ZLIB_VERSION` in `zlib.h`. What `./configure` adds is the
//! C oracle build that `make rust-symbols` compares against.
//!
//! ## Which artifact is the drop-in replacement
//!
//! ★ **The shipping library is the relinked, versioned, localized, symlinked one
//! that `make rust` stages in `target/dropin/`, not the bare `libz.so` rustc
//! emits.** State it that way round, because the difference is not cosmetic and
//! cannot be closed inside cargo:
//!
//! | Property of the C `libz.so.1.3.2.1-motley` | bare rustc `cdylib` | `make rust` artifact |
//! |---|---|---|
//! | all 95 exported functions | **no** — `gzprintf` and `gzvprintf` are variadic, so they live in the C shim, whose objects a cdylib cannot re-export; the `libz.a` cargo emits does define all 95 | yes, relinked from that archive |
//! | 16 `ZLIB_*` symbol-version nodes | **no**, and unreachable: rustc's own anonymous-tag version script cannot be combined with `zlib.map`'s named tags | yes |
//! | `zlib.map`'s `local:` names hidden | **no** — three helpers stay visible, measured with `nm -D`: `_zlib_rs_gzprintf_begin`, `_zlib_rs_gzprintf_commit` and `_zlib_rs_inflate_table`, because stable Rust has no per-item hidden visibility for a `#[no_mangle]` item. (`inflate_table` itself is *not* among them: like `gzprintf`, it is defined in a C shim, and rustc's own `local: *` gives a C-contributed symbol no dynamic entry at all.) | yes, by `objcopy --localize-symbols` before the relink |
//! | `SONAME libz.so.1` | yes | yes |
//! | `libz.so.1` and `libz.so`, each a symlink directly onto the real `libz.so.1.3.2.1-motley` | **no**, and deliberately so — see below | yes, the C build's exact topology: two direct aliases, not a chain through one another |
//!
//! The symlinks are the part that bites silently rather than loudly. The SONAME
//! is `libz.so.1`, so a program linked against this library asks the loader for
//! that exact name; when only `libz.so` exists the loader falls back to the
//! **system** `libz.so.1` and every drop-in test passes while exercising the
//! wrong library. Staging the chain beside the *bare cdylib* does not fix that —
//! it makes it worse, because the name then resolves to an object that cannot
//! satisfy a variadic-`gz` caller, and cargo puts that directory first on the
//! library search path of every build script it launches. `build.rs` therefore
//! prunes any such alias an earlier revision left behind and writes
//! `README-cargo-artifacts.txt` saying so; only `make rust` stages the chain, in
//! `target/dropin`, beside a library that can answer for it. Every drop-in check
//! has to confirm with `ldd` which file was actually bound.
//!
//! ## The import name, which is not the package name
//!
//! ★ Note the asymmetry, and note it precisely, because it is easy to get
//! backwards. `[lib] name = "z"` renames the **library target**, and a library
//! target's name is also its extern crate name — it does not merely set the
//! emitted file name. So the *package* is `libz-rs-sys`, which is what a
//! `cargo -p` argument and a dependency line name, while the *library target*
//! is `z`, which is the default extern crate name a dependent gets.
//!
//! Two rules follow, and they differ:
//!
//! * **Inside this crate** — its doctests, and any `tests/` integration target it
//!   gains — the name is `z`, and cannot be anything else: a crate's own test
//!   targets link its library target under that target's name, with no
//!   opportunity to rename. Verified rather than assumed:
//!   `cargo test -p libz-rs-sys --doc` reports the suite as `Doc-tests z`.
//!
//!   ```text
//!   // every doctest in this crate, and any integration test added to it
//!   use z::{z_stream, z_streamp};
//!   ```
//!
//! * **Outside this crate** a dependent may rename the dependency, and the two
//!   Rust consumers in this workspace both do, restoring the conventional
//!   spelling:
//!
//!   ```text
//!   # crates/zlib-rs-differential/Cargo.toml and fuzz/Cargo.toml
//!   libz_rs_sys = { package = "libz-rs-sys", path = "../crates/libz-rs-sys" }
//!   ```
//!
//!   ```text
//!   // and therefore, in those two crates only
//!   use libz_rs_sys::z_stream;
//!   ```
//!
//! Neither spelling reaches a C caller. C resolves `deflate` and the other
//! ninety-four exports through the dynamic symbol table, where Rust crate names
//! do not exist at all.
//!
//! ## Why the shared object is relinked
//!
//! The relink is not an implementation detail that can be optimised away. A
//! version script cannot be attached to a rustc-built `cdylib`: rustc already
//! passes its own anonymous-tag script, and `ld` refuses to combine an
//! anonymous version tag with the named tags `zlib.map` declares. Reproducing
//! the C build's dynamic symbol table therefore requires linking `libz.a` with
//! the C linker under `--version-script`. `Makefile.in`'s `rust` target owns
//! that step; this crate's `build.rs` owns only the link arguments.
//!
//! # The export contract
//!
//! The reference C shared library exports **111 dynamic global symbols**: 95
//! `nm` type-`T` functions plus 16 type-`A` symbol-version nodes, under
//! `SONAME libz.so.1`. That set is the parity target, and it is checked by
//! diffing `nm -D --defined-only --extern-only` against the C baseline.
//! Four things perform that diff, and none of them needs a human:
//! `tests/symbol_parity.rs` runs it inside `cargo test`, so an export that goes
//! missing fails the ordinary test run; `Makefile.in`'s `rust` target runs it while
//! staging the drop-in tree, so a packaged artefact that fails parity is never
//! produced; `Makefile.in`'s `rust-symbols` target runs it on demand with the full
//! report; and the `symbols` job of `.github/workflows/rust.yml` runs it in CI. The
//! root `Cargo.toml` records the three numbers under `[workspace.metadata.zlib]`, so
//! the expectation is readable from a build as well as from a test.
//!
//! Run `make rust-symbols` for the report. Against the packaged artefact `make rust`
//! stages, the expected result is 111 symbols, an empty diff against the C
//! `libz.so.1.3.2.1-motley`, 95 type-`T` and 16 type-`A`, `SONAME libz.so.1`, and
//! none of the `zlib.map` `local:` names visible.
//!
//! ## Where 96 and 95 come from
//!
//! Count the DECLARATIONS THE PREPROCESSOR CAN REACH, not the occurrences of the
//! word. `zlib.h` contains 119 lines mentioning `ZEXTERN`, and **12 of them sit
//! inside comment blocks** — the API documentation shows `deflateInit`,
//! `deflateInit2`, `inflateInit`, `inflateInit2` and `inflateBackInit` written out
//! as though they were functions, which is exactly the shape a `grep` cannot tell
//! from a declaration. Strip the comments and **107 declaration sites** remain,
//! naming **96 distinct functions**. That 96 is the public API contract, and it is
//! the same 96 the AAP states.
//!
//! Those five documented-but-not-declared names are `#define` macros further down
//! the header — `deflateInit` over `deflateInit_` (L1932), `inflateInit` over
//! `inflateInit_` (L1934), `deflateInit2` over `deflateInit2_` (L1936),
//! `inflateInit2` over `inflateInit2_` (L1939), `inflateBackInit` over
//! `inflateBackInit_` (L1942) — and they exist so that a caller passes its own
//! compile-time `ZLIB_VERSION` and `sizeof(z_stream)` to the `_`-suffixed real
//! function. The version and layout check that makes drop-in replacement safe is
//! precisely what those macros arrange, which is why they are entry points in a
//! caller's source and not symbols in a library.
//!
//! From 96 names to 95 exported symbols is one subtraction, and it is
//! platform-dependent:
//!
//! | Name | `zlib.h` | Why it is not a symbol here |
//! |---|---|---|
//! | `gzopen_w` | L2042 | declared under `#if defined(_WIN32)`, so it is a symbol on Windows and absent on every other target |
//!
//! So **96 declared − 1 Windows-only = 95 exported functions on a non-Windows
//! target**, plus the 16 version nodes = the 111 dynamic globals above. There are
//! zero exported-but-not-declared symbols: the surface is neither a subset nor a
//! superset of the header.
//!
//! Where the 107 sites and the 96 names diverge is worth recording, because the
//! difference is what makes a naive count come out wrong. Measured on this header:
//!
//! * **83 active sites, naming 82 functions, begin at column zero** — outside every
//!   conditional block. One name has two of those sites: `gzprintf` is declared with
//!   a prototype under `#if defined(STDC) || defined(Z_HAVE_STDARG_H)` (L1549) and
//!   with an empty parameter list in the `#else` arm (L1551), for a pre-ANSI
//!   compiler with no `<stdarg.h>`.
//! * **The remaining 14 names appear only inside the large-file `#if` arms**: the
//!   seven `*64` entry points `gzopen64`, `gzseek64`, `gztell64`, `gzoffset64`,
//!   `adler32_combine64`, `crc32_combine64` and `crc32_combine_gen64`, and their
//!   seven unsuffixed counterparts, which `zlib.h` re-declares there because
//!   `Z_LARGE64` and `Z_WANT64` decide which spelling a caller sees. Both families
//!   must be exported; the indentation of a declaration says nothing about whether
//!   it is part of the contract.
//!
//! `crates/zlib-rs-differential/build.rs` derives its own rename list from the same
//! 96 and says so, so the two derivations cannot drift apart.
//!
//! ## Per-module allocation
//!
//! | Module | Exports | Contents |
//! |---|---|---|
//! | `deflate` | 17 | the `deflate*` family, including both `_`-suffixed init forms |
//! | `inflate` | 18 | the 18 public `inflate*` exports; `inflate_table` is a nineteenth `#[no_mangle]` symbol, defined in Rust over a validated `int`, named in `zlib.map`'s `local:` block and hidden from both shared objects |
//! | `infback` | 3 | `inflateBackInit_`, `inflateBack`, `inflateBackEnd` |
//! | `compress` | 10 | five one-shot wrappers and their five `_z` `size_t` forms |
//! | `gz` | 32 | 30 `gz*` functions here, plus 2 from `csrc/gzprintf_shim.c` |
//! | `checksum` | 11 | the Adler-32 and CRC-32 families |
//! | `util` | 4 | `zlibVersion`, `zlibCompileFlags`, `zError`, `get_crc_table` |
//!
//! 17 + 18 + 3 + 10 + 32 + 11 + 4 = **95**, of which this crate's Rust code supplies
//! 93 and `csrc/` supplies `gzprintf` and `gzvprintf`. The tally reconciles to the
//! built artifacts as follows, every figure measured with `nm`, and the arithmetic is
//! worth following once because it is the same arithmetic the artifact matrix above
//! tabulates:
//!
//! * **97** items carry `#[no_mangle]` across the seven modules — 94 contract names
//!   plus the three internal helpers `inflate_table`, `_zlib_rs_gzprintf_begin` and
//!   `_zlib_rs_gzprintf_commit`. `gzopen_w` is `#[cfg(windows)]`, so it is one of the
//!   94 only on Windows; **96** are compiled off Windows, being 93 contract names and
//!   the three helpers.
//! * `build.rs` compiles `csrc/gzprintf_shim.c` and emits `-l static=`, which rustc
//!   merges into the archive: `libz.a` therefore defines **98** unmangled global text
//!   symbols — those 96 plus `gzprintf` and `gzvprintf` — so every one of the 95
//!   functions the header declares off Windows is present, and the three helpers with
//!   them.
//! * `nm -D` on the `cdylib` reports **93** dynamic globals: rustc's own version
//!   script exports the Rust items and nothing else, the two C-defined names among
//!   them are consequently absent, and the three helpers are hidden by the `.hidden`
//!   directives below.
//! * The packaged shared object is relinked from the archive under `zlib.map`, which
//!   hides three of the 98 — `inflate_table` by name, and the two `_zlib_rs_*`
//!   helpers through its `_*` wildcard — leaving exactly **95**.
//!
//! Measured, not asserted: `make rust` reports "95 exported symbols, every one
//! declared in zlib.h" and "111 symbols, identical to the C
//! libz.so.1.3.2.1-motley".
//!
//! Two numbers are easy to get wrong when re-deriving this table, so they are
//! stated once: there are **17** `deflate*` exports, not 20, and `get_crc_table` is
//! counted **once**, in `util`, even though the C sources group it with the
//! checksums. **The 111-symbol parity diff is the authority**; per-module counts are
//! bookkeeping that has to reconcile to it, never the other way round.
//!
//! ## Version nodes and decoration
//!
//! Of the 111 symbols, **16 are version nodes** — `ZLIB_1.2.0`, `ZLIB_1.2.0.2`,
//! `ZLIB_1.2.0.8`, `ZLIB_1.2.2`, `ZLIB_1.2.2.3`, `ZLIB_1.2.2.4`,
//! `ZLIB_1.2.3.3`, `ZLIB_1.2.3.4`, `ZLIB_1.2.3.5`, `ZLIB_1.2.5.1`,
//! `ZLIB_1.2.5.2`, `ZLIB_1.2.7.1`, `ZLIB_1.2.9`, `ZLIB_1.2.12`,
//! `ZLIB_1.3.1.2` and `ZLIB_1.3.2`. `zlib.map` names 54 functions inside those
//! nodes, so **54 exports carry `@@VERSION` decoration** and the remaining
//! **41 are undecorated** base-set names.
//!
//! Nothing in the Rust sources creates the decoration: it is produced entirely
//! by the `--version-script=zlib.map` argument the packaging link passes. A
//! `#[no_mangle]` function is emitted undecorated, and the linker attaches the
//! node when it applies the script.
//!
//! # Crate-level attributes, and why each one is required
//!
//! ### `#![allow(non_camel_case_types)]`
//!
//! The C ABI names the boundary types: `z_stream`, `z_streamp`,
//! `internal_state`, `gz_header`, `gz_headerp`, `alloc_func`, `free_func`,
//! `in_func`, `out_func`. Every one of those spellings is fixed by `zlib.h`,
//! which is immutable, and the generated header has to be able to reproduce them —
//! so renaming them to satisfy Rust's casing convention is not available.
//! Since the workspace lint policy is enforced with `-D warnings`, the default
//! `non_camel_case_types` warning would otherwise be a hard error on every
//! boundary type. The allow is scoped to this crate alone; `zlib-rs` keeps the
//! convention.
//!
//! No matching `non_snake_case` or `non_upper_case_globals` allow is needed at
//! the crate root, and neither is present. The C function names are spelled on
//! `#[no_mangle]` items, whose export name is not subject to the naming lints,
//! and the `Z_*` constants below are already upper case. Adding either allow
//! would widen the exemption past what the ABI actually forces.
//!
//! ### `#![deny(unsafe_op_in_unsafe_fn)]`
//!
//! Without it, the body of an `unsafe fn` is an implicit unsafe block, so a
//! pointer dereference twenty lines below the signature carries no marker and
//! no `// SAFETY:` comment is prompted for. With it, every unsafe operation
//! must sit inside an explicit `unsafe { … }` that names its invariant, which
//! is what makes the audit in the next section mechanically checkable rather
//! than aspirational.
//!
//! ### `#![deny(missing_docs)]`
//!
//! The exported surface *is* the product. An undocumented public item here is a
//! gap in the contract, not a stylistic lapse.
//!
//! ### Deliberately absent: `#![forbid(unsafe_code)]`
//!
//! That attribute belongs to `zlib-rs` and to `zlib-rs` alone. Applying it here
//! would make the crate unable to do its one job, and the failure would be a
//! build error rather than a subtle regression — so this note exists to stop a
//! well-meant "hardening" pass from adding it. The workspace consequently
//! declines to blanket-forbid `unsafe_code` at `[workspace.lints]` level, and
//! the containment guarantee is expressed structurally instead: exactly one
//! crate may write `unsafe`, and it is this one.
//!
//! ### Deliberately absent: `#![no_std]`
//!
//! `zlib-rs` is `no_std` + `alloc` by default and that is correct for it. This
//! crate is unconditionally `std`, and the reason is measured rather than
//! stylistic: building the `cdylib` and `staticlib` targets under `#![no_std]`
//! fails with three errors — `no global memory allocator found but one is
//! required`, `` `#[panic_handler]` function required, but not found``, and
//! `unwinding panics are not supported without std`. Satisfying them would mean
//! installing a `#[global_allocator]` and a panic handler into every C process
//! that loads `libz.so`, which a drop-in replacement for a system library must
//! never do — all the more so because caller-visible memory is required to come
//! from the caller's own `zalloc` and go back to the caller's own `zfree`.
//!
//! Where a narrowed feature set makes part of `std` unnecessary, the gate goes
//! on the module that needs it — as `gz` does — never on the crate root.
//!
//! ### Deliberately absent: `#![feature(…)]`
//!
//! The declared MSRV is 1.80 on the stable channel, so no unstable feature may
//! be required to build this crate. Nightly appears in this port only for Miri
//! and for `-Zsanitizer=address`, both of which analyse code that already
//! compiles on stable.
//!
//! # Unsafe containment
//!
//! Unsafety is confined to six categories of site — the six AAP §0.6.1 fixes as
//! the sites the FFI contract *forces*. Anywhere else, `unsafe` in this crate is a
//! defect. Every block carries a `// SAFETY:` comment naming the invariant it
//! relies on, which `clippy::undocumented_unsafe_blocks` enforces rather than
//! trusting; where a block's category is not obvious from the operation, the
//! comment names it, and `grep -rn 'unsafe-site categor' crates/libz-rs-sys/src`
//! is how to read the mapping off the code instead of off this list.
//!
//! Each category is stated at the width the code actually uses it, which is wider
//! than the one operation that names it. A category is a *kind* of trust the C
//! contract obliges this crate to extend, not a single function call.
//!
//! 1. **The caller's stream, through a raw pointer: validation, and every field
//!    access.** Every exported stream function receives a `z_streamp`, which must
//!    be non-null, aligned, and addressing a caller-allocated `z_stream`.
//!    Nullability is part of the published contract — a null stream yields
//!    `Z_STREAM_ERROR` — so the guard always precedes the first dereference. The
//!    same category then covers each individual READ and WRITE of a member
//!    through that pointer: `next_in`/`avail_in`, `total_in`/`total_out`, `msg`,
//!    `adler`, `data_type`, and the three hook members. They are separate unsafe
//!    operations from the validation, and the same guarantee discharges them.
//! 2. **Pointer/length pairs, and the caller's out-parameters.**
//!    `(next_in, avail_in)` and `(next_out, avail_out)` are reconstructed with
//!    `from_raw_parts` once, on entry, and only the resulting safe slices travel
//!    inward; an `avail_in` of zero with a null `next_in` must produce an empty
//!    slice rather than undefined behaviour. The same reasoning covers the scalar
//!    OUT-PARAMETERS the API is full of — `deflatePending`'s `*mut c_uint` and
//!    `*mut c_int`, `inflateGetDictionary`'s `dictLength`, the `*mut uLong` and
//!    `*mut z_size_t` lengths of the one-shot wrappers, `gzerror`'s status slot —
//!    each of which is a caller-owned location this crate reads or writes exactly
//!    once, having established that it is non-null and aligned.
//! 3. **The opaque handle round-trip, for both handles.** `z_stream::state` is
//!    caller-visible but caller-opaque; before it is treated as state it is
//!    tag-validated, exactly as `deflateStateCheck` and `inflateStateCheck` do in
//!    the C sources, so a foreign, stale, or already-freed stream is rejected
//!    instead of trusted. `gzFile` is the same category in the gz layer, and this
//!    crate owns rather more of its life cycle: reserving the block, initialising
//!    it, handing back the handle, recovering the state from it on each later
//!    call, and releasing it on `gzclose` — with the caller-visible `gzFile_s`
//!    prefix (`have`, `next`, `pos`) at offset 0, because the `gzgetc` macro
//!    dereferences those three fields in code this crate cannot recompile.
//! 4. **Calling back into caller-supplied C function pointers.** `zalloc` and
//!    `zfree` are modelled as `Option<unsafe extern "C" fn(…)>` so that `Z_NULL`
//!    is `None` and selects the internal default, and a block obtained from a
//!    caller's `zalloc` is released only through that same caller's `zfree`. The
//!    `inflateBack` hooks are the same category with a different signature: `in()`
//!    hands this crate a buffer it must then treat as valid for the length
//!    returned, and `out()` is handed one of ours; both are invoked through a raw
//!    function pointer whose validity only the caller can establish.
//! 5. **C strings and varargs.** Paths reaching `gzopen` are assumed
//!    NUL-terminated and readable for the duration of the call; returned strings
//!    are `'static` or owned by the stream and outlive the caller's use of them.
//!    `gzprintf` is variadic and `gzvprintf` takes a `va_list`, neither of which
//!    stable Rust can declare, so both are C shims under `csrc/`.
//! 6. **Writing through a caller's `gz_header`.** `inflateGetHeader` fills caller
//!    buffers, and every write is clamped to `extra_max`, `name_max` and
//!    `comm_max`. This is historically the source of gzip-header overflow
//!    defects, and it becomes a length-checked slice write. `deflateSetHeader`
//!    reads the same struct under the same rules.
//!
//! # Panic discipline
//!
//! A panic must never unwind into a C caller: the caller was not compiled to
//! support unwinding, and letting an unwind cross the boundary is undefined
//! behaviour. Three mechanisms enforce that, and they are deliberately
//! redundant:
//!
//! * Every one of the 95 exported functions is declared `extern "C"`, never
//!   `extern "C-unwind"`. A panic reaching an `extern "C"` boundary aborts.
//! * The root `[profile.release]` sets `panic = "abort"`, so in the shipped
//!   configuration there is no unwinding machinery to escape through at all.
//! * `panic_guard` concentrates the behaviour in one auditable place instead
//!   of leaving it implicit across the whole exported surface. It is a private
//!   module: the guarantee is the crate's, not a knob a consumer reaches for.
//!
//! The lint policy inherited from the workspace additionally denies
//! `unwrap_used`, `expect_used`, `indexing_slicing`, `panic`, `todo`,
//! `unimplemented` and `unreachable`, so an abort should be unreachable rather
//! than merely contained.
//!
//! # Feature flags
//!
//! | Feature | Default | What it does |
//! |---|---|---|
//! | `libz-compat` | on | Gates the exported `extern "C"` surface — the unmangled symbols that make this library ABI-compatible with the C original. |
//! | `gz` | on | Gates the `gzFile` layer. File I/O is the only part of the port that needs `std` in the core, so this forwards into `zlib-rs/std`. **Implies `libz-compat`**: every item it turns on is an exported `extern "C"` entry point, so a gzFile layer outside the C ABI surface does not exist. |
//! | `simd` | off | Pass-through to `zlib-rs/simd`: vectorised CRC-32 and Adler-32 backends. |
//! | `libc` | off | Explicit opt-in for the optional pinned `libc` dependency, for the rare platform type or raw syscall that `std::fs` and `std::io` cannot express. |
//!
//! `libz-compat` is named exactly as the documented build command requires, so
//! that `cargo build --release --features libz-compat` selects the C ABI layer
//! by the name the port's own documentation uses. Because it is also in
//! `default`, that command and a plain `cargo build --release` resolve to the
//! same feature set; the explicit form exists to make the intent legible.
//!
//! There is no `rust-api` feature here. The idiomatic Rust surface is
//! `zlib-rs`'s concern and lives behind `zlib-rs/rust-api`, which this crate
//! deliberately does not enable — the facade needs the core's algorithms, not
//! its ergonomic wrappers.
//!
//! `--no-default-features` is a supported configuration. It compiles cleanly
//! and yields a library whose exported surface and gz layer are simply gated
//! out, leaving the crate-root ABI type mirrors — which is useful for checking
//! the feature matrix and for a consumer that wants those types without the
//! symbols. It is not a shipping configuration: a `libz.so` with no exports
//! replaces nothing. Nothing in this crate may raise a `compile_error!` for it.
//!
//! ★ Supported means supported **in both profiles**, and that has to be tested in
//! both: every `#[cfg(feature = "libz-compat")]` item must carry the gate on each
//! `debug_assertions` arm it splits into, or the configuration compiles in one
//! profile and fails in the other. `types.rs`'s `FILL_BYTE` did exactly that — the
//! release arm was missing the feature gate, so `--no-default-features` built in
//! debug and failed in release, which is the profile that ships. A feature-matrix
//! check that only builds debug cannot see it, so the release cell is the one that
//! matters. `build.rs` observes the same rule from the other side: with
//! `libz-compat` off it emits no SONAME, because an artifact that exports nothing
//! must not present itself as `libz.so.1`.
//!
//! `simd` is confined to the two checksums and is **output-neutral**: a checksum
//! is one scalar however it is computed, so vectorising it cannot perturb the
//! bitstream. Vectorising match-finding would change the compressed output and
//! is prohibited outright, because byte-identical output is a hard requirement
//! of this port. Runtime target-feature detection lives in the core, so a
//! `simd`-enabled build still executes correctly on hardware lacking the
//! instructions; nothing in this file performs CPU detection.
//!
//! ## The `ZLIB_RS_SIMD` build-time toggle
//!
//! `ZLIB_RS_SIMD=0|1` is read by this crate's `build.rs`, which declares
//! `cargo::rerun-if-env-changed=ZLIB_RS_SIMD` so that flipping it forces a
//! rebuild. It is an *assertion about the resolved feature set*, not a second
//! way of selecting one: a build script cannot switch a Cargo feature on or
//! off, so the environment variable is validated against the `simd` feature and
//! the outer build system performs the translation. `Makefile.in`'s `rust`
//! target turns `ZLIB_RS_SIMD=1` into `--features libz-rs-sys/simd`.
//!
//! The build script reports the outcome to the crate two ways, and both are
//! consumed here rather than left dangling:
//!
//! * `cargo::rustc-cfg=zlib_rs_simd`, emitted only when the feature actually
//!   resolved on, and surfaced by [`simd_enabled`]. Its paired
//!   `cargo::rustc-check-cfg=cfg(zlib_rs_simd)` is emitted unconditionally by
//!   the build script, which is what keeps the cfg free of the
//!   `unexpected_cfgs` warning that `-D warnings` would turn into a build
//!   failure. This file must not re-declare it.
//! * `cargo::rustc-env=ZLIB_RS_CHECKSUM_BACKEND=simd|scalar`, surfaced by
//!   [`checksum_backend`], so that a benchmark can state which implementation it
//!   measured instead of inferring it.
//!
//! # Version identity
//!
//! The library's self-reported version is `ZLIB_VERSION "1.3.2.1-motley"` with
//! `ZLIB_VERNUM 0x1321`. That string has four components and is therefore not
//! valid Cargo semver, so it must live as a literal and must never be derived
//! from `env!("CARGO_PKG_VERSION")`, which is the three-component `1.3.2`.
//! The distinction is load-bearing rather than cosmetic: `deflateInit_` and
//! `inflateInit_` compare the caller's compile-time `ZLIB_VERSION` against the
//! library's and return `Z_VERSION_ERROR` on a major mismatch, and
//! `test/example.c` performs exactly that check at startup.
//!
//! # Header immutability
//!
//! `zlib.h`, `zconf.h` and `zlib.map` are **immutable**. They are the contract,
//! not an artefact of this implementation, and no part of this port edits them.
//! Four mechanisms catch a divergence by tooling rather than by a reviewer's
//! memory, and each one now runs without being remembered:
//!
//! 1. **Every build.** The compile-time assertions in `layout_assertions` pin every
//!    struct size, field offset and integer width the two headers fix. Layout drift
//!    is a build failure rather than silent memory corruption in every program that
//!    links the result.
//! 2. **`make rust-header`, and the `header` job of `rust.yml`.** cbindgen
//!    regenerates a header from this crate, its stderr is held to an enumerated
//!    allowance, the artifact is compiled standalone as C89, C99, C17 and C++17 with
//!    warnings fatal, and its declared function set is compared against `zlib.h`'s
//!    `ZEXTERN` set in both directions. `cbindgen.toml` explains why a verbatim
//!    `diff` can never be empty and states the comparison that replaces it; the
//!    per-signature half of it is a step of that CI job, which re-declares every
//!    generated prototype in a translation unit that has already included `zlib.h`
//!    so the C compiler itself reports a conflicting type.
//! 3. **`cargo test`, `make rust`, and the `symbols` job of `rust.yml`.** An `nm`
//!    diff against the 111-symbol baseline catches both a missing and an extra
//!    export. `tests/symbol_parity.rs` performs it inside `cargo test` (16 tests,
//!    including the `inflate_table` archive-versus-dynamic split, the SONAME, the
//!    two-direct-alias symlink layout and the measured shape of the bare cdylib);
//!    `Makefile.in`'s `rust` target performs it against `zlib.h` and `zlib.map`
//!    while staging, and its `rust-symbols` target demands the direct diff against a
//!    built C library.
//! 4. **CI.** The `c-std.yml` workflow keeps compiling the unchanged `zlib.h` from
//!    C89 through gnu2x, so the header stays valid for every dialect a caller may
//!    use.
//!
//! So a change to an exported signature or to the export set fails a gate rather
//! than depending on somebody remembering to look.
//!
//! # cbindgen compatibility
//!
//! This file is the crate root cbindgen parses:
//!
//! ```text
//! cbindgen --config cbindgen.toml --crate libz-rs-sys --output generated_zlib.h
//! diff -u zlib.h generated_zlib.h    # the two FILES: published, not gated
//! make rust-header                   # names, warnings, C89/C99/C17/C++17
//! ```
//!
//! Two diffs, and only one of them can be empty. The `diff` of the two FILES never
//! will be, for the structural reasons `cbindgen.toml` enumerates -- no comments, no
//! `ZEXTERN`/`ZEXPORT`, no function-like macros -- so the `header` job runs it and
//! publishes it as evidence. The diff that IS the gate is rule A11's: both headers'
//! contract signatures rendered canonically by the C++ compiler's own mangling and
//! diffed, with **no output permitted**. It is empty today for all 95 signatures,
//! with no allowance of any kind. Compiling the generated header is a third,
//! independent check, and it is the one that catches a doc comment which breaks the
//! output.
//!
//! Three consequences shape the module tree below.
//!
//! * **Reachability under the default feature set.** cbindgen sees only what it
//!   can reach from here, so every module holding a `#[repr(C)]` type or a
//!   `#[no_mangle] pub extern "C"` function is declared unconditionally or
//!   behind a default-on feature. A module gated off in the configuration
//!   cbindgen parses would silently become a missing declaration in the diff.
//! * **A plain module tree.** No `mod` declaration is macro-generated and no
//!   `include!` appears anywhere in this crate. cbindgen parses the source
//!   syntactically without expanding macros, so an item it cannot see is an item
//!   it cannot emit.
//! * **The generated header is never committed.** `/generated_zlib.h` is in
//!   `.gitignore`, `cbindgen.toml`'s own banner marks the output a verification
//!   artefact that must not be installed or hand-edited, and `zlib.h` is
//!   read-only and is never regenerated from it. The header guard is `ZLIB_H` on
//!   purpose so the generated file cannot double-declare the API alongside the
//!   real header — which also means it shadows `zlib.h` and must never sit on an
//!   include path a real build searches.
//!
//! # Modules
//!
//! Every module in this crate is **private**. The two below hold the boundary
//! machinery rather than the C contract, so unlike the seven export modules they
//! contribute nothing to the crate root beyond the ABI types; they are named in
//! code spans here because a link to a private item resolves to nothing in the
//! rendered documentation.
//!
//! * `panic_guard` — the boundary's panic policy: the guard that turns a panic
//!   into an abort, and the `fallback` registry of per-family failure values that
//!   keeps a refusing body from inventing one. Private because the policy is the
//!   crate's to enforce, and because a consumer that could name the guard could
//!   wrap its own body in it and inherit an abort it never chose.
//! * `types` — the `#[repr(C)]` ABI mirror layer: `z_stream`, `gz_header`,
//!   `gzFile_s`, `code`, the `zconf.h` primitive aliases and the four nullable
//!   hook types, all re-exported at the crate root from here; plus the machinery
//!   that is not part of the C contract and is not re-exported — the
//!   `StreamAllocator` that calls a caller's `zalloc` and `zfree`, the raw-slice
//!   constructors, the tagged `StateBlock` and the install/check/take operations
//!   that round-trip the opaque `state` pointer. Those hold the unsafe categories
//!   above, and each one's invariants are discharged by the export that calls it,
//!   which is precisely why they stay `pub(crate)`: an outside caller has nothing
//!   to discharge them with. Every other module depends on this one.
//! * `layout_assertions` — the ABI gate. A private module of nothing but
//!   `const _: () = assert!(…)` items pinning every struct size, field offset
//!   and integer width `zlib.h`, `zconf.h` and `inftrees.h` fix, so that layout
//!   drift is a build failure rather than silent memory corruption in every
//!   program that links the result. It exports nothing — deliberately, since an
//!   extra name in the dynamic symbol table would fail the 111-symbol parity
//!   diff — and is the one of the four mechanisms above that a build cannot skip.
//! * `checksum` — the eleven exported Adler-32 and CRC-32 entry points:
//!   `adler32`, `adler32_z`, `adler32_combine`, `adler32_combine64`, `crc32`,
//!   `crc32_z`, `crc32_combine`, `crc32_combine64`, `crc32_combine_gen`,
//!   `crc32_combine_gen64` and `crc32_combine_op`. Width conversion,
//!   null-pointer handling and sentinel pass-through only; all arithmetic lives
//!   in `zlib_rs`. `get_crc_table` belongs to the same C family but is exported
//!   from the introspection module, not from here. Named in a code span rather
//!   than as an intra-doc link for the same reason as `compress` and `util`: the
//!   module is private and `cfg`-gated, so a link would name no public item — and
//!   no item at all in the `--no-default-features` build this crate supports.
//! * `compress` — the ten one-shot exports (`compress`, `compress_z`,
//!   `compress2`, `compress2_z`, `compressBound`, `compressBound_z`,
//!   `uncompress`, `uncompress_z`, `uncompress2`, `uncompress2_z`) over
//!   `compress.c` and `uncompr.c`. Gated on `libz-compat`, because it is
//!   exported surface rather than shared machinery, and **private**, with its ten
//!   functions re-exported by name at the crate root. A C caller resolves them
//!   through the dynamic symbol table, which Rust visibility does not govern; a
//!   Rust caller uses the curated crate-root path. That division is what keeps
//!   `zlib.map` rather than Rust visibility in charge of what the *library*
//!   exports, and it is the arrangement the root manifest records where it
//!   explains why `unreachable_pub` is not enabled.
//! * `deflate` — the seventeen exported `deflate*` entry points (`deflateInit_`,
//!   `deflateInit2_`, `deflate`, `deflateEnd`, `deflateSetDictionary`,
//!   `deflateGetDictionary`, `deflateCopy`, `deflateReset`, `deflateResetKeep`,
//!   `deflateParams`, `deflateTune`, `deflateBound`, `deflateBound_z`,
//!   `deflatePending`, `deflateUsed`, `deflatePrime`, `deflateSetHeader`) over
//!   `deflate.c`. Pointer validation, allocator construction, slice
//!   reconstruction, the opaque `state` round-trip and width mapping only; every
//!   compression decision lives in `zlib_rs`. `deflateInit` and `deflateInit2` are
//!   deliberately **absent**: `zlib.h` declares both, but both are macros over the
//!   `_`-suffixed functions and neither is an exported symbol, so exporting them
//!   would fail the 111-symbol parity diff. Gated on `libz-compat` and private,
//!   for the same two reasons as `compress`, and named in a code span rather than
//!   as an intra-doc link for the same reason too.
//! * `inflate` — the eighteen exported `inflate*` entry points over `inflate.c`,
//!   plus `inflate_table` over `inftrees.c`. `inflateInit` and `inflateInit2`
//!   are absent for the same reason their deflate counterparts are. Gated and
//!   private like the other export modules.
//! * `infback` — `inflateBackInit_`, `inflateBack` and `inflateBackEnd` over
//!   `infback.c`, adapting the C `in`/`out` function pointers to the core's
//!   callback traits. It sits beside `inflate` rather than inside it because its
//!   public entry points are distinct, which is exactly why `infback.c` is a
//!   separate translation unit that shares `inflate.h`, `inftrees.h` and
//!   `inffast.h` with `inflate.c`.
//! * `gz` — the `gzFile` layer over `gzlib.c`, `gzread.c`, `gzwrite.c` and
//!   `gzclose.c`. Gated on `libz-compat` **and** `gz`, and private like the
//!   other export modules.
//! * `util` — library introspection: `zlibVersion`, `zlibCompileFlags`, `zError`
//!   and `get_crc_table`, four of the ninety-five exports. Also the home of the
//!   version identity — `util::ZLIB_VERSION` and the `ZLIB_VER_*` numbers — which
//!   is why it is the module the init entry points take their comparison string
//!   from, and which is re-exported at the crate root below. Gated behind
//!   `libz-compat`, like every module that contributes exported symbols, and
//!   private like every other export module. Named in code spans rather than as
//!   intra-doc links on purpose: the module is private and `cfg`-gated, so a link
//!   would name no public item — and no item at all in a `--no-default-features`
//!   build, which this crate supports.
//!
//! # The crate-root Rust surface
//!
//! Everything re-exported below exists for *Rust* consumers, and there are four
//! classes of them: this crate's own integration tests under `tests/`,
//! `zlib-rs-differential`'s suites, the five `fuzz/` targets and the three benches.
//! Each reaches the ABI types the same way. Re-exporting creates no dynamic symbol: a
//! `pub use` is a Rust-level path and nothing more, which is why adding one cannot
//! disturb the 111-symbol parity diff.
//!
//! Three groups, and the crate root is the *only* Rust path to any of them,
//! because every module below it is private:
//!
//! * the `#[repr(C)]` ABI types and the `zconf.h` aliases, from `types`;
//! * the `Z_*`, `MAX_*` and `ZLIB_VER*` constant surface, declared here and
//!   re-exported from `util`;
//! * the **ninety-three exported C functions**, from the seven export modules —
//!   which is what lets an external crate hand `deflate` and `inflate` to a
//!   differential harness without writing an `extern "C"` block of its own. A
//!   hand-written declaration would compile whether or not this library agreed
//!   with it, so naming the real item is the only form that proves anything.
//!
//! ```
//! # #[cfg(feature = "libz-compat")]
//! # {
//! use core::ffi::{c_int, CStr};
//!
//! // `z` is the extern crate name: `[lib] name = "z"`. Outside this crate the
//! // two Rust consumers rename the dependency to `libz_rs_sys`.
//! let reported = unsafe { CStr::from_ptr(z::zlibVersion()) };
//! assert_eq!(reported, z::ZLIB_VERSION);
//! assert_eq!(z::compressBound(0), 13);
//!
//! // Named, not redeclared -- and annotated with the signature `zlib.h` L254 and
//! // L405 give, so the binding fails to compile if either ever drifts.
//! let deflate: unsafe extern "C" fn(z::z_streamp, c_int) -> c_int = z::deflate;
//! let inflate: unsafe extern "C" fn(z::z_streamp, c_int) -> c_int = z::inflate;
//! assert!(!core::ptr::eq(deflate as *const (), inflate as *const ()));
//! # }
//! ```
//!
//! ## What a consumer cannot reach, and why that is checked here
//!
//! The inverse of the list above matters just as much. The boundary machinery
//! lives in private modules as `pub(crate)` items, and the examples below are what
//! hold that line: each one is *required to fail* to compile. `compile_fail`
//! doctests are the only mechanism that can express "this must not compile"
//! without a third-party crate, and this crate takes no dependency it does not
//! need.
//!
//! Two properties of that mechanism are worth stating exactly, because it is easy
//! to over-read. Each example carries the error code it is expected to produce --
//! `E0603` for a path into a private module, `E0432` for a name that is not at the
//! crate root -- but **rustdoc on the stable channel does not verify the code**:
//! verified rather than assumed, a deliberately mis-annotated `compile_fail`
//! example still passes. The annotation is therefore documentation for the reader,
//! and what carries the assertion is the second property. Every example below is
//! a bare `use` of the item and nothing else -- no call to get wrong, no type to
//! infer, no result to unwrap -- so the privacy error is the only thing in it that
//! can fail, and there is no second way for it to fail and pass anyway. The codes
//! themselves were read off a real out-of-crate consumer rather than guessed.
//!
//! The allocator adapter that invokes a caller's `zalloc`, and the module holding
//! it:
//!
//! ```compile_fail,E0603
//! use z::types::StreamAllocator;
//! ```
//!
//! The generic raw-slice constructor — an unsafe function whose invariants only
//! the export that calls it can discharge:
//!
//! ```compile_fail,E0603
//! use z::types::input_slice;
//! ```
//!
//! The tagged state block and the operations that round-trip the opaque `state`
//! pointer:
//!
//! ```compile_fail,E0603
//! use z::types::{checked_state, install_state, take_state, StateBlock, StateKind};
//! ```
//!
//! The panic policy, which is the crate's to enforce rather than a knob a consumer
//! wraps its own body in:
//!
//! ```compile_fail,E0603
//! use z::panic_guard::guard;
//! ```
//!
//! ```compile_fail,E0603
//! use z::panic_guard::fallback::STREAM_ERROR;
//! ```
//!
//! And the three names this crate exports as symbols but keeps out of the Rust
//! API, for the reason the next paragraphs give:
//!
//! ```compile_fail,E0432
//! use z::inflate_table;
//! ```
//!
//! ```compile_fail,E0432
//! use z::_zlib_rs_gzprintf_begin;
//! ```
//!
//! ```compile_fail,E0432
//! use z::_zlib_rs_gzprintf_commit;
//! ```
//!
//! The barrel deliberately stops short in two places:
//!
//! * **No `#[no_mangle]` item is declared in this file.** Its job is wiring, and
//!   every extra exported symbol fails the parity diff. The 95 exports live in
//!   the seven export modules and nowhere else.
//! * **Nothing `zlib.map` marks `local:` is re-exported.** That covers
//!   `deflate_copyright`, `inflate_copyright`, `inflate_fast`, `inflate_table`,
//!   `zcalloc`, `zcfree`, `z_errmsg`, `gz_error`, `gz_intmax`, the `_*` wildcard
//!   and `inflate_fixed`. A `pub use` would not itself create a dynamic symbol,
//!   but putting a hidden name in the barrel invites a future `#[no_mangle]`, so
//!   the names stay out. Three of those names are defined by this crate and are
//!   therefore the concrete gap between the 96 items it compiles and the 93 it
//!   re-exports: `inflate_table`, which the unmodified `test/infcover.c` calls
//!   directly and links from `libz.a` where the version script does not apply,
//!   and the two `_zlib_rs_gzprintf_*` helpers that `csrc/gzprintf_shim.c`
//!   calls. All three are exported as symbols and none is re-exported here.

// ---------------------------------------------------------------------------
//  Crate-level attributes
// ---------------------------------------------------------------------------
//
// Each of these is justified at length in the crate documentation above, under
// "Crate-level attributes, and why each one is required" -- including the three
// attributes that are deliberately ABSENT: `forbid(unsafe_code)`, `no_std` and
// `feature(...)`.  Adding any of the three would break this crate, so read that
// section before touching this block.
//
// The workspace lint policy is deliberately NOT restated here: the root
// `Cargo.toml` owns it and this crate opts in with `[lints] workspace = true`.
// Re-declaring any of it here would let the two copies drift.
#![allow(non_camel_case_types)]
#![deny(unsafe_op_in_unsafe_fn)]
#![deny(missing_docs)]
// Dead code stays an error in every configuration that has an exported surface --
// which is every configuration that ships.  It is allowed in exactly one place: a
// build with `libz-compat` off.
//
// That build compiles out all seven export modules, and with them every consumer of
// the boundary machinery: nothing is left to call `panic_guard::guard`,
// `types::input_slice`, `types::install_state`, `types::checked_state_mut` or the
// `inftrees` constants, because the only callers were the entry points that just
// disappeared.  The machinery is not dead -- it is unreachable in a configuration
// whose entire purpose, as `Cargo.toml` records, is feature-matrix checking rather
// than shipping, and which must therefore compile warning-free.
//
// Written as `cfg_attr` rather than as a bare `allow` so the relaxation cannot leak
// into the shipping configurations: with `libz-compat` on -- the default, and what
// `cargo build --release` produces -- an item that genuinely loses its last caller
// still fails the zero-warning bar.
#![cfg_attr(not(feature = "libz-compat"), allow(dead_code))]
// The export functions are re-exported at the crate root (see the re-export section
// below), which makes their documentation PUBLIC documentation -- and their
// documentation deliberately names the crate-private helpers that discharge each
// unsafe-site category: `block_mut`, `checked_state_mut`, `input_slice`,
// `take_state`, `StreamAllocator::from_stream_ptr`, `panic_guard::guard` and the
// rest.  rustdoc cannot link a public item's docs to a private one, so it warns.
//
// The links stay, and the lint is allowed, because those references ARE the audit
// trail: an unsafe boundary is reviewable only if each entry point says which
// validated helper it goes through, and `cargo doc --document-private-items` -- how
// this crate is read by the people maintaining it -- resolves every one of them into
// a navigable link.  The alternative, rewriting some fifty links as plain code
// spans, would keep rustdoc quiet by deleting the trail from the maintainer's view
// as well as the consumer's.  In the public view they render as code spans anyway,
// which is exactly what a consumer needs: the name is informative, the target is not
// theirs to call.
//
// This is narrow.  `rustdoc::broken_intra_doc_links` stays at its default of warn,
// so a link to a name that does not exist at all is still reported -- only the
// private-target case is allowed.
#![allow(rustdoc::private_intra_doc_links)]

use core::ffi::c_int;

// The core's error newtype and its parameter constants, used only by the
// compile-time agreement checks further down.  Aliased so that the imported
// names cannot be confused with this crate's own `Z_*` constants, which are
// spelled exactly as `zlib.h` spells them.
use zlib_rs::config as core_config;
use zlib_rs::error::ReturnCode;

// ---------------------------------------------------------------------------
//  Module tree -- eleven modules, no twelfth
// ---------------------------------------------------------------------------
//
// Declared in dependency order: the three support modules first, then the seven
// export modules that build on them.  cbindgen parses this list to find the
// `#[repr(C)]` types and the `#[no_mangle]` functions, so it is kept plain --
// no macro-generated `mod`, no `include!`.

// Compile-time only, and intentionally not `pub`: the module declares no
// nameable item, so there is nothing for a consumer to reach. Declaring it is
// what makes its assertions run — a `const` item is evaluated because it exists,
// not because something uses it.
// `cbindgen:ignore` -- the generator must not traverse this module.  It declares
// nothing nameable and nothing exportable: every item in it is an anonymous
// `const _: () = assert!(…)`, and cbindgen reports each one it walks past as
// "Skip libz-rs-sys::_ (not `pub`)".  That was 175 warnings out of 269, drowning
// the ones that would matter, and the header gate's rule is that ANY cbindgen
// warning is a failure -- which is only enforceable if the expected count is
// zero.  Skipping the module changes the generated header not at all (verified by
// diffing the artifact before and after) because there was never anything here to
// generate.
/// cbindgen:ignore
mod layout_assertions;

// The two support modules, both PRIVATE, and for a reason that is the mirror image
// of the one above `mod compress`: what they hold is not the C contract but the
// machinery that implements it -- the allocator adapter that invokes a caller's
// `zalloc`, the generic raw-slice constructors, the tagged state block and the
// install/check/take operations that round-trip the opaque `state` pointer, and
// the panic guard with its per-family recovery values. Every one of those is an
// unsafe or unsafe-adjacent helper whose invariants are discharged by the export
// that calls it, so publishing them would invite a consumer to discharge those
// invariants itself, outside the audited boundary. The `#[repr(C)]` ABI mirrors
// that ARE the contract -- `z_stream`, `gz_header`, `gzFile_s`, `code`, the
// `zconf.h` aliases and the four nullable hook types -- are re-exported from
// `types` at the crate root below, which is the whole of what a Rust consumer
// needs and the whole of what it gets. The `compile_fail` examples in the crate
// documentation are what hold that line: they fail to compile precisely because
// these two modules are private.
#[cfg(feature = "libz-compat")]
mod panic_guard;
mod types;

// Gated at the crate root, as `Cargo.toml` requires of every export: a
// `--no-default-features` build is supported and must simply have the exported
// `extern "C"` surface absent, never raise a `compile_error!`.  Private, and
// re-exported by name below, for the two reasons given above `mod compress`.
#[cfg(feature = "libz-compat")]
mod checksum;

// Exported `extern "C"` surface. Two properties, both deliberate:
//
//   * `libz-compat` gates it, as the feature table above describes: with the
//     feature off the crate still compiles and still provides the ABI type
//     mirrors, it simply exports no unmangled symbols.
//   * The module is PRIVATE, and the crate root re-exports its contract functions
//     BY NAME further down.  The two audiences reach them by different routes and
//     both routes are deliberate: a C caller resolves a `#[no_mangle]` item
//     through the dynamic symbol table, which Rust visibility does not govern at
//     all, while a Rust caller -- this crate's `tests/`, `zlib-rs-differential`,
//     the `fuzz/` targets and the benches -- needs a path, and gets exactly one,
//     at the barrel.  Keeping the module private is what makes that path
//     CURATED: a `pub mod` would publish every item the module happens to spell
//     `pub`, whereas an explicit `pub use` list publishes exactly the names
//     `zlib.h` declares and nothing else.  `zlib.map` still owns the dynamic
//     symbol table either way -- a `pub use` is a Rust-level path and creates no
//     symbol -- which is also why the root manifest leaves `unreachable_pub` off.
#[cfg(feature = "libz-compat")]
mod compress;

// Exported `extern "C"` surface, private and `libz-compat`-gated for exactly the two reasons
// given above `mod compress`.
#[cfg(feature = "libz-compat")]
mod deflate;

// The decompression exports. Private, with its contract functions re-exported by
// name below, for the two reasons given above `mod compress`.
//
// ★ This module additionally exports `inflate_table`, which `zlib.map`'s `local:`
// block hides from the shared library's dynamic table but which the unmodified
// `test/infcover.c` calls directly and links from `libz.a`. See the module's own
// documentation for why the two requirements are compatible rather than in conflict.
// It is the one exported name this file deliberately leaves OUT of the re-export
// list: a hidden symbol has no Rust consumer, and the curated list is where that
// decision is visible.
#[cfg(feature = "libz-compat")]
mod inflate;

// The three `inflateBack*` exports. Private and `libz-compat`-gated for the same
// two reasons `compress` is.
//
// ★ It sits beside `inflate` rather than inside it because its public entry points
// are distinct, which is exactly why `infback.c` is a separate translation unit
// that shares `inflate.h`, `inftrees.h` and `inffast.h` with `inflate.c`. The two
// modules likewise share one state representation and one message table: this one
// reads `inflate`'s `version_error` and `message_ptr` rather than growing second
// copies that could drift.
#[cfg(feature = "libz-compat")]
mod infback;

// Gated with the exported surface it belongs to: `libz-compat` is what turns the
// unmangled C symbols on, so a `--no-default-features` build compiles this module
// out along with the rest of them.  Private, and re-exported by name below --
// including the version identity, which is a constant rather than a function and
// which the init entry points compare against -- for the two reasons given above
// `mod compress`.  What stays unreachable is the machinery
// beside those four functions: the `zlibCompileFlags` bit ladder, the `z_errmsg`
// message table and `error_message`, none of which is part of any contract.
#[cfg(feature = "libz-compat")]
mod util;

// The `gzFile` layer: thirty exported `gz*` symbols plus the two hidden helpers
// `_zlib_rs_gzprintf_begin` and `_zlib_rs_gzprintf_commit` that `csrc/gzprintf_shim.c`
// calls. Three properties, all deliberate:
//
//   * TWO features gate it, not one. `libz-compat` turns the unmangled C symbols on,
//     as it does for every export module; `gz` additionally forwards into
//     `zlib-rs/std`, because file I/O is the only part of the port that needs `std` in
//     the core and `zlib-rs` declares its own `gz` subtree `#[cfg(feature = "std")]`.
//     With either feature off the crate still compiles and simply exports no `gz*`
//     symbols -- `--no-default-features` is a supported configuration and nothing here
//     may raise a `compile_error!` for it.
//   * The module is PRIVATE, with its thirty contract functions re-exported by name
//     below, for the two reasons given above `mod compress`. The two
//     `_zlib_rs_gzprintf_*` helpers are left out of that list: `zlib.map`'s `_*`
//     wildcard hides them, their only caller is `csrc/gzprintf_shim.c`, and a C
//     caller reaches them through the archive's symbol table rather than through
//     any Rust path.
//   * `gzprintf` and `gzvprintf` are NOT here. Stable Rust at the declared MSRV can
//     neither define a C-variadic function nor name a `va_list`, so those two symbols
//     come from the single C translation unit the packaging step links in; the module
//     documentation records the full reasoning and the helper contract.
#[cfg(all(feature = "libz-compat", feature = "gz"))]
mod gz;

// ---------------------------------------------------------------------------
//  Hidden internals -- the cdylib's dynamic table, narrowed to the contract
// ---------------------------------------------------------------------------
//
// ★ Three `#[no_mangle]` items in the modules above are not part of any contract:
// `inflate_table`, which `zlib.map`'s `ZLIB_1.2.0` `local:` block names outright, and
// the two `_zlib_rs_gzprintf_*` helpers, which its trailing `_*` pattern covers. The
// packaged shared library is relinked under that version script, so it hides all
// three. The `cdylib` cargo links directly is not, and rustc's own anonymous version
// script lists every `#[no_mangle]` item of the crate under `global:` -- so without the
// directives below those three names appear in its `.dynsym`, and `nm -D` on
// `target/<profile>/libz.so` reports 96 dynamic globals where the contract has 93.
//
// `.hidden` closes that gap, and it is the only mechanism that does. Measured on rustc
// 1.97.1 against both `rust-lld` and `ld.bfd`, in debug (16 codegen units) and release
// (fat LTO, one codegen unit):
//
//   * `.hidden <name>` gives the symbol ELF `STV_HIDDEN`, and rustc's version script
//     cannot re-export it: the name is absent from `.dynsym` on both linkers.
//   * `.symver` does bind a cdylib symbol to a named version node -- and then emits
//     both `name` and `name@@NODE` into the object, so a C program linking the
//     resulting `libz.a` fails with `multiple definition of 'name'`. Unusable.
//   * `#[export_name = "name@@NODE"]` makes rustc write `@@` into its own version
//     script, which `rust-lld` then rejects (`expected ; but got @`). Unusable.
//   * A second, anonymous version script listing the names exports them under
//     `rust-lld` and is refused by `ld.bfd` ("anonymous version tag cannot be combined
//     with other version tags"). Linker-dependent, therefore unusable.
//
// ★ Hidden visibility is a property of a SHARED OBJECT's dynamic table and says
// nothing about static linking. `libz.a` still defines all three names as ordinary
// globals -- `nm` reports `T` whether or not the symbol is hidden -- which is what the
// unmodified `test/infcover.c` needs from `inflate_table`, since its `cover_trees`
// calls it directly, and what `csrc/gzprintf_shim.c` needs from the two helpers it
// forwards to. Verified end to end: a C program that links the archive and calls a
// hidden symbol builds and runs.
//
// The cfgs are the defining modules' own, exactly. Naming a symbol a configuration does
// not define would leave an undefined hidden reference and fail the link, so they are
// load-bearing rather than tidy.
//
// ELF only. `.hidden` is a GNU-as directive; Mach-O spells the property
// `.private_extern` and PE/COFF has no equivalent, and neither platform is reached by
// the packaging step `build.rs` implements. Elsewhere the directives are simply not
// emitted, the cdylib keeps the three extra names, and
// `tests/symbol_parity.rs::cargo_cdylib_matches_its_measured_shape` runs only where the
// artifact it measures exists.
#[cfg(all(feature = "libz-compat", target_os = "linux"))]
core::arch::global_asm!(".hidden inflate_table");

#[cfg(all(feature = "libz-compat", feature = "gz", target_os = "linux"))]
core::arch::global_asm!(
    ".hidden _zlib_rs_gzprintf_begin",
    ".hidden _zlib_rs_gzprintf_commit"
);

// ---------------------------------------------------------------------------
//  Build-script bridge
// ---------------------------------------------------------------------------
//
// `build.rs` reports its decisions to the crate through one cfg and one
// environment variable, and both are surfaced here so that a consumer never has
// to guess which backend it linked.  Neither accessor is `extern "C"`, so
// neither adds a symbol to the dynamic table and neither appears in the header
// cbindgen generates.
//
// ★ The `cargo::rustc-check-cfg=cfg(zlib_rs_simd)` declaration that keeps
// `cfg!(zlib_rs_simd)` free of the `unexpected_cfgs` warning is emitted by
// `build.rs`, unconditionally and for every build.  Do NOT re-declare it here:
// two declarations of the same cfg are redundant, and moving the declaration
// into the source would make it disappear whenever the build script is skipped,
// which is exactly when the warning would be hardest to attribute.

/// Whether the vectorised checksum backends are compiled into this build.
///
/// `true` when the `simd` Cargo feature resolved on, which is when `build.rs`
/// emits `cargo::rustc-cfg=zlib_rs_simd`. The value is a property of the build,
/// not of the machine: SIMD is **output-neutral**, so this reports which code
/// path is present, never a different result. Runtime target-feature detection
/// lives in the core, so a `true` here on hardware without the instructions
/// still computes the same checksums by a different route.
#[must_use]
pub const fn simd_enabled() -> bool {
    cfg!(zlib_rs_simd)
}

/// The name of the checksum backend compiled into this build.
///
/// `"simd"` or `"scalar"`, taken from the `ZLIB_RS_CHECKSUM_BACKEND` value
/// `build.rs` emits with `cargo::rustc-env`. A benchmark can report the string
/// directly instead of inferring which implementation it measured.
#[must_use]
pub const fn checksum_backend() -> &'static str {
    env!("ZLIB_RS_CHECKSUM_BACKEND")
}

// The build script derives the cfg and the backend name from the RESOLVED
// feature set -- never from `ZLIB_RS_SIMD`, which is an assertion about that set
// rather than a way of choosing it.  These checks hold the two reports to that
// rule from the crate's side, so a build script edit that let them diverge
// would fail to compile instead of silently mislabelling a benchmark.
/// cbindgen:ignore
const _: () = {
    assert!(
        simd_enabled() == cfg!(feature = "simd"),
        "cfg(zlib_rs_simd) disagrees with the `simd` feature: build.rs must derive \
         the cfg from CARGO_FEATURE_SIMD, never from ZLIB_RS_SIMD"
    );
    // `str` equality is not const-callable at the declared MSRV and indexing is
    // denied by the workspace lint policy, so the two spellings are discriminated
    // by length -- which is exact, because `"simd"` and `"scalar"` are the only
    // two values build.rs emits and they differ in length.  build.rs owns the
    // spelling; this owns the agreement.
    let expected = if simd_enabled() { "simd" } else { "scalar" };
    assert!(
        checksum_backend().len() == expected.len(),
        "ZLIB_RS_CHECKSUM_BACKEND disagrees with cfg(zlib_rs_simd)"
    );
};

// ---------------------------------------------------------------------------
//  Crate-root re-exports -- the ABI types
// ---------------------------------------------------------------------------
//
// The types a C caller passes across the boundary, lifted to the crate root --
// which is the ONLY Rust path to them, because `types` is private.  The canonical
// definitions, with their documentation, their `#[repr(C)]` attributes and the
// layout assertions that pin them, stay in that module; this list is what decides
// which of them a consumer outside the crate may name.
//
// It is exactly the C contract and nothing else: the four `#[repr(C)]` structs,
// the `zconf.h` primitive and pointer aliases, and the four nullable hook types.
// The module's remaining items -- `StreamAllocator`, `StateBlock`, `StatePrefix`,
// `StateKind`, `OverlapStage`, `input_slice`, `output_region`, `output_slots_mut`,
// `reserve_state`, `publish_state`, `commit_state`, `install_state`,
// `checked_state`, `checked_state_mut`, `take_state`, `widen` and the `inftrees.h`
// bounds -- are `pub(crate)` machinery and are deliberately absent, for the reason
// recorded above the module declaration.
//
// This creates no dynamic symbol.  A `pub use` is a Rust-level path and nothing
// more, so the 111-symbol parity diff is untouched by anything in this section.
pub use crate::types::{
    alloc_func, charf, code, free_func, gzFile, gzFile_s, gz_header, gz_headerp, in_func,
    internal_state, intf, out_func, uInt, uIntf, uLong, uLongf, voidp, voidpc, voidpf, z_crc_t,
    z_off64_t, z_off_t, z_size_t, z_stream, z_streamp, Byte, Bytef,
};

// The measured layout of the crate-private state prefix -- the four numbers
// `tests/abi_layout.rs` §9 needs in order to pin the REAL prefix rather than a
// look-alike mirror of it.  The prefix TYPE stays `pub(crate)`, because handing an
// integration test the type would hand it the ability to construct a block a C caller
// owns an opaque pointer to; the facts are safe to publish and the capability is not.
//
// Gated exactly as `StatePrefix` itself is, and named in `cbindgen.toml`'s
// `[export] exclude` so it reaches no generated header.  Like every other `pub use`
// here it creates no dynamic symbol.
#[cfg(feature = "libz-compat")]
pub use crate::types::{state_prefix_layout, StatePrefixLayout};

// The version identity, re-exported from the module that owns it. Gated exactly
// as `util` is: with `libz-compat` off there is no introspection module to take
// it from, and a `--no-default-features` build must still compile.
#[cfg(feature = "libz-compat")]
pub use crate::util::{
    ZLIB_VERNUM, ZLIB_VERSION, ZLIB_VER_MAJOR, ZLIB_VER_MINOR, ZLIB_VER_REVISION,
    ZLIB_VER_SUBREVISION,
};

// ---------------------------------------------------------------------------
//  Crate-root re-exports -- the exported C functions
// ---------------------------------------------------------------------------
//
// The ninety-three contract functions this crate defines, lifted to the crate
// root so that a Rust consumer writes `z::deflate` -- and so that it CAN: the
// export modules are private, so without these lines a consumer outside the crate
// cannot name `deflate` at all (E0603) and would have to redeclare it
// `extern "C"` by hand, which proves nothing about this library because a hand
// written declaration compiles whether or not the definition agrees with it.
// Every Rust consumer in the workspace depends on this list: the sibling
// `tests/`, `zlib-rs-differential`'s byte-identity and interoperability suites,
// the `fuzz/` targets and the benches.
//
// This creates no dynamic symbol.  A `pub use` is a Rust-level path and nothing
// more, so the 111-symbol parity diff is untouched by anything in this section --
// which is precisely why the list can be curated for Rust consumers without
// disturbing what `zlib.map` publishes to C.
//
// THREE names are exported as symbols and deliberately absent from this list,
// because all three are hidden from the shared library's dynamic table and none
// of them is any consumer's contract:
//
//   * `inflate_table` -- `zlib.map`'s `local:` block hides it; its one caller is
//     the unmodified `test/infcover.c`, which links `libz.a` and includes
//     `inftrees.h` for the declaration.
//   * `_zlib_rs_gzprintf_begin` and `_zlib_rs_gzprintf_commit` -- covered by the
//     same block's `_*` wildcard; their one caller is `csrc/gzprintf_shim.c`.
//
// The counts below are per module and are asserted, target by target, by
// `tests/rust_api_surface.rs`; they reconcile to the 95-symbol export tally the
// way the crate documentation sets out -- 96 `#[no_mangle]` items compiled on this
// target, minus those three hidden names, plus `gzprintf` and `gzvprintf` from the
// C shim.
//
// Gated exactly as the modules they come from: `libz-compat` for all of them and
// additionally `gz` for the `gz*` family, so that `--no-default-features` remains
// the supported configuration it is documented to be, with the surface simply
// absent rather than half-present.

/// The eleven Adler-32 and CRC-32 entry points — `zlib.h` L1809-L1901.
#[cfg(feature = "libz-compat")]
pub use crate::checksum::{
    adler32, adler32_combine, adler32_combine64, adler32_z, crc32, crc32_combine, crc32_combine64,
    crc32_combine_gen, crc32_combine_gen64, crc32_combine_op, crc32_z,
};

/// The ten one-shot wrappers and their `size_t` forms — `zlib.h` L1271-L1337.
#[cfg(feature = "libz-compat")]
pub use crate::compress::{
    compress, compress2, compress2_z, compressBound, compressBound_z, compress_z, uncompress,
    uncompress2, uncompress2_z, uncompress_z,
};

/// The seventeen `deflate*` entry points — `zlib.h` L254-L836.
///
/// `deflateInit` and `deflateInit2` are absent because `zlib.h` defines both as
/// macros over the `_`-suffixed functions; neither is a symbol, so neither can be
/// re-exported. A Rust caller performs the same version-and-layout handshake the
/// macros arrange by calling [`deflateInit_`] or [`deflateInit2_`] with
/// [`ZLIB_VERSION`] and `size_of::<z_stream>()`.
#[cfg(feature = "libz-compat")]
pub use crate::deflate::{
    deflate, deflateBound, deflateBound_z, deflateCopy, deflateEnd, deflateGetDictionary,
    deflateInit2_, deflateInit_, deflateParams, deflatePending, deflatePrime, deflateReset,
    deflateResetKeep, deflateSetDictionary, deflateSetHeader, deflateTune, deflateUsed,
};

/// The eighteen `inflate*` entry points — `zlib.h` L405-L1106.
///
/// `inflateInit` and `inflateInit2` are absent for the reason their deflate
/// counterparts are, and `inflate_table` for the reason recorded above.
#[cfg(feature = "libz-compat")]
pub use crate::inflate::{
    inflate, inflateCodesUsed, inflateCopy, inflateEnd, inflateGetDictionary, inflateGetHeader,
    inflateInit2_, inflateInit_, inflateMark, inflatePrime, inflateReset, inflateReset2,
    inflateResetKeep, inflateSetDictionary, inflateSync, inflateSyncPoint, inflateUndermine,
    inflateValidate,
};

/// The three `inflateBack*` entry points — `zlib.h` L1138-L1208 and L1913.
///
/// `inflateBackInit` is absent for the reason `deflateInit` is: `zlib.h` L1113
/// defines it as a macro over [`inflateBackInit_`].
#[cfg(feature = "libz-compat")]
pub use crate::infback::{inflateBack, inflateBackEnd, inflateBackInit_};

/// The four introspection entry points — `zlib.h` L224, L2035, L2062 and L2064.
#[cfg(feature = "libz-compat")]
pub use crate::util::{get_crc_table, zError, zlibCompileFlags, zlibVersion};

/// The thirty `gz*` entry points — `zlib.h` L1357-L1801 and L1966-L2013.
///
/// `gzprintf` and `gzvprintf` are absent because this crate does not define them:
/// stable Rust at the declared MSRV can neither define a C-variadic function nor
/// name a `va_list`, so the packaging link takes those two symbols from
/// `csrc/gzprintf_shim.c`. A Rust caller that needs formatted output writes the
/// bytes itself and calls [`gzwrite`].
///
/// `gzgetc` is here as a real function as well as being a `zlib.h` macro, and
/// `gzgetc_` beside it, exactly as the C library provides both: the macro
/// dereferences the caller-visible `gzFile_s` prefix and falls back to the
/// function, so both names have to exist.
#[cfg(all(feature = "libz-compat", feature = "gz"))]
pub use crate::gz::{
    gzbuffer, gzclearerr, gzclose, gzclose_r, gzclose_w, gzdirect, gzdopen, gzeof, gzerror,
    gzflush, gzfread, gzfwrite, gzgetc, gzgetc_, gzgets, gzoffset, gzoffset64, gzopen, gzopen64,
    gzputc, gzputs, gzread, gzrewind, gzseek, gzseek64, gzsetparams, gztell, gztell64, gzungetc,
    gzwrite,
};

/// The Windows-only wide-path entry point and the `wchar_t` alias its signature
/// needs — `zlib.h` L2042.
///
/// Gated on `windows` exactly as the definition is, so this target's export count
/// is thirty `gz*` functions and the Windows one is thirty-one. `zlib.h` declares
/// `gzopen_w` under `#if defined(_WIN32)`, so a caller on any other platform has
/// no declaration to link against either.
#[cfg(all(feature = "libz-compat", feature = "gz", windows))]
pub use crate::gz::{gzopen_w, wchar_t};

// ---------------------------------------------------------------------------
//  Crate-root re-exports -- the public constant surface
// ---------------------------------------------------------------------------
//
// `zlib.h` publishes its constants as `#define`s and `zconf.h` publishes two
// more, so a Rust consumer that wants to say `Z_FINISH` rather than `4` needs
// them as items.  They are declared here, at the barrel, because that is where a
// consumer looks and because no single export module owns them: the flush values
// are read by `deflate` and `inflate`, the strategies by `deflate`, the return
// codes by all seven.
//
// THREE decisions, each deliberate:
//
//   * The VALUE is the literal from the header, not an expression referring to
//     the core.  The header is the immutable contract, so quoting it directly is
//     the honest form -- and it is the only form cbindgen can render, because
//     `[parse] parse_deps = false` means cbindgen cannot resolve a `zlib_rs::…`
//     initialiser and would skip the constant with a warning, exactly as it
//     already does for the `panic_guard::fallback` constants typed on
//     `ReturnCode`, `c_ulong` and `usize`.
//
//   * The TYPE is `c_int`, because a bare integer `#define` used in `int`
//     position is an `int` in C.  `zconf.h` maps nothing else onto these, and
//     `layout_assertions` already pins `size_of::<c_int>() == 4` on LP64.
//
//   * AGREEMENT with the core is asserted at compile time, immediately after
//     each family.  The literal therefore cannot drift: if `zlib-rs` ever
//     disagreed with `zlib.h`, this crate would fail to build rather than export
//     a value the algorithms do not honour.  The nine return codes live in the
//     core as private constants reachable only through `ReturnCode`, which is
//     why those assertions go through `as_i32()` while the rest compare against
//     `zlib_rs::config`.

/// Successful completion — `zlib.h` L181.
pub const Z_OK: c_int = 0;
/// End of the compressed stream reached — `zlib.h` L182.
pub const Z_STREAM_END: c_int = 1;
/// A preset dictionary is needed before decompression can continue — `zlib.h` L183.
pub const Z_NEED_DICT: c_int = 2;
/// A file-system error occurred; consult `errno` — `zlib.h` L184.
pub const Z_ERRNO: c_int = -1;
/// The stream state is inconsistent, or a parameter was invalid — `zlib.h` L185.
pub const Z_STREAM_ERROR: c_int = -2;
/// The input data is corrupt or was not produced by a compatible encoder — `zlib.h` L186.
pub const Z_DATA_ERROR: c_int = -3;
/// Memory could not be allocated — `zlib.h` L187.
pub const Z_MEM_ERROR: c_int = -4;
/// No progress was possible: no input consumed and no output produced — `zlib.h` L188.
pub const Z_BUF_ERROR: c_int = -5;
/// The caller's `ZLIB_VERSION` is incompatible with the library's — `zlib.h` L189.
pub const Z_VERSION_ERROR: c_int = -6;

// The nine return codes, held to the core's `ReturnCode` newtype.  A mismatch
// here would mean an exported function returning a value the algorithms never
// produce, which is why it is a build failure rather than a test.
/// cbindgen:ignore
const _: () = {
    assert!(Z_OK == ReturnCode::OK.as_i32());
    assert!(Z_STREAM_END == ReturnCode::STREAM_END.as_i32());
    assert!(Z_NEED_DICT == ReturnCode::NEED_DICT.as_i32());
    assert!(Z_ERRNO == ReturnCode::ERRNO.as_i32());
    assert!(Z_STREAM_ERROR == ReturnCode::STREAM_ERROR.as_i32());
    assert!(Z_DATA_ERROR == ReturnCode::DATA_ERROR.as_i32());
    assert!(Z_MEM_ERROR == ReturnCode::MEM_ERROR.as_i32());
    assert!(Z_BUF_ERROR == ReturnCode::BUF_ERROR.as_i32());
    assert!(Z_VERSION_ERROR == ReturnCode::VERSION_ERROR.as_i32());
};

/// Accumulate input without forcing output — `zlib.h` L172.
pub const Z_NO_FLUSH: c_int = 0;
/// Flush to a byte boundary, then continue — `zlib.h` L173.
pub const Z_PARTIAL_FLUSH: c_int = 1;
/// Flush and align to a byte boundary with an empty stored block — `zlib.h` L174.
pub const Z_SYNC_FLUSH: c_int = 2;
/// Flush, align, and reset the compression state so decoding can restart here — `zlib.h` L175.
pub const Z_FULL_FLUSH: c_int = 3;
/// Finish the stream: no further input will be supplied — `zlib.h` L176.
pub const Z_FINISH: c_int = 4;
/// Stop at a block boundary, for `inflate` — `zlib.h` L177.
pub const Z_BLOCK: c_int = 5;
/// Stop when the deflate block header has been decoded — `zlib.h` L178.
///
/// Valid for `inflate` only. `deflate` rejects it: `zlib.h` documents the
/// allowed deflate flush values as `Z_NO_FLUSH` through `Z_BLOCK`, and passing
/// `Z_TREES` to `deflate` yields `Z_STREAM_ERROR`.
pub const Z_TREES: c_int = 6;

// The seven flush values, held to the core's `Flush` parameter constants.
/// cbindgen:ignore
const _: () = {
    assert!(Z_NO_FLUSH == core_config::Z_NO_FLUSH);
    assert!(Z_PARTIAL_FLUSH == core_config::Z_PARTIAL_FLUSH);
    assert!(Z_SYNC_FLUSH == core_config::Z_SYNC_FLUSH);
    assert!(Z_FULL_FLUSH == core_config::Z_FULL_FLUSH);
    assert!(Z_FINISH == core_config::Z_FINISH);
    assert!(Z_BLOCK == core_config::Z_BLOCK);
    assert!(Z_TREES == core_config::Z_TREES);
};

/// Store only, no compression — `zlib.h` L194.
pub const Z_NO_COMPRESSION: c_int = 0;
/// Fastest compression — `zlib.h` L195.
pub const Z_BEST_SPEED: c_int = 1;
/// Smallest output — `zlib.h` L196.
pub const Z_BEST_COMPRESSION: c_int = 9;
/// Let the library choose; equivalent to level 6 — `zlib.h` L197.
pub const Z_DEFAULT_COMPRESSION: c_int = -1;

/// Tuned for data produced by a filter or predictor — `zlib.h` L200.
pub const Z_FILTERED: c_int = 1;
/// Never look for matches; emit Huffman-coded literals only — `zlib.h` L201.
pub const Z_HUFFMAN_ONLY: c_int = 2;
/// Limit match distances to one, for run-length encoding — `zlib.h` L202.
pub const Z_RLE: c_int = 3;
/// Forbid dynamic Huffman codes, for a simpler decoder — `zlib.h` L203.
pub const Z_FIXED: c_int = 4;
/// The normal strategy — `zlib.h` L204.
pub const Z_DEFAULT_STRATEGY: c_int = 0;

// The four levels and the five strategies, held to the core's parameter
// constants.  These feed `deflateInit2_`, `deflateParams` and `deflateTune`, and
// the level in particular selects a row of the configuration table that decides
// the emitted bytes -- so a divergence would break byte-identical output.
/// cbindgen:ignore
const _: () = {
    assert!(Z_NO_COMPRESSION == core_config::Z_NO_COMPRESSION);
    assert!(Z_BEST_SPEED == core_config::Z_BEST_SPEED);
    assert!(Z_BEST_COMPRESSION == core_config::Z_BEST_COMPRESSION);
    assert!(Z_DEFAULT_COMPRESSION == core_config::Z_DEFAULT_COMPRESSION);
    assert!(Z_FILTERED == core_config::Z_FILTERED);
    assert!(Z_HUFFMAN_ONLY == core_config::Z_HUFFMAN_ONLY);
    assert!(Z_RLE == core_config::Z_RLE);
    assert!(Z_FIXED == core_config::Z_FIXED);
    assert!(Z_DEFAULT_STRATEGY == core_config::Z_DEFAULT_STRATEGY);
};

/// `z_stream::data_type`: the input looks like binary data — `zlib.h` L207.
pub const Z_BINARY: c_int = 0;
/// `z_stream::data_type`: the input looks like text — `zlib.h` L208.
pub const Z_TEXT: c_int = 1;
/// Retained spelling of [`Z_TEXT`], for callers written against 1.2.2 and earlier — `zlib.h` L209.
pub const Z_ASCII: c_int = Z_TEXT;
/// `z_stream::data_type`: the classification is not yet decided — `zlib.h` L210.
pub const Z_UNKNOWN: c_int = 2;

/// The DEFLATE method, the only compression method this format defines — `zlib.h` L213.
pub const Z_DEFLATED: c_int = 8;

// The three data-type values, their compatibility alias, and the method.
// `detect_data_type` in the core is what actually produces the first three, so
// the assertion ties the reported `data_type` to the code that computes it.
/// cbindgen:ignore
const _: () = {
    assert!(Z_BINARY == core_config::Z_BINARY);
    assert!(Z_TEXT == core_config::Z_TEXT);
    assert!(Z_ASCII == core_config::Z_ASCII);
    assert!(Z_UNKNOWN == core_config::Z_UNKNOWN);
    assert!(Z_DEFLATED == core_config::Z_DEFLATED);
};

/// The null value for `zalloc`, `zfree` and `opaque` — `zlib.h` L216.
///
/// `zlib.h` spells it `#define Z_NULL 0`, an untyped zero used in both integer
/// and pointer position, so the Rust form is the integer and this constant
/// exists for parity and for reading C code alongside this crate. Rust callers
/// should not use it to build a `z_stream`: the hook types are
/// `Option<unsafe extern "C" fn(…)>`, so a null hook is [`None`], and `opaque`
/// is a raw pointer, so a null opaque is `core::ptr::null_mut()`. Modelling the
/// hooks as `Option` is what lets the compiler prove a null hook was considered.
pub const Z_NULL: c_int = 0;

/// The largest `windowBits` `deflateInit2_` and `inflateInit2_` accept — `zconf.h` L287.
///
/// A 32 KiB LZ77 window. `zconf.h` guards the definition with `#ifndef`, so a
/// caller may compile against a smaller window — and warns that reducing it
/// makes `minigzip` unable to extract files produced by `gzip`. This library is
/// built at the full 15, which is what the installed header declares.
pub const MAX_WBITS: c_int = 15;

/// The largest `memLevel` `deflateInit2_` accepts — `zconf.h` L277.
///
/// `zconf.h` defines 8 under `MAXSEG_64K` — the 16-bit segmented configurations,
/// none of which is a supported target here — and 9 otherwise, which is the
/// value this library is built with.
pub const MAX_MEM_LEVEL: c_int = 9;

// The two `zconf.h` bounds, held to the core's validation limits.  These are the
// values `deflateInit2_` and `inflateInit2_` range-check their arguments
// against, so a divergence would accept a configuration the core cannot honour
// or reject one it can.
/// cbindgen:ignore
const _: () = {
    assert!(MAX_WBITS == core_config::MAX_WBITS);
    assert!(MAX_MEM_LEVEL == core_config::MAX_MEM_LEVEL);
};
