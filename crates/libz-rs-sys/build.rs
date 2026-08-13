// ============================================================================
//  crates/libz-rs-sys/build.rs -- the linker and packaging build script
// ============================================================================
//
// This script has exactly five jobs and deliberately does nothing else.  Every
// directive it emits traces back to `configure`, to `Makefile.in`, or to a
// requirement of the port:
//
//   1. ZLIB_RS_SIMD -- the documented build-time toggle for the vectorised
//      checksum backends.  A build script cannot switch a Cargo feature on or
//      off, so this script VALIDATES the variable against the resolved `simd`
//      feature and refuses a request it cannot satisfy; the translation from
//      variable to feature belongs to whatever invokes cargo, and
//      `Makefile.in`'s `rust` target performs it.  See the long comment above
//      `check_simd_request`.
//
//   2. SONAME -- `-Wl,-soname,libz.so.1` on ELF targets, byte for byte the
//      argument `configure` bakes into LDSHARED (configure L334 for
//      Linux/GNU/Solaris/Haiku, L336 for the BSDs).  This is what makes an
//      already-linked binary that records `DT_NEEDED libz.so.1` bind to this
//      library without being recompiled.
//
//   3. zlib.map -- the immutable version script.  It is located, validated and
//      declared as a rebuild trigger here, and a request to hand it to rustc's
//      own cdylib link is REFUSED with the measurements that show why it cannot
//      work.  See the long comment above `reject_version_script_passthrough`.
//
//   4. Artifact-directory hygiene -- a plain-text notice next to cargo's
//      artifacts saying what each one is and which command produces an
//      installable library, plus removal of the `libz.so.1` /
//      `libz.so.<ZLIB_VERSION>` symlinks an older revision of this script used to
//      stage there.  It NO LONGER STAGES THE VERSIONED CHAIN: a build script runs
//      before the link, so the links it creates can be left dangling by a
//      `cargo check`, a failed build or a `cargo clean -p`, and cargo puts that
//      same directory first on the library search path of every build script,
//      which made the host build tools load the library under construction.  Both
//      were measured; see the long comment above `tidy_artifact_dir`.  The chain
//      belongs to the packaging step, which runs after the link and stages the
//      library that can actually satisfy the names.
//
//   5. The C ABI shim -- `csrc/gzprintf_shim.c`, and it is the only one: it defines
//      the variadic `gzprintf` and the `va_list`-taking `gzvprintf`, which stable
//      Rust cannot define at all (`c_variadic` and `core::ffi::VaList` are both
//      unstable; measured E0658 on 1.80.1 and on current stable).  It is COMPILED
//      HERE, with the platform compiler, and archived so that rustc merges it into
//      the `libz.a` it produces; `cfg(zlib_rs_gzprintf)` -- and with it
//      `zlibCompileFlags()` bit 27 -- follows from that compile actually happening.
//      Everything else in the contract, `inflate_table` included, is defined in
//      Rust.  See the long comment above `build_c_abi_shims`.
//
// WHAT A BARE `cargo build` PRODUCES, STATED PRECISELY
//
// After this script has run and the crate has been linked, `target/<profile>`
// holds `libz.a`, `libz.so` with `SONAME libz.so.1`, `libz.rlib`, and job 4's
// `README-cargo-artifacts.txt` saying which of them may be installed.  It holds
// NO `libz.so.1` and no `libz.so.<ZLIB_VERSION>`, deliberately: those names
// belong to the packaged library that can satisfy them.
//
// `libz.a` IS COMPLETE: it defines all 95 functions `zlib.h` declares, plus
// `inflate_table` as a hidden global, because job 5's object is merged into it.
// It is the installable static library and it is what `test/infcover.c` links.
//
// `libz.so` is NOT the installable shared library, and nothing this script can
// emit would make it one.  One mechanism accounts for the whole difference:
// rustc attaches its OWN anonymous version script to every cdylib link.  So
// `zlib.map` cannot be layered on top of it (four distinct measured failure modes
// are catalogued above `reject_version_script_passthrough`), which costs all 16
// symbol-version nodes; a symbol job 5's objects define gets no dynamic entry at
// all, which costs `gzprintf` and `gzvprintf`; and the three `_zlib_rs_*` helpers
// `zlib.map` hides through `_*` are visible.  Measured: 96 dynamic globals and 0
// version nodes.
//
// The installable shared library is therefore produced by ONE further step --
// `Makefile.in`'s `rust` target, or the CMake equivalent -- which relinks that
// same complete archive through `zlib.map`, filters the compiler-runtime globals,
// and stages the versioned chain in an isolated directory.
// `crates/libz-rs-sys/src/lib.rs` carries the artifact matrix that states this
// once; nothing this script does is a substitute for that step and none of it may
// be described as one.
//
// WHY THE VERSIONED NAMES ARE NOT STAGED HERE
//
// The chain is mandatory where the library is installable, and for a precise
// reason: the SONAME is `libz.so.1`, so that is the name the loader searches for,
// and a directory holding only `libz.so` makes the loader keep searching and
// SILENTLY BIND THE SYSTEM libz.  Reproduced during planning -- `ldd` resolved to
// /lib/x86_64-linux-gnu/libz.so.1 and the probe printed the system version -- so
// every drop-in validation asserts with `ldd` which file was bound.  That is the
// packaging step's job, it stages the chain unconditionally every time it runs,
// and it stages the relinked library that can honour the names.
//
// Staging the same names HERE was measured to cause the very failures the chain
// prevents, because a build script runs BEFORE the link and cannot know whether a
// cdylib will appear at all:
//
//   * `cargo check`, any failed build, and `cargo clean -p libz-rs-sys --release`
//     each left two DANGLING versioned links behind -- and a dangling `libz.so.1`
//     on a loader path is treated as a miss, so the loader falls through to the
//     system libz exactly as if the link had never been there.
//   * `target/<profile>` is first on the library search path of every build script
//     cargo launches, and the host binutils carry `DT_NEEDED libz.so.1` with no
//     RPATH of their own (`ld.so --list /usr/bin/nm`).  With the alias present:
//     six `no version information available` lines per rebuild in this script's
//     own cargo-hidden stderr, 32 in one build of the differential crate, and --
//     after a supported `--no-default-features` build left a zero-export
//     `libz.so` -- `as: symbol lookup error: undefined symbol: deflate`, which
//     wedged every subsequent build until the alias was deleted by hand.
//   * Three files named exactly like an installable drop-in, all reporting
//     `SONAME libz.so.1`, none of them installable, with nothing at the path to
//     say so.
//
// So job 4 prunes those links and writes a notice instead, and this script strips
// LD_LIBRARY_PATH / DYLD_LIBRARY_PATH from its own environment before spawning a
// host tool (`sanitize_library_search_path`), which is the property rather than
// the symptom: nothing on the search path handed to this script may influence the
// tools it drives.  `crates/zlib-rs-differential/build.rs` carries the same call
// for the same reason, and any future build script here that shells out to a
// zlib-linked tool needs it too.
//
// AAP §0.4.1.3's build.rs row asks for the chain here, while §0.3.1.2 and §0.8.3
// assign it to the packaging step; the two cannot both be honoured, and only the
// packaging step's copy is a chain that resolves.
//
// Hard rules this file lives by:
//
//   * It never writes to `zlib.h`, `zconf.h` or `zlib.map`.  Those three are
//     zlib's immutable public contract; this script only ever reads them, and
//     it fails the build loudly rather than silently proceeding without them.
//
//   * It compiles exactly one C translation unit in `csrc/` for the shipped
//     library -- `gzprintf_shim.c` -- plus the test-only `gzvprintf_probe.c`, and no
//     others.  This crate has no `[build-dependencies]` and must not acquire `cc`:
//     the platform compiler is invoked directly, so the AAP's frozen dependency
//     inventory is untouched.  Compiling the C reference implementation is
//     exclusively the differential crate's job and nothing here goes near it.
//
//   * It never touches the network.
//
//   * Outside `OUT_DIR`, the only thing it creates is job 4's
//     `README-cargo-artifacts.txt`, and the only thing it deletes is a symbolic
//     link at one of the two versioned-alias paths whose link text is exactly the
//     name cargo's own artifact has -- that is, precisely what an older revision of
//     this script wrote there.  It will not remove or overwrite a regular file even
//     at those paths, and it will not touch a link that points anywhere else.
//     Job 5's object files and their archive live in `OUT_DIR`, which is cargo's own
//     scratch directory for exactly that purpose.  It writes nothing in the source
//     tree, and in particular it never touches `libz.so`, `libz.a`, or anything else
//     cargo owns.
//
//   * It uses `std` only, and the modern `cargo::` directive prefix
//     throughout -- never the legacy single-colon `cargo:` form.  The two are
//     not interchangeable: under `cargo::`, an unrecognised key is a hard
//     error instead of being silently ignored, which is exactly the behaviour
//     wanted from a script that governs the ABI of a system library.
//
// About the one lint allowed below.  `clippy::panic` is denied workspace-wide
// because a compression library must never abort a caller's process; library
// code returns a status code instead.  A build script is not a library path:
// it runs at build time, in its own process, and a panic there is precisely
// how Cargo reports "this build cannot proceed".  The obvious alternative,
// `std::process::exit`, is denied too (`clippy::exit`) and would throw the
// diagnostic away.  So the lint is allowed for this file only, and every
// `panic!` below names the offending input and the fix for it.  Nothing on the
// ordinary build path panics.
#![allow(clippy::panic)]

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
//  Contract files and artifact names
// ---------------------------------------------------------------------------

// zlib's public header, at the repository root.  Read for two reasons: it is
// the marker that identifies the repository root, and `ZLIB_VERSION` in it is
// the authoritative source of the versioned library name (configure L56 does
// the same thing with sed).
const PUBLIC_HEADER: &str = "zlib.h";

// The linker version script, at the repository root.  Consumed, never edited.
const VERSION_SCRIPT: &str = "zlib.map";

// Present in every Cargo checkout of this tree; used together with the public
// header so that the root search cannot mistake a subdirectory for the root.
const WORKSPACE_MANIFEST: &str = "Cargo.toml";

// The unversioned ELF shared-library name, from Makefile.in L47
// (`SHAREDLIB=libz.so`).  It is the stem the SONAME is built on.  The Mach-O
// spelling is `libz.dylib` (configure L364, `SHAREDLIB=libz$shared_ext` with
// `shared_ext='.dylib'`) and is written out at the one place it is needed,
// because on that platform the version goes BEFORE the extension and the name
// cannot be formed by suffixing this constant.
const ELF_SHARED_LIB: &str = "libz.so";

// The Mach-O spelling, from configure L364 (`SHAREDLIB=libz$shared_ext` with
// `shared_ext='.dylib'`).  It is a separate constant because on that platform
// the version goes BEFORE the extension -- `libz.1.dylib`, not
// `libz.dylib.1` -- so the versioned names cannot be formed by suffixing it.
const MACHO_SHARED_LIB: &str = "libz.dylib";

// The directory component that proves the `OUT_DIR` shape.  Cargo lays out
// `<target>/[<triple>/]<profile>/build/<pkg>-<hash>/out`, so the artifact
// directory is exactly three levels above `OUT_DIR` and the component two levels
// up is this one.  The derivation is VERIFIED against that shape rather than
// assumed -- see `artifact_dir`.
const CARGO_BUILD_DIR: &str = "build";

// The notice job 4 writes beside cargo's artifacts, and the whole of its text.
//
// The NAME is chosen so that no linker and no loader can ever consider it: `-lz`
// looks for `libz.so`/`libz.a` exactly, `ld.so` looks for the SONAME
// `libz.so.1` exactly, and neither spelling can be reached by adding a suffix to
// this one.  That is the entire point -- the finding this file answers is that
// contract-SHAPED names in this directory are indistinguishable from an
// installable library, so the replacement must not be another name of that shape.
const ARTIFACT_NOTICE: &str = "README-cargo-artifacts.txt";

