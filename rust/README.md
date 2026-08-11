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
    clippy.toml                   lint thresholds and the clippy-side msrv
    deny.toml                     cargo-deny licence, advisory, ban and source policy
    cbindgen.toml                 header-generation config for the verification artifact

    crates/zlib-rs/               the safe core
    crates/libz-rs-sys/           the C ABI facade
    crates/zlib-rs-differential/  dev-only oracle harness
        corpus/minimal/           the committed differential corpus
        corpus/fetch_silesia.sh   opt-in performance corpus, run by hand

    benches/                      three criterion suites; its own excluded package
    fuzz/                         five cargo-fuzz targets; its own excluded package

    rust/README.md                this file

| Path | Role |
|---|---|
| `crates/zlib-rs` | The safe core. `#![forbid(unsafe_code)]`, `no_std` plus `alloc`, and an empty `[dependencies]` table. Every algorithm lives here. Package `zlib-rs`, library target `zlib_rs`, an rlib and nothing else. |
| `crates/libz-rs-sys` | The C ABI facade, and the only crate in the shipped library that writes `unsafe`. `[lib] name = "z"` with `crate-type = ["cdylib", "staticlib", "rlib"]`, which is why cargo emits `libz.so` and `libz.a` rather than Rust-flavoured names. The `rlib` is what lets the differential crate, the benchmarks, the fuzz targets and its own test suites import it as `libz_rs_sys`. It has a `build.rs`, and three small C files under `csrc/`: two are archived into `libz.a` for the variadic `gzprintf`/`gzvprintf` pair and for a prototype stable Rust cannot express, and the third is a test-only probe that calls `gzvprintf` the way a real C consumer does, since stable Rust cannot even construct a `va_list`. |
| `crates/zlib-rs-differential` | Dev-only oracle harness, `publish = false`. Its build script compiles the in-tree C translation units into a test-only archive, so both implementations are co-resident in one process and comparing them is a memory compare. `cc` is a build-dependency of this crate alone. It has an unsafe boundary of its own -- `src/oracle.rs` declares the C entry points and `src/port.rs` wraps them in safe gates -- which is why the benchmarks and the fuzz targets need none. |
| `benches/` | Three criterion suites, `deflate_bench.rs`, `inflate_bench.rs` and `checksum_bench.rs`. Its own package `zlib-rs-benches` with its own `Cargo.lock` and its own `deny.toml`. |
| `fuzz/` | Five cargo-fuzz targets. Its own package `zlib-rs-fuzz` with its own `Cargo.lock` and its own `deny.toml`. |

The module tree inside `crates/zlib-rs` mirrors the C translation units one for
one -- `deflate/`, `inflate/`, `trees/`, `adler32/`, `crc32/`, `gz/`,
`infback.rs`, `compress.rs`, `uncompress.rs` -- with two foldings that follow the
C include graph rather than a preference. `trees.c` includes only `deflate.h`, so
the Huffman coder is a sibling of `deflate/` operating on the same state rather
than an independent module; and `inftrees.c` and `inffast.c` share an include set
with `inflate.c` exactly, so they are `inflate/inftrees.rs` and
`inflate/inffast.rs`. The generated tables in `crc32.h`, `trees.h` and
`inffixed.h` become `const` arrays, which is what removes the run-time table
construction and the `<stdatomic.h>` dependency along with it.

`crates/zlib-rs-differential` appears in the `[dependencies]` of neither shipped
crate. That is what keeps reference zlib an oracle rather than a build
dependency: `cargo build --release -p zlib-rs -p libz-rs-sys` never invokes a C
compiler for it.

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
suites still measure against the same oracle, through a path dependency on the
differential crate, so nothing is built twice.

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

Two command-line tools are pinned rather than floating, because a different
release changes the output the gates compare:

    cargo install cbindgen --locked --version 0.29.4
    cargo install cargo-fuzz --locked --version 0.13.2

All three workspace members declare `rust-version = "1.80"` and
`edition = "2021"` through `[workspace.package]`, so the MSRV is a property of
the whole workspace rather than of a hand-picked package list, and both of these
hold:

    cargo +1.80 metadata --locked
    cargo +1.80 check --locked --workspace --all-targets

Edition 2021 with a 1.80 floor is the only valid pairing, not a conservative
choice: edition 2024 requires rustc 1.85 or newer and cargo refuses the
combination outright, so raising the edition would mean raising the floor.
`make rust-msrv` checks the two shipped crates on 1.80 in all three feature
configurations. `benches/` is the one package with a higher floor, for the reason
given above.

