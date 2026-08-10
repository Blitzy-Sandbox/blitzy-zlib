# The Rust implementation of zlib — developer guide

This is the developer guide for the memory-safe Rust reimplementation of this
library. The top-level `README` is the *user* document: it says how to build and
install the library and what the Cargo features do. This file is for people
working *on* the port, and it covers the four things the user document
deliberately leaves out — the full feature surface, the differential tests
against the in-tree C sources, the verification tooling (Miri, AddressSanitizer,
cargo-fuzz, criterion), and the exact drop-in install procedure.

Nothing here is a substitute for the comment blocks in the files themselves. Each
manifest, build script and test module carries the derivation of its own rules;
this file is the map, not the territory. Where the two ever disagree, the file is
right and this one needs updating.

## What the workspace contains

The Cargo workspace sits at the repository root, beside the C sources it was
ported from. Those sources are read and never edited: they are the algorithmic
oracle the port is differentially tested against, and the C build stays green so
that oracle keeps working.

| Path | Role |
|---|---|
| `crates/zlib-rs` | The safe core. `#![forbid(unsafe_code)]`, `no_std` + `alloc`, and an empty `[dependencies]` table. Every algorithm lives here. |
| `crates/libz-rs-sys` | The C ABI facade, and the only crate permitted to write `unsafe`. `[lib] name = "z"` with `crate-type = ["cdylib", "staticlib", "rlib"]`. |
| `crates/zlib-rs-differential` | Dev-only oracle harness. Its build script compiles the in-tree C translation units into a test-only archive, so both implementations are co-resident in one process and comparing them is a memory compare. |
| `benches/` | Three criterion suites, attached to the differential crate because they measure against the same oracle. |
| `fuzz/` | Five cargo-fuzz targets. Its own workspace, listed under `exclude` in the root `Cargo.toml`, with its own `Cargo.lock` and its own `deny.toml`. |

`crates/zlib-rs-differential` appears in the `[dependencies]` of neither shipped
crate. That is what keeps reference zlib an oracle rather than a build
dependency: `cargo build --release -p zlib-rs -p libz-rs-sys` never invokes a C
compiler for the oracle.

## Toolchain

`rust-toolchain.toml` pins `channel = "stable"` with `rustfmt` and `clippy`, so
cargo selects a suitable compiler by itself inside this checkout.

The two shipped crates declare `rust-version = "1.80"`, and that is the MSRV the
library promises. `crates/zlib-rs-differential` declares `rust-version = "1.86"`
for itself, because criterion 0.8.2 declares 1.86 and reaches clap crates whose
manifests are `edition = "2024"`; cargo 1.80 cannot parse those, so any command
that resolves the whole workspace dev graph on 1.80 fails at resolve time. The
MSRV check therefore names the shipped packages explicitly:

    make rust-msrv

Miri and AddressSanitizer both need nightly. `miri` is a nightly-only rustup
component and `-Zsanitizer=address` is a nightly-only flag:

    rustup toolchain install nightly --profile minimal --component miri,rust-src

Two command-line tools are pinned rather than floating, because a different
release changes the output the gates compare:

    cargo install cbindgen --locked --version 0.29.4
    cargo install cargo-fuzz --locked

`rust-toolchain.toml` also carries the supported-target matrix. Read it before
making a platform claim: `x86_64-unknown-linux-gnu` is the only target that is
built, tested, packaged *and* measured, while macOS and Windows are built and
tested but not packaged and not measured.

## Building

    cargo build --release -p zlib-rs -p libz-rs-sys
    cargo build --release -p libz-rs-sys --features libz-compat

Name the packages. A bare `cargo build` at the workspace root selects
`default-members`, which is the shipped pair — but `--workspace` also reaches
`crates/zlib-rs-differential`, whose build script needs a C toolchain.