// Kept in one place because it is written verbatim and compared verbatim: the
// write is skipped when the file already holds exactly this, so an up-to-date
// build directory is not touched and its mtimes do not churn.
const ARTIFACT_NOTICE_TEXT: &str = "\
This directory holds cargo's build output for crates/libz-rs-sys, and one of the
files in it is NOT what its name suggests.  (A build script runs before the crate
is linked, so after a `cargo check' this note is here and the artifacts it
describes are not.)

  libz.a    IS the installable static library.  It defines all 95 functions
            zlib.h declares -- 93 of them in Rust, and gzprintf and gzvprintf
            from csrc/gzprintf_shim.c, which build.rs compiles into it because
            stable Rust cannot define a variadic function.  It also defines
            inflate_table, in Rust, as a hidden global: test/infcover.c calls
            that name directly and links this file.

  libz.so   is NOT the installable shared library.  rustc attaches its own
            anonymous version script to every cdylib link, so zlib.map cannot be
            layered on: this object carries 0 of the 16 zlib symbol-version
            nodes, and it does not export gzprintf or gzvprintf -- both are
            variadic, so they live in the C shim, and a cdylib cannot re-export a
            symbol defined by an archive object it links.  Measured: 93 dynamic
            globals, against the C library's 111.  No build-script argument can
            change either fact.

            What it does NOT do, contrary to what this note used to say, is leak
            internals: it exports exactly those 93 names and nothing else, so
            neither inflate_table nor the two _zlib_rs_gzprintf_* helpers appears
            in its dynamic table.  Measured with `comm' against the ZEXTERN set.
            The leak that sentence described was real when it was written and was
            fixed without the sentence being updated; it is corrected here rather
            than deleted, so that a reader who remembers the old claim can see
            what became of it.

            The counts above belong to named sets and are not interchangeable.
            zlib.h yields 101 ZEXTERN-shaped matches; five of those -- deflateInit,
            deflateInit2, inflateInit, inflateInit2 and inflateBackInit -- are
            documentation comments showing the effective signature of a MACRO, so
            96 is the number of real declarations; gzopen_w is behind
            `#if defined(_WIN32)', so 95 is the number a non-Windows build can
            define; and 93 is that minus the two variadic names above.  95
            functions plus the 16 version nodes is where the C library's 111
            comes from.

  libz.rlib is a Rust library, for Rust dependents.  It exports no C symbol.

Note also that cargo's debug and release directories hold identically NAMED files
-- there is no `libz.so' and `libz-debug.so' -- so the profile is carried by the
path and by nothing else.  Packaging selects release deliberately and prints the
directory it read.

There is deliberately no libz.so.1 or libz.so.<version> here.  Those names belong
to a library that can actually satisfy them, and staging them beside an
incomplete one is worse than leaving them out: a consumer or CI job that finds
them binds an object that fails to link every variadic-gz caller and warns on
every load, and -- because cargo puts this directory first on the library search
path of every build script it launches, while the host binutils record
DT_NEEDED libz.so.1 -- the build tools themselves load it.  Both were measured.

To obtain an installable drop-in libz, run from the repository root:

    make rust           # relinks libz.a under zlib.map into target/dropin:
                        # SONAME libz.so.1, 16 version nodes, 95 exported
                        # functions, internals hidden, and the versioned
                        # symlink chain the SONAME requires
    make rust-test      # links the UNMODIFIED test/example.c, test/minigzip.c
                        # and test/infcover.c against it and runs them
    make rust-symbols   # diffs the staged symbol tables against a built C libz

Install, and point any consumer, LD_LIBRARY_PATH or pkg-config path at what
`make rust` stages -- never at this directory.  Measured, so that it is not a
surprise: a C program linked -L against THIS directory builds, and then binds
/lib/.../libz.so.1 at run time, because nothing here answers to the SONAME
libz.so.1.  Against what `make rust` stages, the same program binds the staged
library.  Check with `ldd`; never infer which library ran from a passing test.

crates/libz-rs-sys/src/lib.rs carries the full artifact matrix.
This file is regenerated by build.rs; editing it has no effect.
";

// The loader search paths cargo exports to a build script, and that this script
// removes from its own process before it spawns a host tool.  See
// `sanitize_library_search_path`.  `DYLD_LIBRARY_PATH` is the Mach-O spelling;
// macOS strips it from system binaries under SIP, so it is belt and braces
// rather than the load-bearing half.
const CHILD_LIBRARY_PATH_VARS: [&str; 2] = ["LD_LIBRARY_PATH", "DYLD_LIBRARY_PATH"];

// How far above `CARGO_MANIFEST_DIR` the repository-root search may walk.  The
// documented distance is exactly two (crates/libz-rs-sys -> crates -> root);
// the extra slack absorbs a vendored or relocated crate without ever letting
// the search wander far outside the checkout.
const MAX_ROOT_SEARCH_DEPTH: usize = 4;

// ---------------------------------------------------------------------------
//  Build-time environment knobs
// ---------------------------------------------------------------------------

// The toggle the port documents: `ZLIB_RS_SIMD=0|1`.
const ENV_SIMD: &str = "ZLIB_RS_SIMD";

// The C compiler this script drives, in the order it is consulted.  These are the
// spellings every C-building build script in the ecosystem honours, so a
// cross-compiling caller configures this one the way it already configures the rest:
// `CC_<target>` (with `-` turned into `_`) is the most specific, `TARGET_CC` names the
// target compiler generically, and `CC` is the ordinary variable.  With none of them
// set the platform default is used -- `cc` everywhere except MSVC, where it is
// `cl.exe`.  `CFLAGS` follows the same cascade.
//
// ★ EVERY ONE OF THESE IS A COMMAND, NOT A PROGRAM NAME, and `split_command' below is
// what makes that true.  `CC="ccache gcc"', `CC="sccache cc"', `CC="gcc -m32"',
// `CC="clang --target=aarch64-linux-gnu"' and `CC="zig cc"' are all ordinary things to
// find in an environment, and `Makefile.in' exports CC, CFLAGS and AR into the cargo
// build -- so a tree configured with a compiler cache reached this script with a value
// containing a space.  Handing that to Command::new asks the operating system to
// execute a file whose name literally contains the space, and it answers `No such file
// or directory' while naming a program nobody installed.  Quoting inside CFLAGS is
// honoured for the same reason: `-I'/opt/my sdk'' and `-DBANNER="a b"' are ordinary,
// and whitespace splitting turns each into two broken arguments.  What is NOT done is
// any other shell behaviour -- no expansion, no globbing, no substitution -- because
// this code runs with the building user's privileges on every machine that builds the
// library.
const ENV_CC: &str = "CC";
const ENV_TARGET_CC: &str = "TARGET_CC";
const ENV_CFLAGS: &str = "CFLAGS";
const ENV_TARGET_CFLAGS: &str = "TARGET_CFLAGS";
const ENV_AR: &str = "AR";
const ENV_TARGET_AR: &str = "TARGET_AR";

// A knob this script once offered, kept only so that setting it is an ERROR
// with an explanation rather than a variable that is silently ignored.  It used
// to hand `zlib.map` to rustc's cdylib link; on every toolchain this port
// supports that produces a library with ZERO version nodes, which is precisely
// the silent, unversioned artifact it looked like it was preventing.  See
// `reject_version_script_passthrough`.
const ENV_VERSION_SCRIPT: &str = "ZLIB_RS_VERSION_SCRIPT";

// The Cargo feature `ZLIB_RS_SIMD` is an assertion about.  Cargo sets
// CARGO_FEATURE_<NAME> with the name upper-cased and hyphens turned into
// underscores, so the `simd` feature is `CARGO_FEATURE_SIMD`.
const CARGO_FEATURE_SIMD: &str = "CARGO_FEATURE_SIMD";

// The two features the remaining C shim is gated on, and it needs BOTH.
// `libz-compat` turns the unmangled C symbols on; `gz` compiles the `gzFile` layer.
// The shim is compiled exactly when the Rust adapters it calls exist -- see
// `SHIM_SOURCE_GZPRINTF` -- because an archive holding an undefined symbol would fail
// the link of every consumer rather than of the crate that produced it, while an
// archive MISSING a symbol a supported configuration needs fails the link just as
// surely.  Both mistakes were made here in turn.
//
// ★ There used to be a second shim, `csrc/inftrees_shim.c`, gated on `libz-compat`
// ALONE, because `inflate_table`'s first parameter is the C enum `codetype` and only a
// C translation unit can restate that prototype.  It is gone: `src/inflate.rs` now
// defines `inflate_table` directly, taking the plain `int` the enum is passed as and
// VALIDATING it, so a `--no-default-features --features libz-compat` build compiles no
// C at all.  What that removed is the C compiler's declaration-compatibility check,
// whose only subject was a unit that both included `inftrees.h` and defined the
// function; the consumer that matters -- the unmodified `test/infcover.c` -- includes
// the header and merely CALLS the symbol, and `make rust-test` compiles and links it,
// so a C compiler still performs the check where it means something.
const CARGO_FEATURE_LIBZ_COMPAT: &str = "CARGO_FEATURE_LIBZ_COMPAT";
const CARGO_FEATURE_GZ: &str = "CARGO_FEATURE_GZ";

// The one C translation unit the shipped library takes, relative to
// CARGO_MANIFEST_DIR.  It defines `gzprintf` and `gzvprintf`.
//
// Gated on `libz-compat` AND `gz`: it calls `_zlib_rs_gzprintf_begin` and
// `_zlib_rs_gzprintf_commit`, which live behind both, so a build with `gz` off has
// nothing for it to link against -- and, since this is the only shipped C source,
// `--no-default-features --features libz-compat` invokes no C compiler at all.  A build without the `gzFile` layer is not a
// drop-in libz and is not claimed to be -- `CFG_GZPRINTF` stays unset and
// `zlibCompileFlags` bit 27 reports the artifact that was actually built.
const SHIM_SOURCE_GZPRINTF: &str = "csrc/gzprintf_shim.c";

// The TEST-ONLY C translation unit, relative to CARGO_MANIFEST_DIR.  It builds a
// `va_list` and forwards it to `gzvprintf`, which no Rust caller can do: `va_list` has a
// different type on every ABI and `core::ffi::VaList` is unstable, so without a line of C
// `gzvprintf` can only ever be reached as the tail of `gzprintf`.
//
// DELIBERATELY NOT IN `SHIM_SOURCES`.  That array is what gets archived into the `libz.a`
// rustc produces, and this file must never appear in the shipped library: it is handed to
// the linker through `cargo::rustc-link-arg-tests`, which cargo applies to TEST TARGETS
// ONLY -- see `link_test_probe`.
const TEST_PROBE_SOURCE: &str = "csrc/gzvprintf_probe.c";

// The name of the static archive the shims are collected into, as `-l static=` names
// it.  Both halves matter: the file must be `lib<name>.a` for a GNU-style linker to
// find it, and the name must not collide with the crate's own `z`.
const SHIM_LIB_NAME: &str = "zlib_rs_cabi";

// `zutil.h` L15-L19 spells `ZLIB_INTERNAL` as `__attribute__((visibility("hidden")))`
// only when this is defined, and `configure` L962-L963 defines it for both CFLAGS and
// SFLAGS.  The shim is compiled with it for the same reason the C library is: so that
// this port's C flags are the C build's C flags.  It no longer changes any symbol --
// `gzprintf` and `gzvprintf` are `ZEXPORT`, not `ZLIB_INTERNAL` -- because the one
// `ZLIB_INTERNAL` definition that used to be compiled here, `inflate_table`, is now
// Rust, where the same property comes from the `.hidden` directive in `src/lib.rs`.
const VISIBILITY_DEFINE: &str = "HAVE_HIDDEN";

// The cfg that records "`gzprintf` and `gzvprintf` are compiled into this build".
// `util.rs` derives `zlibCompileFlags` bit 27 from it, so the flags word cannot
// claim a formatting entry point the artifact does not contain.  Declared to rustc
// unconditionally, for the same reason `CFG_SIMD` is.
const CFG_GZPRINTF: &str = "zlib_rs_gzprintf";

// The cfg that records "the vectorised checksum backends are compiled into this
// build".  Derived from the RESOLVED feature set, never from the environment
// variable, so it cannot claim a backend that was not compiled.  Declared to
// rustc unconditionally, because Rust 1.80 turned `unexpected_cfgs` on by
// default and this port's bar is zero build warnings.
const CFG_SIMD: &str = "zlib_rs_simd";

// The environment variable that names the compiled backend, so a test or a
// benchmark can assert which implementation it is measuring with `env!` rather
// than inferring it.  The two values name `zlib_rs::adler32::Adler32Simd` /
// `zlib_rs::crc32::StrideBraid` and `Adler32Generic` / `Braid` respectively.
//
// ★ Note the asymmetry in those names, which is deliberate and is documented at
// length in the two backend modules: the Adler-32 path really does compile to
// vector instructions (91 SSE2 instructions on x86_64), while the CRC-32 path
// compiles to none at all and is therefore called `StrideBraid` rather than
// `Simd`.  The value of this variable stays `simd`/`scalar` because it names the
// FEATURE that selected the pair, not either implementation.
const ENV_BACKEND: &str = "ZLIB_RS_CHECKSUM_BACKEND";
const BACKEND_SIMD: &str = "simd";
const BACKEND_SCALAR: &str = "scalar";

fn main() {
    // Emitting any `rerun-if-changed` replaces cargo's default "re-run when
    // anything in the package changed" rule with exactly this list, so the
    // list has to be complete.  build.rs itself comes first; the two contract
    // files follow once their location is known.
    println!("cargo::rerun-if-changed=build.rs");

    let repo_root = repo_root();
    let public_header = repo_root.join(PUBLIC_HEADER);
    let version_script = repo_root.join(VERSION_SCRIPT);

    // Bumping ZLIB_VERSION renames the versioned library, and editing the
    // version script changes the exported symbol set.  Either one has to force
    // this script to run again and the library to be relinked.
    println!("cargo::rerun-if-changed={}", public_header.display());
    println!("cargo::rerun-if-changed={}", version_script.display());

    require_version_script(&version_script);

    // ZLIB_VERSION supplies two names: its leading integer is the SONAME's major
    // component, and the whole four-component string is the versioned name job 4
    // prunes.  Both are parsed from the header so neither can drift from it, and
    // the second is why the prune recognises exactly the names an older revision
    // of this script would have written rather than a pattern.
    let version = zlib_version(&public_header);
    let major = major_version(&version, &public_header).to_owned();
    let target = TargetInfo::from_env();

    check_simd_request();

    // Before anything spawns a host tool.  `build_c_abi_shims` runs `cc` (which
    // runs `as`) and `ar`, all of them linked against libz on a GNU host, and
    // cargo has put the cargo artifact directory first on this process's library
    // search path.  See `sanitize_library_search_path`.
    sanitize_library_search_path();

    build_c_abi_shims(&repo_root);
    reject_version_script_passthrough(&version_script);
    emit_link_args(&target, &major);
    tidy_artifact_dir(&target, &version, &major);
}

// ---------------------------------------------------------------------------
//  1. The ZLIB_RS_SIMD build-time toggle
// ---------------------------------------------------------------------------
//
// `ZLIB_RS_SIMD=0|1` is the toggle the port documents for the vectorised
// CRC-32 and Adler-32 backends.  Two properties of that choice constrain what
// the toggle is allowed to do, and both are worth stating where the code lives:
//
//   * It is OUTPUT-NEUTRAL.  A checksum yields one scalar however it is
//     computed, so vectorising it cannot perturb a single emitted byte -- it
//     may change speed only.  That is precisely why SIMD is confined to the
//     two checksums: vectorising match finding would change the compressed
//     output and would break the byte-identical-output requirement outright.
//
//   * It is not a promise about the hardware, but not because anything probes
//     for one.  The vectorised backends contain no architecture-specific
//     intrinsic at all: they are portable fixed-width integer arithmetic that
//     LLVM is free to autovectorise, so a SIMD-enabled build computes the right
//     answer on a machine with no vector unit.  Be precise about the two
//     backends, because they differ.  `crates/zlib-rs/src/crc32/simd.rs`
//     performs NO detection whatsoever.
//     `crates/zlib-rs/src/adler32/simd.rs` exposes `is_supported()`, which the
//     parent module consults as a THROUGHPUT HINT -- a cached
//     `is_x86_feature_detected!("sse2")` query on x86 with `std`,
//     `cfg!(target_feature = "sse2")` on x86 without it, and unconditionally
//     true elsewhere -- and either answer yields identical checksums.  Nothing
//     here guards correctness, so do not describe this toggle as gated at run
//     time.
//
// WHAT THIS FUNCTION DOES, AND WHY IT IS ONLY A CHECK
//
// A build script cannot switch a Cargo feature on, and it cannot switch one
// off either.  Feature resolution happens before any build script runs, and
// nothing a script prints can revise it.  So there are exactly two coherent
// designs for an environment variable that names a feature, and this file
// implements the second:
//
//   (a) the variable becomes a `cfg`, and the crate keys its own code off that
//       cfg instead of off the feature.  For this crate that is a dead end: the
//       vectorised backends live in `zlib-rs`, behind ITS `simd` feature, and no
//       cfg emitted here can reach into a dependency's feature set.  A `cfg`
//       that nothing can act on is worse than no cfg at all -- it makes
//       `ZLIB_RS_SIMD=1` look like it did something when the build it produced
//       is byte for byte the scalar one.
//
//   (b) the variable is an ASSERTION about the resolved feature set, and the
//       translation from variable to feature is performed by whatever invokes
//       cargo.  `Makefile.in`'s `rust` target does exactly that: it turns
//       `ZLIB_RS_SIMD=1` into `--features libz-rs-sys/simd`.  A direct
//       `cargo build` gets the assertion checked instead of silently ignored.
//
// So: `ZLIB_RS_SIMD=1` with the `simd` feature off is a hard error naming the
// feature to pass, `ZLIB_RS_SIMD=0` with the feature on is a hard error naming
// how to turn it off, agreement is silent, and unset means "no assertion, the
// feature governs".  Nothing is emitted in any case, because there is nothing
// honest to emit.
fn check_simd_request() {
    // Without this, cargo caches the previous decision and flipping the
    // variable appears to do nothing at all -- which for an assertion means a
    // stale build passing a check it would now fail.
    println!("cargo::rerun-if-env-changed={ENV_SIMD}");

    // Declared whether or not it is set, so that the declaration does not appear
    // and disappear with the environment -- which would make an
    // `unexpected_cfgs` warning intermittent and hard to attribute.
    println!("cargo::rustc-check-cfg=cfg({CFG_SIMD})");

    // What cargo actually resolved.  This is the single source of truth for both
    // outputs below and for the assertion further down.
    let feature_on = env::var_os(CARGO_FEATURE_SIMD).is_some();
    if feature_on {
        println!("cargo::rustc-cfg={CFG_SIMD}");
        println!("cargo::rustc-env={ENV_BACKEND}={BACKEND_SIMD}");
    } else {
        println!("cargo::rustc-env={ENV_BACKEND}={BACKEND_SCALAR}");
    }

    let requested = match env::var(ENV_SIMD) {
        Ok(value) => value,
        // Unset is the normal case: no assertion is being made, and the crate's
        // own `simd` Cargo feature governs.
        Err(env::VarError::NotPresent) => return,
        // A non-Unicode value cannot be a valid `0` or `1`, so it is handled
        // together with the other malformed values below.
        Err(env::VarError::NotUnicode(value)) => panic!(
            "{ENV_SIMD} is set to a non-Unicode value ({value:?}). Set it to `0` or `1`, \
             or unset it to leave the decision to this crate's `simd` Cargo feature."
        ),
    };

    match (requested.trim(), feature_on) {
        // The assertion holds.  Nothing to say and nothing to emit.
        ("1", true) | ("0", false) => {}

        // Refused rather than warned about.  A warning here would leave the
        // caller with a scalar library it believes is vectorised, and the two
        // are indistinguishable from the outside: identical bytes out, identical
        // exported symbols, only the throughput differs.  That is exactly the
        // class of mistake a build knob must not be able to make.
        ("1", false) => panic!(
            "{ENV_SIMD}=1 asks for the vectorised checksum backends, but the `simd` Cargo \
             feature is not enabled for this build, and a build script cannot enable one. \
             Rebuild with `--features libz-rs-sys/simd` (from the workspace root) or \
             `--features simd` (from this crate), which is the translation `Makefile.in`'s \
             `rust` target performs for you. Unset {ENV_SIMD} to leave the decision to the \
             feature alone."
        ),
        ("0", true) => panic!(
            "{ENV_SIMD}=0 asks for the scalar checksum backends, but the `simd` Cargo feature \
             IS enabled for this build, and a build script cannot disable one. Rebuild \
             without `--features simd`, or with `--no-default-features` plus the features you \
             do want. Unset {ENV_SIMD} to leave the decision to the feature alone."
        ),

        (other, _) => panic!(
            "{ENV_SIMD} must be `0` or `1`, got {other:?}. Unset it to leave the decision \
             to this crate's `simd` Cargo feature."
        ),
    }
}

// ---------------------------------------------------------------------------
//  2. Target classification
// ---------------------------------------------------------------------------
//
// `-Wl,-soname` and `-Wl,--version-script` are GNU-ld / LLD constructs.  On a
// Mach-O or PE target they are not merely useless, they are link errors, so the
// platform guard is mandatory rather than defensive.  The classification below
// follows `configure`'s own case analysis so that the Rust build and the C
// build agree on every platform they both support.
//
// Note that these come from CARGO_CFG_TARGET_OS / CARGO_CFG_TARGET_ENV, which
// describe the *target*.  A build script itself is compiled for the host, so
// `cfg!(target_os = ...)` inside this file would answer the wrong question and
// would silently do the wrong thing when cross-compiling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SharedObjectFormat {
    // ELF with a GNU-ld-compatible driver: takes both `-soname` and
    // `--version-script`.  configure L330-L337.
    Elf,
    // ELF, but the linker spells the soname `-h` and is given no version
    // script.  configure L347-L348 (QNX: `-Wl,-hlibz.so.1`).
    ElfDashH,
    // Mach-O: no soname and no version script; the analogue of a soname is
    // `-install_name`, which this branch emits below, and the analogue of a
    // version script is an exported-symbols list, which is not emitted HERE --
    // rustc controls the cdylib link, and the artifact that needs the list is the
    // one staged from the complete archive.  `.github/workflows/rust.yml`'s
    // `platform-abi-macos` job stages it and passes
    // `-Wl,-exported_symbols_list` with a list derived from `zlib.h`, then
    // measures the `-install_name` this branch emits and relinks the three
    // unmodified C drivers against the result.  configure L362-L367.
    MachO,
    // PE/COFF, MSVC or MinGW.  Neither construct exists.  configure L341-L345
    // uses a plain `-shared` for MinGW.  The PE analogue of zlib.map is a
    // module-definition file, and `win32/zlib.def` does exist in this tree -- but
    // AAP 0.2.2.3 places it out of scope, so nothing is emitted here and this
    // port produces no packaged, versioned, export-controlled Windows install.
    // Should that change, `win32/zlib.def` is the file to wire up, as `/DEF:` on
    // MSVC.
    //
    // What IS verified on that platform, by `platform-abi-windows` in
    // `.github/workflows/rust.yml`: the DLL cargo produces is read for its export
    // table, `gzopen_w` is required to be in it, and MSVC compiles a C consumer
    // and the unmodified `test/minigzip.c` against cargo's import library and
    // runs them.  So the `#[cfg(windows)]` items in the source (`gzopen_w`, the
    // `_WIN32` entries in cbindgen.toml's `[defines]`) are exercised rather than
    // merely present.
    //
    // ★ What is NOT verified there, stated as the class it belongs to: `z.dll`
    // does not export `gzprintf`/`gzvprintf`, so the Windows ABI is a documented
    // SUBSET of the contract rather than a complete drop-in, and a Windows
    // consumer that calls either -- `zlib.h` declares both unconditionally --
    // fails to link.  The mechanism is NOT PE-specific and it is worth being
    // exact, because blaming the format hides the fix: `csrc/gzprintf_shim.c` is
    // compiled and archived on every target, but a cdylib link retains only what
    // something reaches, and nothing in Rust calls either name.  Measured on ELF
    // for the same reason: `nm --defined-only target/release/libz.a` shows
    // `gzprintf_shim.o` defining both, while
    // `nm -D --defined-only --extern-only target/release/libz.so` shows neither.
    // ELF closes it in PACKAGING -- `Makefile.in`'s `rust` target names both in
    // the relink, and the staged library exports them -- and PE has no such step
    // only because `win32/zlib.def` is out of scope.  So this is a
    // `PACKAGING_GAP` in `.github/scripts/platform_abi_gate.py`'s vocabulary,
    // classified apart from `gzopen_w`'s `NOT_ON_PLATFORM` absence on Linux, and
    // `platform-abi-windows` passes `--expect-abi subset` to say so.
    Pe,
    // Everything else: wasm, emscripten, AIX, HP-UX, bare metal.  Emitting an
    // ELF-only argument here would break the link, so nothing is emitted.
    Other,
}

