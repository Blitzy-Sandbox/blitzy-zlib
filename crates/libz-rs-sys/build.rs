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
//   4. The versioned aliases -- `libz.so.1` and `libz.so.<ZLIB_VERSION>` beside
//      the cdylib in `target/<profile>`, as relative symlinks to it.  See the
//      long comment above `stage_versioned_aliases`; the direction is inverted
//      relative to the C build and the reason it exists at all is a failure that
//      was reproduced, not a tidiness preference.
//
//   5. The C ABI shims -- `csrc/gzprintf_shim.c`, which defines the variadic
//      `gzprintf` and `gzvprintf`, and `csrc/inftrees_shim.c`, which defines
//      `inflate_table` with `inftrees.h`'s own `codetype` prototype.  Stable Rust
//      can declare none of the three.  They are COMPILED HERE, with the platform
//      compiler, and archived so that rustc merges them into the `libz.a` it
//      produces; `cfg(zlib_rs_gzprintf)` -- and with it `zlibCompileFlags()` bit
//      27 -- follows from that compile actually happening.  See the long comment
//      above `build_c_abi_shims`.
//
// WHAT A BARE `cargo build` PRODUCES, STATED PRECISELY
//
// After this script has run and the crate has been linked, `target/<profile>`
// holds `libz.a`, `libz.so` with `SONAME libz.so.1`, and the two aliases job 4
// creates.
//
// `libz.a` IS COMPLETE: it defines all 95 functions `zlib.h` declares, plus
// `inflate_table` as a hidden global, because job 5's objects are merged into it.
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
// same complete archive through `zlib.map` and stages the versioned chain in an
// isolated directory.  `crates/libz-rs-sys/src/lib.rs` carries the artifact matrix
// that states this once; this script's job 4 is not a substitute for that step and
// must not be described as one.
//
// WHY JOB 4 IS WORTH DOING ANYWAY, AND WHAT IT COSTS
//
// Without `libz.so.1` beside it, a consumer linked against this artifact does
// not fail -- it SILENTLY BINDS THE SYSTEM libz.  Reproduced during planning:
// `ldd` resolved to /lib/x86_64-linux-gnu/libz.so.1 and the probe printed the
// system library's version, so a "drop-in replacement" check passed while
// exercising the C library.  With the alias present, the same consumer binds
// this file and any incompleteness above surfaces as a diagnosable symbol or
// version error.  Turning a silent wrong-library success into a visible failure
// is the whole point, and it is why AAP §0.4.1.3 assigns the chain here.
//
// The cost is real and is mitigated rather than ignored.  `target/<profile>` is
// on the library search path of everything cargo launches, and the host binutils
// carry `DT_NEEDED libz.so.1` with no RPATH of their own (`ld.so --list
// /usr/bin/nm`).  Measured: with the alias in place, the differential crate's
// build script emitted 32 `no version information available` notices, because
// its `nm`/`objcopy` calls loaded this unversioned library instead of the
// system one.  The fix is at the caller: `crates/zlib-rs-differential/build.rs`
// strips LD_LIBRARY_PATH and DYLD_LIBRARY_PATH from its own environment before
// spawning any host tool, so its children resolve libz the way the system
// intends.  Any future build script that shells out to a libz-linked tool needs
// the same two lines.
//
// Hard rules this file lives by:
//
//   * It never writes to `zlib.h`, `zconf.h` or `zlib.map`.  Those three are
//     zlib's immutable public contract; this script only ever reads them, and
//     it fails the build loudly rather than silently proceeding without them.
//
//   * It compiles exactly the two C translation units in `csrc/`, and no others.
//     This crate has no `[build-dependencies]` and must not acquire `cc`: the
//     platform compiler is invoked directly, so the AAP's frozen dependency
//     inventory is untouched.  Compiling the C reference implementation is
//     exclusively the differential crate's job and nothing here goes near it.
//
//   * It never touches the network.
//
//   * Outside `OUT_DIR`, the only things it creates are the two symlinks of job 4,
//     and the only thing it deletes is a symlink it would otherwise replace at one
//     of those two exact paths; it will not remove or overwrite a regular file even
//     there.  Job 5's object files and their archive live in `OUT_DIR`, which is
//     cargo's own scratch directory for exactly that purpose.  It writes nothing in
//     the source tree, and in particular it never touches `libz.so`, `libz.a`, or
//     anything else cargo owns.
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