`rust-toolchain.toml` also carries the supported-target matrix, and it is worth
reading before making a platform claim. `x86_64-unknown-linux-gnu` is the only
target the port is built, tested, packaged *and* measured on; macOS and Windows
are built and tested by the `build-test` matrix and nothing more; every other
triple is not built at all. "Tier 1" names a rustc platform tier, not a
commitment made here.

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
needs a C toolchain -- correct, but slower, and a machine without a C compiler
will stop there.

Read those as the first step of one recipe rather than as the whole of it.
**`cargo build` alone does not produce an installable shared library**, and the
shortfall is measurable rather than theoretical: the shared object cargo emits
publishes 96 dynamic globals where the C library publishes 111. rustc attaches
its own anonymous version script to every cdylib link and a second one cannot be
layered on top, so `zlib.map` never reaches it; the two variadic entry points
that live in `csrc/` get no dynamic entry; and internal helpers stay visible.
Cargo's `libz.a` *is* complete, because those shim objects are archived into it.
Producing the installable shared library is a separate relink step, and
`crates/libz-rs-sys/src/lib.rs` carries the artifact matrix that states this once.

### With make ###

Seven targets, and the comment at the top of `Makefile.in` describes what each
one does and does not establish:

    make rust          # stage the drop-in libz under target/dropin/
    make rust-test     # link the unmodified C drivers against it and run them
    make rust-symbols  # diff the staged symbol set against a built C library
    make rust-pc       # emit the pkg-config descriptor and gate a consumer
    make rust-header   # regenerate the C header with cbindgen and gate it
    make rust-msrv     # check the two shipped crates on the 1.80 floor
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

The option is off by default, and while it is off no Rust tool is probed:

    ZLIB_BUILD_RUST=OFF -- Use the memory-safe Rust libz for the exported ZLIB::* targets

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

This path is covered in CI. The `cmake-rust` job of `.github/workflows/rust.yml`
walks it in the order a consumer meets it -- configure with the option on, build,
`ctest`, install -- so a break here fails on push rather than waiting to be found
by hand. `.github/workflows/cmake.yml` remains the C matrix.

This path is ELF-only, and it is the CMake path that is ELF-only rather than the
port as a whole: the relink needs a linker that accepts `-soname` and
`--version-script`, the same platform set that gets `zlib.map` for the C shared
library. Configuring with the option on elsewhere stops with a message rather
than emitting a half-formed dylib or import library, and so do a
multi-configuration generator and a cross-compilation toolchain.

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
| `zlib-rs` | `simd` | off | the vectorisable CRC-32 and Adler-32 backends |
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
port mentions -- `cc`, `criterion`, `libfuzzer-sys`, `arbitrary` -- is a
build-dependency or a dev-dependency of a package that ships nothing, so none of
them can reach `libz.so` or `libz.a`. `libc` is different: it is a real, optional
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

The name `simd` promises more than it delivers, and that is deliberate. Both
backends behind it are ordinary portable integer code, written so that LLVM has a
shape it can autovectorise at opt-level 3. There is not one architecture
intrinsic in either, which is exactly what lets the safe core keep
`#![forbid(unsafe_code)]`, and it means correctness does not depend on a CPU
probe: the CRC-32 backend performs no run-time feature test at all, and
Adler-32's `is_supported()` is a throughput hint that selects between two
arrangements of the same arithmetic. A simd-enabled binary computes the right
answer on a machine with no vector unit.

`ZLIB_RS_SIMD=1` in the environment is how make and CMake ask for the feature;
both translate it into `simd` for cargo, and `crates/libz-rs-sys/build.rs` reads
it under `cargo::rerun-if-env-changed=ZLIB_RS_SIMD` -- so flipping it triggers a
rebuild -- and then checks that the environment request and the feature request
agree rather than letting a mismatch pass unnoticed. A direct cargo build passes
`--features simd` instead. `ZLIB_RS_SIMD=0`, or leaving it unset, builds the
scalar backends.

    ZLIB_RS_SIMD=1 cargo build --locked -p libz-rs-sys --features simd

Why the feature reaches the two checksums and nothing else: a checksum is one
scalar however it is computed, so vectorising it cannot perturb the bitstream.
Vectorising match-finding would change which matches are emitted, and that is
prohibited for the reason the next section gives. Neutrality is checked rather
than argued -- the crate's own equivalence sweep, the differential suites and a
300-second fuzz run all execute with the feature on.

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