// One field, because one question is asked of the target: which shared-object
// format is being produced.  `CARGO_CFG_TARGET_ENV` is deliberately NOT carried
// here -- `windows-gnu` and `windows-msvc` classify identically, and the one place
// that does care reads the variable directly (`build_c_abi_shims`, for the MSVC
// object and archive spellings).
#[derive(Debug)]
struct TargetInfo {
    format: SharedObjectFormat,
}

impl TargetInfo {
    fn from_env() -> Self {
        let format = match require_env("CARGO_CFG_TARGET_OS").as_str() {
            "linux" | "android" | "freebsd" | "netbsd" | "openbsd" | "dragonfly" | "solaris"
            | "illumos" | "haiku" | "hurd" | "redox" | "fuchsia" => SharedObjectFormat::Elf,
            "nto" => SharedObjectFormat::ElfDashH,
            "macos" | "ios" | "tvos" | "watchos" | "visionos" => SharedObjectFormat::MachO,
            "windows" => SharedObjectFormat::Pe,
            _ => SharedObjectFormat::Other,
        };

        Self { format }
    }
}

// ---------------------------------------------------------------------------
//  5. The C ABI shim -- the one pair of declarations stable Rust cannot express
// ---------------------------------------------------------------------------
//
// ★ **This script compiles C, and the reason is exactly two functions wide.**
// `gzprintf` is variadic (`zlib.h` L1549) and `gzvprintf` takes a `va_list`
// (`zlib.h` L2047). DEFINING either needs the unstable `c_variadic` feature or
// `core::ffi::VaList`, and AAP §0.7.1 (h) pins this workspace to stable Rust 1.80 --
// measured on both ends of that range: `rustc 1.80.1` and current stable each reject
// `extern "C" fn f(...)` and `use core::ffi::VaList` with E0658. `zlib.h` declares
// them, AAP §0.1.1.1 Goal 1 requires all 96 declarations, and AAP §0.8.1 directive 12
// requires the tests that exercise them, so the pair cannot be dropped, cannot be
// written here, and cannot be moved to the packaging layer without taking
// `tests/c_api_parity.rs` and `tests/gz_printf.rs` with it -- a `cargo test` links
// the same archive this script contributes to. `csrc/gzprintf_shim.c` defines both
// over two hidden Rust helpers, and that TU is the whole of the shipped C surface.
//
// ★ NOTHING ELSE IN THE CONTRACT NEEDS C, and one entry point that used to be here
// no longer is. `inflate_table`'s first parameter is `codetype`, a C enum
// (`inftrees.h` L54-L62) that the unmodified `test/infcover.c` compiles a call
// against; an earlier revision presented that prototype from a second translation
// unit, `csrc/inftrees_shim.c`. It does not need one. An enumeration of `0`, `1` and
// `2` is passed in a 32-bit integer register or stack slot on every target in this
// port's matrix, exactly as an `int` is, and no translation unit in this tree
// restates the prototype -- so `src/inflate.rs` defines the name in Rust with a
// `c_int` parameter that it VALIDATES rather than transmutes, and `infcover.c`
// resolves it at link time. That halved the shipped C surface and made
// `--no-default-features --features libz-compat` a build that invokes no C compiler
// at all.
//
// Compiling the remaining unit here rather than in the packaging layer is what makes
// ONE artifact story possible: `target/<profile>/libz.a` -- the `staticlib` cargo
// produces -- contains every one of the 95 functions `zlib.h` declares, and the
// packaged shared object is relinked from exactly that archive instead of from an
// archive plus a separately compiled object. Measured: `ar t` lists the shim object
// in `libz.a` and `nm` reports `T gzprintf`, `T gzvprintf` and `T inflate_table`
// in it.
//
// # ★ What the cdylib cargo produces still cannot be, and why
//
// `-l static=` is emitted with cargo's default modifiers, `+bundle,-whole-archive`.
// The archive is therefore merged into the `staticlib` and, because no Rust item
// references the shims, its objects are left out of the `cdylib`. That is not a
// compromise, it is the only useful arrangement, and the measurement that settles
// it is worth recording so nobody spends an afternoon rediscovering it:
//
//   * rustc always hands its OWN version script to a `cdylib` link -- an anonymous
//     tag listing the crate's `#[no_mangle]` items under `global:` and `local: *`.
//   * So a shim object pulled in with `+whole-archive` contributes its code and
//     gets NO dynamic symbol: `local: *` hides it. `-Wl,--export-dynamic-symbol=`
//     does not override a version script (measured with both `rust-lld` and
//     `ld.bfd`: the name is absent from `.dynsym` either way).
//   * `zlib.map` cannot be added alongside rustc's script either. `ld.bfd` refuses
//     outright -- "anonymous version tag cannot be combined with other version
//     tags" -- and `rust-lld` warns "attempt to reassign symbol ... to version" and
//     ignores it, yielding zero version nodes.
//
// A rustc-linked `cdylib` consequently cannot export `gzprintf`/`gzvprintf` and
// cannot carry the 16 zlib version nodes, whatever this script emits. What it CAN be
// held to is exporting nothing but the contract: the `.hidden` directives in
// `src/lib.rs` keep this crate's three internal helpers out of its dynamic table, so
// the cdylib's 93 globals are 93 contract names and nothing else. The
// installable shared object is therefore produced by ONE documented step from the
// complete archive -- `Makefile.in`'s `rust` target, or the CMake equivalent --
// which links it under `zlib.map` with the right SONAME and symlink chain.
// `crates/libz-rs-sys/src/lib.rs` carries the artifact matrix that states this once.
//
// # Requiring a C compiler
//
// A C compiler is a hard requirement for a `libz-compat` + `gz` build -- and for no
// other configuration, now that `inflate_table` is Rust: `--no-default-features
// --features libz-compat` returns from `build_c_abi_shims` before naming a compiler.
// Where it is required the failure is loud. The alternative -- skipping the shim when no compiler is found
// -- is exactly the silently-incomplete artifact this arrangement exists to remove:
// a library that links but has no `gzprintf` fails at the *consumer*, long after the
// build that produced it went green. No new crate dependency is taken to do it
// (AAP §0.5.1.3 freezes the inventory and §0.6.4.1 keeps `cc` to the differential
// crate alone); the platform compiler is invoked directly, through the same
// `CC`/`CC_<target>`/`TARGET_CC` and `CFLAGS` cascade every C-building build script
// honours, so a cross-compiling caller configures this one the way it already
// configures the rest.
//
// ★ THE PLAN DIVERGENCE THIS CREATES, STATED IN FULL, because it is real and a
// reader is entitled to the whole of it.
//
// AAP §0.6.4.1 states that "`cargo build --release` for either shipped artifact never
// touches a C compiler". That sentence is not literally satisfied: `cc`, `as` and `ar`
// are invoked here, for ONE translation unit, whenever `libz-compat` and `gz` are both
// on -- which is the default feature set. It was TWO until `inflate_table` moved into
// Rust and `csrc/inftrees_shim.c` was deleted; the remaining one is irreducible on
// stable, for the reason set out below.
//
// It cannot be satisfied while the rest of the plan is. Three of its requirements meet
// here and only two of any three can hold at once:
//
//   * §0.7.1 (h) pins the workspace to stable Rust 1.80, where defining a variadic
//     function or naming `core::ffi::VaList` is E0658 (measured on 1.80.1 and on
//     current stable);
//   * §0.1.1.1 Goal 1 requires all 96 `ZEXTERN` declarations, `gzprintf` and
//     `gzvprintf` among them, and §0.6.3.6 requires the 95-function export surface;
//   * §0.8.1 directive 12 requires the existing test coverage, and both
//     `tests/c_api_parity.rs` and `tests/gz_printf.rs` call those two entry points
//     through the archive a `cargo test` links -- so moving the unit to the packaging
//     layer would remove their coverage rather than relocate it.
//
// The alternatives were considered and rejected on merit: nightly violates the MSRV;
// hand-written `global_asm!` variadic thunks would need one correct per-ABI register
// save area per Tier-1 target, which is a far larger and less auditable unsafe surface
// than 300 lines of C that the platform compiler is authoritative about; and dropping
// the two functions fails Goal 1 and symbol parity outright.
//
// So the RULE behind §0.6.4.1 -- reference zlib is an ORACLE and not a build
// dependency -- is satisfied exactly: the 15 retained C translation units of the
// reference implementation are compiled by `crates/zlib-rs-differential`'s build
// script and by nothing else, which `cargo build -vv` confirms for both shipped
// crates (`-p zlib-rs` invokes no compiler at all, and `-p libz-rs-sys
// --no-default-features --features libz-compat` now invokes none either). What
// remains is a C toolchain as a documented hard requirement of the DEFAULT C-ABI
// facade build: it is named in this crate's manifest, in `rust/README.md`, in
// `Makefile.in`'s variable block and here, its scope is one file and two functions,
// and its absence names itself at once rather than yielding a library that links and
// then fails at a consumer.