`cargo build` alone does **not** produce an installable shared library. Cargo's
`libz.a` IS complete, because the two C translation units in
`crates/libz-rs-sys/csrc/` — the variadic `gzprintf`/`gzvprintf` and
`inflate_table`'s `codetype` prototype, none of which stable Rust can declare —
are archived into it. Cargo's `libz.so` is not: rustc attaches its own anonymous
version script to every cdylib link, so `zlib.map` cannot be layered on top, the
two variadic exports get no dynamic entry, and internal helpers stay visible.
Producing the installable shared library is a separate relink step, and
`crates/libz-rs-sys/src/lib.rs` carries the artifact matrix that states this
once.

## The Cargo feature surface

Every feature every member declares, and what each one expands to. `cargo
metadata` is the authority; this table is a convenience.

| Crate | Feature | Default | Expands to |
|---|---|---|---|
| `zlib-rs` | `default` | — | nothing; the core is `no_std` + `alloc` with no feature on |
| `zlib-rs` | `rust-api` | off | the idiomatic Rust surface — typed configs and slice-based helpers |
| `zlib-rs` | `simd` | off | vectorised CRC-32 and Adler-32 backends |
| `zlib-rs` | `std` | off | the core's file I/O; without it the core is `no_std` |
| `libz-rs-sys` | `default` | on | `libz-compat`, `gz` |
| `libz-rs-sys` | `libz-compat` | on (via `default`) | nothing further; it gates the exported `extern "C"` surface |
| `libz-rs-sys` | `gz` | on (via `default`) | `libz-compat`, `zlib-rs/std`, `dep:libc` |
| `libz-rs-sys` | `libc` | off | `dep:libc` alone |
| `libz-rs-sys` | `simd` | off | `zlib-rs/simd` — a forwarding feature, so `-p libz-rs-sys --features simd` reaches the core |
| `zlib-rs-differential` | `simd` | off | `zlib-rs/simd`, `libz_rs_sys/simd` — forwards to both |

Three consequences are worth stating outright, because a feature table read as a
flat list hides them.

`std` is off in `zlib-rs` but on in a default facade build, because `gz` enables
`zlib-rs/std`. The gz layer needs file I/O; the core does not.