// The two features the C shims need: `libz-compat` turns the unmangled C symbols on,
// and `gz` compiles the `gzFile` layer that owns the two Rust helpers the variadic
// shim calls.  With either one off the shim has nothing to link against, so it is not
// compiled at all -- an archive with two undefined symbols in it would fail the link
// of every consumer rather than of the crate that produced it.
const CARGO_FEATURE_LIBZ_COMPAT: &str = "CARGO_FEATURE_LIBZ_COMPAT";
const CARGO_FEATURE_GZ: &str = "CARGO_FEATURE_GZ";

// The C translation units this library contains, relative to CARGO_MANIFEST_DIR, and
// the archive they are collected into.  `csrc/gzprintf_shim.c` defines the two variadic
// entry points; `csrc/inftrees_shim.c` defines `inflate_table` with `inftrees.h`'s own
// `codetype` prototype.  Both are compiled by THIS script -- see
// `build_c_abi_shims` -- so that the archive cargo produces is a complete libz.
const SHIM_SOURCES: [&str; 2] = ["csrc/gzprintf_shim.c", "csrc/inftrees_shim.c"];

// The name of the static archive the shims are collected into, as `-l static=` names
// it.  Both halves matter: the file must be `lib<name>.a` for a GNU-style linker to
// find it, and the name must not collide with the crate's own `z`.
const SHIM_LIB_NAME: &str = "zlib_rs_cabi";

// `zutil.h` L15-L19 spells `ZLIB_INTERNAL` as `__attribute__((visibility("hidden")))`
// only when this is defined, and `configure` L962-L963 defines it for both CFLAGS and
// SFLAGS.  The shims are compiled with it for the same reason the C library is: it is
// what keeps `inflate_table` out of a shared object's dynamic table, which is the
// property `zlib.map`'s `local:` block asks for and the one the C build has.
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
// `zlib_rs::crc32::Simd` and `Adler32Generic` / `Braid` respectively.
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
    // component, and the whole four-component string is the versioned alias job
    // 4 stages.  Both are parsed from the header so neither can drift from it.
    let version = zlib_version(&public_header);
    let major = major_version(&version, &public_header).to_owned();
    let target = TargetInfo::from_env();

    check_simd_request();
    build_c_abi_shims(&repo_root);
    reject_version_script_passthrough(&version_script);
    emit_link_args(&target, &major);
    stage_versioned_aliases(&target, &version, &major);
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
    // `-install_name`, and the analogue of a version script is an exported
    // symbols list, which this port does not ship.  configure L362-L367.
    MachO,
    // PE/COFF, MSVC or MinGW.  Neither construct exists.  configure L341-L345
    // uses a plain `-shared` for MinGW.  The PE analogue of zlib.map is a
    // module-definition file, and `win32/zlib.def` does exist in this tree --
    // but Windows is OUT OF SCOPE for this port, as the target matrix in
    // `rust-toolchain.toml` states, so nothing is emitted here and no Windows
    // build has been attempted or verified.  Should Windows ever be brought in
    // scope, `win32/zlib.def` is the file to wire up, as `/DEF:` on MSVC.  The
    // `#[cfg(windows)]` items that already exist in the source (`gzopen_w`, the
    // `_WIN32` entries in cbindgen.toml's `[defines]`) are forward
    // compatibility, not a claim that this branch works.
    Pe,
    // Everything else: wasm, emscripten, AIX, HP-UX, bare metal.  Emitting an
    // ELF-only argument here would break the link, so nothing is emitted.
    Other,
}

#[derive(Debug)]
struct TargetInfo {
    os: String,
    env: String,
    format: SharedObjectFormat,
}