/// Removes the dynamic-library search path from this process, so that every tool
/// this script spawns resolves `libz` the way the system intends rather than out
/// of the build directory.
///
/// # The problem, measured rather than supposed
///
/// Cargo puts `target/<profile>` and `target/<profile>/deps` FIRST on
/// `LD_LIBRARY_PATH` for every build script it launches, and the host binutils are
/// themselves linked against zlib: `ld.so --list` on `as`, `ar`, `ld`, `objcopy`
/// and `nm` shows `DT_NEEDED libz.so.1` with no `RPATH` of their own, and `rustc`
/// reaches it through libLLVM.  This script spawns `cc` (which spawns `as`) and
/// `ar` to build the `csrc/` shim and the test-only probe beside it.  So without this call, the tools that
/// COMPILE the library can load the library -- while it is being built.
///
/// Measured, before job 4 stopped staging a `libz.so.1` alias in that directory:
/// six `no version information available` lines per rebuild in this script's own
/// stderr, which cargo hides on a successful build; and, after a
/// `--no-default-features` build left a zero-export `libz.so` behind,
/// `/usr/bin/x86_64-linux-gnu-as: symbol lookup error: undefined symbol: deflate`
/// -- the assembler could not start, so the build failed and stayed failing while
/// blaming `csrc/gzprintf_shim.c`.
///
/// # Why it stays even though job 4 removed the cause
///
/// Job 4 no longer creates the name that triggered it, and that is the primary
/// fix; this is the property, stated once and enforced here: nothing on the
/// library search path handed to this script may influence the tools it drives.
/// It costs two lines, it holds regardless of what else a caller, a packaging
/// step or a future revision leaves in that directory, and
/// `crates/zlib-rs-differential/build.rs` carries the same call for the same
/// reason -- it shells out to `cc`, `objcopy`, `ar` and `nm`.
///
/// Removing the variables from THIS process is what covers every child, including
/// the `cc` invocation, which offers no hook for a child environment: children
/// inherit the environment as modified.  Nothing in this script loads a dynamic
/// library itself -- its own process is already loaded and everything after this
/// point is either pure computation or a spawned system tool -- so there is
/// nothing for the removal to break.
///
/// Any future build script in this workspace that shells out to a zlib-linked
/// tool needs these same two lines.
fn sanitize_library_search_path() {
    for name in CHILD_LIBRARY_PATH_VARS {
        env::remove_var(name);
    }
}