Eight decision points determine the output. They are numbered the same way in the
module comments, so a comment saying "decision point #3" means this one:

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

* checksum vectorisation behind the `simd` feature;
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

1. **Stream-pointer validation.** Every exported stream function receives a
   `z_streamp`. It must be non-null, aligned, and addressing a caller-allocated
   `z_stream`. Nullability is part of the published contract -- a null stream
   yields `Z_STREAM_ERROR` -- so the guard always precedes the first dereference.
2. **Reconstructing the input and output slices.** `(next_in, avail_in)` and
   `(next_out, avail_out)` are pointer/length pairs. `from_raw_parts` is called
   once, on entry, and only the resulting safe slices travel inward. An
   `avail_in` of zero with a null `next_in` must produce an empty slice rather
   than undefined behaviour.
3. **The opaque `state` round-trip.** `z_stream::state` is caller-visible but
   caller-opaque. Before it is treated as state it is tag-validated, exactly as
   `deflateStateCheck` and `inflateStateCheck` do in the C sources, so a foreign,
   stale or already-freed stream is rejected instead of trusted.
4. **Invoking `zalloc` and `zfree`.** These are caller-supplied C function
   pointers, modelled as a nullable `Option<unsafe extern "C" fn(...)>` so that
   `Z_NULL` is `None` and selects the internal default. A block obtained from a
   caller's `zalloc` is released only through that same caller's `zfree`.
5. **C strings and varargs.** Paths reaching `gzopen` are assumed NUL-terminated
   and readable for the duration of the call; strings returned by `gzerror`,
   `zError` and `zlibVersion` are `'static` or owned by the stream and outlive
   the caller's use of them. `gzprintf` is variadic and `gzvprintf` takes a
   `va_list`, which makes them the hardest two items in the port: stable Rust can
   neither define a C-variadic function nor name a `va_list`, so both are C shims
   under `crates/libz-rs-sys/csrc/` that route into a bounded formatting buffer
   mirroring the C `vsnprintf` path.
6. **Writing through a caller's `gz_header`.** `inflateGetHeader` fills caller
   buffers, and every write is clamped to `extra_max`, `name_max` and `comm_max`.
   This is historically the source of gzip-header overflow defects, and here it
   is a length-checked slice write.

Those are the shipped library. Outside it, three dev-only places have `unsafe`
and each is unavoidable: `crates/zlib-rs-differential/src/oracle.rs` declares the
C oracle, `src/port.rs` holds the safe boundary gates everything else drives the
C ABI through, and the suites under `crates/libz-rs-sys/tests/` need it because
*calling* an `extern "C"` function is itself an unsafe operation. Those gates are
deliberately not part of the shipped facade -- a test harness is not one of the
six categories -- and what they buy is that all three benchmark suites and all
five fuzz targets contain no `unsafe` at all.

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
  `unsigned long`: 8 bytes on LP64 and 4 on LLP64 Windows. Hardcoding `u64` or
  `u32` silently corrupts `total_in`, `total_out`, `adler` and every checksum
  return value on one of the two. The same reasoning governs `z_size_t`,
  `z_off_t` and `z_off64_t`.
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

### The contract is immutable, and four mechanical gates hold it ###

`zlib.h`, `zconf.h` and `zlib.map` are the public contract. They are read, and
`zlib.map` is handed to the linker; none of the three is ever edited. That does
not depend on review:

1. **Compile-time layout assertions** --
   `crates/libz-rs-sys/src/layout_assertions.rs`. `const _: () = assert!(...)`
   over `size_of` and `offset_of!`, evaluated on every build, so layout drift is a
   compile error rather than a run-time surprise.
2. **The cbindgen header comparison** -- `make rust-header` and the `header` CI
   job, against the rules `cbindgen.toml` specifies.
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

Because the crate is dev-only and appears in no shipped `[dependencies]`,
`cargo build --release` for either shipped artifact never touches a C compiler,
and there is **no build-time network dependency and no submodule** anywhere in
this arrangement.

### Running it ###

The default run is quick and is what a plain `cargo test` reaches:

    cargo test --locked -p zlib-rs-differential --release

The exhaustive sweeps are `#[ignore]`d so that a routine `cargo test` stays
usable, and are armed by environment variable:

    ZLIB_RS_DIFFERENTIAL_EXHAUSTIVE=1 ZLIB_RS_INTEROP_EXHAUSTIVE=1 \
      cargo test --locked -p zlib-rs-differential --release -- \
      --ignored --test-threads "$(nproc)"