impl TargetInfo {
    fn from_env() -> Self {
        let os = require_env("CARGO_CFG_TARGET_OS");
        // CARGO_CFG_TARGET_ENV is legitimately empty for many targets, so its
        // absence is normal and must not be treated as an error.
        let env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();

        let format = match os.as_str() {
            "linux" | "android" | "freebsd" | "netbsd" | "openbsd" | "dragonfly" | "solaris"
            | "illumos" | "haiku" | "hurd" | "redox" | "fuchsia" => SharedObjectFormat::Elf,
            "nto" => SharedObjectFormat::ElfDashH,
            "macos" | "ios" | "tvos" | "watchos" | "visionos" => SharedObjectFormat::MachO,
            "windows" => SharedObjectFormat::Pe,
            _ => SharedObjectFormat::Other,
        };

        Self { os, env, format }
    }

    // Used only in diagnostics, which is the whole reason CARGO_CFG_TARGET_ENV
    // is read: `windows-gnu` and `windows-msvc` behave identically here, but a
    // message that cannot name which one it saw is a message that costs
    // somebody an afternoon.
    fn describe(&self) -> String {
        if self.env.is_empty() {
            self.os.clone()
        } else {
            format!("{}-{}", self.os, self.env)
        }
    }
}

// ---------------------------------------------------------------------------
//  5. The C ABI shims -- the two declarations stable Rust cannot express
// ---------------------------------------------------------------------------
//
// ★ **This script compiles C, and it has to.** Two of the declarations a drop-in
// `libz` must define cannot be written in stable Rust at all, and a third must be
// written with a C type Rust has no spelling for:
//
//   * `gzprintf` is variadic (`zlib.h` L1549) and `gzvprintf` takes a `va_list`
//     (`zlib.h` L2047). Defining either needs the unstable `c_variadic` feature or
//     `core::ffi::VaList`, and AAP §0.7.1 (h) pins this workspace to stable Rust
//     1.80. `csrc/gzprintf_shim.c` defines both over two hidden Rust helpers.
//   * `inflate_table`'s first parameter is `codetype`, a C enum (`inftrees.h`
//     L54-L62), and the unmodified `test/infcover.c` compiles a call against that
//     prototype. A Rust `#[no_mangle]` function cannot present a C enum parameter
//     without making an out-of-range `int` from a C caller into instant undefined
//     behaviour, so `csrc/inftrees_shim.c` presents the exact prototype and
//     forwards to a hidden Rust entry point that validates the value.
//
// Compiling them here rather than in the packaging layer is what makes ONE
// artifact story possible: `target/<profile>/libz.a` -- the `staticlib` cargo
// produces -- then contains every one of the 95 functions `zlib.h` declares, and
// the packaged shared object is relinked from exactly that archive instead of from
// an archive plus a separately compiled object. Measured: `ar t` lists both shim
// objects in `libz.a` and `nm` reports `T gzprintf`, `T gzvprintf` and
// `T inflate_table` in it.
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
// cannot carry the 16 `ZLIB_1.2.*` version nodes, whatever this script emits. The
// installable shared object is therefore produced by ONE documented step from the
// complete archive -- `Makefile.in`'s `rust` target, or the CMake equivalent --
// which links it under `zlib.map` with the right SONAME and symlink chain.
// `crates/libz-rs-sys/src/lib.rs` carries the artifact matrix that states this once.
//
// # Requiring a C compiler
//
// A C compiler is a hard requirement for a `libz-compat` + `gz` build, and the
// failure is loud. The alternative -- skipping the shims when no compiler is found
// -- is exactly the silently-incomplete artifact this arrangement exists to remove:
// a library that links but has no `gzprintf` fails at the *consumer*, long after the
// build that produced it went green. No new crate dependency is taken to do it
// (AAP §0.5.1.3 freezes the inventory and §0.6.4.1 keeps `cc` to the differential
// crate alone); the platform compiler is invoked directly, through the same
// `CC`/`CC_<target>`/`TARGET_CC` and `CFLAGS` cascade every C-building build script
// honours, so a cross-compiling caller configures this one the way it already
// configures the rest.
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
    // `_zlib_rs_gzprintf_commit`, and `inflate_table` through
    // `_zlib_rs_inflate_table`; all three live behind `libz-compat`, the first two
    // additionally behind `gz`. Compiling the shims against a build without those
    // features would archive undefined symbols, so there is nothing to build here --
    // and nothing missing either, because such a build is not a C-ABI libz at all.
    if env::var_os(CARGO_FEATURE_LIBZ_COMPAT).is_none() || env::var_os(CARGO_FEATURE_GZ).is_none() {
        return;
    }

    let manifest_dir = PathBuf::from(require_env("CARGO_MANIFEST_DIR"));
    let out_dir = PathBuf::from(require_env("OUT_DIR"));
    let msvc = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default() == "msvc";

    let mut objects = Vec::with_capacity(SHIM_SOURCES.len());
    for relative in SHIM_SOURCES {
        let source = manifest_dir.join(relative);
        assert!(
            source.is_file(),
            "the C ABI shim {} is missing. The two shims define `gzprintf`, `gzvprintf` \
             and `inflate_table`, none of which stable Rust can declare with the \
             prototype the contract fixes, so a library built without them is not a \
             drop-in libz. Restore the file from version control.",
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

    // Now, and only now, may the Rust code claim the two formatting entry points.
    // `util.rs` derives `zlibCompileFlags` bit 27 from this cfg, so the flags word
    // describes the artifact that was actually built.
    println!("cargo::rustc-cfg={CFG_GZPRINTF}");
}

/// Compiles one shim translation unit, returning the object file it produced.
///
/// The include path is the repository root, because both shims include the immutable
/// `zlib.h` (and, through it, `zconf.h`) rather than restating prototypes that could
/// then drift from the contract. `-DHAVE_HIDDEN` is what makes `zutil.h`'s
/// `ZLIB_INTERNAL` expand to hidden visibility, matching `configure` L962-L963.
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
    let mut command = std::process::Command::new(&compiler);
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

    run_tool(&mut command, &compiler, "compile", source);
    assert!(
        object.is_file(),
        "{} reported success but produced no object file at {}",
        compiler,
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
    let mut command = std::process::Command::new(&archiver);
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

    run_tool(&mut command, &archiver, "archive", &archive);
    assert!(
        archive.is_file(),
        "{} reported success but produced no archive at {}",
        archiver,
        archive.display()
    );
    archive
}

/// Resolves one tool name through the `<VAR>_<target>` / `TARGET_<VAR>` / `<VAR>`
/// cascade, falling back to the platform default.
fn tool_from_env(base: &str, target_key: &str, default: &str) -> String {
    let target = env::var("TARGET").unwrap_or_default().replace('-', "_");
    env::var(format!("{base}_{target}"))
        .or_else(|_| env::var(target_key))
        .or_else(|_| env::var(base))
        .map(|value| value.trim().to_owned())
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| default.to_owned())
}