fn build_c_abi_shims(repo_root: &Path) {
    // Declared whether or not it is set, so the declaration cannot appear and
    // disappear with the configuration -- an intermittent `unexpected_cfgs`
    // warning is the hardest kind to attribute.
    println!("cargo::rustc-check-cfg=cfg({CFG_GZPRINTF})");

    let target = env::var("TARGET").unwrap_or_default();
    let underscored = target.replace('-', "_");
    for key in [
        format!("{ENV_CC}_{underscored}"),
        ENV_TARGET_CC.to_owned(),
        ENV_CC.to_owned(),
        format!("{ENV_CFLAGS}_{underscored}"),
        ENV_TARGET_CFLAGS.to_owned(),
        ENV_CFLAGS.to_owned(),
        format!("{ENV_AR}_{underscored}"),
        ENV_TARGET_AR.to_owned(),
        ENV_AR.to_owned(),
    ] {
        println!("cargo::rerun-if-env-changed={key}");
    }

    // `gzprintf`/`gzvprintf` reach the core through `_zlib_rs_gzprintf_begin` and
    // `_zlib_rs_gzprintf_commit`, which live behind `libz-compat` AND `gz`.  With
    // `libz-compat` off there is no unmangled C surface at all and the shim has
    // nothing to link against, so nothing is built -- and nothing is missing either,
    // because such a build is not a C-ABI libz.
    if env::var_os(CARGO_FEATURE_LIBZ_COMPAT).is_none() {
        return;
    }

    // ★ ONE SHIM, and it needs `gz` as well.  `csrc/gzprintf_shim.c` reaches
    // `_zlib_rs_gzprintf_begin` and `_zlib_rs_gzprintf_commit`, which live behind
    // `libz-compat` AND `gz`, so a build without `gz` has nothing for it to link
    // against -- and, with `inflate_table` now defined in Rust, nothing else to
    // compile either.  A `--no-default-features --features libz-compat` build
    // therefore runs no C compiler, no assembler and no archiver at all, which is
    // what the early return below makes true rather than merely claims.
    let gz = env::var_os(CARGO_FEATURE_GZ).is_some();
    if !gz {
        return;
    }
    let sources: [&str; 1] = [SHIM_SOURCE_GZPRINTF];

    let manifest_dir = PathBuf::from(require_env("CARGO_MANIFEST_DIR"));
    let out_dir = PathBuf::from(require_env("OUT_DIR"));
    let msvc = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default() == "msvc";

    let mut objects = Vec::with_capacity(sources.len());
    for relative in sources {
        let source = manifest_dir.join(relative);
        assert!(
            source.is_file(),
            "the C ABI shim {} is missing. It defines `gzprintf` and `gzvprintf`, \
             neither of which stable Rust can declare -- a variadic definition needs \
             the unstable `c_variadic` feature and a `va_list` parameter needs the \
             unstable `core::ffi::VaList` -- so a library built without it is not a \
             drop-in libz. The caller-side `va_list` probe beside it is the only way \
             `gzvprintf` can be exercised at all. Restore the file from version \
             control.",
            source.display()
        );
        println!("cargo::rerun-if-changed={}", source.display());
        objects.push(compile_shim(&source, &out_dir, repo_root, msvc));
    }

    let archive = archive_shims(&objects, &out_dir, msvc);

    // `+bundle,-whole-archive` -- cargo's defaults -- for the reason given above: the
    // archive is merged into `libz.a`, and its objects stay out of the cdylib that
    // could not export them anyway. An integration test that names `gzprintf` still
    // links, because naming it is what makes the linker take the member.
    println!(
        "cargo::rustc-link-search=native={}",
        archive.parent().unwrap_or(&out_dir).display()
    );
    println!("cargo::rustc-link-lib=static={SHIM_LIB_NAME}");

    // The probe is a TEST-ONLY translation unit and it is linked as one: `link_test_probe`
    // hands it to the linker through `cargo::rustc-link-arg-tests`, which cargo applies to
    // test targets and to nothing else, so `libz.a`, `libz.so` and the rlib are byte-for-byte
    // what they would be without it.  Reaching this line already means `gz` is on -- the
    // early return above is what establishes it -- and the only thing the probe does is
    // forward a `va_list` to `gzvprintf`, which exists only under `gz`;
    // `tests/gz_printf.rs` carries the same gate at its root.
    //
    // Now, and only now, may the Rust code claim the two formatting entry points -- and only
    // when the translation unit that DEFINES them was one of the objects archived above.
    // `util.rs` derives `zlibCompileFlags` bit 27 from this cfg, so the flags word describes the
    // artifact that was actually built; setting it for a `gz`-less build would claim a
    // `gzprintf` the archive does not contain.
    //
    // ★ WHAT THIS CFG DOES AND DOES NOT ESTABLISH, because the difference is one artifact wide.
    // It records that the shim was COMPILED and archived, which is what makes bit 27 honest for
    // the two artifacts that ship: `libz.a`, whose members these objects are, and the packaged
    // `libz.so.<ZLIB_VERSION>` that is relinked from it.  It cannot record what a given artifact
    // EXPORTS, and for cargo's `cdylib` the two differ -- rustc's `local: *` gives a
    // C-contributed symbol no dynamic entry, so that library reports bit 27 clear while its
    // `.dynsym` holds neither name.  No cfg could say otherwise: cargo emits all three crate
    // types from ONE rustc invocation (measured: a single `--crate-name z` command carrying
    // `--crate-type` three times), so the compiled constant is shared.  That is why the `cdylib`
    // is documented as a development artifact rather than an installable one, and why
    // `tests/symbol_parity.rs::compile_flags_bit_27_agrees_with_the_shipping_artifacts` checks
    // the bit against the archive and the packaged library instead of trusting this line.
    link_test_probe(&manifest_dir, &out_dir, repo_root, msvc, &archive);
    println!("cargo::rustc-cfg={CFG_GZPRINTF}");
}

/// Compiles [`TEST_PROBE_SOURCE`] and links it into the crate's TEST TARGETS ONLY.
///
/// `cargo::rustc-link-arg-tests` is the whole reason this can exist without touching the
/// shipped artifact: cargo forwards it to the link of every test target and to nothing else,
/// so `libz.a`, `libz.so` and the `rlib` are byte-for-byte what they would be without this
/// file. `crates/libz-rs-sys/tests/symbol_parity.rs` is what holds that claim to account.
///
/// TWO INPUTS, IN THIS ORDER, AND THE ORDER IS THE POINT. Cargo appends link-args after the
/// libraries, so by the time the linker reaches them it has already finished with
/// `lib{SHIM_LIB_NAME}.a`:
///
///   1. the probe **object**, which a linker always loads whether or not anything references
///      it, and which is what brings the undefined `gzvprintf` into the link;
///   2. the shim **archive again**, so that `gzvprintf` can still be resolved from it.
///
/// Without (2) the probe would resolve only by luck -- specifically, only in a test binary
/// that happens to name `gzprintf` and so pulled `gzprintf_shim.o` in earlier. Listing the
/// archive a second time costs nothing (the member is taken at most once) and makes the link
/// deterministic for every test target, including one that names neither entry point.
///
/// The archive is passed by PATH rather than as `-l`, because a path is the one spelling both
/// a GNU-style driver and `link.exe` accept, and this function must not grow a per-linker
/// flag vocabulary for a test-only convenience.
fn link_test_probe(
    manifest_dir: &Path,
    out_dir: &Path,
    repo_root: &Path,
    msvc: bool,
    archive: &Path,
) {
    let source = manifest_dir.join(TEST_PROBE_SOURCE);
    assert!(
        source.is_file(),
        "the test-only C probe {} is missing. It builds the `va_list` that lets \
         `tests/c_api_parity.rs` call `gzvprintf` as its own entry point rather than only \
         through `gzprintf`; without it that contract function has no behavioural coverage. \
         Restore the file from version control.",
        source.display()
    );
    println!("cargo::rerun-if-changed={}", source.display());

    let object = compile_shim(&source, out_dir, repo_root, msvc);

    println!("cargo::rustc-link-arg-tests={}", object.display());
    println!("cargo::rustc-link-arg-tests={}", archive.display());
}

/// Compiles one shim translation unit, returning the object file it produced.
///
/// The include path is the repository root, because the shim includes the immutable
/// `zlib.h` (and, through it, `zconf.h`) rather than restating prototypes that could
/// then drift from the contract. `-DHAVE_HIDDEN` is what makes `zutil.h`'s
/// `ZLIB_INTERNAL` expand to hidden visibility, matching `configure` L962-L963; no
/// symbol in the one remaining shim is `ZLIB_INTERNAL`, so it is passed for exactly
/// one reason -- the C build passes it, and a shim compiled with different flags from
/// the library it joins is a difference waiting to matter.
///
/// Position-independent code is not optional: this object is archived into `libz.a`,
/// and the packaged shared library is relinked from that archive.
fn compile_shim(source: &Path, out_dir: &Path, repo_root: &Path, msvc: bool) -> PathBuf {
    let stem = source.file_stem().map_or_else(
        || "shim".to_owned(),
        |stem| stem.to_string_lossy().into_owned(),
    );
    let object = out_dir.join(format!("{stem}{}", if msvc { ".obj" } else { ".o" }));
    let _ = fs::remove_file(&object);

    let compiler = tool_from_env(ENV_CC, ENV_TARGET_CC, if msvc { "cl" } else { "cc" });
    let compiler_name = compiler.display();
    let mut command = compiler.command();
    if msvc {
        command
            .arg("/nologo")
            .arg("/c")
            .arg("/O2")
            .arg(format!("/D{VISIBILITY_DEFINE}"))
            .arg(format!("/I{}", repo_root.display()))
            .arg(format!("/Fo{}", object.display()));
    } else {
        command
            .arg("-c")
            .arg("-O2")
            .arg("-fPIC")
            .arg(format!("-D{VISIBILITY_DEFINE}"))
            .arg("-I")
            .arg(repo_root)
            .arg("-o")
            .arg(&object);
    }
    // The caller's own flags go last so that they win: a cross-compilation sysroot or
    // an `-fno-...` a platform needs must be able to override the defaults above.
    command.args(flags_from_env(ENV_CFLAGS, ENV_TARGET_CFLAGS));
    command.arg(source);

    run_tool(&mut command, &compiler_name, "compile", source);
    assert!(
        object.is_file(),
        "{} reported success but produced no object file at {}",
        compiler_name,
        object.display()
    );
    object
}

/// Collects the compiled shims into one static archive and returns its path.
///
/// The archive is removed first rather than updated in place, so a source file that
/// stops existing cannot leave a stale member behind.
fn archive_shims(objects: &[PathBuf], out_dir: &Path, msvc: bool) -> PathBuf {
    let archive = out_dir.join(if msvc {
        format!("{SHIM_LIB_NAME}.lib")
    } else {
        format!("lib{SHIM_LIB_NAME}.a")
    });
    let _ = fs::remove_file(&archive);

    let archiver = tool_from_env(ENV_AR, ENV_TARGET_AR, if msvc { "lib" } else { "ar" });
    let archiver_name = archiver.display();
    let mut command = archiver.command();
    if msvc {
        command
            .arg("/nologo")
            .arg(format!("/OUT:{}", archive.display()));
    } else {
        // `c` create without a diagnostic, `r` insert, `s` write an index -- the same
        // three `Makefile.in`'s ARFLAGS uses.
        command.arg("crs").arg(&archive);
    }
    command.args(objects);

    run_tool(&mut command, &archiver_name, "archive", &archive);
    assert!(
        archive.is_file(),
        "{} reported success but produced no archive at {}",
        archiver_name,
        archive.display()
    );
    archive
}

/// One resolved tool: the program to execute, and the arguments that came with it.
///
/// ★ The second field is why this is a struct and not a `String`. `CC` is very
/// commonly a *command* rather than a program name -- `ccache gcc`, `sccache cc`,
/// `gcc -m32`, `clang --target=aarch64-linux-gnu`, `zig cc` -- and the same is true of
/// `AR`. Handing the whole value to [`std::process::Command::new`] asks the operating
/// system to execute a file whose name literally contains a space, which fails with
/// `No such file or directory` and a message naming a program nobody installed.
/// `Makefile.in` exports `CC` and `AR` into the cargo build, so anyone who configured
/// this tree with a compiler cache reached that failure.
struct Tool {
    /// The executable.
    program: String,

    /// Arguments that were part of the variable's value, in order, before this build
    /// script's own.
    leading: Vec<String>,
}

impl Tool {
    /// Starts a [`std::process::Command`] for this tool with its leading arguments applied.
    fn command(&self) -> std::process::Command {
        let mut command = std::process::Command::new(&self.program);
        command.args(&self.leading);
        command
    }

    /// How to name this tool in a diagnostic: the command as the caller wrote it.
    fn display(&self) -> String {
        if self.leading.is_empty() {
            self.program.clone()
        } else {
            format!("{} {}", self.program, self.leading.join(" "))
        }
    }
}