Both commands are the gate; the `differential` and `differential-exhaustive` jobs
of `.github/workflows/rust.yml` run them, the latter across eight shards selected
with `ZLIB_RS_DIFFERENTIAL_SHARD=i/8`. `--release` is not an optimisation
preference: the sweeps are large enough that a debug build turns them from slow
into impractical.

### The matrix ###

The exhaustive run is what covers the space the port is required to be
byte-identical across:

| Dimension | Values |
|---|---|
| level | 0 through 9 |
| `windowBits` | 9 to 15 (zlib), -9 to -15 (raw), 25 to 31 (gzip) -- all three container formats |
| `memLevel` | 1 through 9 |
| strategy | `Z_DEFAULT_STRATEGY`, `Z_FILTERED`, `Z_RLE`, `Z_HUFFMAN_ONLY`, `Z_FIXED` |
| flush | `Z_NO_FLUSH`, `Z_PARTIAL_FLUSH`, `Z_SYNC_FLUSH`, `Z_FULL_FLUSH`, `Z_BLOCK`, `Z_FINISH` |
| input | every fixture in the committed corpus |
| feeding | single-shot **and** incremental, because chunk boundaries interact with flush handling and with pending-buffer state |

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

Each command below is the form CI uses. `.github/workflows/rust.yml` runs all of
them across eighteen jobs, and its per-job comment blocks record what each one
establishes and what it does not.

### Unit and integration tests ###

    cargo test --locked --workspace
    cargo test --locked --workspace --release
    cargo test --locked --workspace --release --all-features

The unit tests of all three crates plus the integration suites under
`crates/*/tests/`. `crates/libz-rs-sys/tests/` carries `abi_layout.rs`,
`c_api_parity.rs`, `gz_memory.rs`, `gz_printf.rs`, `rust_api_surface.rs`,
`symbol_parity.rs` and `write_only_window.rs`; `symbol_parity.rs` is the `nm`
parity gate in test form.

Several assertions in that suite inspect the *staged* library, which cargo does
not build, so they skip by default. Arm them:

    make rust CARGO=cargo
    ZLIB_RS_REQUIRE_PACKAGED=1 cargo test --locked --release \
        -p libz-rs-sys --test symbol_parity -- --nocapture

`ZLIB_RS_REQUIRE_PACKAGED=1` turns each skip into a failure naming what went
unverified; the `symbols` job sets it after staging and additionally fails on any
`SKIP:` line, so the artifact that ships cannot be recorded green without having
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

The `asan` job runs that command twice -- the second time in a separate
`CARGO_TARGET_DIR`, because the two fingerprint differently and would otherwise
invalidate each other on every run -- and then three more things: the three C
drivers against an instrumented `libz.a`; the differential harness with
`CFLAGS=-fsanitize=address` so the C oracle archive is instrumented rather than
merely linked; and the criterion suites compiled under the sanitizer and executed
once each through criterion's test mode. What ASan does *not* cover is the fuzz
targets, because `cargo fuzz` applies AddressSanitizer itself, per target.

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

### Lint, format, dependency and documentation policy ###

    cargo fmt --all -- --check
    cargo clippy --locked --workspace --all-targets -- -D warnings
    cargo deny --locked --all-features check

Zero warnings, everywhere. Those three are the `format`, `lint` and `deny` jobs.

Documentation is enforced by the compiler rather than by a fourth command: every
crate carries `#![deny(missing_docs)]` and the rustdoc link lints
(`broken_intra_doc_links`, `private_intra_doc_links`, `redundant_explicit_links`,
`invalid_codeblock_attributes`) in its own source, so an undocumented public item
and a broken intra-doc link both fail an ordinary build. The convention on top of
that is that each ported function names the C source it derives from, so behaviour
can be traced back to the oracle. To read the result locally:

    cargo doc --locked --workspace --no-deps --all-features

The lint levels live in the root `Cargo.toml`'s `[workspace.lints]`:
`clippy::all` and `clippy::pedantic` at deny, plus the no-panic set listed in the
safety section. Thresholds and the clippy-side `msrv`
live in `clippy.toml`; `.rustfmt.toml` carries the formatting policy and may hold
stable options only, since the channel is pinned to stable. `deny.toml` is the
licence, advisory, ban and source policy -- among other things it denies wildcard
requirements, denies duplicate versions including through dev-dependencies, and
allows exactly one registry.