`libc` is off as a named feature but present in a default build, for the same
reason: `gz` enables `dep:libc`. The explicit `libc` feature exists so the
dependency is a stable part of the facade's surface and a downstream `--features
libc` keeps resolving; enabling it alone changes nothing observable, because
every `libc` call site is inside gz code. `--no-default-features`,
`--features libz-compat` and every `zlib-rs`-only build resolve without `libc`,
and the core keeps its empty `[dependencies]` unconditionally.

`simd` is declared by three crates, not one. The core has the implementation; the
facade and the differential crate declare forwarding features so a package-scoped
`--features simd` reaches it. Whichever spelling is used, the feature is
**output-neutral** — it reaches the two checksums and nothing else, and a
checksum is one scalar however it is computed.

`ZLIB_RS_SIMD=1` in the environment is how make and CMake ask for it; both
translate it into `simd` for cargo, and `crates/libz-rs-sys/build.rs` then checks
that the environment request and the feature request agree rather than letting a
mismatch pass. A direct cargo build uses `--features simd`.

## Testing

    cargo test --locked --workspace

That runs the unit tests of all three crates plus the integration suites under
`crates/*/tests/`. `crates/libz-rs-sys/tests/` carries `abi_layout.rs`,
`c_api_parity.rs`, `gz_memory.rs`, `rust_api_surface.rs` and `symbol_parity.rs`;
the last of those is the `nm` parity gate in test form.

### The differential tests

`crates/zlib-rs-differential` is where byte-identical output is established. Its
build script compiles the in-tree C sources into a test-only archive and exposes
them under a `c_`-prefixed `extern "C"` surface, so a test can call both
implementations and compare buffers directly.

The default run is quick and is what `cargo test` reaches:

    cargo test --locked -p zlib-rs-differential --release

The exhaustive sweeps are `#[ignore]`d so that a routine `cargo test` stays
usable, and are armed by environment variable:

    ZLIB_RS_DIFFERENTIAL_EXHAUSTIVE=1 ZLIB_RS_INTEROP_EXHAUSTIVE=1 \
      cargo test --locked -p zlib-rs-differential --release -- \
      --ignored --test-threads "$(nproc)"

Both commands are the gate. The `differential` job of
`.github/workflows/rust.yml` runs both, in that order, and the exhaustive one is
what covers the full matrix the port is required to be byte-identical across:
level 0-9, `windowBits` in all three container forms, `memLevel` 1-9, all five
strategies, all six flush modes, every corpus fixture, and single-shot against
incremental feeding. `--release` is not an optimisation preference — the sweeps
are large enough that a debug build changes them from slow to impractical.

Interoperability is asserted separately from round-tripping:
C-compressed streams inflate under the Rust implementation, and Rust-compressed
streams inflate under the C one.

### The corpus

`crates/zlib-rs-differential/corpus/minimal/` is committed, deterministic and
small, and every correctness gate runs against it — so `cargo test` and CI never
need the network. `corpus/README.md` records the provenance of each fixture and
why its class is there.

`corpus/fetch_silesia.sh` is opt-in and is invoked by a human and by nothing
else. The throughput and memory acceptance targets are read from Silesia
members, so a benchmark run without it produces a useful local signal but not the
acceptance measurement.

## Verification tooling

### Miri

    cargo +nightly miri test -p zlib-rs

Scoped to the safe core, which is what makes it meaningful: the core contains no
FFI, so Miri can interpret it without hitting an operation it cannot model. Read
the result as "no undefined behaviour observed while interpreting the `zlib-rs`
test suite" — Miri is an interpreter, not a prover, and what a test does not
execute it does not analyse. The corpus-folding tests carry
`#[cfg_attr(miri, ignore)]` and are skipped here; the differential job covers
them.

### AddressSanitizer

    RUSTFLAGS=-Zsanitizer=address RUSTDOCFLAGS=-Zsanitizer=address \
      cargo +nightly test --locked -p libz-rs-sys --target x86_64-unknown-linux-gnu

Scoped to the facade, which is where raw pointers actually exist. Setting
`RUSTFLAGS` without `RUSTDOCFLAGS` instruments the library but not the doctests,
which then fail to link with undefined `__asan_*` symbols — set both. An explicit
`--target` is required so build scripts and proc macros are built uninstrumented.

The same job also builds an instrumented `libz.a` and links the unmodified
`test/example.c`, `test/minigzip.c` and `test/infcover.c` against it under
`ASAN_OPTIONS=detect_leaks=1:detect_stack_use_after_return=1:abort_on_error=1`.
`infcover.c` earns its place here: its allocator fills every block with `0xa5`
rather than zero, so any code path assuming zero-initialised memory shows up, and
its `mem_done()` detects leaks, non-LIFO frees and rogue frees through the
injected `zalloc`/`zfree` hooks.

Note the scope honestly. ASan covers `crates/libz-rs-sys` and those three C
drivers. It does not cover `crates/zlib-rs-differential`, the bench targets, or
the C oracle archive, which is compiled uninstrumented.

### Fuzzing

    cargo +nightly fuzz run fuzz_deflate      -- -max_total_time=300
    cargo +nightly fuzz run fuzz_inflate      -- -max_total_time=300
    cargo +nightly fuzz run fuzz_inflate_back -- -max_total_time=300
    cargo +nightly fuzz run fuzz_gz_roundtrip -- -max_total_time=300
    cargo +nightly fuzz run fuzz_checksum     -- -max_total_time=300

Run from `fuzz/`, which is a workspace of its own. The bar is zero crashes and
zero hangs at 300 seconds per target. `fuzz_inflate` is the one that matters
most: `inflate` is the entry point that reads untrusted bytes.

Because `fuzz/` is excluded from the root workspace, the root `cargo clippy
--workspace --all-targets` cannot reach it, and a workspace lint table does not
cross a workspace boundary. `fuzz/Cargo.toml` therefore restates the lint policy
in `[lints.rust]` and `[lints.clippy]` tables of its own, and the `lint` job runs
clippy a second time with `working-directory: fuzz`.

### Benchmarks

    cargo bench --locked -p zlib-rs-differential

The `-p` is required: the suites are attached to the differential crate, so a
bare `cargo bench` from the root finds nothing. Each suite measures the port and
the in-process C oracle under one sampler, which is what removes cross-process
noise from the comparison.

`deflate_bench.rs` and `inflate_bench.rs` emit `RATIO-SUMMARY`, `MEMORY-SUMMARY`
and `ALLOC-BALANCE` lines that the `bench` job parses. `checksum_bench.rs` emits
none of those and is not gated: it compares backends against each other, which is
information rather than a threshold.

Tier matters when reading a number. Every summary line CI sees was produced from
the committed minimal corpus, so a green run is the CI signal. The acceptance
measurement — within 10% for decompression and for levels 1, 6 and 9, and within
15% per-stream memory — is read from the `deflate_silesia` and `inflate_silesia`
groups, which need `fetch_silesia.sh` to have been run and which no CI job
provisions.

## Quality gates

    cargo fmt --all -- --check
    cargo clippy --locked --workspace --all-targets -- -D warnings
    cargo deny --all-features check
    RUSTDOCFLAGS='-D warnings' cargo doc --locked --workspace --no-deps --all-features

Levels live in the root `Cargo.toml`'s `[workspace.lints]` — `clippy::all` and
`clippy::pedantic` at deny, plus `unwrap_used`, `expect_used`,
`indexing_slicing`, `panic`, `panic_in_result_fn`, `todo`, `unimplemented`,
`unreachable`, `exit` and `undocumented_unsafe_blocks`. Thresholds and `msrv`
live in `clippy.toml`. `.rustfmt.toml` carries the formatting policy and may hold
stable options only, since the channel is pinned to stable.

`fuzz/` needs the same three commands run again from inside that directory,
because it is a separate workspace with a separate lock file and a separate
cargo-deny configuration.

## The ABI contract, and the four gates that hold it

`zlib.h`, `zconf.h` and `zlib.map` are the immutable public contract. They are
read, and `zlib.map` is handed to the linker; none of the three is ever edited.
Four independent mechanical gates enforce that, so the contract does not depend
on review:

1. **Compile-time layout assertions** — `crates/libz-rs-sys/src/layout_assertions.rs`.
   `const _: () = assert!(...)` over `size_of` and `offset_of!`, evaluated on
   every build, so layout drift is a compile error rather than a runtime
   surprise.
2. **The cbindgen header comparison** — `cbindgen.toml` specifies it in ten
   rules. `make rust-header` owns the name half (A1, A2, A5, A9) and the
   standalone C89/C99/C17/C++17 compile sweep with and without `_WIN32`; the
   `header` CI job invokes that target and then owns the shape half (A3, A4, A6,
   A7, A8 and rule 10) by re-declaring every generated prototype against
   `zlib.h` so the C compiler decides whether the signatures agree, and by
   diffing struct sizes, field offsets and integer macro values from a probe
   program per side.
3. **The `nm` symbol-parity diff** — `crates/libz-rs-sys/tests/symbol_parity.rs`
   under any `cargo test -p libz-rs-sys`, `make rust-symbols` locally against a
   built C library, and the `symbols` CI job as the gate. The baseline is 111
   exported symbols: 95 functions plus the 16 symbol-version nodes `zlib.map`
   declares.
4. **The C89 → gnu2x compile sweep** — `.github/workflows/c-std.yml`, which
   proves the unchanged `zlib.h` still compiles across every dialect.

`zlibCompileFlags()` is computed from the Rust build's own type sizes rather than
copied, because the value is target-dependent. `ZLIB_VERSION` and `ZLIB_VERNUM`
are reproduced verbatim, because `deflateInit_` and `inflateInit_` compare the
caller's compile-time version against the library's.

## Installing the drop-in library

    make rust
    make rust-test
    make rust-symbols

`make rust` relinks cargo's complete static archive through `zlib.map` with the
right SONAME and stages the installable set under `target/dropin/`, naming it
from `ZLIB_VERSION` in `zlib.h`:

    libz.so.1.3.2.1-motley     the real file
    libz.so.1                  symlink, and the SONAME
    libz.so                    symlink
    libz.a                     the static library

**The symlink chain is a correctness requirement, not packaging polish.** The
SONAME is `libz.so.1`, so a directory holding only `libz.so` leaves the loader
free to keep searching and bind the system zlib instead — silently. A drop-in
test then passes while exercising the C library. That is why `make rust-test`
asserts the binding with `ldd` rather than inferring it from a test that passed,
and why link steps must name what `make rust` staged rather than cargo's
`target/<profile>` directory. Cargo's build script leaves a
`README-cargo-artifacts.txt` there saying exactly this.

Relinking is a post-link step and therefore belongs to `Makefile.in`'s `rust`
target and to the CMake block, never to `build.rs`: a build script runs *before*
the link, so any versioned name it created would dangle. `build.rs` prunes stale
aliases for that reason.

`make rust` also diffs the staged library's exported symbols against the C
library's — **but only when that library exists.** `ZLIBREF` defaults to the C
`$(SHAREDLIBV)` built in this tree; when it is absent, `make rust` prints a note
saying the extra C-versus-Rust diff was skipped and carries on. It still
enforces the checks that need no C library: every function `zlib.h` declares is
exported and nothing else is, the count is exactly the expected function total,
the version-node count matches `zlib.map`, none of `zlib.map`'s `local:` names is
exported, and the SONAME is the derived one.

So do not read a green `make rust` on an unconfigured tree as symbol parity
against C. Use either of these, both of which fail rather than skip:

    make                                    # build the C library first, then make rust
    make rust-symbols ZLIBREF=/path/to/libz.so.<version>

The `symbols` CI job takes the first route: it builds the C library, stages the
drop-in with `make rust ZLIBREF=<C lib>`, and diffs both `nm` tables.

## CMake

    cmake -S . -B build -DZLIB_BUILD_RUST=ON
    cmake --build build

`ZLIB_BUILD_RUST` re-points the exported `ZLIB::ZLIB` and `ZLIB::ZLIBSTATIC`
targets at the Rust libraries. The C targets `zlib` and `zlibstatic` are always
defined and built regardless, because they are the differential oracle and
`contrib/` consumes them by name.

Re-pointing those two aliases is the entire mechanism by which the CTest suite
runs against either implementation: `example.c`, `minigzip.c` and `infcover.c`
link the alias and are compiled from the same unmodified sources either way. Note
what that does and does not give you — the suite runs against *one*
implementation per configure, the one selected by the option, so a single build
tree does not compare them side by side. `test/CMakeLists.txt` records the
current state of that in full.

This CMake path is ELF-only, and it is the CMake path that is ELF-only rather
than the port as a whole: the relink through `zlib.map` needs a linker that
accepts `-soname` and `--version-script`, which is the same platform set the C
shared target hands `zlib.map` to. Configuring with the option on elsewhere
fails with a message rather than emitting a half-formed dylib or import library.

Note also that no CI job configures with `ZLIB_BUILD_RUST=ON`:
`.github/workflows/cmake.yml` carries no Rust job, so this path is exercised by
hand.

## Where the rules are written down

Each file carries the derivation of its own rules, and that is deliberate — the
reasoning stays next to the code it governs. When something here is not enough:

| Question | File |
|---|---|
| Why is the unsafe surface shaped this way, and what does each block promise? | `crates/libz-rs-sys/src/lib.rs`, and the `// SAFETY:` comment on each block |
| Which artifact is installable, and why is cargo's cdylib not? | `crates/libz-rs-sys/src/lib.rs` — the artifact matrix |
| What exactly must the generated header match? | `cbindgen.toml` — the ten normalisation rules and the accepted divergences |
| What may a dependency be, and what may it never be? | `deny.toml`, and the root `Cargo.toml`'s `[workspace.dependencies]` notes |
| Which targets are supported, and to what degree? | `rust-toolchain.toml` — the supported-target matrix |
| What does each corpus fixture exist to exercise? | `crates/zlib-rs-differential/corpus/README.md` |
| Which decisions must be byte-identical to the C encoder? | the module comments in `crates/zlib-rs/src/deflate/` and `crates/zlib-rs/src/trees/` |
| What does each CI job establish, and what does it not? | `.github/workflows/rust.yml` — the per-job comment blocks |