/// Resolves one tool through the `<VAR>_<target>` / `TARGET_<VAR>` / `<VAR>` cascade,
/// falling back to the platform default, and splits the result into program and
/// arguments.
fn tool_from_env(base: &str, target_key: &str, default: &str) -> Tool {
    let target = env::var("TARGET").unwrap_or_default().replace('-', "_");
    let raw = env::var(format!("{base}_{target}"))
        .or_else(|_| env::var(target_key))
        .or_else(|_| env::var(base))
        .map(|value| value.trim().to_owned())
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| default.to_owned());

    let mut words = split_command(&raw, base).into_iter();
    let program = words.next().unwrap_or_else(|| {
        panic!(
            "{base} is set to `{raw}', which contains no program name.  Set it to the \
             compiler to run, optionally followed by arguments."
        )
    });
    Tool {
        program,
        leading: words.collect(),
    }
}

/// Resolves one flag list through the same cascade, split the same way.
fn flags_from_env(base: &str, target_key: &str) -> Vec<String> {
    let target = env::var("TARGET").unwrap_or_default().replace('-', "_");
    let raw = env::var(format!("{base}_{target}"))
        .or_else(|_| env::var(target_key))
        .or_else(|_| env::var(base))
        .unwrap_or_default();
    split_command(&raw, base)
}

/// Splits one environment variable into arguments the way a POSIX shell would word-split
/// it, and does **nothing else**.
///
/// Quoting is honoured because the alternative silently mis-splits: `-I'/opt/my sdk'`
/// and `-DBANNER="a b"` are ordinary things to find in `CFLAGS`, and
/// `str::split_whitespace` turns each into two broken arguments -- an include path that
/// does not exist and a macro definition that does not compile. The previous version of
/// this function split on whitespace and argued that `make` behaves the same way. It
/// does not: `make` hands the value to a shell, which is precisely what performs this
/// splitting.
///
/// What it deliberately does NOT do is anything else a shell would. There is no
/// variable expansion, no `~`, no globbing, no command substitution, no operator
/// handling: a `$(...)` or a backtick in `CFLAGS` arrives at the compiler as those
/// literal characters. A build script runs with the invoking user's privileges on
/// every machine that builds this library, so evaluating a string as shell here would
/// turn a stray environment variable into arbitrary code execution. Splitting is safe
/// and sufficient; evaluating is neither.
///
/// The rules, which are the shell's for the constructs it accepts:
///
/// * unquoted runs of whitespace separate arguments, and leading or trailing whitespace
///   produces no empty argument;
/// * `'...'` is literal throughout -- no escape has any meaning inside it;
/// * `"..."` is literal except that a backslash before `"` or `\` escapes it;
/// * outside quotes, a backslash escapes the next character, whatever it is;
/// * quotes may open and close mid-argument, so `-DA="b c"d` is the single argument
///   `-DA=b cd`, exactly as a shell would produce.
///
/// An unterminated quote is a hard error rather than a guess, because both guesses are
/// wrong in a way the compiler reports as something else entirely.
fn split_command(raw: &str, source: &str) -> Vec<String> {
    /// Which quoting context the scanner is in.
    enum Quote {
        /// Outside quotes: whitespace separates, backslash escapes.
        None,
        /// Inside `'`: everything is literal.
        Single,
        /// Inside `"`: backslash escapes `"` and `\`.
        Double,
    }

    let mut words = Vec::new();
    let mut current = String::new();
    // Distinguishes "no argument started" from "an argument that is the empty string",
    // which is what makes `CFLAGS=''` produce nothing while `CFLAGS='""'` produces one
    // empty argument -- the same distinction a shell draws.
    let mut started = false;
    let mut quote = Quote::None;
    let mut characters = raw.chars();

    while let Some(character) = characters.next() {
        match quote {
            Quote::None => match character {
                c if c.is_whitespace() => {
                    if started {
                        words.push(std::mem::take(&mut current));
                        started = false;
                    }
                }
                '\'' => {
                    started = true;
                    quote = Quote::Single;
                }
                '"' => {
                    started = true;
                    quote = Quote::Double;
                }
                '\\' => {
                    started = true;
                    if let Some(escaped) = characters.next() {
                        current.push(escaped);
                    } else {
                        panic!(
                            "{source} is set to `{raw}', which ends in a lone backslash.  \
                             Remove it, or double it to pass a literal backslash."
                        );
                    }
                }
                c => {
                    started = true;
                    current.push(c);
                }
            },
            Quote::Single => match character {
                '\'' => quote = Quote::None,
                c => current.push(c),
            },
            Quote::Double => match character {
                '"' => quote = Quote::None,
                '\\' => match characters.next() {
                    Some(escaped @ ('"' | '\\')) => current.push(escaped),
                    Some(other) => {
                        current.push('\\');
                        current.push(other);
                    }
                    None => panic!(
                        "{source} is set to `{raw}', which ends in a lone backslash inside \
                         a double-quoted string."
                    ),
                },
                c => current.push(c),
            },
        }
    }

    match quote {
        Quote::None => {}
        Quote::Single => panic!(
            "{source} is set to `{raw}', which has an unterminated single quote.  Close \
             it; guessing where it ended would pass the compiler an argument you did not \
             write."
        ),
        Quote::Double => panic!(
            "{source} is set to `{raw}', which has an unterminated double quote.  Close \
             it; guessing where it ended would pass the compiler an argument you did not \
             write."
        ),
    }

    if started {
        words.push(current);
    }
    words
}

/// Runs one tool invocation, turning both failure modes into an explanatory panic.
///
/// The two are genuinely different and the message says which happened: a tool that
/// could not be spawned is a missing or misnamed compiler, and a tool that ran and
/// failed has already printed its own diagnostics above this message.
fn run_tool(command: &mut std::process::Command, tool: &str, action: &str, subject: &Path) {
    let status = command.status();
    match status {
        Ok(status) if status.success() => {}
        Ok(status) => panic!(
            "failed to {action} {} with `{tool}`: {status}. The two C ABI shims define \
             `gzprintf`, `gzvprintf` and `inflate_table`, which stable Rust cannot \
             declare, so this is a hard failure rather than a skipped optimisation -- a \
             library without them links here and fails at every consumer that calls \
             one. The tool's own diagnostics are above.",
            subject.display()
        ),
        Err(error) => panic!(
            "could not run `{tool}` to {action} {}: {error}. A C compiler and archiver \
             are required to build this crate with the `libz-compat` and `gz` features, \
             because two of the declarations `zlib.h` fixes cannot be written in stable \
             Rust: `gzprintf` is variadic and `gzvprintf` takes a `va_list`. Both `CC` \
             and `AR` may be commands rather than bare program names -- `CC=\"ccache \
             gcc\"` is split into program and arguments, and quoting in `CFLAGS` is \
             honoured -- so name them however your toolchain is spelled:\n    \
             CC=<compiler> AR=<archiver> cargo build",
            subject.display()
        ),
    }
}

// ---------------------------------------------------------------------------
//  4. SONAME and version script
// ---------------------------------------------------------------------------
//
// Both arguments are scoped to the cdylib with `rustc-cdylib-link-arg` rather
// than the blanket `rustc-link-arg`.  That matters for two reasons:
//
//   * `--version-script` has no meaning when linking an executable, and the
//     integration tests under `tests/` are executables.  A blanket
//     `rustc-link-arg` would push it at every one of them.
//
//   * `-soname` on a test binary is at best ignored and at worst records a
//     bogus dependency name.
//
// Verified during development: with only the soname argument emitted, a
// `crate-type = ["cdylib", "staticlib", "rlib"]` crate builds clean, with zero
// warnings, and `readelf -d` on the result reports `SONAME  libz.so.1` --
// identical to the C build.  `cargo test` links and runs unaffected.
//
// ★ THE SONAME IS GATED ON `libz-compat`, and the gate is not tidiness.  A
// SONAME is a PROMISE: it tells the loader "a binary that recorded
// `DT_NEEDED libz.so.1` may bind to me", and it is what lets this file satisfy
// such a lookup at all.  With `libz-compat` off, this crate exports NOTHING --
// measured, `nm -D --defined-only --extern-only` reports 0 -- because the whole
// `extern "C"` surface is behind that feature; the artifact is the `#[repr(C)]`
// ABI mirrors and nothing else, which is what makes `--no-default-features`
// useful for feature-matrix checking.  Stamping `libz.so.1` on that object made
// it a silent trap: it would satisfy the loader's search, win over the real
// library, and then fail every single symbol resolution at load time.  The
// honest artifact for a build with no exported surface is an unversioned
// `libz.so` that no `DT_NEEDED libz.so.1` can reach, so that is what is emitted.
// Nothing is lost for the shipping configuration: `libz-compat` is in `default`,
// so a plain `cargo build` and every documented build command still get the
// SONAME, byte for byte as before.
fn emit_link_args(target: &TargetInfo, major: &str) {
    if env::var_os(CARGO_FEATURE_LIBZ_COMPAT).is_none() {
        return;
    }

    match target.format {
        SharedObjectFormat::Elf => {
            // configure L334/L336: `-Wl,-soname,libz.so.1`.  The name is built
            // from the major component of ZLIB_VERSION, which is how
            // configure L482 derives SHAREDLIBM (`libz$shared_ext.$VER1`),
            // rather than being hardcoded here.  At 1.3.2.1-motley the two
            // spellings agree, and deriving it means they cannot drift apart.
            cdylib_link_arg(&format!("-Wl,-soname,{ELF_SHARED_LIB}.{major}"));
        }
        SharedObjectFormat::ElfDashH => {
            // configure L348: QNX takes `-Wl,-hlibz.so.1` and no version
            // script.
            cdylib_link_arg(&format!("-Wl,-h{ELF_SHARED_LIB}.{major}"));
        }
        SharedObjectFormat::MachO => {
            // configure L366-L367: SHAREDLIBM is `libz.$VER1.dylib` and it is
            // passed as `-install_name`.  configure uses an absolute
            // `$libdir/...` because it knows the install prefix; a build script
            // does not, and a bare name is the correct, relocatable choice for
            // an artifact that has not been installed yet.  There is
            // deliberately no version script anywhere on this branch: Mach-O has
            // no such concept, and passing one would be a link error.
            cdylib_link_arg(&format!("-Wl,-install_name,libz.{major}.dylib"));
        }
        // Nothing either construct maps onto.  See the enum comments.
        SharedObjectFormat::Pe | SharedObjectFormat::Other => {}
    }
}

