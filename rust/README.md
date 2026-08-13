# The Rust implementation of zlib -- developer guide

This is the developer guide for the memory-safe Rust implementation of this
library. It is a reimplementation, not a wrapper, not a binding and not a
mechanical transpilation of the C sources: every algorithm was written again in
safe Rust so that it reproduces the reference implementation's observable
behaviour exactly, with the C translation units in the root directory serving as
the algorithmic oracle it is differentially tested against. Those sources are
read and never edited, and the C build stays green so that the oracle keeps
working.

This file is for people working *on* the port. The top-level `README` is the user
document -- how to build and install the library, what the Cargo features do,
and what a consumer has to know -- and `FAQ` questions 45 to 47 carry the
summary-level answers. Nothing is duplicated between them on purpose: what
follows is the deep version of the same material, plus the two things a
contributor needs that a user does not, namely which encoder decisions may never
be changed and where `unsafe` is allowed to exist.

`zlib.h` remains the API documentation of record. No signature is restated here;
individual functions are named only where naming one makes a point.

Nothing here is a substitute for the comment blocks in the files themselves. Each
manifest, build script, module and test carries the derivation of its own rules,
so that the reasoning stays next to the code it governs; this file is the map,
not the territory. Where the two ever disagree, the file is right and this one
needs updating.

## What the workspace contains ##

The Cargo workspace sits at the repository root, beside the C sources. It is
purely additive: nothing was removed, nothing is vendored, and there are no git
submodules, because the reference implementation is already in this tree.

    Cargo.toml                    virtual workspace root: members, shared package
                                  metadata, dependency pins, lints, release profile
    Cargo.lock                    committed -- this ships as a system library, so
                                  builds have to be reproducible
    rust-toolchain.toml           channel, components, and the supported-target matrix
    .rustfmt.toml                 formatting policy
    clippy.toml                   lint thresholds; deliberately no msrv key, so
                                  clippy reads each package's own rust-version
    deny.toml                     cargo-deny licence, advisory, ban and source policy
    cbindgen.toml                 header-generation config for the verification artifact

    crates/zlib-rs/               the safe core
    crates/libz-rs-sys/           the C ABI facade
    crates/zlib-rs-differential/  dev-only oracle harness
        corpus/minimal/           the committed differential corpus
        corpus/fetch_silesia.sh   opt-in performance corpus, fetched by hand
        corpus/silesia.pin        the approved archive's identity and bounds
        corpus/silesia.sha256     its twelve members' digests

    benches/                      three criterion suites; its own excluded package
    fuzz/                         five cargo-fuzz targets; its own excluded package

    rust/README.md                this file

| Path | Role |
|---|---|
| `crates/zlib-rs` | The safe core. `#![forbid(unsafe_code)]`, `no_std` plus `alloc`, and an empty `[dependencies]` table. Every algorithm lives here. Package `zlib-rs`, library target `zlib_rs`, an rlib and nothing else. |
| `crates/libz-rs-sys` | The C ABI facade, and the only crate in the shipped library that writes `unsafe`. `[lib] name = "z"` with `crate-type = ["cdylib", "staticlib", "rlib"]`, which is why cargo emits `libz.so` and `libz.a` rather than Rust-flavoured names. The `rlib` is what lets the differential crate, the benchmarks, the fuzz targets and its own test suites import it as `libz_rs_sys`. It has a `build.rs`, and three small C files under `csrc/`: two are archived into `libz.a` for the variadic `gzprintf`/`gzvprintf` pair and for a prototype stable Rust cannot express, and the third is a test-only probe that calls `gzvprintf` the way a real C consumer does, since stable Rust cannot even construct a `va_list`. |
| `crates/zlib-rs-differential` | Dev-only oracle harness, `publish = false`. Its build script compiles the in-tree C translation units into a test-only archive, so both implementations are co-resident in one process and comparing them is a memory compare. `cc` is a build-dependency of this crate alone. It has an unsafe boundary of its own -- `src/oracle.rs` declares the C entry points, `src/port.rs` wraps them, and `src/retain.rs` carries the lifetime-bearing `Session` type through which every retained pointer is handed over -- which is why the benchmarks and the fuzz targets need none. |
| `benches/` | Three criterion suites, `deflate_bench.rs`, `inflate_bench.rs` and `checksum_bench.rs`. Its own package `zlib-rs-benches` with its own `Cargo.lock` and its own `deny.toml`. |
| `fuzz/` | Five cargo-fuzz targets, plus `src/lib.rs` -- a shared `zlib_rs_fuzz` library holding the reachability reporting the seed gate reads -- and `seeds/`, a committed per-target seed corpus with the generator that produced it. Its own package `zlib-rs-fuzz` with its own `Cargo.lock` and its own `deny.toml`. |

The module tree inside `crates/zlib-rs` is organised around the C translation
units -- `deflate/`, `inflate/`, `trees/`, `adler32/`, `crc32/`, `gz/`,
`infback.rs`, `compress.rs`, `uncompress.rs` -- rather than being a strict
one-for-one map, and the departures are deliberate. Two are FOLDINGS that follow
the C include graph rather than a preference: `trees.c` includes only
`deflate.h`, so the Huffman coder is a sibling of `deflate/` operating on the
same state rather than an independent module; and `inftrees.c` and `inffast.c`
share an include set with `inflate.c` exactly, so they are
`inflate/inftrees.rs` and `inflate/inffast.rs`. One is a SPLIT: `zutil.c` is a
grab-bag of allocator, error-table and introspection concerns, so it becomes
`allocate.rs` and `error.rs`, with `zlibVersion`, `zlibCompileFlags`, `zError`
and `get_crc_table` surfacing through the facade's `util.rs` (AAP §0.8.2
ambiguity 7). And several modules answer to no C translation unit at all,
because they exist to make the port safe rather than to mirror it:
`config.rs`, `read_buf.rs`, `weak_slice.rs`, each `state.rs`,
`inflate/mode.rs`, and the per-concern files under `deflate/` and `trees/`
(`window.rs`, `hash_chain.rs`, `longest_match.rs`, `pending.rs`,
`bit_writer.rs`, `build.rs` and the five `deflate/algorithm/` strategies) --
which in the C sources are `local` functions inside one 2,185-line file. The generated tables in `crc32.h`, `trees.h` and
`inffixed.h` become `const` arrays, which is what removes the run-time table
construction and the `<stdatomic.h>` dependency along with it.

`crates/zlib-rs-differential` appears in the `[dependencies]` of neither shipped
crate. That is what keeps reference zlib an oracle rather than a build
dependency: `cargo build --release -p zlib-rs -p libz-rs-sys` compiles none of the
fifteen reference translation units. It does still run a C compiler, for the two
shims in `crates/libz-rs-sys/csrc/` -- see "With cargo" below.

### Two members are excluded, and each for its own reason ###

`benches/` and `fuzz/` are both named in the root manifest's `exclude`, and a
reader who assumes symmetry there will look for things in the wrong place.

`benches/` is excluded because criterion's floor is not the library's floor and a
workspace has exactly one dependency graph. criterion 0.8.2 declares
`rust-version = "1.86"` and reaches clap crates whose manifests are
`edition = "2024"`, which cargo 1.80 cannot parse at all -- so while the suites
were attached to `crates/zlib-rs-differential` the workspace declared a 1.80
floor that 1.80 could not resolve, which is a claim rather than a floor. The
three suites are therefore `[[bench]]` targets of the `zlib-rs-benches` package
that lives in that directory, **not** of any workspace member, and they are
reached with an explicit manifest path:

    cargo bench --locked --manifest-path benches/Cargo.toml

A bare `cargo bench` at the repository root finds no bench target at all. The
suites measure against the same oracle *source* -- they reach it through a path
dependency on the differential crate, whose build script compiles the in-tree C
translation units -- so the comparison is against the same C code the correctness
gates use.

What that does **not** mean is that the archive is compiled only once. `benches/`
is a separate package with its own `Cargo.lock`, and several gates deliberately
run in their own `CARGO_TARGET_DIR` (the MSRV check, the sanitizer passes, the
feature permutations, the Miri jobs) because `crates/libz-rs-sys` sets `[lib] name
= "z"` and cargo writes an unhashed `libz.rlib` that two differently-configured
builds would otherwise fight over. Each of those target directories compiles the
oracle archive for itself. That is the cost of keeping the builds from poisoning
each other, and it is worth knowing before wondering why a full local run is not
as incremental as it looks.

`fuzz/` is excluded because that is how cargo-fuzz is laid out. It is reached
from inside the directory, or with the same kind of manifest path.

One consequence applies to both: a workspace lint table does not cross a
workspace boundary, so `cargo clippy --workspace` cannot reach either package.
Each therefore restates the lint policy in `[lints.rust]` and `[lints.clippy]`
tables of its own, and CI runs clippy, rustfmt and cargo-deny a second and third
time with the corresponding manifest path.

## Toolchain and MSRV ##

`rust-toolchain.toml` pins `channel = "stable"` with `rustfmt` and `clippy`, so
cargo selects a suitable compiler by itself inside this checkout. It is
deliberately not nightly: nightly appears in this port only for Miri and for
`-Zsanitizer=address`, and both are invoked with an explicit `cargo +nightly`
that overrides the pin.

    rustup toolchain install stable  --profile minimal --component rustfmt,clippy
    rustup toolchain install nightly --profile minimal --component miri,rust-src

Three command-line tools are pinned rather than floating, because a different
release changes the output the gates compare or the policy they enforce:

    cargo install cbindgen --locked --version 0.29.4
    cargo install cargo-fuzz --locked --version 0.13.2
    cargo install cargo-deny --locked --version 0.20.2

The third is needed by the `cargo deny check` commands further down and by the
`deny` job, which runs it once per policy graph -- three of them, since `fuzz/`
and `benches/` are excluded packages with their own `deny.toml`.

All three workspace members declare `rust-version = "1.80"` and
`edition = "2021"` through `[workspace.package]`, so the MSRV is a property of
the whole workspace rather than of a hand-picked package list, and both of these
hold:

    cargo +1.80 metadata --locked
    cargo +1.80 check --locked --workspace --all-targets

Edition 2021 is what AAP §0.7.1(h) requires and the latest edition compatible with
a 1.80 floor, rather than a conservative choice: edition 2024 requires rustc 1.85 or
newer and cargo refuses that combination outright, so raising the edition means
raising the floor first. The older editions remain valid at this floor and are
simply worse.
`make rust-msrv` is where that is enforced locally, and it is a **check**, not a
build and not a test run: `cargo +1.80 metadata --locked`, then
`cargo +1.80 check --locked --workspace --all-targets`, then the same check over
the two shipped crates with `--all-features`, all three with
`RUSTFLAGS=-D warnings`. It builds no artifact, runs no test, and does not reach the
two excluded packages. The `msrv` job of `.github/workflows/rust.yml` goes
further and *compiles* the shipped pair on 1.80 in all three feature
configurations -- default, `--all-features` and `--no-default-features` -- because
a check does not run codegen and codegen is where an MSRV break can still hide.
`benches/` is the one package with a higher floor, for the reason given above.

`rust-toolchain.toml` also carries the supported-target matrix, and it is worth
reading before making a platform claim. Stated exactly, because the difference
between these lines is the difference between a claim and a measurement.
`x86_64-unknown-linux-gnu` is the only target the port is built, tested, packaged,
measured *and* benchmarked on -- it is where every gate runs. macOS and Windows are
built and tested by the `build-test` matrix, and their PACKAGED artifacts are gated
by one job each:

* `platform-abi-macos` measures the `-install_name` the build script emits, stages
  the dylib from the complete archive with an exported-symbols list derived from
  `zlib.h` (Mach-O's analogue of `zlib.map`), gates its export table, and relinks
  and runs `example.c`, `minigzip.c` and `infcover.c` unmodified with an `otool -L`
  proof that no system zlib is bound.
* `platform-abi-windows` reads the PE export table, requires `gzopen_w` -- which no
  Linux job can see, since it is declared only under `_WIN32` -- and has MSVC
  compile a C consumer plus the unmodified `test/minigzip.c` against cargo's import
  library and run them. That consumer also expands the `gzgetc` MACRO, so the
  caller-visible `gzFile_s` layout is checked by a second compiler against a library
  rustc built. A packaged Windows *install* stays out of scope: its mechanism is
  `win32/zlib.def`, which the AAP excludes, and the visible consequence is that
  `z.dll` does not export `gzprintf`/`gzvprintf`.

macOS additionally gets the CMake **static** Rust path: the `macOS Clang Rust` row
of `.github/workflows/cmake.yml` configures `-DZLIB_BUILD_RUST=ON
-DZLIB_BUILD_SHARED=OFF` and runs the CTest suite against the Rust `libz.a`.
CMake's SHARED half is refused rather than attempted -- `CMakeLists.txt` fails the
configuration on Apple, AIX, SunOS and not-UNIX, because the relink needs `-soname`
and `--version-script`.

Still unverified off Linux: the header comparison, the sanitizers, Miri, the
differential sweeps and the benchmarks. Treat any performance number as
Linux-only. Every other triple is not built at all. "Tier 1" names a rustc platform
tier, not a commitment made here.

## Building ##

Three entry points reach the same code. Which one to use depends on whether you
want a Rust artifact, an installable library, or a CMake build tree.

### With cargo ###

    cargo build --release
    cargo build --release -p libz-rs-sys --features libz-compat

The first builds the two shipped crates with their default features, which
already include the C ABI layer. The second names that layer explicitly, which is
the form to use when features are being chosen by hand, and it names the package
too: this is a virtual workspace, so a bare `--features libz-compat` leaves cargo
to decide which selected package the feature was meant for.

A bare `cargo build` selects `default-members`, which is the shipped pair.
`--workspace` also reaches `crates/zlib-rs-differential`, whose build script
compiles the fifteen in-tree C reference translation units -- correct, but
slower.

A C COMPILER AND AN ARCHIVER ARE PREREQUISITES OF THE SHIPPED BUILD TOO, and
that is worth stating plainly because the oracle boundary is easy to read as "no
C is ever compiled". `crates/libz-rs-sys/build.rs` compiles the two translation
units in its own `csrc/` -- the variadic `gzprintf`/`gzvprintf` pair and
`inflate_table`'s `code`-typed prototype, neither of which stable Rust can
declare -- and archives them into the `libz.a` cargo emits, plus a third,
test-only probe that reaches test targets alone. So `cc` and `ar` (or `cl.exe`
and `lib.exe`) are needed by both commands above; `cargo build -p zlib-rs` is
the only build in this workspace that needs neither. What the differential
crate's dev-only boundary buys is that the REFERENCE IMPLEMENTATION is an oracle
rather than a dependency, which is a different claim.

Read those as the first step of one recipe rather than as the whole of it.
**`cargo build` alone does not produce an installable shared library.** The
shortfall is measured, not theoretical. These are this tree's numbers; the right
column is what `make rust-symbols` re-derives and diffs against a built C library,
and the left column is one command --
`nm -D --defined-only --extern-only target/release/libz.so`:

| | `target/release/libz.so`, straight from cargo | `target/dropin/libz.so.1.3.2.1-motley`, after `make rust` |
|---|---|---|
| dynamic globals | 93 | **111**, matching the C library exactly |
| of which contract functions | 93 of the 95 | **95** |
| symbol version nodes | **0** | **16** |
| `gzprintf`, `gzvprintf` | absent | present |
| `inflate_table`, `_zlib_rs_gzprintf_begin`, `_zlib_rs_gzprintf_commit` | hidden | hidden |

Two causes, two remaining symptoms, and neither cause is a bug in the crate. rustc
attaches its own anonymous version script to every cdylib link and a second one
cannot be layered on top -- passing `--version-script` there is answered with
`attempt to reassign symbol ... of VER_NDX_GLOBAL to version` and produces **no**
nodes -- so `zlib.map` never reaches that link, which costs the 16 nodes.
Separately, the two variadic entry points are compiled from `csrc/` because
`core::ffi::VaList` is unstable on the 1.80 floor *and* on current stable; their
objects are archived into `libz.a` but nothing in the cdylib references them, so
they get no dynamic entry. So the gap is exactly **2 functions and 16 version
nodes**, and both halves are structural.

A third symptom used to sit beside them -- `zlib.map` not reaching the cdylib also
left the three internal helpers visible there -- and that one was avoidable and is
gone: `.hidden` directives in `crates/libz-rs-sys/src/lib.rs` give all three ELF
`STV_HIDDEN` visibility, which no version script can undo, and that took the cdylib
from 96 dynamic globals to 93. Static linking is untouched, because hidden
visibility does not reach a static archive, so `test/infcover.c` still resolves
`inflate_table` against `libz.a`.

So: cargo's **`libz.a` is already complete and installable** -- those objects are
in it, verified with `nm` -- while the shared library needs one further relink
that applies `zlib.map` and `-soname`. Exactly one command produces the
installable pair:

    make rust        # stages libz.a and the libz.so.1.3.2.1-motley chain in target/dropin/

`cmake --build build` with `-D ZLIB_BUILD_RUST=ON` performs the same relink for
the CMake install tree. Verify the ABI against `target/dropin`, never against
`target/release`; `crates/libz-rs-sys/src/lib.rs` carries the artifact matrix that
states this once, and `crates/libz-rs-sys/tests/dropin_chain.rs` refuses a staged
tree whose chain or SONAME is wrong.

### With make ###

Seven targets, and the comment at the top of `Makefile.in` describes what each
one does and does not establish:

    make rust          # stage the drop-in libz under target/dropin/
    make rust-test     # link the unmodified C drivers against it and run them
    make rust-symbols  # diff the staged symbol set against a built C library
    make rust-pc       # emit the pkg-config descriptor and gate a consumer
    make rust-header   # regenerate the C header with cbindgen and gate it
    make rust-msrv     # cargo check the workspace and the shipped pair on 1.80
    make rust-clean    # remove what the Rust targets produced

Sequencing is worth stating. The `Makefile` shipped in this tree is a stub that
asks you to run `./configure` first, but it forwards all seven Rust targets to
`Makefile.in` rather than refusing them, so they work on an unconfigured tree:
they name the staged library from `ZLIB_VERSION` in `zlib.h` rather than from
`Makefile.in`'s unconfigured default, and they supply the relink flags
themselves. What `./configure` adds is a `$(CARGO)` recorded for you and the C
library that `make rust-symbols` can then diff against, so the documented happy
path is still `./configure` and then `make rust`. On an unconfigured tree, name
the tool yourself with `make rust CARGO=cargo`.

When cargo is absent, `configure` leaves `CARGO` empty and says so, and the C
build is entirely unaffected -- no C target reads that variable. A machine with no
Rust toolchain builds, tests and installs the C library exactly as before.

### With CMake ###

    cmake -S . -B build -D ZLIB_BUILD_RUST=ON
    cmake --build build

The option is off by default, and while it is off no Rust tool is probed. This is
how `README-cmake.md` lists it, in CMake's own `NAME=<default> -- <help text>`
form:

    ZLIB_BUILD_RUST=OFF -- Use the memory-safe Rust libz for the exported ZLIB::* targets

Read as an instruction that line says the opposite of what it means, so read it as
what it is: `OFF` is the shipped **default**, and it selects the **unchanged C
build** -- the C sources, the C `libz.so` and `libz.a`, and no cargo invocation,
no Rust toolchain probe and no `rust-toolchain.toml` at all. `ON` is what selects
the Rust implementation, which is why both commands above pass
`-D ZLIB_BUILD_RUST=ON`.

The mechanism is worth one paragraph, because it is the reason the C test drivers
need no edits. The option publishes the Cargo artifacts and **re-points the
exported `ZLIB::ZLIB` and `ZLIB::ZLIBSTATIC` targets at them**. Everything
`test/CMakeLists.txt` registers links one of those two aliases and nothing else,
so `example.c`, `minigzip.c` and `infcover.c` are compiled from the same
unmodified sources either way and simply resolve to the other library. The C
targets `zlib` and `zlibstatic` stay defined and built regardless, because they
are the differential oracle and `contrib/` consumes them by name, and the
`find_package` component list is unchanged at `shared` and `static`.

Note what that does and does not give you: the suite runs against *one*
implementation per configure, the one the option selected, so a single build tree
does not compare them side by side.

This path is covered in CI twice over. The `cmake-rust` job of
`.github/workflows/rust.yml` walks it in the order a consumer meets it --
configure with the option on, build, `ctest`, install, then consume the install
tree through `find_package` and `pkg-config` -- so a break here fails on push
rather than waiting to be found by hand. It and then configures a SECOND build
tree with `-DZLIB_INSTALL=OFF` and runs `ctest` there too, because the CTest cases
that consume this project by `add_subdirectory` must pass without an installation
and once did not: their generated source trees sat inside the `if(ZLIB_INSTALL)`
guard while the cases themselves were registered unconditionally, so with
installation off two cases failed on a directory nothing had created. That gate
asserts in both directions -- the six `add_subdirectory` cases present, and zero
install-consuming cases registered -- so neither half can drift.

`.github/workflows/cmake.yml` is **not** a C-only matrix, and reading it as one is
a mistake this file used to encourage: it carries four `rust: yes` rows built
through the same `CMakeLists.txt` with `-DZLIB_BUILD_RUST=ON`, which is what keeps
the Rust path's install rules, exported targets and CTest registrations from
rotting across compilers. Its macOS Rust row is static-only, because `CMakeLists`
refuses a Rust *shared* library on non-ELF formats, and it carries no Windows Rust
row at all; `rust.yml`'s own `platform-abi-macos` and `platform-abi-windows` jobs
are where those two platforms' packaged artifacts are gated instead.
 And `.github/workflows/cmake.yml`, whose
matrix is otherwise the C library's, carries four `-DZLIB_BUILD_RUST=ON` rows at
the end of it: `Ubuntu GCC Rust`, `Ubuntu GCC Rust no shared`, `Ubuntu Clang Rust`
and `macOS Clang Rust` (static-only, because the shared relink is ELF-only). Each
builds and runs the CTest suite, so the block that publishes and re-points the
exported targets is exercised by both generators of coverage rather than by one.

**Shared linking through this option is ELF-only; static linking is not, and the
distinction is the whole of it.** A shared build has to be relinked with `-soname`
and `--version-script`, which is the same platform set that gets `zlib.map` for the
C shared library, so `ZLIB_BUILD_SHARED` with the option on stops at configure time
on Apple, AIX, SunOS and non-UNIX rather than emitting a half-formed dylib or
import library. `-DZLIB_BUILD_RUST=ON -DZLIB_BUILD_SHARED=OFF` needs none of that
-- cargo emits a complete archive, and it is copied under the platform's own
archive name -- so static-only mode is supported wherever cargo runs, and that is
exactly what the macOS row above exercises. A multi-configuration generator and a
cross-compilation toolchain are refused for their own reasons, both of them
unrelated to ELF.

`README-cmake.md` carries the full cache-option list and the static-link
advertisement this option adds; it is not duplicated here.

## The Cargo feature surface ##

Every feature every member declares, and what each one expands to. `cargo
metadata` is the authority; this table is the convenience the top-level `README`
points at.

| Crate | Feature | Default | Expands to |
|---|---|---|---|
| `zlib-rs` | `default` | -- | nothing; the core is `no_std` plus `alloc` with no feature on |
| `zlib-rs` | `rust-api` | off | the idiomatic Rust surface -- typed configs and slice-based helpers |
| `zlib-rs` | `simd` | off | two optional checksum backends: a vectorised Adler-32 and a scalar stride-braid CRC-32 |
| `zlib-rs` | `std` | off | the core's file I/O; without it the core is `no_std` |
| `libz-rs-sys` | `default` | on | `libz-compat`, `gz` |
| `libz-rs-sys` | `libz-compat` | on, via `default` | nothing further; it gates the exported `extern "C"` surface |
| `libz-rs-sys` | `gz` | on, via `default` | `libz-compat`, `zlib-rs/std`, `dep:libc` |
| `libz-rs-sys` | `libc` | off | `dep:libc` alone |
| `libz-rs-sys` | `simd` | off | `zlib-rs/simd` -- a forwarder, so a package-scoped request reaches the core |
| `zlib-rs-differential` | `default` | on | `gz` |
| `zlib-rs-differential` | `gz` | on, via `default` | `libz_rs_sys/gz` |
| `zlib-rs-differential` | `simd` | off | `zlib-rs/simd`, `libz_rs_sys/simd` -- forwards to both |

Four consequences are worth stating outright, because a feature table read as a
flat list hides them.

`std` is off in `zlib-rs` but on in a default facade build, because `gz` enables
`zlib-rs/std`. The gz layer needs file I/O; the core does not.

`libc` is off as a named feature but present in a default build, for the same
reason: `gz` enables `dep:libc`. This is the one place the Rust library differs
from the C one on dependencies, so it is stated plainly rather than glossed. The
core's `[dependencies]` table is empty unconditionally, and every other crate the
port mentions is kept out of the artifact -- though the kinds differ, and calling
them all "dev-dependencies" would be wrong:

| Crate | Declared by | As |
|---|---|---|
| `cc` | `crates/zlib-rs-differential` | `[build-dependencies]` |
| `criterion` | `benches/` (package `zlib-rs-benches`) | `[dev-dependencies]`, `=0.8.2` |
| `libfuzzer-sys`, `arbitrary` | `fuzz/` (package `zlib-rs-fuzz`) | ordinary `[dependencies]`, `=0.4.13` and `=1.4.2` |

`libfuzzer-sys` and `arbitrary` are ORDINARY dependencies, because a fuzz target
is a normal binary that links them. What keeps all four out of `libz.so` and
`libz.a` is therefore not the dependency kind but the PACKAGE BOUNDARY: all three
declaring packages are `publish = false`, and none of them appears in the
dependency graph of either shipped crate -- so nothing they pull in is resolved for
a `cargo build -p zlib-rs -p libz-rs-sys` at all. That is the property to check
when adding one, not whether the table is spelled `dev-dependencies`. `libc` is different: it is a real, optional
dependency of the facade, and a default build links it, because a `gzFile` is a
real file descriptor and the descriptor-level calls and `O_*` constants behind it
are platform facts rather than algorithms. None of this puts third-party *code*
in the distribution -- nothing is vendored and there are no submodules, so the
statement at the foot of the top-level `README` about the sources in this tree
stands unchanged; `libc` is a declared dependency resolved from the registry at
build time, and `deny.toml` is what governs what may be declared. The explicit
`libc` feature exists so
that the dependency is a stable part of the facade's surface and a downstream
`--features libc` keeps resolving; enabling it alone changes nothing observable,
since every call site is inside gz code. `--no-default-features`,
`--features libz-compat` and every `zlib-rs`-only build resolve without it.

`simd` is declared by three crates, not one. The core has the implementation; the
facade and the differential crate declare forwarding features so that a
package-scoped `--features simd` reaches it.

The feature covers two backends, and **they are not the same kind of thing** --
so any sentence that describes them together gets one of them wrong. What they do
share: neither contains an architecture intrinsic, which is exactly what lets the
safe core keep `#![forbid(unsafe_code)]`; both are written so that LLVM has a
shape it *can* autovectorise at opt-level 3; and neither depends on a CPU probe
for correctness, so a `simd`-enabled binary computes the right answer on a machine
with no vector unit. Where they differ is whether any vector instruction actually
comes out, and on `x86_64` the answer is not the same for the two:

| | vector instructions in the release library | run-time probe |
|---|---|---|
| **Adler-32** (`adler32::simd::Simd`) | **91** -- `paddd`, `pshufd`, `punpck*`, SSE2 only, so no raised `-C target-cpu` baseline is needed | `is_supported()`, an SSE2 check that is a throughput hint and not a correctness gate |
| **CRC-32** (`crc32::simd::StrideBraid`) | **zero** -- 487 instructions of `mov`, `xor`, `shr` and `movzbl` | none at all |

The CRC-32 backend is therefore a **scalar** reformulation and is named for what
it is. Its type is `StrideBraid`, its `Crc32Backend::NAME` is `"stride-braid"`
and its entry point is `crc32_stride_braid`; the file is still `crc32/simd.rs`
and the Cargo feature is still `simd` because AAP §0.3.1 and §0.5.1.4 fix those
two names, and everything a reader can rename was renamed. It wins by exposing
instruction-level parallelism and by replacing the braided algorithm's serial
byte epilogue -- up to `N * W - 1` steps in `crc32.c` -- with at most `W - 1`.
Genuine vector CRC-32 needs the carry-less-multiply intrinsics, which
`#![forbid(unsafe_code)]` rules out, and stable Rust at the 1.80 MSRV has no
portable vector API to reach for instead; that is a divergence from AAP §0.4.1.2,
which described this path as a "carry-less-multiply" one.

Both wins are **measured and enforced** rather than asserted. On
`x86_64-unknown-linux-gnu`, candidate time over portable-backend time, each figure
the median of nine paired rounds run in alternating order:

| bytes | 64 | 256 | 4096 | 65536 | 1 MiB |
|---|---:|---:|---:|---:|---:|
| Adler-32 vs `generic` | 0.98 | 0.51 | 0.36 | 0.36 | 0.36 |
| CRC-32 vs `braid` | 0.24 | 0.46 | 0.90 | 0.97 | 0.99 |

The two shapes differ because the two wins have different sources. Adler-32 needs
enough data to fill its lanes, so it improves as inputs grow and merely breaks
even at 64 bytes. CRC-32 gains most between 55 and 256 bytes, where the epilogue
it removes is most of the work, and converges past 4 KiB where the braided body
dominates -- the honest expectation for two arrangements of the same table folded
over the same words, and the region that matters because 55 to 256 bytes is what a
`gzread` call actually checksums. `benches/checksum_bench.rs` emits both sweeps as
`BACKEND` lines and `.github/scripts/bench_gate.py` fails the `bench` job on two
conditions per family: the candidate may not be slower than its baseline at *any*
measured length, and it must be at least 10% faster at one of them. The second
condition exists because a backend that merely forwarded to its baseline would
satisfy the first one perfectly.

`ZLIB_RS_SIMD=1` in the environment is how make and CMake ask for the feature;
both translate it into `simd` for cargo, and `crates/libz-rs-sys/build.rs` reads
it under `cargo::rerun-if-env-changed=ZLIB_RS_SIMD` -- so flipping it triggers a
rebuild -- and then checks that the environment request and the feature request
agree rather than letting a mismatch pass unnoticed. A direct cargo build passes
`--features simd` instead.

The variable is an ASSERTION about the build rather than a switch the build
script can honour on its own -- a build script cannot enable a Cargo feature --
so the three cases are not two:

    ZLIB_RS_SIMD=1   asserts the simd feature is on.  With it off, the build
                     fails and names the feature to pass.
    ZLIB_RS_SIMD=0   asserts it is off.  With it on, the build fails and names
                     how to turn it off.
    unset            asserts nothing; the Cargo features as resolved decide.

So the default build -- no feature, no variable -- is scalar, but *unset* means
"no assertion" rather than "scalar": with `--features simd` and the variable
unset, the vectorised backends are built and nothing complains.

    ZLIB_RS_SIMD=1 cargo build --locked -p libz-rs-sys --features simd

Why the feature reaches the two checksums and nothing else: a checksum is one
scalar however it is computed, so no rearrangement of the computation -- vectorised
or merely re-braided -- can perturb the bitstream. Rearranging match-finding
*would* change which matches are emitted, and that is prohibited for the reason the
next section gives. Neutrality is checked rather than argued -- the crate's own
equivalence sweep, the differential suites and a 300-second fuzz run all execute
with the feature on.

## Byte-identical output: what a contributor may not change ##

**Read this section before touching anything under `crates/zlib-rs/src/deflate/`
or `crates/zlib-rs/src/trees/`.** It is the one place where the obvious
improvement is the regression, and it is the likeliest way for well-intentioned
work to break the port.

The requirement is that the encoder emit *the same bytes* as the C encoder, for
every configuration the differential matrix covers. That is much stricter than
conformance, and it is counter-intuitive, because RFC 1951 constrains the
**format** and not the **choices**. Which of several equally valid matches to
emit, when to abandon a lazy match, how to break a Huffman frequency tie and when
to switch block type are all implementation-defined. A fully conformant DEFLATE
encoder -- including a strictly better one -- produces a different byte stream and
fails here. zlib's specific answers are the specification, and they are ported
verbatim.

Eight decision points determine the output, and this list is the index the code
uses: the module that implements a point carries its number, so a comment saying
"decision point #3" means the third entry below.

    grep -rniE 'decision point' crates/zlib-rs/src

finds all eight -- #1 in `deflate/config_table.rs`, #2 and #3 in
`deflate/longest_match.rs`, #4 and #5 in `deflate/algorithm/slow.rs`, #6 to #8 in
`trees/build.rs` -- and several tests under `crates/zlib-rs/tests/` and
`crates/zlib-rs-differential/tests/` name the number they pin.

1. **The per-level tuning table.** `configuration_table[10]` supplies
   `{good_length, max_lazy, nice_length, max_chain}` and the strategy function for
   each level. It decides which matches are looked for at all, so any deviation
   changes the output at that level.
2. **The hash-chain walk in `longest_match`.** Chain length, the starting
   `best_len`, `nice_match`, and the `limit`/`NIL` rule that deliberately prevents
   a match against window index 0. Traversal order decides which of several
   equal-length matches wins, and therefore which distance is emitted.
3. **The early-rejection quartet inside that loop.** Four conditions, and the
   *order* matters because the test also advances the match pointer as it goes.
   The C comment records that one of the four is a heuristic that is not always a
   win -- so it is deliberately non-optimal, and it is reproduced as written.
4. **The `TOO_FAR` lazy-match rejection.** A three-byte match further away than
   4096 bytes is thrown away, as is any match of five bytes or fewer under
   `Z_FILTERED`, on the grounds that the distance costs more than the match saves.
   Pure heuristic, no RFC basis, and nothing in the format from which to re-derive
   it.
5. **The lazy-emit decision and the hash insertion that follows it.** This one is
   the most dangerous, because its effect is not local: it changes both the order
   of the emitted symbols *and* which positions enter the hash chain, so it
   perturbs every match found afterwards.
6. **Huffman tie-breaking.** Equal-frequency symbols are ordered by depth,
   inclusively. Break the tie the other way and the code lengths differ.
7. **The forced two-code case in `build_tree`.** When fewer than two symbols have
   a non-zero frequency, codes are created anyway, because the pkzip format
   requires at least one distance code to exist. A clean-room implementation would
   plausibly handle this differently.
8. **Block-type selection.** Integer-truncating comparisons between the dynamic,
   static and stored costs decide the three block-type bits. Port them with
   identical integer arithmetic: no floating point, and no algebraic reordering,
   because both change where the truncation lands.

### The standing prohibition ###

**No zlib-ng-style encoder improvement may be adopted, however much faster it
would be.** Better match selection, smarter lazy-match thresholds, a different
hash function or hash-chain strategy, and any other change to what the encoder
chooses are all out of scope -- not because they are bad ideas, but because each
one changes emitted bytes and the whole drop-in claim rests on those bytes being
the same.

The C sources also carry compile-time switches that change output: `FASTEST`,
`LIT_MEM`, `UNALIGNED_OK`, `FORCE_STATIC`, `FORCE_STORED` and `GEN_TREES_H`. The
port implements the **default shipped configuration** -- the one the C build
actually uses -- and deliberately does not expose any of them as a runtime or
feature option, because a build flag that changes emitted bytes is a second
implementation to verify.

### What optimisation is admissible ###

Everything output-neutral, which is more than it sounds:

* the two optional checksum backends behind the `simd` feature, one vectorised and
  one scalar;
* memory-copy strategy and buffer handling;
* branch layout and the shape given to LLVM;
* fat LTO with a single codegen unit at opt-level 3, which the release profile
  already sets;
* the `-Cllvm-args=-enable-dfa-jump-thread` codegen flag, which is a pure
  codegen transformation.

If you are chasing a throughput gap, those are the levers. See the performance
section for what the gates actually are.

### Two bound functions are part of this contract ###

`deflateBound` and `compressBound`, and their `_z` variants, must return the
identical numbers, because callers size their output buffers with the answer. A
bound that is too small becomes a buffer overflow **in caller code**, which is
the worst failure mode available here; one that is too large breaks tests that
assert exact sizes. The differential suite compares them numerically rather than
inferring them from a successful round trip.

### The cheapest check comes first ###

Before debugging an algorithm, run the table comparison. `crc32.h`, `trees.h` and
`inffixed.h` are generated tables, and `tests/table_equality.rs` compares the
ported `const` arrays against the C arrays element for element. A transcription
error shows up there in seconds, and no amount of staring at `longest_match` will
find it.

## The safety posture ##

Safety here is achieved by **relocating** unsafety to a boundary small enough to
audit exhaustively, not by eliminating it -- at an FFI edge, eliminating it is
impossible. What makes the relocation credible is that the compiler enforces both
halves.

`crates/zlib-rs` carries `#![forbid(unsafe_code)]`. Every algorithm in the
library is inside it, so the algorithmic surface has zero unsafe code as a
compile-time fact rather than as a convention or a grep result.

`crates/libz-rs-sys` is the only crate in the shipped library permitted `unsafe`,
and it carries `#![deny(unsafe_op_in_unsafe_fn)]`, so each unsafe operation is
scoped on its own instead of being blanket-permitted by an `unsafe fn` signature.
`undocumented_unsafe_blocks` is denied workspace-wide, so every block carries a
`// SAFETY:` comment naming the invariant it relies on -- the compiler rejects one
that does not.

### The six site categories ###

Unsafety is confined to six categories of site. These are the sites the FFI
contract *forces*; **anywhere else, `unsafe` in this crate is a defect.**

Each is stated below at the width the code actually uses it, which is wider than
the one operation that names it: a category is a *kind* of trust the C contract
obliges the facade to extend, not a single function call. The same six, in the same
order and with the same reasoning, head `crates/libz-rs-sys/src/lib.rs`, and

    grep -rn 'unsafe-site categor' crates/libz-rs-sys/src

reads the mapping off the code rather than off this list.

1. **The caller's stream, through a raw pointer: validation, and every field
   access.** Every exported stream function receives a `z_streamp`, which must be
   non-null, aligned, and addressing a caller-allocated `z_stream`. Nullability is
   part of the published contract -- a null stream yields `Z_STREAM_ERROR` -- so the
   guard always precedes the first dereference. The category then covers each
   individual READ and WRITE of a member through that pointer -- `next_in` and
   `avail_in`, `total_in` and `total_out`, `msg`, `adler`, `data_type`, and the
   three hook members. Those are separate unsafe operations from the validation,
   discharged by the same guarantee.
2. **Pointer/length pairs, and the caller's out-parameters.** `(next_in, avail_in)`
   and `(next_out, avail_out)` are reconstructed with `from_raw_parts` once, on
   entry, and only the resulting safe slices travel inward; an `avail_in` of zero
   with a null `next_in` must produce an empty slice rather than undefined
   behaviour. The same reasoning covers the scalar OUT-PARAMETERS this API is full
   of -- `deflatePending`'s `*mut c_uint` and `*mut c_int`,
   `inflateGetDictionary`'s `dictLength`, the `*mut uLong` and `*mut z_size_t`
   lengths of the one-shot wrappers, `gzerror`'s status slot -- each a caller-owned
   location written or read exactly once, after establishing that it is non-null
   and aligned.
3. **The opaque handle round-trip, for both handles.** `z_stream::state` is
   caller-visible but caller-opaque; before it is treated as state it is
   tag-validated, exactly as `deflateStateCheck` and `inflateStateCheck` do in the C
   sources, so a foreign, stale or already-freed stream is rejected instead of
   trusted. `gzFile` is the same category in the gz layer, where the facade owns
   rather more of the life cycle: reserving the block, initialising it, handing back
   the handle, recovering the state on each later call, and releasing it on
   `gzclose` -- with the caller-visible `gzFile_s` prefix at offset 0, for the reason
   the next section gives.
4. **Calling back into caller-supplied C function pointers.** `zalloc` and `zfree`
   are modelled as a nullable `Option<unsafe extern "C" fn(...)>` so that `Z_NULL`
   is `None` and selects the internal default, and a block obtained from a caller's
   `zalloc` is released only through that same caller's `zfree`. The `inflateBack`
   hooks are the same category with a different signature: `in()` hands the facade a
   buffer it must then treat as valid for the length returned, and `out()` is handed
   one of ours. Both are invoked through a raw function pointer whose validity only
   the caller can establish.
5. **C strings and varargs.** Paths reaching `gzopen` are assumed NUL-terminated
   and readable for the duration of the call; strings returned by `gzerror`,
   `zError` and `zlibVersion` are `'static` or owned by the stream and outlive
   the caller's use of them. `gzprintf` is variadic and `gzvprintf` takes a
   `va_list`, which makes them the hardest two items in the port: stable Rust can
   neither define a C-variadic function nor name a `va_list`, so both are C shims
   under `crates/libz-rs-sys/csrc/` that route into a bounded formatting buffer
   mirroring the C `vsnprintf` path.
6. **Reading and writing a caller's `gz_header`.** `inflateGetHeader` fills caller
   buffers, and every write is clamped to `extra_max`, `name_max` and `comm_max`.
   This is historically the source of gzip-header overflow defects, and here it
   is a length-checked slice write. `deflateSetHeader` reads the same struct under
   the same rules.

Those are the shipped library. Outside it, three dev-only places have `unsafe`
and each is unavoidable: `crates/zlib-rs-differential/src/oracle.rs` declares the
C oracle, `src/port.rs` holds the boundary gates everything else drives the C ABI
through, and the suites under `crates/libz-rs-sys/tests/` need it because *calling*
an `extern "C"` function is itself an unsafe operation. Those gates are
deliberately not part of the shipped facade -- a test harness is not one of the
six categories -- and what they buy is that all three benchmark suites and all
five fuzz targets contain no `unsafe` at all.

Calling those gates "safe" needs one qualification, because for three of them a
signature that merely took `&mut` was NOT safe. The C library RETAINS pointers past
the call that installs them: a `gz_header` given to `deflateSetHeader` or
`inflateGetHeader`, the window given to `inflateBackInit_`, and the ledger
installed as a tracking allocator's `opaque`. A wrapper whose borrow ended when it
returned let the caller drop or move that storage while the library still held its
address, and nothing in the type system objected.

`src/retain.rs` is the repair. `Session<'r, S>` owns a boxed stream -- address
stability matters, because an initialised `deflate_state` keeps a back-pointer to
its own `z_stream` -- carries an invariant `PhantomData<&'r mut ()>`, and has a
`Drop` impl that does nothing at run time and everything at compile time: dropck
then REQUIRES every retained storage to strictly outlive the session, so the
storage must be declared first. Installing goes through the session, so a retained
pointer cannot be handed over without that obligation. Two further consequences
worth knowing: `inflate_back` no longer takes a window argument at all (the extent
is recorded by the init that installed it, which removes a "pass the same slice
twice" convention a caller could get wrong), and the inflateBack window is
`&'r mut [u8]` rather than `&[u8]`, because the library writes through it.

Those obligations are compile-enforced rather than described.
`crates/zlib-rs-differential/tests/retention_compile_fail/` holds six programs --
five that must NOT compile, each annotated with the exact error code it must
produce, and one control that must -- and `.github/scripts/retention_compile_fail.py`
runs them in the `differential` job. The `miri-harness` job additionally
interprets the harness's own port-side gates under Miri (`-- --skip oracle::`,
since Miri cannot execute the C oracle's foreign functions), so the retention
behaviour is UB-checked and not merely asserted.

### Panic discipline ###

A panic must never unwind into a C caller: the caller was not compiled to support
unwinding, and letting an unwind cross the boundary is undefined behaviour. So
every exported function is `extern "C"` and **never** `extern "C-unwind"`, which
makes a panic abort at the boundary instead; `panic_guard.rs` centralises that so
the guarantee is auditable in one place rather than implicit across the whole
export surface; and `panic = "abort"` in the release profile makes it
unconditional.

That is the containment. The intent is that it is unreachable: `unwrap_used`,
`expect_used`, `indexing_slicing`, `panic`, `panic_in_result_fn`, `todo`,
`unimplemented`, `unreachable` and `exit` are all denied workspace-wide, and
fallible operations return a `ReturnCode` instead. An abort should be a
belt-and-braces guarantee, not the error path.

### Three ABI facts a contributor could break without noticing ###

* **`uLong` maps to `core::ffi::c_ulong`, never to a fixed-width integer.** It is
  `unsigned long`: 8 bytes on LP64, 4 on ILP32 and 4 on LLP64 Windows. Hardcoding
  `u64` or `u32` silently corrupts `total_in`, `total_out`, `adler` and every
  checksum return value on one of them. The same reasoning governs `z_size_t`,
  `z_off_t` and `z_off64_t`. The `integer-models` job is what catches this class:
  every other job runs on LP64 only, so it BUILDS the tests for
  `i686-unknown-linux-gnu` -- `build` and not `check`, because
  `arithmetic_overflow` is produced by a MIR pass that runs during codegen, so
  `check` reports success on code `build` rejects. It found three `uLong << 32`
  expressions and a `uInt::MAX as usize + 1` in the facade's own tests that were
  overflows at 32 bits. Compiling is enough; running them would need an emulator.
* **`struct gzFile_s`'s three fields `{ have, next, pos }` must stay at offset 0
  of the Rust gz state.** `gzgetc` is a macro in `zlib.h`, so that field
  arithmetic is compiled into *caller* object code that this port cannot change.
  Any other layout is silent memory corruption in every caller that uses it.
* **Both large-file symbol families are exported** -- `gzopen`, `gzseek`,
  `gztell`, `gzoffset`, `adler32_combine`, `crc32_combine` and
  `crc32_combine_gen`, and their `*64` counterparts. Which name a given caller
  references depends on that caller's own `_FILE_OFFSET_BITS` and
  `_LARGEFILE64_SOURCE` at *its* compile time, so dropping either family breaks
  somebody's link.

Two more, for completeness. `zlibCompileFlags()` is computed from the Rust build's
own type sizes rather than copied, because the value is target-dependent -- it is
`0xa9` on the reference target and would be wrong to hardcode anywhere else. And
because the port uses `const`-evaluated CRC tables instead of building them at
run time, it reports the `DYNAMIC_CRC_TABLE` bit clear, which is the honest
answer: the caveat in `FAQ` question 21 about that define and a system without
atomics cannot apply to a table that was computed at compile time. In the same
vein, `FAQ` question 36 explains that the C `deflate` intentionally branches on an
uninitialized value and that its output is unaffected; the safe core cannot read
uninitialized memory at all, so the observation stands and the behaviour it
describes is simply absent here.

And one layout obligation that is emphatically **not** public ABI, but is a
constraint all the same, because a test in this tree depends on it. `test/infcover.c`
includes the private `inflate.h` and writes straight through the opaque pointer:

    ((struct inflate_state *)strm.state)->mode = DICT;   /* its L330 */
    state->mode = SYNC;                                  /* its L459, inside pull() */

Those stores are compiled into *caller* object code that the port cannot change, so
the block installed at `z_stream::state` carries a `#[repr(C)]` prefix of
`{ z_streamp strm; int tag; }` at offset 0 -- the private `StatePrefix` at the head
of `StateBlock<S>` in `crates/libz-rs-sys/src/types.rs` -- and every inflate entry
point re-reads that tag from memory on entry and writes it back before returning, so
a mode written by caller code between two calls actually takes effect. The tag uses
C's own discriminants, where `HEAD` is 16180 and `SYNC` is 16211, because a
zero-based enum would make both of those stores silently meaningless.

The prefix serves both state kinds, because both C structs begin the same way:
`deflate.h` L105 and `inflate.h` L83 are both `z_streamp strm`, and the four bytes
after it are `status` for a deflate state and `mode` for an inflate one. `strm` is
there because `inflateStateCheck` compares `state->strm != strm` and the facade
performs the same owner-identity test. A third field, `flavor`, follows the tag in
padding C leaves unused, and distinguishes an `inflate` payload from an
`inflateBack` one.

Three things follow, and the distinction matters. This is **private test
compatibility, not published ABI**: no documented consumer may look inside
`z_stream::state`, `zlib.h` says so, nothing in `zlib.h` or `zconf.h` describes this
layout, and the rest of the state behind the pointer is ordinary Rust with no
`#[repr(C)]` and field types chosen for Rust rather than for byte-for-byte
agreement. It is honoured because the acceptance criterion is that
`test/infcover.c` compiles and passes *unmodified*, and it would neither compile nor
pass without it. And it is enforced rather than remembered: `types.rs`
carries `const _: () = assert!(offset_of!(StatePrefix, ...))` for `strm` at 0 and
the tag at `size_of::<z_streamp>()` -- relationally, plus absolute 0/8/12/16 on a
64-bit target -- so a reordering or a widened field is a build failure;
`crates/libz-rs-sys/tests/abi_layout.rs` §9 pins the same numbers at run time
through a published four-`usize` summary rather than by reaching into the private
type; and the `dropin` job compiles and runs the unmodified driver.

### The contract is immutable, and four mechanical gates hold it ###

`zlib.h`, `zconf.h` and `zlib.map` are the public contract. They are read, and
`zlib.map` is handed to the linker; none of the three is ever edited. That does
not depend on review:

1. **Compile-time layout assertions** --
   `crates/libz-rs-sys/src/layout_assertions.rs`. `const _: () = assert!(...)`
   over `size_of` and `offset_of!`, evaluated on every build, so layout drift is a
   compile error rather than a run-time surprise.
2. **The cbindgen header comparison** -- `make rust-header` and the `header` CI
   job, against the rules `cbindgen.toml` specifies. Two gates, not one: the
   signatures are compared by compiler and by a literal diff of both sides'
   resolved forms, and EVERY line of the raw `diff -u zlib.h generated_zlib.h` is
   then classified by `.github/scripts/header_raw_diff_gate.py`, which fails on any
   line no rule accounts for.
3. **The `nm` symbol-parity diff** -- `crates/libz-rs-sys/tests/symbol_parity.rs`,
   `make rust-symbols`, and the `symbols` CI job, against a baseline of 111
   exported symbols.
4. **The C89 to gnu2x compile sweep** -- `.github/workflows/c-std.yml`, which
   keeps proving that the unchanged `zlib.h` still compiles in every dialect.

`ZLIB_VERSION` and `ZLIB_VERNUM` are reproduced verbatim, because `deflateInit_`
and `inflateInit_` compare the caller's compile-time version against the
library's and reject a major mismatch.

## Differential testing against the C oracle ##

### How both implementations end up in one process ###

`crates/zlib-rs-differential/build.rs` uses `cc` to compile the fifteen in-tree C
translation units into a **test-only** static archive in `OUT_DIR`. The file set
is `Makefile.in`'s `OBJZ` plus `OBJG` object lists taken verbatim, so it cannot
drift from what the C build compiles:

    adler32.c crc32.c deflate.c infback.c inffast.c inflate.c inftrees.c trees.c
    zutil.c compress.c uncompr.c gzclose.c gzlib.c gzread.c gzwrite.c

They are compiled with the flags the in-tree `configure` actually applies --
`-O3 -fPIC -D_LARGEFILE64_SOURCE=1 -DHAVE_HIDDEN` -- so the oracle is the
reference build rather than an approximation of it. `-D_LARGEFILE64_SOURCE=1` is
mandatory rather than a nicety: it is what makes the oracle expose the `*64`
family the port has to be compared against.

Every symbol the archive defines is then exposed under a **`c_` prefix**, so both
implementations are callable from one test without colliding. Most of the renaming
is done by the preprocessor; the rest is an `objcopy --redefine-syms` pass, which
exists because `gzread.c` does `#undef gzgetc` before defining the real function
and that `#undef` deletes any command-line `-D` along with it. The build verifies
that no defined external symbol escapes without the prefix.

Because the crate is dev-only and appears in no shipped `[dependencies]`, **the
reference implementation is an oracle rather than a dependency**: neither shipped
artifact links a line of it, and `cargo build --release` never compiles these
fifteen files. There is also **no build-time network dependency and no submodule**
anywhere in this arrangement.

That is a claim about the oracle, not about C tooling in general, and the two are
easy to conflate. A shipped build DOES run a C compiler and an archiver, because
`crates/libz-rs-sys/build.rs` compiles the two translation units in its own
`csrc/` -- the variadic `gzprintf`/`gzvprintf` pair and `inflate_table`'s
`code`-typed prototype -- and archives them into `libz.a`. So `cc` and `ar` are
prerequisites of `cargo build -p libz-rs-sys` as well; what the differential
crate's boundary buys is that nothing in `deflate.c` or `inflate.c` is compiled
into the artifact.

### Running it ###

The default run is quick and is what a plain `cargo test` reaches:

    cargo test --locked -p zlib-rs-differential --release

The exhaustive sweeps are `#[ignore]`d so that a routine `cargo test` stays
usable, and are armed by environment variable:

    ZLIB_RS_DIFFERENTIAL_EXHAUSTIVE=1 ZLIB_RS_INTEROP_EXHAUSTIVE=1 \
      cargo test --locked -p zlib-rs-differential --release -- \
      --ignored --test-threads "$(nproc)"

**`1` is not the unabridged product, and the difference is worth knowing before
quoting a coverage number.** The variable takes two arming values:

    ZLIB_RS_DIFFERENTIAL_EXHAUSTIVE=1      9,459,450 of 9,922,500 cells -- 95.30%.
                                           One exclusion stands: a fixture smaller
                                           than the window is not re-run at every
                                           windowBits, which would cost roughly
                                           four times the wall clock for cells that
                                           differ only in a parameter the encoder
                                           cannot act on.
    ZLIB_RS_DIFFERENTIAL_EXHAUSTIVE=full   the same product with that exclusion
                                           lifted: the remaining 463,050 cells,
                                           excluded_tiny_window=0, coverage 100.00%.

Use `full` when the instruction is "run the unabridged matrix". Both are gated:
`push` and `pull_request` run `1`, the nightly `schedule` runs `full`, and the
coverage gate checks the event against the mode, so a workflow edit that quietly
returned the scheduled run to the abridged product fails rather than passes.

Both commands are the gate; the `differential` and `differential-exhaustive` jobs
of `.github/workflows/rust.yml` run them, the latter across sixteen runners -- eight
cell shards selected with `ZLIB_RS_DIFFERENTIAL_SHARD=i/8`, each once per checksum
backend. `--release` is not an optimisation preference: the sweeps are large enough
that a debug build turns them from slow into impractical.

### The matrix ###

The exhaustive run is what covers the space the port is required to be
byte-identical across:

| Dimension | Values |
|---|---|
| level | 0 through 9 |
| `windowBits` | 9 to 15 (zlib), -9 to -15 (raw), 25 to 31 (gzip) -- all three container formats |
| `memLevel` | 1 through 9 |
| strategy | `Z_DEFAULT_STRATEGY`, `Z_FILTERED`, `Z_RLE`, `Z_HUFFMAN_ONLY`, `Z_FIXED` |
| flush | all seven: `Z_NO_FLUSH`, `Z_PARTIAL_FLUSH`, `Z_SYNC_FLUSH`, `Z_FULL_FLUSH`, `Z_FINISH`, `Z_BLOCK`, and `Z_TREES` |
| input | every fixture in the committed corpus -- ten of them |
| feeding | fifteen chunking schedules, listed below |

`Z_TREES` is in the list and is not an oversight. `deflate` **refuses** it
(`deflate.c:985` answers `Z_STREAM_ERROR` for `flush > Z_BLOCK`), so its cells
assert status parity rather than byte parity: the port must refuse it the same way,
with the same return code and the same untouched stream, rather than inventing a
meaning for it. `Z_TREES` on the *inflate* side is a real
flush mode rather than a refusal, and it is covered there instead:
`crates/zlib-rs/tests/inflate.rs` drives every raw success vector with it, in C's
own loop shape, and asserts no error and no message -- deliberately *not*
`Z_STREAM_END`, because stopping at each block boundary is the point of the mode.

The fifteen chunkings are the single-shot baseline plus fourteen windowed
schedules, and they vary the input side, the output side, and both at once:

| Side | Windows |
|---|---|
| input only | 1, 3, 7, 251, 1024, 16384 |
| output only | 1, 3, 251, 1024 |
| both | 1/1, 7/3, 251/1024, 16384/251 |

1, 3, 7 and 251 are odd or prime so that a boundary never lands on an internal
buffer size; 1024 and 16,384 are powers of two chosen to *coincide* with one,
because a boundary that lands exactly on a buffer edge is as interesting as one
that never does. The default sweep takes eight of the fifteen -- one of each
*shape* -- so that no kind of boundary goes unexercised on a bare `cargo test`; the
exhaustive shards sweep all fifteen.

### The three test files ###

* `tests/byte_identical.rs` -- the Rust output bytes equal the C output bytes,
  across the matrix above. This is where the previous section's eight decision
  points are actually held to account.
* `tests/roundtrip_interop.rs` -- bidirectional interoperability: C-compressed
  streams inflate under the Rust implementation, **and** Rust-compressed streams
  inflate under the C one. Asserted as its own class rather than inferred from a
  round trip, since a port that made the same mistake twice would round-trip
  perfectly. Preset dictionaries are covered in both directions, and
  `deflateBound`/`compressBound` outputs are compared numerically.
* `tests/table_equality.rs` -- the ported `const` tables against the C arrays from
  `crc32.h`, `trees.h` and `inffixed.h`, element for element. Cheapest and
  highest-signal check in the workspace; run it before any algorithmic debugging.

### Miri cannot run these ###

Miri interprets Rust MIR and **cannot execute the compiled C oracle at all**, so
no differential test is reachable under it. That is why the Miri job is scoped to
`crates/zlib-rs` and the differential tests are excluded from it -- and it is also
why the safe-core/facade split is what makes that job meaningful in the first
place: the core contains no FFI, so Miri can interpret all of it.

## Verification suites ##

Each command below is the form CI uses, with ONE exception called out where it
appears: `cargo doc`, which no workflow invokes. `.github/workflows/rust.yml` runs
all the others across twenty-two jobs -- eighteen on `ubuntu-latest` and four
elsewhere (`build-test`'s matrix, the two packaged-ABI jobs, and `bench` when a
repository variable redirects it) -- and its per-job comment blocks record what each
one establishes and what it does not.

Several of those jobs drive a committed script rather than an inline heredoc,
because a committed file can be compiled, linted, diffed, reviewed and -- decisively
-- tested. The Python gates are `.github/scripts/bench_gate.py` (with its own
`.github/scripts/bench_gate_selftest.py`, which runs the real gate against a
synthetic log and twenty-nine broken variants of it, before any measurement),
`.github/scripts/header_raw_diff_gate.py`,
`.github/scripts/platform_abi_gate.py` (shared by both packaged-ABI jobs),
`.github/scripts/retention_compile_fail.py` and
`.github/scripts/docs_consistency_gate.py`, which checks the claims in THIS file
against the tree -- job names, script paths, the job count and split, the
documented `ZLIB_RS_*` toggles and the CMake options it names -- because a guide
describing executable gates is a second copy of facts that rots silently, and this
one had rotted in ten places at once. One shell gate sits beside them,
`.github/scripts/whitespace_gate.sh`; the drop-in chain gate that used to be a
second one is now `crates/libz-rs-sys/tests/dropin_chain.rs`, so `cargo test`
runs it, `cargo clippy -- -D warnings` lints it and `cargo fmt --check` formats
it -- none of which a script under `.github/` ever got.

### Unit and integration tests ###

    cargo test --locked --workspace
    cargo test --locked --workspace --release
    cargo test --locked --workspace --release --all-features

The unit tests of all three crates plus the integration suites under
`crates/*/tests/`. `crates/libz-rs-sys/tests/` carries `abi_layout.rs`,
`c_api_parity.rs`, `dropin_chain.rs`, `gz_memory.rs`, `gz_printf.rs`,
`rust_api_surface.rs`, `symbol_parity.rs` and `write_only_window.rs`. Two of
those are artifact gates rather than API tests: `symbol_parity.rs` is the `nm`
parity gate in test form, and `dropin_chain.rs` is the installable-chain gate --
it reads the descriptor `make rust` stages, asserts each symlink by its literal
target text *and* its canonical destination, requires an ELF inspector and proves
the recorded SONAME against the alias that was staged, and self-tests itself
against eleven deliberately broken copies of the staged tree so that a gate which
cannot fail cannot pass.

Several assertions in those two inspect the *staged* library, which cargo does
not build, so they skip by default. Arm them:

    make rust CARGO=cargo
    ZLIB_RS_REQUIRE_PACKAGED=1 cargo test --locked --release \
        -p libz-rs-sys --test symbol_parity -- --nocapture
    ZLIB_RS_REQUIRE_PACKAGED=1 cargo test --locked --release \
        -p libz-rs-sys --test dropin_chain -- --nocapture

`ZLIB_RS_REQUIRE_PACKAGED=1` turns each skip into a failure naming what went
unverified; the `symbols` job sets it after staging and additionally fails on any
`SKIP:` line, and the `dropin` job runs the chain gate the same way -- three
times, the third with `ZLIB_RS_DROPIN_DIR` pointed at an archive copy of the
staged tree -- so the artifact that ships cannot be recorded green without having
been opened.

### Miri, over the safe core ###

    cargo +nightly miri test -p zlib-rs --all-features

Undefined-behaviour detection. Miri is a nightly-only rustup component, which is
why a nightly job exists beside the stable ones. Scoped to `crates/zlib-rs`,
which is what makes it meaningful and also what makes it possible -- see the
differential section for why the oracle is out of reach. Read the result as "no
undefined behaviour observed while interpreting this test suite": Miri is an
interpreter, not a prover, and what a test does not execute it does not analyse.

### AddressSanitizer, over the boundary ###

    RUSTFLAGS=-Zsanitizer=address RUSTDOCFLAGS=-Zsanitizer=address \
    ASAN_OPTIONS=detect_leaks=1:detect_stack_use_after_return=1:strict_string_checks=1 \
      cargo +nightly test --locked -p libz-rs-sys \
        -Zbuild-std --target x86_64-unknown-linux-gnu

Scoped to the facade, which is where raw pointers actually exist. The division of
labour is the point: **Miri exercises the safe core, ASan exercises the boundary
layer.**

Three flags in there are load-bearing. `RUSTDOCFLAGS` must be set as well as
`RUSTFLAGS` -- with only the latter, the library is instrumented but the doctest
binaries are not, and they then fail to link with undefined `__asan_*` symbols.
The explicit `--target` is required so that build scripts and proc macros are
built uninstrumented. And `-Zbuild-std` rebuilds `std` under the sanitizer, so a
fault inside a `std` routine reached from the facade is attributed rather than
missed.

The `asan` job runs the facade suite twice, and the two runs are deliberately
DIFFERENT rather than the same command repeated. The first omits `-Zbuild-std`:
the facade is instrumented while `std` is the one cargo ships, which is the
configuration a consumer who sets `RUSTFLAGS` and nothing else actually gets, and
it is the run that would catch a fault the sanitizer can only see when `std` is
*not* rebuilt under it. The second adds `-Zbuild-std`, as above. Each has its own
`CARGO_TARGET_DIR` -- `target/asan-facade` and `target/asan-build-std` -- because
they fingerprint differently and would otherwise invalidate each other on every
run.

Then three more things: the three C drivers against an instrumented `libz.a`; the
differential harness with `CFLAGS=-fsanitize=address` so the C oracle archive is
instrumented rather than merely linked; and the criterion suites compiled under
the sanitizer and executed once each through criterion's test mode. What ASan does
*not* cover is the fuzz targets, because `cargo fuzz` applies AddressSanitizer
itself, per target.

### Fuzzing ###

    cargo +nightly fuzz run fuzz_deflate      -- -max_total_time=300
    cargo +nightly fuzz run fuzz_inflate      -- -max_total_time=300
    cargo +nightly fuzz run fuzz_inflate_back -- -max_total_time=300
    cargo +nightly fuzz run fuzz_gz_roundtrip -- -max_total_time=300
    cargo +nightly fuzz run fuzz_checksum     -- -max_total_time=300
    cargo +nightly fuzz run fuzz_checksum --features simd -- -max_total_time=300

Run from `fuzz/`, which is a package of its own; `cargo install cargo-fuzz
--locked --version 0.13.2` is the prerequisite. The bar is **zero crashes and
zero hangs at 300 seconds per target**, the same budget the existing OSS-Fuzz
workflow uses for the C library. `fuzz_inflate` is the one that matters most:
`inflate` is the entry point that reads untrusted bytes.

Five binaries, six invocations -- `fuzz_checksum` runs once per checksum backend,
because its neutrality assertions are `#[cfg(feature = "simd")]` and are compiled
away in the default build. The `fuzz` job's matrix is those same six rows with the
same budget.

**Seeds matter more than the budget here.** Four of the five targets decode their
input through `arbitrary` into a typed record, so an unstructured byte string
usually decodes into a configuration that fails immediately -- every row used to be
seeded from the same ten uncompressed corpus fixtures, which meant the inflate and
inflateBack targets began in an error path and never reached a real decode.
`fuzz/seeds/` now holds per-target seeds carrying C-oracle-produced raw, zlib and
gzip streams already wrapped in each target's record encoding, `fuzz/seeds/README.md`
records how each was constructed, and `fuzz/seeds/generate.py --check` verifies in
CI that the committed bytes are the ones the generator produces. Measured on
`fuzz_deflate` over 40 seconds, adding them moved coverage from 3,384 to 3,592
features and throughput from 1,434 to 2,040 exec/s.

Because a seed that decodes is not necessarily a seed that gets anywhere, each
target reports what it reached when `ZLIB_RS_FUZZ_REPORT=1` -- `Z_STREAM_END`, the
output callback, a completed round trip -- through the shared `zlib_rs_fuzz`
library, and the `fuzz` job runs every seed once and asserts that at least one
reaches the outcome its row names. Pass seeds to `cargo fuzz run` INDIVIDUALLY when
reproducing locally: naming the directory makes libFuzzer treat it as a writable
corpus and it will add generated inputs to your working tree.

### Lint, format, dependency and documentation policy ###

    cargo fmt --all -- --check
    cargo clippy --locked --workspace --all-targets -- -D warnings
    cargo deny --locked --all-features check

Zero warnings, everywhere -- with one thing to know about "everywhere". The
commands above run at the repository root, and `--workspace` there does not reach
`benches/` or `fuzz/`, which are packages of their own. The `format`, `lint` and
`deny` jobs therefore run each command once per workspace: the root, then
`--manifest-path benches/Cargo.toml`, then `fuzz/` on nightly (its targets are
`#![no_main]`, so stable cannot build them). The fuzz workspace's nightly toolchain
is installed with `--component rustfmt,clippy` for exactly that reason -- a minimal
nightly cannot run either gate, and that is the only place either one reaches those
five targets.

Documentation is enforced by the compiler rather than by a fourth command: every
crate carries `#![deny(missing_docs)]` and the rustdoc link lints
(`broken_intra_doc_links`, `private_intra_doc_links`, `redundant_explicit_links`,
`invalid_codeblock_attributes`) in its own source, so an undocumented public item
and a broken intra-doc link both fail an ordinary build. The convention on top of
that is that each ported function names the C source it derives from, so behaviour
can be traced back to the oracle. To render the result locally -- this one is a
LOCAL command, not a gate, and no workflow invokes it:

    cargo doc --locked --workspace --no-deps --all-features

That is not a coverage gap, because the lints it would exercise are not deferred
to it: `missing_docs` and the rustdoc link lints are crate attributes, so
`cargo build` and `cargo test` already fail on an undocumented public item or a
broken intra-doc link, and the `asan` job additionally sets `RUSTDOCFLAGS` so the
doctest binaries are built too. What `cargo doc` adds locally is the rendered
HTML.

The lint levels live in the root `Cargo.toml`'s `[workspace.lints]`:
`clippy::all` and `clippy::pedantic` at deny, plus the no-panic set listed in the
safety section. Thresholds live in `clippy.toml`, which deliberately declares
**no `msrv` key**: with it absent, clippy reads each package's own
`rust-version` -- 1.80 for the three workspace members, 1.86 for `benches/`,
which criterion 0.8.2 requires -- rather than forcing one floor on all five.
`.rustfmt.toml` carries the formatting policy and may hold
stable options only, since the channel is pinned to stable. `deny.toml` is the
licence, advisory, ban and source policy -- among other things it denies wildcard
requirements, denies duplicate versions including through dev-dependencies, and
allows exactly one registry.

`benches/` and `fuzz/` each need those three run again with their own manifest
path and their own cargo-deny configuration, for the workspace-boundary reason
given earlier -- six further invocations, not three, and the `format`, `lint` and
`deny` jobs each run all three of their own:

    cargo fmt    --manifest-path fuzz/Cargo.toml --all -- --check
    cargo clippy --locked --manifest-path fuzz/Cargo.toml --all-targets -- -D warnings
    cargo deny   --locked --manifest-path fuzz/Cargo.toml --config fuzz/deny.toml check

    cargo fmt    --manifest-path benches/Cargo.toml --all -- --check
    cargo clippy --locked --manifest-path benches/Cargo.toml --all-targets -- -D warnings
    cargo deny   --locked --manifest-path benches/Cargo.toml --config benches/deny.toml check

Those six exist because an excluded package is unreachable from the root
`--workspace` invocation, not because the tools differ. CI's shapes are slightly
tighter than the copyable forms above: it runs the fuzz clippy from inside `fuzz/`
on nightly and twice, default features then `--all-features`, and the benchmark
clippy twice for the same reason. Both forms above pass as written -- the fuzz
package needs nightly to *fuzz*, not to lint.

### The generated header, diffed against the contract ###

    cbindgen --config cbindgen.toml --crate libz-rs-sys --output generated_zlib.h
    diff -u zlib.h generated_zlib.h

`make rust-header` drives this with the comparison rules `cbindgen.toml`
specifies, and the `header` CI job adds the shape half: it re-declares every
generated prototype against `zlib.h` so that a C compiler decides whether the
signatures agree, diffs struct sizes, field offsets and integer macro values from
a probe program per side, and compiles the result standalone as C89, C99, C17 and
C++17.

Two things to know before reading the raw `diff` output. It is necessarily
non-empty -- cbindgen cannot emit comments, the declaration macros or the
function-like macros -- but "non-empty" is no longer the same as "unchecked", and
this file used to say the artifact existed so a reviewer could confirm by eye that
the difference was only that. A reviewer confirming 6,020 lines by eye is not a
gate. `.github/scripts/header_raw_diff_gate.py` now classifies every changed line
against the divergence rules `cbindgen.toml` enumerates, and each rule names the
OTHER mechanism that checks that class exhaustively -- the standalone
C89/C99/C17/C++17 compiles for the include and typedef lines, the by-value macro
comparison for the `#define`s, `layout_assertions.rs` and `tests/abi_layout.rs` for
the struct members, the signature passes for the declarations. A line matching no
rule is residue and fails the job, so a divergence of a NEW kind cannot arrive
unnoticed: it has to be given a rule, and a mechanism that checks it.

That script also re-derives both headers' declared-name sets independently and
compares them in BOTH directions, since an added declaration is how a header grows
a surface the contract never promised. And because classifying a line as "a
declaration" says nothing about the types inside it -- `int deflate(z_streamp, int)`
and `int deflate(z_streamp, long)` are both declarations -- it requires the
resolved-signature diff's own product to exist and be empty before it will pass, so
that deferral is checked rather than assumed.

And `generated_zlib.h` is git-ignored scratch output that must never be committed:
its header guard is `ZLIB_H` on purpose, so it would shadow the real header if it
ever reached an include path. `zlib.h` is read-only and is never regenerated from
it.
`cargo install cbindgen --locked --version 0.29.4` is the prerequisite.

### Symbol parity ###

    make rust-symbols ZLIBREF=/path/to/libz.so.<version>

which reduces to sorting and diffing two `nm` tables:

    nm -D --defined-only --extern-only <library> | awk '{print $3}' | sort

The baseline is **111 dynamic global symbols: 95 functions plus the 16
symbol-version nodes `zlib.map` declares.** A diff catches both a missing export
and an extra one, which is why it is a diff and not a count. (For orientation:
`zlib.h` declares 96 entry points; 95 of them are symbols on a non-Windows target,
because `gzopen_w` is Windows-only.)

The `local:` names in `zlib.map` -- `deflate_copyright`, `inflate_copyright`,
`inflate_fast`, `inflate_table`, `zcalloc`, `zcfree`, `z_errmsg`, `gz_error`,
`gz_intmax`, `inflate_fixed` and the `_*` family -- must stay out of the dynamic
symbol table. That is achieved two ways at once: none of them is ever
`#[no_mangle]`, and the shared library is linked with
`--version-script=zlib.map`. `inflate_table` is the instructive case, and the one
exception to the first half of that sentence: it IS `#[no_mangle]`, because the C
reference keeps it reachable in `libz.a` and the unmodified `test/infcover.c` calls
it directly, while `zlib.map` hides it in `libz.so`. The port reproduces exactly
that split, and adds a `.hidden` directive so the cdylib -- which no version script
of this project's reaches -- hides it too. Hidden visibility is a property of a
shared object's dynamic table and leaves the archive's global definition alone.

`make rust` performs the same diff as one of its own checks, **but the comparison
against the C library is conditional**: `ZLIBREF` defaults to the C shared library
built in this tree, and when that file is absent `make rust` prints a note saying
the C-versus-Rust diff was skipped and carries on. It still enforces what needs no
C library -- every function `zlib.h` declares is exported and nothing else is, the
function count is 95, the version-node count is 16, no `local:` name is exported,
and the SONAME is the derived one. So do not read a green `make rust` on an
unconfigured tree as parity with C. Either build the C library first
(`make` then `make rust`) or name one with `ZLIBREF=`; both fail rather than skip.

### The relink-and-run gate ###

    make rust-test

This is the gate that gives the phrase "drop-in replacement" an operational
meaning. `test/example.c`, `test/minigzip.c` and `test/infcover.c` are compiled
**unmodified** against the staged Rust library and run. Their `#include "zlib.h"`
stays byte-for-byte as written, and *that invariance is the definition being
tested*. The hand form is:

    gcc -I. -o example_rust test/example.c \
        -L target/dropin -lz -Wl,-rpath,"$PWD/target/dropin"
    ldd ./example_rust | grep libz    # must name the staged file, not the system one
    ./example_rust

`infcover.c` is disproportionately valuable here, for three reasons that are hard
to reproduce deliberately. Its instrumented allocator fills every allocation with
the byte pattern `0xa5` rather than zero, so any code path that assumes
zero-initialised memory shows up immediately. Its `mem_done()` detects leaks,
non-LIFO frees and rogue frees through the injected `zalloc`/`zfree` hooks, which
validates unsafe-site category 4 directly rather than by inspection. And its
`mem_limit()` forces `Z_MEM_ERROR` at controlled points, exercising error paths
that fuzzing reaches only by chance.

There is a fourth reason, and it is the one that constrains the implementation
rather than merely exercising it: `infcover.c` includes the private `inflate.h` and
assigns to `((struct inflate_state *)strm.state)->mode` directly, so it is also the
gate on the private `{ z_streamp strm; int mode; }` prefix described in the safety
section. No other test in this tree, and no documented consumer, reaches inside
`z_stream::state` at all.

The `ldd` line is not optional -- see the install section for what happens without
it.

### What the other workflows still do ###

Five of the seven pre-existing workflows stay C-only, and two of them now execute
Rust as well. Naming which is which matters, because "the old workflows are
C-focused" was written here while two of them had already changed.

C-only, deliberately, and touched by this port only in comments:

* `c-std.yml` -- the header-immutability guard, still proving the unchanged
  `zlib.h` compiles from C89 through gnu2x.
* `fuzz.yml` -- keeps OSS-Fuzz pointed at the C oracle.
* `configure.yml`, `msys-cygwin.yml`, `others.yml` -- the cross-compilation and
  exotic-OS matrices, C-only because those platforms are outside the port's
  supported-target matrix.

The two functional exceptions:

* `cmake.yml` -- its matrix is the C library's, plus four `-DZLIB_BUILD_RUST=ON`
  rows (three on `ubuntu-latest`, one static-only on `macos-latest`) that build
  and `ctest` the Rust-backed exported targets through the same `CMakeLists.txt`.
  Every step that touches a Rust toolchain is guarded on `matrix.rust`, so a C row
  acquires no new requirement from the addition.
* `contribs.yml` -- the most external signal available: alongside its C matrix it
  carries a `ci-cmake-rust` job that installs a `-DZLIB_BUILD_RUST=ON` prefix,
  symlink chain and all, into a unique `$RUNNER_TEMP` directory and then builds
  the out-of-scope `contrib/` consumers against it. Those consumers were written
  against the C library and know nothing about this port, so they are a drop-in
  test nobody here wrote.

## The corpus ##

Two tiers, and the split exists so that correctness never depends on the network.
A build-time or test-time download would be unacceptable in CI, so **every
correctness gate runs against committed data and `cargo test` never needs a
network**; only performance measurement is opt-in.

### Tier 1: committed, deterministic, small ###

`crates/zlib-rs-differential/corpus/minimal/`, ten fixtures, each chosen for a
code path rather than for size:

| File | Exercises |
|---|---|
| `empty.bin` | zero-length input, an edge case at every layer |
| `single_byte.bin` | the shortest non-empty input |
| `repetitive.bin` | long matches and the RLE strategy |
| `random.bin` | incompressible data, so stored-block selection |
| `text.txt` | natural-language text |
| `binary.bin` | binary data, so the text-versus-binary heuristic behind `detect_data_type` |
| `gray_list.bin` | the "gray" bytes of that same heuristic, reaching its final fall-through, which `doc/txtvsbin.txt` describes |
| `window_boundary.bin` | data crossing the 32 KiB window boundary: window refill, hash sliding, the maximum-distance cutoff |
| `hello.bin` | the exact `"hello, hello!"` payload from `test/example.c`, 14 bytes including the NUL |
| `dictionary.bin` | the exact `"hello"` preset dictionary from the same file, 6 bytes including the NUL |

Adding a fixture widens the differential matrix everywhere at once, which makes
fixtures unusually cheap coverage. `corpus/README.md` records the provenance and
licensing of each one and why its class is there; that is not duplicated here.

### Tier 2: opt-in Silesia ###

`crates/zlib-rs-differential/corpus/fetch_silesia.sh` has three modes and they
have to be described separately, because only one of them is the download. It is
reached by no `Cargo.toml`, no `build.rs` and no test, so `cargo test` never
touches the network. The other two modes are read-only, and CI runs both:

| mode | reaches the network | writes anything | invoked by CI |
|---|---|---|---|
| *(no mode flag)* -- fetch | **yes** | yes, the corpus | only by a hand dispatch |
| `--verify-only` | no | no | yes, when a cache restore claims a corpus |
| `--pin-status` | no | no | yes, unconditionally, on every runner |

Two workflow jobs name it, and the distinction between them is the one that
matters -- it is not "no workflow references it" but **"no gate depends on a
download"**:

* `bench`, which measures, invokes it only as `--verify-only`, the mode that
  fetches nothing and writes nothing. It uses it to decide whether a corpus
  provisioned elsewhere is the pinned one.
* `silesia-provision`, which measures nothing and gates nothing, invokes it in
  fetching mode -- once, to populate the cache `bench` restores. It is reachable
  only by dispatching the workflow by hand with its `provision_silesia` input
  enabled, so a push, a pull request, the nightly schedule and a release can none
  of them reach it.

**The Silesia measurement is required where it counts.** AAP 0.8.4 states the
throughput and memory bars against this corpus, so on a `schedule` or a published
`release` the `bench` job FAILS when no pinned corpus can be verified. On a push or
a pull request it announces the omission with a warning and measures the committed
minimal corpus instead, which is what keeps a fork that has configured nothing
green and network-free. Arming used to depend on a repository variable and nothing
else, which meant the default -- variable unset -- passed every run without ever
measuring the named corpus.

    ./crates/zlib-rs-differential/corpus/fetch_silesia.sh --help
    ./crates/zlib-rs-differential/corpus/fetch_silesia.sh --pin-status
    ZLIB_RS_SILESIA_DIR=/var/cache/silesia \
      ./crates/zlib-rs-differential/corpus/fetch_silesia.sh

An expected SHA-256 is **required for every fetch or verify operation**, including
`--verify-only`; `--help` is the exception, and the only one -- it prints and exits,
so it asks for no digest. The committed `corpus/silesia.pin` supplies the digest,
which is why the third command above needs no flag. `--sha256` and `ZLIB_RS_SILESIA_SHA256` outrank the pin, for
a mirror or a repackaging with a different digest; if the pin were absent and
neither were given, the run would be refused rather than trusted. The remaining
flag is `--force`, to re-fetch into an already-populated destination. The
destination is `$ZLIB_RS_SILESIA_DIR` when set and `<repo-root>/target/silesia`
otherwise, which is git-ignored through `/target/`; `ZLIB_RS_SILESIA_URL`,
`ZLIB_RS_SILESIA_MEMBERS` and `ZLIB_RS_SILESIA_MEMBER_SHA256` cover the rest, the
last of which defaults to the committed `corpus/silesia.sha256`. That manifest is
what makes `--verify-only` a statement about the BYTES rather than about the
directory's own stamp and file names -- the archive is not retained after a fetch,
so its digest can never be recomputed from an installed directory -- and
`ZLIB_RS_SILESIA_MEMBER_SHA256=-` is the only way to waive it, after which the
report says so in as many words. Run `--help` for
the authoritative list.

Two files carry the corpus's identity and both are committed and reviewable:
`corpus/silesia.pin` records the approved archive's URL, SHA-256, byte size,
member count, expanded size, largest member and compression ratio, and
`corpus/silesia.sha256` records a digest for each of the twelve members. The pin
is what turns the resource ceilings from constants in a script into bounds derived
from the archive that was actually reviewed, and it is what the `bench` job reads
to decide whether the acceptance tier is armed -- so arming is a property of the
repository rather than of whether someone remembered to set a variable.
`corpus/README.md` carries the provenance, the ceilings and the
trust-on-first-use caveat.

Benchmarks probe that path and **skip gracefully** when it is absent, so a
benchmark run without Silesia produces a useful local signal but not the
acceptance measurement. `ZLIB_RS_SILESIA_REQUIRED=1` turns the skip into a
failure, which is how an acceptance run refuses to be mistaken for a regression
run.

## Performance targets ##

Every number in the table below is a **goal or a gate**, not a measurement. The
criterion output on the machine in front of you is what reports an achieved
throughput; the only measured figures written down in this file are the checksum
backend ratios in the `simd` section above, which exist because a claim that one
backend is faster than another is worthless without them and which a CI gate keeps
true.

| Metric | Gate | Measured by |
|---|---|---|
| decompression throughput | within 10% of the C implementation | `benches/inflate_bench.rs` |
| compression throughput at levels 1, 6 and 9 | within 10% of the C implementation | `benches/deflate_bench.rs` |
| per-stream memory | within 15% of the C implementation | `benches/deflate_bench.rs`, through an instrumented allocator |
| fuzz resilience | zero crashes and zero hangs, 300 s per target | `cargo fuzz`, five targets and six runs |

Running them:

    cargo bench --locked --manifest-path benches/Cargo.toml
    cargo bench --locked --manifest-path benches/Cargo.toml --bench deflate_bench
    cargo bench --locked --manifest-path benches/Cargo.toml --features simd

The `--manifest-path` is required, for the excluded-package reason given at the
top of this file. Each suite measures the port and the in-process C oracle **in one
process**, which removes two build artifacts, two page-cache states and two
process start-ups from the comparison -- so a number is always a ratio rather than
an absolute. The oracle is compiled from the same in-tree C sources by the same
`crates/zlib-rs-differential/build.rs`, reached through a path dependency, so the C
code being measured is the C code the correctness gates compare against. It is a
separate COMPILATION -- the benches are their own workspace -- rather than a second
source of truth.

### How a ratio is actually obtained, and what it cannot tell you ###

Sharing a process is necessary and it is not sufficient, because *within* one
process the side measured second still inherits warm pages, a warm allocator, a
warm branch predictor and whatever clock state the first side raised. Two things
address that, and both are worth knowing before reading a figure:

* **The criterion rows alternate.** criterion samples each registered row in
  registration order, so registering the port first everywhere would hand the
  oracle a second-mover advantage in every case. `register_pair!` chooses the order
  per case from the parity of an FNV-1a hash of the case name -- deterministic, so
  a re-run is comparable, but not uniform, so the advantage does not accumulate on
  one side. The `CRITERION` line each suite prints publishes
  `registration=alternating`.
* **The self-timed diagnostic is paired.** Beside the criterion rows each suite
  runs its own sampler, `paired_ns`, which times both sides **within** each of
  `PAIRED_ROUNDS = 9` rounds and **swaps which side goes first** between rounds.
  The published `ratio` is the quotient of the two medians and `ratio_hi` is the
  worst individual round, so a reader can see how much of the number is dispersion.
  Every `RATIO` line carries `rounds=9 order=alternating`.

What this does **not** do is make a 10% tolerance meaningful on an arbitrary
machine. Alternation removes a systematic order bias; it does nothing about a
noisy neighbour. Measured on a shared 4-core host, two consecutive runs of one
unchanged binary read 1.185 and 1.110 for the same case -- a 6.8% swing against a
10% limit, which is most of the tolerance. That is why the self-timed figure is a
**diagnostic only**: `bench_gate.py` treats an over-limit self-timed case as a
warning and lets criterion's own estimates decide, and why the `bench` job's
throughput verdict is release-blocking only on a designated runner and
informational on a hosted one. A tight number from a busy box is not a result.

Memory is measured through an instrumented `zalloc` high-water counter. That is
not a new technique invented for this port: it is what `test/infcover.c` already
does with `mem_high()`, so the approach is proven against this library's
allocation patterns.

`benches/Cargo.toml` owns two measurement profiles, and the distinction matters
when reading a throughput figure. `bench` is fat-LTO with a single codegen unit;
`bench-parity` (`--profile bench-parity`) is deliberately non-LTO and
multi-codegen-unit, which is structurally comparable to a per-translation-unit C
build. The `bench` job measures both and decides each case on the
worse-for-the-port of the two.

Tier matters too, and the `bench` job has **two shapes** rather than one.

UNARMED is what a runner without a provisioned corpus sees: the restore-only cache
keyed `silesia-<digest>` does not hit, `ZLIB_RS_SILESIA_DIR` is unset, and the
Silesia groups skip themselves. Every summary line the job then parses came from the
committed minimal corpus, so the run is a **regression signal, not the acceptance
measurement** -- it says the ratio has not moved on inputs small enough to fit in
cache.

ARMED is a repository that has provisioned the corpus out of band -- populating the
`silesia-<digest>` cache once, or pointing `ZLIB_RS_SILESIA_DIR` at a runner-local
copy. The digest itself comes from the committed `corpus/silesia.pin`, so arming
needs no repository configuration at all; a repository variable can override the pin
for a mirror. The job then runs `fetch_silesia.sh --verify-only` against what it was
given, **fetches nothing**, and gates the `deflate_silesia` and `inflate_silesia`
groups on the same thresholds -- so an armed run that restored a corrupt or
incomplete corpus fails rather than measuring it. It also fails loudly rather than
silently degrading: a separate step proves the refusal path by running the benches
with `ZLIB_RS_SILESIA_REQUIRED=1` against an empty directory and checking that they
decline to report a figure.

**No CI job downloads the corpus** on any automatic event: a cache miss leaves the
tier unarmed rather than reaching the network, which is the two-tier rule AAP
§0.6.4.4 sets, and the one job that does fetch -- `silesia-provision` -- is reachable
only by a hand dispatch and gates nothing. What an unarmed tier costs is stated
rather than hidden: it is a *warning* on an ordinary push to a hosted runner, and a
*failure* on the designated release runner and on a schedule or a release, so the
acceptance measurement cannot be quietly skipped where it is supposed to happen.

All three suites emit gated lines. `deflate_bench.rs` and `inflate_bench.rs` emit
`RATIO`, `RATIO-SUMMARY`, `MEMORY-SUMMARY` and `ALLOC-BALANCE`; `checksum_bench.rs`
emits `BACKEND` and `BACKEND-SUMMARY` for the two optional backends. All of them
are parsed by `.github/scripts/bench_gate.py`, which the `bench` job runs over one
combined log, and all of them can fail it. The checksum family is gated on the two
conditions the `simd` section above states -
If a gate is tight, the admissible levers are the output-neutral ones from the
byte-identity section -- the optional checksum backends behind `simd`, memory-copy
strategy, branch layout, fat LTO at opt-level 3, and
`-Cllvm-args=-enable-dfa-jump-thread`. Encoder-level changes are not an option
here however large the win, so read that section before optimising anything under
`deflate/` or `trees/`.

## Installing the drop-in library ##

    make rust
    make rust-test
    make rust-symbols
    make rust-pc

`make rust` relinks cargo's complete static archive through `zlib.map` with the
right SONAME and stages the installable set under `target/dropin/`:

    libz.so.1.3.2.1-motley     the real file
    libz.so.1                  symlink to it, and the SONAME
    libz.so                    symlink to it
    libz.a                     the static library

That is the same set, the same SONAME and the same symlink topology the C build
installs, so a consumer resolves the library identically either way. The installed
`zlib.h` and generated `zconf.h` are byte-identical to the C build's, since
neither is modified.

The versioned name is **derived, not hardcoded**. It comes from `ZLIB_VERSION` in
`zlib.h`, which is `"1.3.2.1-motley"` (`ZLIB_VERNUM` is `0x1321`); `configure`
reads that macro to compute `SHAREDLIBV` and `SHAREDLIBM`, and the Rust targets
parse the same macro so that they agree on an unconfigured tree too. It is
specifically *not* derived from `CARGO_PKG_VERSION`, which cannot express a
four-component version at all.

### The two trees have opposite symlink topologies ###

This is the single easiest thing here to get backwards, so both are stated
separately.

**Cargo's `target/<profile>/` directory** holds `libz.so` and `libz.a` as real
files and **no versioned names at all**. `crates/libz-rs-sys/build.rs` does not
create the aliases -- it cannot: a build script runs *before* the link, so any
versioned name it created would dangle. What it does instead is prune the stale
aliases an older revision of itself used to leave behind, and write a
`README-cargo-artifacts.txt` in that directory saying which artifact may be
installed and which may not.

**The staged and installed layout** reproduces the C topology instead: the
versioned file is the real one, and `libz.so.1` and `libz.so` are two symlinks
fanning out from it. That mirrors `Makefile.in`, which links each shorter name to
the versioned file rather than building a three-link chain. Relinking is a
post-link step, which is why it belongs to `Makefile.in`'s `rust` target and to
the CMake block and never to `build.rs`.

The practical instruction that follows: **link against what `make rust` staged,
not against cargo's `target/<profile>` directory.**

### Why the symlinks are a correctness requirement, not packaging polish ###

The SONAME is `libz.so.1`, so the dynamic loader searches for a file with that
literal name. A directory holding only `libz.so` leaves it free to keep searching
-- and it will find the system `libz.so.1` and bind that instead, **silently**.
This was observed directly during development: `ldd` resolved to the system path
and the test program printed the system library's version. Nothing failed.

That is the failure mode worth internalising, because a drop-in validation run
that binds the wrong library **passes**. `FAQ` question 14 already warns that most
Unix systems ship a zlib and that `-lz` will probably link to it; this is the same
hazard one step further along, at load time rather than link time, and creating the
symlink is what corrects it.

So: **every drop-in validation must assert with `ldd` that the Rust artifact is
the one actually bound**, rather than inferring it from a test that passed. That
is exactly what `make rust-test` does, and it is why the `ldd` line in the
relink-and-run recipe above is not decoration.

Two checks, one per property:

    readelf -d target/dropin/libz.so.1.3.2.1-motley | grep -i soname   # expect libz.so.1
    ldd ./example_rust | grep libz                                     # expect the staged path

### pkg-config and CMake consumption ###

`zlib.pc` keeps advertising `-lz` unchanged, and the exported `ZLIB::ZLIB` and
`ZLIB::ZLIBSTATIC` targets resolve to the Rust artifacts when the Rust build is
selected, so existing `find_package` and `add_subdirectory` integrations keep
working without being touched.

One thing does have to be advertised that the C build never needed, and
`make rust-pc` is the target that does it rather than an optional extra: a Rust
`libz.a` carries the Rust runtime's own object code, whose undefined references
reach into libc and its neighbours, and a static archive cannot record a
dependency the way a shared object records `DT_NEEDED`. The set is target-specific
and only rustc knows it, so it is probed once and published as the single
`Libs.private` key in the generated `zlib.pc` -- which is what
`pkg-config --static --libs zlib` reports -- and as the interface link libraries of
the exported `ZLIB::ZLIBSTATIC` target. `make rust-pc` gates its own output by
building and running a C program from nothing but the flags `pkg-config` answers,
and prints the command that installs the descriptor over the installed `zlib.pc`.
Do that whenever you install the staged `libz.a`. Shared consumers are unaffected,
and `README-cmake.md` documents the CMake side of the same mechanism.

## Where the rest is written down ##

Each file carries the derivation of its own rules, so the reasoning stays next to
the code it governs. When something here is not enough:

| Question | File |
|---|---|
| What does a given function do, and what is its signature? | `zlib.h` -- the API documentation of record, and the immutable contract |
| How do I build or install this as a user? | `README`, and `FAQ` questions 45 to 47 |
| What are the CMake cache options? | `README-cmake.md` |
| What does the C build do, and what do the seven Rust make targets establish? | the comment at the top of `Makefile.in` |
| What changed? | `ChangeLog` |
| What is the normative wire format? | `doc/rfc1950.txt`, `doc/rfc1951.txt`, `doc/rfc1952.txt` |
| Why does the algorithm work the way it does? | `doc/algorithm.txt`, and `doc/txtvsbin.txt` for the text-versus-binary heuristic |
| Why is the unsafe surface shaped this way, and what does each block promise? | `crates/libz-rs-sys/src/lib.rs`, and the `// SAFETY:` comment on each block |
| Which artifact is installable, and why is cargo's cdylib not? | `crates/libz-rs-sys/src/lib.rs` -- the artifact matrix |
| Exactly what must the generated header match? | `cbindgen.toml` -- the normalisation rules and the accepted divergences |
| What may a dependency be, and what may it never be? | `deny.toml`, and the root `Cargo.toml`'s `[workspace.dependencies]` notes |
| Which targets are supported, and to what degree? | `rust-toolchain.toml` -- the supported-target matrix |
| What does each corpus fixture exist to exercise? | `crates/zlib-rs-differential/corpus/README.md` |
| Which encoder decisions must stay byte-identical, and why? | the module comments in `crates/zlib-rs/src/deflate/` and `crates/zlib-rs/src/trees/` |
| What does each CI job establish, and what does it not? | `.github/workflows/rust.yml` -- the per-job comment blocks |