/// Resolves one flag list through the same cascade, split on whitespace.
///
/// Whitespace splitting is what every other consumer of `CFLAGS` does; a flag with an
/// embedded space has to be passed through a response file or a wrapper script, which
/// is also true of `make`.
fn flags_from_env(base: &str, target_key: &str) -> Vec<String> {
    let target = env::var("TARGET").unwrap_or_default().replace('-', "_");
    let raw = env::var(format!("{base}_{target}"))
        .or_else(|_| env::var(target_key))
        .or_else(|_| env::var(base))
        .unwrap_or_default();
    raw.split_whitespace().map(str::to_owned).collect()
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
             because three of the declarations `zlib.h` and `inftrees.h` fix cannot be \
             written in stable Rust. Name them if they are not on PATH under their usual \
             names:\n    CC=<compiler> AR=<archiver> cargo build",
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
fn emit_link_args(target: &TargetInfo, major: &str) {
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
//  4. The versioned aliases
// ---------------------------------------------------------------------------
//
// THE DIRECTION IS INVERTED RELATIVE TO THE C BUILD.  Read that first, because
// getting it backwards produces a chain of links to nothing.
//
//   * In the C build the REAL FILE is `libz.so.1.3.2.1-motley`, and `libz.so`
//     and `libz.so.1` are symlinks pointing at it (`Makefile.in`'s
//     `$(SHAREDLIBV)` recipe: `ln -s $@ $(SHAREDLIB)`, `ln -s $@
//     $(SHAREDLIBM)`).
//   * Cargo emits the REAL FILE as `libz.so`.  So the two versioned names are
//     the symlinks here, and they point AT `libz.so`.
//
// The net effect is the same and it is the only thing the loader cares about:
// all three names resolve to one inode.  The link target is RELATIVE -- the
// single component `libz.so`, never an absolute path -- so the set survives
// being copied, moved or installed as a unit.
//
// WHY IT EXISTS.  The SONAME recorded in the object is `libz.so.1`, so that is
// the name the dynamic loader searches for.  With only a bare `libz.so` present
// the loader does not fail: it keeps searching and SILENTLY BINDS THE SYSTEM
// libz.  Reproduced during planning against the reference `.so` with an rpath
// and no `libz.so.1` alias -- `ldd` resolved to /lib/x86_64-linux-gnu/libz.so.1
// and the probe printed the system version; after `ln -s`, both pointed at the
// local artifact.  Every drop-in validation must therefore assert with `ldd`
// which file was bound, and never infer it from a passing run.
//
// WHAT IT DOES NOT DO.  It does not make the cargo artifact a complete drop-in;
// see the header comment for the three specific things that artifact still
// lacks.  `Makefile.in`'s `rust` target is the drop-in contract.
//
// PROPERTIES THIS IMPLEMENTATION GUARANTEES, each for a reason:
//
//   * Dangling at creation is EXPECTED, not a bug.  A build script runs before
//     the crate is linked, so `libz.so` does not exist yet; `symlink(2)` does
//     not care, and the link resolves the moment the real file appears.  A
//     `cargo check` (which never produces a cdylib) therefore leaves two
//     dangling links behind, which is harmless: `dlopen`/`ld.so` treat an
//     unopenable candidate as a miss and continue searching.  A build script
//     cannot distinguish `check` from `build` -- the manifest's `crate-type`
//     says what the package CAN emit, not what this invocation WILL -- so this
//     is the honest trade, and it is why nothing here reports a stale or
//     missing real file as an error.
//
//   * Idempotent, and it never destroys anything.  An existing symlink that
//     already points at the right name is left alone; one that points somewhere
//     else is replaced.  A REGULAR FILE at either alias path is never removed --
//     nothing in this workspace creates one, so it is somebody's staged library
//     and the build fails naming it rather than deleting it.
//
//   * Race-tolerant.  Two cargo invocations sharing a target directory can
//     reach `symlink(2)` at the same moment; `AlreadyExists` means the other
//     process won and the outcome is the one wanted.
//
//   * Restored on the next run of this script, but NOT on a build that has
//     nothing to do.  Measured: delete `libz.so.1`, and an up-to-date
//     `cargo build` finishes without re-running the build script, so the alias
//     stays missing until something re-triggers it (`touch build.rs`, an edit to
//     `zlib.h` or `zlib.map`, a `ZLIB_RS_SIMD` change, or `cargo clean`).
//     Cargo offers no "always re-run" that does not also force a rebuild of the
//     crate, and paying a full relink on every invocation to police two symlinks
//     is the wrong trade.  This is another reason the packaging step, which
//     stages its chain unconditionally every time it runs, is the drop-in
//     contract.
//
//   * Loud where the chain is required, silent where it is meaningless.  On ELF
//     and Mach-O targets a failure to stage is a build failure.  On PE there is
//     no SONAME concept to mirror and nothing is attempted.  On a non-Unix HOST
//     nothing is attempted either: the artifact is not loaded there, and
//     whatever packages it on the target platform creates the chain.
fn stage_versioned_aliases(target: &TargetInfo, version: &str, major: &str) {
    let Some(layout) = AliasLayout::for_target(target, version, major) else {
        // PE and anything unrecognised: no SONAME, nothing to mirror.
        return;
    };

    // A Unix host is required to create a symlink at all.  Cross-compiling from
    // a non-Unix host to a Unix target is supported; the aliases are simply left
    // to the packaging step that runs where the library is used.
    if !cfg!(unix) {
        return;
    }

    let Some(dir) = artifact_dir() else {
        // The layout is not one this can recognise, so there is nowhere to stage
        // the aliases that a loader would look in.  This is a *skip*, announced,
        // rather than a failure: the aliases are a convenience for a consumer
        // linking straight against the cargo artifact, and the supported drop-in
        // path -- `make rust` -- stages the whole chain itself in `RUSTLIBDIR`.
        // Aborting here would instead make the crate unbuildable under any tool
        // that nests its build directory differently, `cargo miri` among them,
        // which is the instrument the facade's own unsafe boundary is checked
        // with.
        println!(
            "cargo::warning=libz-rs-sys: OUT_DIR has an unrecognised shape, so the \
             versioned library aliases were not staged. Link through `make rust` \
             (which stages libz.so, libz.so.1 and libz.so.<version> itself) rather \
             than against the cargo artifact directly."
        );
        return;
    };

    for alias in layout.aliases {
        // `libz.so.1` and `libz.so.1.3.2.1-motley` are distinct at every real
        // ZLIB_VERSION, but a single-component version string would collapse
        // them onto one name, and staging the same path twice would be a
        // needless remove-and-recreate.
        if alias == layout.real {
            continue;
        }
        stage_alias(&dir, &layout.real, &alias, target);
    }
}

// The real file cargo emits, and the versioned names that must resolve to it.
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

// Points one alias at the real library, in the cargo artifact directory.
fn stage_alias(dir: &Path, real: &str, alias: &str, target: &TargetInfo) {
    let link = dir.join(alias);

    // `symlink_metadata` deliberately does NOT follow the link, so a dangling
    // alias from a previous build is seen as the symlink it is rather than as a
    // missing file.
    match fs::symlink_metadata(&link) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            if fs::read_link(&link).is_ok_and(|current| current == Path::new(real)) {
                return;
            }

            if let Err(error) = fs::remove_file(&link) {
                // A concurrent build removing the same stale link first is the
                // outcome wanted, so `NotFound` is the one acceptable failure.
                assert!(
                    error.kind() == std::io::ErrorKind::NotFound,
                    "could not replace the stale symbolic link {}: {error}. It has to point at \
                     {real} so that a consumer linked against this library binds to it instead \
                     of falling back to the system libz.",
                    link.display()
                );
            }
        }
        Ok(_) => panic!(
            "{} already exists and is not a symbolic link. Nothing in this workspace creates a \
             regular file there -- cargo emits {real}, and the packaged drop-in is staged \
             elsewhere -- so this build will not remove it. Move it aside and rebuild.",
            link.display()
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => panic!(
            "could not inspect {} while staging the versioned aliases: {error}",
            link.display()
        ),
    }

    match create_symlink(real, &link) {
        Ok(()) => {}
        // Another cargo invocation sharing this target directory created it
        // first, which produces exactly the state wanted.
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => panic!(
            "could not create the symbolic link {} -> {real}: {error}. On {} the SONAME recorded \
             in the shared object is a versioned name, so without this link a consumer does not \
             fail -- it silently binds the SYSTEM libz, and every drop-in check then passes while \
             exercising the wrong library.",
            link.display(),
            target.describe()
        ),
    }
}

// The symlink primitive, isolated so the rest of the file is host-agnostic.
#[cfg(unix)]
fn create_symlink(original: &str, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(original, link)
}

// Never reached -- `stage_versioned_aliases` returns early on a non-Unix host --
// but it has to COMPILE there, because `std::os::unix` does not exist on a
// Windows host even when the target is Unix.
#[cfg(not(unix))]
fn create_symlink(_original: &str, _link: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "creating a symbolic link requires a Unix host",
    ))
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
// be `build`.  A layout this cannot recognise yields `None`, and the caller
// announces the skipped staging, instead of quietly staging aliases into a
// directory nothing reads.  Deriving it from CARGO_TARGET_DIR + PROFILE would
// not work: `PROFILE` is `debug` or `release` even under a custom profile whose
// directory is named after the profile, and CARGO_TARGET_DIR is frequently
// unset.
fn artifact_dir() -> Option<PathBuf> {
    let out_dir = PathBuf::from(require_env("OUT_DIR"));

    // Two levels up from `.../build/<pkg>-<hash>/out` has to be `.../build`.
    // Checking that first is what keeps aliases from being staged into a
    // directory no loader searches: an unrecognised layout yields `None`, and the
    // caller announces the skip.  `cargo miri` is a real example -- it nests an
    // extra component under `build`, so the check genuinely fires -- and an
    // ordinary `--target` build is not, because it only adds a component *above*
    // the profile directory.
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