// Why this script does not pass the version script to rustc, and why asking it
// to is now an error rather than an option.
//
// The C build applies `zlib.map` with
//   $cc -shared -Wl,-soname,libz.so.1,--version-script,zlib.map        (configure L334)
// and that is what produces the reference library's exported surface of
// exactly 111 dynamic globals: 95 function symbols plus the 16 symbol-version
// nodes zlib.map declares.  Reproducing that surface is a hard requirement,
// which makes it tempting to pass the same argument straight through to rustc.
//
// Measured, on rustc 1.97.1 with a `crate-type = ["cdylib", ...]` crate whose
// `[lib] name` is `z`, it does not work -- and it fails in three different ways
// depending on how it is spelled:
//
//   * rustc already passes a version script of its own when it links a cdylib,
//     to restrict the exported symbol set, and that script uses an ANONYMOUS
//     version tag (`{ global: ...; local: *; };`).  It also passes
//     `-Wl,--no-undefined-version`.
//
//   * Adding zlib.map on top, with the default linker (rust-lld), fails hard:
//     "version script assignment of 'local' to symbol 'deflate_copyright'
//     failed: symbol not defined", and the same for inflate_copyright,
//     inflate_fast, zcalloc, zcfree, z_errmsg, gz_error, gz_intmax and
//     inflate_fixed.  That failure is structural, not a symptom of an
//     incomplete port: those names are zlib.map `local:` entries, they exist to
//     be hidden, and in this port they are `pub(crate)` Rust items that never
//     become linker symbols at all.  Completing the facade cannot make them
//     appear.
//
//   * Adding `-Wl,--undefined-version` to defeat that check does let the link
//     succeed, but rustc's script has already bound every exported symbol to
//     VER_NDX_GLOBAL, so zlib.map's assignments are rejected one by one:
//     "attempt to reassign symbol 'compressBound' of VER_NDX_GLOBAL to version
//     'ZLIB_1.2.0'".  The resulting library carries zero version nodes -- so
//     the parity target is missed anyway -- and the messages surface through
//     rustc's `linker_messages` lint, which breaks the zero-warnings bar.
//
//   * Forcing GNU ld with `-fuse-ld=bfd` fails earliest of all:
//     "anonymous version tag cannot be combined with other version tags".
//
// So there is no spelling in which a build script can hand zlib.map to rustc's
// cdylib link.  The verified way to apply it is to relink the staticlib that
// this same crate already produces:
//
//   gcc -shared -o libz.so.<ZLIB_VERSION> \
//       -Wl,-soname,libz.so.1 -Wl,--version-script=<repo>/zlib.map \
//       -Wl,--whole-archive libz.a -Wl,--no-whole-archive
//
// Run against the reference archive that recipe yields exactly 16 `A` + 95 `T`
// = 111 dynamic globals and `SONAME libz.so.1`, matching the C library symbol
// for symbol.  A Rust-staticlib prototype also proved why the 111-symbol parity
// check must remain the release gate: zlib.map correctly added all 16 version
// nodes and hid `inflate_table`, but the archive contributed the compiler-owned
// `rust_eh_personality` global, which the upstream map does not mention.  The
// packaging layer must suppress compiler-runtime globals without changing
// zlib.map, then compare the result with the C baseline.
//
// That relink and symbol filtering are packaging steps, and a build script
// cannot perform them: build scripts run *before* the crate is compiled, so
// `libz.a` does not exist yet.  They belong to whatever stages the drop-in
// library -- `Makefile.in`'s `rust` target, the CMake Rust path, or the Rust CI
// workflow.
//
// What this script therefore does, and this is the part that matters for the
// contract-immutability requirement, is make the version script impossible to
// lose sight of: it locates zlib.map, fails the build loudly if it is missing
// (`require_version_script`), and registers it as a rebuild trigger so an edit
// forces a relink.
//
// THE OPT-IN THAT USED TO LIVE HERE IS GONE, AND SETTING IT IS NOW AN ERROR.
//
// `ZLIB_RS_VERSION_SCRIPT=1` used to emit the pass-through anyway, together with
// `-Wl,--undefined-version` to get past rustc's `--no-undefined-version`, and a
// `cargo::warning` advising the caller to check the result by hand.  That was
// the wrong shape for this failure, for one reason: on every toolchain this port
// supports, the link SUCCEEDS and the library it produces carries zero version
// nodes.  The knob therefore delivered exactly the artifact its existence
// implied it was avoiding -- an unversioned `libz.so` that looks correct, passes
// a build, and cannot satisfy a consumer resolving `deflate@ZLIB_1.2.0` -- and
// it reported that outcome as a warning in a build whose bar is zero warnings,
// where the surrounding linker diagnostics would be the only evidence.  A knob
// whose success case is indistinguishable from its failure case is not an
// escape hatch.
//
// So the request is refused, with the reason and the working alternative in the
// message.  A caller who genuinely has a toolchain that does not supply its own
// version script is not stuck: `--version-script` reaches that toolchain
// through the packaging step's relink, which is where the argument has to be
// anyway to pick up the `csrc/gzprintf_shim.c` objects.
fn reject_version_script_passthrough(version_script: &Path) {
    println!("cargo::rerun-if-env-changed={ENV_VERSION_SCRIPT}");

    // `0` and unset both mean "do not pass it through", which is the only
    // supported behaviour, so neither is worth failing over.  Anything else is
    // a request this script cannot honour.
    match env::var(ENV_VERSION_SCRIPT) {
        Err(env::VarError::NotPresent) => return,
        Ok(value) if value.trim() == "0" => return,
        Ok(_) | Err(env::VarError::NotUnicode(_)) => {}
    }

    panic!(
        "{ENV_VERSION_SCRIPT} is set. This build script no longer passes {} to rustc's cdylib \
         link, and cannot: rustc supplies its own anonymous version script for a cdylib, so the \
         link either fails outright or -- measured on rustc 1.97.1 -- succeeds while dropping \
         every symbol-version node, producing an UNVERSIONED library that no consumer resolving \
         `deflate@ZLIB_1.2.0` can bind to. Unset the variable. To obtain a library that really \
         carries zlib.map's 16 version nodes, build the packaged drop-in, which relinks this \
         crate's static archive through the version script and also archives the gzprintf shim \
         into it:\n    make rust\nand verify the result with `make rust-symbols`.",
        version_script.display()
    );
}

// Emits one linker argument, scoped to the cdylib.
fn cdylib_link_arg(arg: &str) {
    println!("cargo::rustc-cdylib-link-arg={arg}");
}

// ---------------------------------------------------------------------------
//  4. Artifact-directory hygiene
// ---------------------------------------------------------------------------
//
// ★ THIS JOB USED TO STAGE THE VERSIONED SYMLINK CHAIN HERE, AND IT NO LONGER
// DOES.  The reasoning is worth keeping in full, because the change looks like a
// removal of a safety net and is the opposite of one.
//
// THE CHAIN ITSELF IS STILL MANDATORY.  The SONAME recorded in the shared object
// is `libz.so.1`, so that is the name the dynamic loader searches for; ship only
// a bare `libz.so` and the loader does not fail -- it keeps searching and
// SILENTLY BINDS THE SYSTEM libz, so a "drop-in replacement" check passes while
// exercising the C library.  Reproduced during planning, `ldd` resolving to
// /lib/x86_64-linux-gnu/libz.so.1.  That is why `Makefile.in`'s `rust` target
// stages `libz.so`, `libz.so.1` and `libz.so.<ZLIB_VERSION>` in `$(RUSTLIBDIR)`
// every time it runs, and why every drop-in validation asserts with `ldd` which
// file was bound instead of inferring it from a passing run.
//
// WHAT CHANGED IS WHERE, AND IT HAD TO.  Staging that chain HERE, in cargo's own
// artifact directory, was measured to do three things, and each one of them is a
// failure the chain exists to prevent:
//
//   * IT CANNOT BE MADE TO RESOLVE.  A build script runs BEFORE rustc links the
//     crate, so at the moment `symlink(2)` is called the real file does not
//     exist and there is no way to tell a `cargo check` (which never produces a
//     cdylib) from a `cargo build` -- the manifest's `crate-type` says what the
//     package CAN emit, not what this invocation WILL.  Measured: `cargo check`,
//     any failed build, and `cargo clean -p libz-rs-sys --release` each leave two
//     DANGLING versioned links in an otherwise artifact-free directory.  And a
//     dangling `libz.so.1` on a loader path is not inert: the loader treats the
//     unopenable candidate as a miss, keeps searching, and binds the system
//     libz -- exactly the silent wrong-library success the chain was staged to
//     turn into a visible failure.
//
//   * IT MAKES THE BUILD NON-HERMETIC.  Cargo puts this directory FIRST on the
//     library search path of every build script it launches, and the host
//     binutils are themselves linked against zlib (`ld.so --list /usr/bin/nm`
//     shows `DT_NEEDED libz.so.1`, no RPATH; `rustc` reaches it through
//     libLLVM, `ar`/`ld`/`objcopy`/`nm`/`as` through libbfd).  So the alias made
//     the tools that BUILD the library load the library, and because a
//     rustc-linked cdylib carries no symbol-version nodes each bind printed
//     `no version information available` -- 6 lines per rebuild into this
//     script's stderr, which cargo hides on success, and 32 in one build of the
//     differential crate.  Worse, the failure mode is not limited to warnings:
//     after a `--no-default-features` build (a SUPPORTED configuration) the
//     alias points at a library that exports nothing, and the next build that
//     compiles the C shims dies with
//     `/usr/bin/x86_64-linux-gnu-as: symbol lookup error: undefined symbol:
//     deflate`, blaming `csrc/gzprintf_shim.c`.  Measured: the build then stays
//     wedged -- even a plain default build fails -- until the alias is deleted by
//     hand.  A truncated file at that path is equally fatal and reports itself as
//     `failed to run rustc: ... file too short` attributed to an unrelated
//     crate's build script.
//
//   * IT PUT CONTRACT-SHAPED NAMES ON A LIBRARY THAT CANNOT HONOUR THEM.  The
//     cdylib cargo emits is not installable and cannot be made installable from
//     here (see the header comment: no version nodes, no `gzprintf`, three
//     `_zlib_rs_*` internals exposed).  Three files named exactly like an
//     installable drop-in, all reporting `SONAME libz.so.1`, with nothing at the
//     path to signal the difference, is a trap for any consumer, CI job or
//     `LD_LIBRARY_PATH` that guesses this directory.
//
// So the chain lives in exactly one place -- the packaging step, which runs AFTER
// the link, stages unconditionally, and stages the library that can actually
// satisfy the names -- and this job does the two things a build script CAN do
// correctly:
//
//   1. PRUNE.  Remove the versioned aliases a previous version of this script
//      left behind, so an existing checkout heals itself on the next build
//      rather than keeping the hazard forever.  Only a symlink whose link text is
//      exactly the real cargo artifact name is removed -- that is precisely what
//      was created -- so a regular file, or a link pointing anywhere else, is
//      somebody else's staged library and is left strictly alone.
//   2. EXPLAIN.  Write a plain-text notice next to the artifacts saying what each
//      one is, that the versioned names are deliberately absent, and which command
//      produces a library fit to install.  Its name cannot satisfy `-lz` or a
//      `libz.so.1` lookup, which is the property the aliases lacked.
//
// AAP §0.4.1.3's build.rs row asks for the chain here; AAP §0.3.1.2 and §0.8.3
// assign it to "the packaging step ... exactly as Makefile.in does".  The two
// cannot both be honoured -- a build script cannot stage a link to a file that
// does not exist yet -- and only the packaging step's copy is a chain that
// resolves, so that is the one implemented.
fn tidy_artifact_dir(target: &TargetInfo, version: &str, major: &str) {
    let Some(dir) = artifact_dir() else {
        // An `OUT_DIR` shape this cannot recognise (`cargo miri` nests an extra
        // component under `build`) means there is no directory to tidy.  Nothing
        // is staged any more, so nothing can be stale: this is a silent skip
        // rather than a `cargo::warning`, because a warning here would only
        // report that an informational file was not written -- and this
        // workspace's bar is zero build warnings.
        return;
    };

    // Only a Unix host could have created the symlinks being pruned.
    if cfg!(unix) {
        if let Some(layout) = AliasLayout::for_target(target, version, major) {
            for alias in layout.aliases {
                // A single-component ZLIB_VERSION would collapse the two
                // versioned names onto the real one; never touch that.
                if alias == layout.real {
                    continue;
                }
                prune_retired_alias(&dir, &layout.real, &alias);
            }
        }
    }

    write_artifact_notice(&dir);
}

// The real file cargo emits, and the versioned names that must NOT sit beside it.
struct AliasLayout {
    real: String,
    aliases: Vec<String>,
}

impl AliasLayout {
    // `None` means "this target has no versioned-name convention to mirror".
    fn for_target(target: &TargetInfo, version: &str, major: &str) -> Option<Self> {
        let (real, aliases) = match target.format {
            // configure L482: SHAREDLIBM is `libz$shared_ext.$VER1` and
            // SHAREDLIBV is `libz$shared_ext.$VER`, i.e. the version is a
            // suffix.  QNX shares the layout; only the soname FLAG differs.
            SharedObjectFormat::Elf | SharedObjectFormat::ElfDashH => (
                ELF_SHARED_LIB.to_owned(),
                vec![
                    format!("{ELF_SHARED_LIB}.{major}"),
                    format!("{ELF_SHARED_LIB}.{version}"),
                ],
            ),
            // configure L365-L366: on Mach-O the version goes before the
            // extension -- SHAREDLIBM is `libz.$VER1.dylib`, SHAREDLIBV is
            // `libz.$VER.dylib` -- which is why the name cannot be formed by
            // suffixing the constant.
            SharedObjectFormat::MachO => (
                MACHO_SHARED_LIB.to_owned(),
                vec![
                    format!("libz.{major}.dylib"),
                    format!("libz.{version}.dylib"),
                ],
            ),
            SharedObjectFormat::Pe | SharedObjectFormat::Other => return None,
        };

        Some(Self { real, aliases })
    }
}