`benches/` and `fuzz/` each need those three run again with their own manifest
path and their own cargo-deny configuration, for the workspace-boundary reason
given earlier:

    cargo fmt    --manifest-path fuzz/Cargo.toml --all -- --check
    cargo clippy --locked --manifest-path fuzz/Cargo.toml --all-targets -- -D warnings
    cargo deny   --locked --manifest-path fuzz/Cargo.toml --config fuzz/deny.toml check

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
function-like macros, so those differences are expected and the artifact exists so
a reviewer can confirm the difference is only that. And `generated_zlib.h` is
git-ignored scratch output that must never be committed: its header guard is
`ZLIB_H` on purpose, so it would shadow the real header if it ever reached an
include path. `zlib.h` is read-only and is never regenerated from it.
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
`--version-script=zlib.map`. `inflate_table` is the instructive case, because the
C reference keeps it reachable in `libz.a` -- the unmodified `test/infcover.c`
calls it directly -- while `zlib.map` hides it in `libz.so`, and the port
reproduces exactly that split.

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

The `ldd` line is not optional -- see the install section for what happens without
it.

### What the other workflows still do ###

The seven pre-existing workflows stay C-focused, and that is deliberate.
`c-std.yml` is the header-immutability guard, still proving the unchanged
`zlib.h` compiles from C89 through gnu2x. `fuzz.yml` keeps OSS-Fuzz pointed at the
C oracle. `configure.yml`, `msys-cygwin.yml` and `others.yml` keep the
cross-compilation and exotic-OS matrices C-only, because those platforms are
outside the port's supported-target matrix.

`contribs.yml` is the one exception, and it is the most external signal available:
alongside its C matrix it carries a `ci-cmake-rust` job that installs a
`-DZLIB_BUILD_RUST=ON` prefix, symlink chain and all, and then builds the
out-of-scope `contrib/` consumers against it. Those consumers were written against
the C library and know nothing about this port, so they are a drop-in test nobody
here wrote.

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

`crates/zlib-rs-differential/corpus/fetch_silesia.sh` is invoked by a human and
by nothing else -- no `Cargo.toml`, no `build.rs`, no test and no workflow
references it.

    ./crates/zlib-rs-differential/corpus/fetch_silesia.sh --help
    ZLIB_RS_SILESIA_DIR=/var/cache/silesia \
      ./crates/zlib-rs-differential/corpus/fetch_silesia.sh --sha256 <digest>

An expected SHA-256 is **required on every run**, including `--verify-only`; a run
without one is refused rather than trusted. The other flags are `--force`, to
re-fetch into an already-populated destination, and `--verify-only`, which
reports on an existing download and fetches nothing. The destination is
`$ZLIB_RS_SILESIA_DIR` when set and `<repo-root>/target/silesia` otherwise, which
is git-ignored through `/target/`; `ZLIB_RS_SILESIA_URL`,
`ZLIB_RS_SILESIA_SHA256`, `ZLIB_RS_SILESIA_MEMBERS` and
`ZLIB_RS_SILESIA_MEMBER_SHA256` cover the rest. Run `--help` for the authoritative
list.

Benchmarks probe that path and **skip gracefully** when it is absent, so a
benchmark run without Silesia produces a useful local signal but not the
acceptance measurement.

## Performance targets ##

Every number here is a **goal or a gate**, not a measurement. Nothing in this file
reports an achieved result; the criterion output on the machine in front of you
is the only thing that does.

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
top of this file. Each suite measures the port and the in-process C oracle **under
one sampler**, which is what removes two build artifacts, two page-cache states
and two process start-ups from the comparison -- so a number is always a ratio
rather than an absolute. The oracle is the same one
`crates/zlib-rs-differential/build.rs` compiles, reached through a path
dependency, so nothing is built twice.

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

Tier matters too. Every summary line CI sees was produced from the committed
minimal corpus, so a green CI run is a regression signal rather than the
acceptance measurement -- the acceptance figures are read from the Silesia groups,
which need `fetch_silesia.sh` to have been run and which no CI job provisions.
`deflate_bench.rs` and `inflate_bench.rs` emit the `RATIO-SUMMARY`,
`MEMORY-SUMMARY` and `ALLOC-BALANCE` lines the `bench` job parses;
`checksum_bench.rs` emits none of those and is not gated, because it compares
backends against each other, which is information rather than a threshold.

If a gate is tight, the admissible levers are the output-neutral ones from the
byte-identity section -- checksum vectorisation behind `simd`, memory-copy
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