// Removes one versioned alias a previous version of this script staged.
//
// The identification is deliberately narrow: the path must be a SYMBOLIC LINK
// whose link text is exactly `real` -- the single relative component this script
// used to write, `libz.so` or `libz.dylib` -- and nothing else qualifies.  So:
//
//   * a REGULAR FILE at that path is never removed.  Nothing in this workspace
//     puts one there, so it is somebody's staged or hand-relinked library, and
//     deleting a library because of its name would be the worse failure.  It is
//     left in place and the build proceeds; it is not this script's to police.
//   * a symlink pointing ANYWHERE ELSE is never removed either -- an absolute
//     path, `../something`, an installed library -- because that is a deliberate
//     arrangement by whoever made it.
//   * a DANGLING link still counts, and this is the case that matters most:
//     `symlink_metadata` does not follow the link and `read_link` reads the link
//     text rather than the target, so the dangling links left by `cargo check`,
//     by a failed build and by `cargo clean -p` are recognised and removed.
//
// A concurrent build removing the same link first produces exactly the wanted
// state, so `NotFound` is not a failure.  Any other removal error IS one: leaving
// a versioned alias next to a library that cannot satisfy it is the hazard this
// whole job exists to end, and failing loudly with the path and the one-line
// remedy beats proceeding while the trap is still armed.
fn prune_retired_alias(dir: &Path, real: &str, alias: &str) {
    let link = dir.join(alias);

    match fs::symlink_metadata(&link) {
        Ok(metadata) if metadata.file_type().is_symlink() => match fs::read_link(&link) {
            // Ours, and pointing where this script used to point it: remove it below.
            Ok(current) if current == Path::new(real) => {}
            // A link somebody else arranged. Never touched.
            Ok(_) => return,
            // ★ AN UNREADABLE LINK IS NOT AN ABSENT ONE. The alias is demonstrably
            // there -- `symlink_metadata` just succeeded on it -- and the one thing
            // that decides whether it is safe to leave in place is where it points.
            // Treating a permission or I/O failure as "not ours" leaves a
            // `libz.so.1` next to an artifact that cannot satisfy it, which is the
            // exact trap this function exists to disarm, and leaves it silently.
            Err(error) => panic!(
                "cannot read the symlink {}: {error}. It is a versioned alias next to cargo's \
                 `{real}`, which is NOT an installable libz -- it carries none of zlib.map's 16 \
                 symbol-version nodes and does not export gzprintf or gzvprintf -- and whether \
                 this build script staged it cannot be established without reading it. Fix the \
                 permissions or remove the link by hand, then rebuild; the installable chain is \
                 staged by `make rust`, never here.",
                link.display()
            ),
        },
        // A regular file or a directory: not ours, and not this script's to judge.
        Ok(_) => return,
        // Nothing there is the wanted state.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        // ★ EVERY OTHER INSPECTION ERROR IS A FAILURE, for the same reason as
        // above: `NotFound` is the only error that means "no alias here". A
        // permission error on the directory, an I/O error, a path that is not a
        // directory -- each of those means the answer is UNKNOWN, and an unknown
        // answer must not be reported as absence when the consequence of being
        // wrong is a stale alias that satisfies a `libz.so.1` lookup with a
        // library exporting 96 of the contract's 111 dynamic globals.
        Err(error) => panic!(
            "cannot inspect {}: {error}. That path is where an older revision of this build \
             script staged a versioned alias beside cargo's `{real}`, and this script cannot \
             establish whether one is present. Resolve the error and rebuild; a stale alias there \
             would satisfy a `libz.so.1` lookup with a library that is not a drop-in.",
            link.display()
        ),
    }

    if let Err(error) = fs::remove_file(&link) {
        assert!(
            error.kind() == std::io::ErrorKind::NotFound,
            "could not remove {}: {error}. It is a versioned alias an older revision of this \
             build script staged next to cargo's `{real}`, which is NOT an installable libz -- it \
             carries none of zlib.map's 16 symbol-version nodes and does not export gzprintf or \
             gzvprintf. Left in place it satisfies a `libz.so.1` lookup with that library, \
             including for the host build tools cargo puts this directory on the library search \
             path of. Remove it by hand and rebuild; the installable chain is staged by \
             `make rust`.",
            link.display()
        );
    }
}

// Writes the notice that explains the artifacts, next to them.
//
// Idempotent by comparison rather than by timestamp: when the file already holds
// exactly `ARTIFACT_NOTICE_TEXT` nothing is written, so an up-to-date build
// directory is not modified and no mtime changes.
//
// Failure to write is deliberately NOT a build failure.  The file is
// informational -- everything it says is also in `crates/libz-rs-sys/src/lib.rs`,
// the root `Cargo.toml` and the `README` -- and an unwritable or read-only build
// directory is not a reason to refuse to produce a library.  This is the one
// place in this script where an error is swallowed, and it is swallowed because
// the alternative would fail a build over a comment.
fn write_artifact_notice(dir: &Path) {
    let notice = dir.join(ARTIFACT_NOTICE);

    if fs::read_to_string(&notice).is_ok_and(|current| current == ARTIFACT_NOTICE_TEXT) {
        return;
    }

    let _ = fs::write(&notice, ARTIFACT_NOTICE_TEXT);
}

// The directory cargo puts the library in: `target/[<triple>/]<profile>`.
//
// There is no environment variable for it.  `OUT_DIR` is the wrong directory to
// use directly -- it is this crate's private scratch space, which no loader and
// no install rule ever looks at -- but its shape is contracted and the artifact
// directory is exactly three levels above it:
//
//     <target>/[<triple>/]<profile>/build/<pkg>-<hash>/out
//     ^ this                        ^ CARGO_BUILD_DIR     ^ OUT_DIR
//
// The shape is VERIFIED rather than assumed: the component two levels up has to
// be `build`.  A layout this cannot recognise yields `None` and the caller does
// nothing, rather than writing into a directory that is not cargo's artifact
// directory at all.  Deriving it from CARGO_TARGET_DIR + PROFILE would not work:
// `PROFILE` is `debug` or `release` even under a custom profile whose directory is
// named after the profile, and CARGO_TARGET_DIR is frequently unset.
fn artifact_dir() -> Option<PathBuf> {
    let out_dir = PathBuf::from(require_env("OUT_DIR"));

    // Two levels up from `.../build/<pkg>-<hash>/out` has to be `.../build`.
    // Checking that first is what keeps job 4 from touching a directory that only
    // resembles the artifact directory: an unrecognised layout yields `None`.
    // `cargo miri` is a real example -- it nests an extra component under
    // `build`, so the check genuinely fires -- and an ordinary `--target` build is
    // not, because it only adds a component *above* the profile directory.
    let build_dir = out_dir.parent().and_then(Path::parent);
    if build_dir.and_then(Path::file_name) != Some(std::ffi::OsStr::new(CARGO_BUILD_DIR)) {
        return None;
    }

    match build_dir.and_then(Path::parent) {
        Some(profile_dir) if !profile_dir.as_os_str().is_empty() => Some(profile_dir.to_path_buf()),
        // The `build` component has no usable parent, so there is no artifact
        // directory to derive either.
        _ => None,
    }
}

// ---------------------------------------------------------------------------
//  Locating the repository and reading the contract
// ---------------------------------------------------------------------------

// The repository root, found from CARGO_MANIFEST_DIR.  The documented distance
// is exactly two levels (crates/libz-rs-sys -> crates -> root), but the search
// walks up looking for the two files that identify the root rather than
// hardcoding a depth, so a relocated or vendored crate still works and a wrong
// answer is impossible: either both markers are there or the search fails.
fn repo_root() -> PathBuf {
    let manifest_dir = PathBuf::from(require_env("CARGO_MANIFEST_DIR"));

    let mut searched = String::new();
    let mut cursor: Option<&Path> = Some(manifest_dir.as_path());

    for _ in 0..MAX_ROOT_SEARCH_DEPTH {
        let Some(dir) = cursor else { break };

        if dir.join(PUBLIC_HEADER).is_file() && dir.join(WORKSPACE_MANIFEST).is_file() {
            return dir.to_path_buf();
        }

        searched.push_str("\n  - ");
        searched.push_str(&dir.display().to_string());
        cursor = dir.parent();
    }

    panic!(
        "could not locate the zlib repository root above {}: no directory searched contains \
         both {PUBLIC_HEADER} and {WORKSPACE_MANIFEST}.{searched}\n\
         This crate has to be built from inside the zlib checkout, because it reads \
         ZLIB_VERSION out of {PUBLIC_HEADER} and hands {VERSION_SCRIPT} to the linker.",
        manifest_dir.display()
    );
}

// zlib.map has to be present.  A silently missing version script is the worst
// case for this library: nothing fails, and the shared object ships with every
// internal symbol leaked into its dynamic table -- which then shows up much
// later as a confusing diff against the reference symbol list.  So this is a
// hard failure with the reason spelled out, not a warning.
fn require_version_script(version_script: &Path) {
    if version_script.is_file() {
        return;
    }

    panic!(
        "the linker version script {} is missing. It is part of zlib's immutable public \
         contract: it declares the 16 symbol-version nodes the reference library exports \
         and the `local:` block that keeps deflate_copyright, inflate_copyright, \
         inflate_fast, inflate_table, zcalloc, zcfree, z_errmsg, gz_error, gz_intmax and \
         inflate_fixed out of the dynamic symbol table. Restore it from version control; \
         it must never be edited or regenerated.",
        version_script.display()
    );
}

// Scrapes ZLIB_VERSION out of the public header.  configure L56 does the same
// thing with
//     sed -n -e '/VERSION "/s/.*"\(.*\)".*/\1/p' < zlib.h
// which takes the text between the first and the last double quote on the line.
// This is parsed rather than being written down as a constant precisely so it
// cannot drift away from the header: `deflateInit_` and `inflateInit_` compare
// the caller's compile-time version string against the library's, so a stale
// copy here would rename the shipped library out from under every consumer.
fn zlib_version(public_header: &Path) -> String {
    let text = match fs::read_to_string(public_header) {
        Ok(text) => text,
        Err(error) => panic!(
            "could not read the public header {}: {error}",
            public_header.display()
        ),
    };

    for line in text.lines() {
        let line = line.trim_start();
        if !line.starts_with("#define ZLIB_VERSION") {
            continue;
        }

        let Some(open) = line.find('"') else { continue };
        // `open` indexes a one-byte `"`, so `open + 1` is a char boundary.
        let Some(rest) = line.get(open + 1..) else {
            continue;
        };
        let Some(close) = rest.rfind('"') else {
            continue;
        };
        let Some(version) = rest.get(..close) else {
            continue;
        };
        if version.is_empty() {
            continue;
        }

        return version.to_owned();
    }

    panic!(
        "could not find a `#define ZLIB_VERSION \"...\"` line in {}. That macro is the \
         authoritative version identity of the library and the source of the versioned \
         shared-object name; the header must not be edited.",
        public_header.display()
    );
}

// The leading integer of ZLIB_VERSION, which is how configure L58 derives VER1
// and hence SHAREDLIBM (`libz$shared_ext.$VER1`, configure L482) and the SONAME.
fn major_version<'a>(version: &'a str, public_header: &Path) -> &'a str {
    let end = version
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(version.len());

    match version.get(..end) {
        Some(major) if !major.is_empty() => major,
        _ => panic!(
            "ZLIB_VERSION in {} is {version:?}, which does not start with a version number. \
             The SONAME is derived from its leading integer, so it cannot be parsed.",
            public_header.display()
        ),
    }
}

// ---------------------------------------------------------------------------
//  Cargo environment helpers
// ---------------------------------------------------------------------------
// There is deliberately no shared `0`/`1` environment-variable helper.  This
// script has two knobs and they are not the same shape: `ZLIB_RS_SIMD` is a
// tri-state assertion (unset, agree, disagree) whose two disagreement cases need
// their own messages, and `ZLIB_RS_VERSION_SCRIPT` has no honourable `1` case at
// all.  A common helper returning `bool` could express neither, so each one
// reads `env::var` directly, next to the reasoning that governs it.

// Reads a variable cargo is contracted to set.  Its absence means this file is
// not running as a build script, and there is nothing sensible to fall back to.
fn require_env(key: &str) -> String {
    match env::var(key) {
        Ok(value) => value,
        Err(error) => panic!(
            "the cargo build-script environment variable {key} is unavailable ({error}). \
             This file is a Cargo build script and cannot be run standalone."
        ),
    }
}
