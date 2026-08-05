// ============================================================================
//  crates/libz-rs-sys/build.rs -- the linker and packaging build script
// ============================================================================
//
// This script has exactly four jobs and deliberately does nothing else.  Every
// directive it emits traces back to `configure`, to `Makefile.in`, or to a
// requirement of the port:
//
//   1. ZLIB_RS_SIMD -- the documented build-time toggle for the vectorised
//      checksum backends, surfaced to the crate as `cfg(zlib_rs_simd)` and
//      always paired with the `rustc-check-cfg` declaration that keeps the
//      build warning-free on Rust 1.80 and later.
//
//   2. SONAME -- `-Wl,-soname,libz.so.1` on ELF targets, byte for byte the
//      argument `configure` bakes into LDSHARED (configure L334 for
//      Linux/GNU/Solaris/Haiku, L336 for the BSDs).  This is what makes an
//      already-linked binary that records `DT_NEEDED libz.so.1` bind to this
//      library without being recompiled.
//
//   3. zlib.map -- the immutable version script.  It is located, validated and
//      declared as a rebuild trigger here.  See the long comment above
//      `emit_version_script` for the measured reason it cannot be handed to
//      rustc's own cdylib link, and for the verified place it is applied
//      instead.
//
//   4. The versioned symlink chain next to the cdylib, so the dynamic loader
//      binds this artifact rather than quietly falling back to the system
//      libz.  See the comment block above `install_soname_aliases`: this is
//      the single most consequential thing this file does.
//
// Hard rules this file lives by:
//
//   * It never writes to `zlib.h`, `zconf.h` or `zlib.map`.  Those three are
//     zlib's immutable public contract; this script only ever reads them, and
//     it fails the build loudly rather than silently proceeding without them.
//
//   * It never compiles C.  This crate has no `[build-dependencies]` and must
//     not acquire `cc`: `cargo build --release` for the shipped artifacts has
//     to work on a machine with no C compiler at all.  Compiling the C
//     reference implementation is exclusively the differential crate's job.
//
//   * It never touches the network.
//
//   * It writes nothing outside cargo's own artifact directory, which the root
//     `.gitignore` already covers (`/target/` plus the pre-existing
//     `**/libz.so*` rule), so the working tree stays clean.
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
use std::ffi::OsStr;
use std::fs;
use std::io;
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

// The unversioned shared-library names.  ELF form from Makefile.in L47
// (`SHAREDLIB=libz.so`); Mach-O form from configure L364
// (`SHAREDLIB=libz$shared_ext` with `shared_ext='.dylib'`).
const ELF_SHARED_LIB: &str = "libz.so";
const MACHO_SHARED_LIB: &str = "libz.dylib";

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

// Opt-in for handing `zlib.map` to rustc's cdylib link.  Off by default for
// the measured reasons documented above `emit_version_script`.
const ENV_VERSION_SCRIPT: &str = "ZLIB_RS_VERSION_SCRIPT";

// Opt-out for the versioned symlink chain.  On by default; see the comment
// block above `install_soname_aliases` for the one situation that wants it off.
const ENV_SONAME_LINKS: &str = "ZLIB_RS_SONAME_LINKS";

// The cfg that `ZLIB_RS_SIMD=1` turns on.
const CFG_SIMD: &str = "zlib_rs_simd";

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

    let version = zlib_version(&public_header);
    let major = major_version(&version, &public_header).to_owned();
    let target = TargetInfo::from_env();

    configure_simd();
    emit_link_args(&target, &version_script, &major);
    install_soname_aliases(&target, &version, &major);
}

// ---------------------------------------------------------------------------
//  1. The ZLIB_RS_SIMD build-time toggle
// ---------------------------------------------------------------------------
//
// `ZLIB_RS_SIMD` selects the vectorised CRC-32 and Adler-32 backends.  Two
// properties of that choice are worth stating where the code lives, because
// both constrain what this toggle is allowed to do:
//
//   * It is OUTPUT-NEUTRAL.  A checksum yields one scalar however it is
//     computed, so vectorising it cannot perturb a single emitted byte -- it
//     may change speed only.  That is precisely why SIMD is confined to the
//     two checksums: vectorising match finding would change the compressed
//     output and would break the byte-identical-output requirement outright.
//
//   * It is not a promise about the hardware.  Runtime target-feature
//     detection still guards every vectorised path, so a SIMD-enabled build
//     runs correctly on a machine that lacks the instructions; it simply takes
//     the scalar path there.
//
// A build script cannot switch a Cargo feature on, so this knob does what a
// build script legitimately can: it publishes the request as a `cfg`, which
// the crate root can key off.  The Cargo feature `simd` remains the way to
// actually pull the vectorised backends in from the core crate.
fn configure_simd() {
    // Without this, cargo caches the previous decision and flipping the
    // variable appears to do nothing at all.
    println!("cargo::rerun-if-env-changed={ENV_SIMD}");

    // The cfg is declared unconditionally, not only when it is set.  Rust 1.80
    // turned `unexpected_cfgs` on by default, so a `#[cfg(zlib_rs_simd)]` in
    // src/ that this script never declares is a warning -- and the quality bar
    // for this port is zero build warnings.  Declaring it always also means
    // the declaration does not appear and disappear with the environment,
    // which would make the warning intermittent and hard to attribute.
    println!("cargo::rustc-check-cfg=cfg({CFG_SIMD})");

    let requested = match env::var(ENV_SIMD) {
        Ok(value) => value,
        // Unset is the normal case and means "leave the decision alone": the
        // crate's own `simd` Cargo feature governs, and nothing is emitted.
        Err(env::VarError::NotPresent) => return,
        // A non-Unicode value cannot be a valid `0` or `1`, so it is handled
        // together with the other malformed values below.
        Err(env::VarError::NotUnicode(value)) => panic!(
            "{ENV_SIMD} is set to a non-Unicode value ({value:?}). Set it to `0` or `1`, \
             or unset it to leave the decision to this crate's `simd` Cargo feature."
        ),
    };

    match requested.trim() {
        "1" => println!("cargo::rustc-cfg={CFG_SIMD}"),
        "0" => {
            // Nothing to emit -- but say so if the request contradicts the
            // resolved feature set, because a build script cannot turn a
            // Cargo feature off and silently ignoring the conflict would be
            // the confusing outcome.  This warning fires only when a caller
            // has actually created the contradiction, so the ordinary build
            // stays warning-free.
            if env::var_os("CARGO_FEATURE_SIMD").is_some() {
                println!(
                    "cargo::warning={ENV_SIMD}=0 cannot switch off the `simd` Cargo feature, \
                     which is enabled for this build. Rebuild without `--features simd` \
                     (or with `--no-default-features`) to get the scalar checksum backends."
                );
            }
        }
        other => panic!(
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
    // but Windows is outside the port's Tier-1 target set, so nothing is
    // emitted here.  Should a Windows target ever be brought in scope,
    // `win32/zlib.def` is the file to wire up, as `/DEF:` on MSVC.
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

    // The chain of versioned names only makes sense where a shared library is
    // actually produced under a `libz.*` name.
    fn is_elf(&self) -> bool {
        matches!(
            self.format,
            SharedObjectFormat::Elf | SharedObjectFormat::ElfDashH
        )
    }
}

// ---------------------------------------------------------------------------
//  3. SONAME and version script
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
fn emit_link_args(target: &TargetInfo, version_script: &Path, major: &str) {
    match target.format {
        SharedObjectFormat::Elf => {
            // configure L334/L336: `-Wl,-soname,libz.so.1`.  The name is built
            // from the major component of ZLIB_VERSION, which is how
            // configure L482 derives SHAREDLIBM (`libz$shared_ext.$VER1`),
            // rather than being hardcoded here.  At 1.3.2.1-motley the two
            // spellings agree, and deriving it means they cannot drift apart.
            cdylib_link_arg(&format!("-Wl,-soname,{ELF_SHARED_LIB}.{major}"));
            emit_version_script(target, version_script);
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
            // deliberately no version script on this branch: Mach-O has no such
            // concept, and passing one would be a link error.
            cdylib_link_arg(&format!("-Wl,-install_name,libz.{major}.dylib"));
        }
        // Nothing either construct maps onto.  See the enum comments.
        SharedObjectFormat::Pe | SharedObjectFormat::Other => {}
    }
}

// Why the version script is opt-in rather than unconditional.
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
// forces a relink.  The `ZLIB_RS_VERSION_SCRIPT=1` opt-in remains for a
// toolchain on which rustc does not supply a version script of its own; on
// such a toolchain the pass-through is correct, and the argument is emitted
// with the `--undefined-version` relaxation that the zlib.map `local:` entries
// require.
fn emit_version_script(target: &TargetInfo, version_script: &Path) {
    println!("cargo::rerun-if-env-changed={ENV_VERSION_SCRIPT}");

    if !parse_bool_env(ENV_VERSION_SCRIPT, false) {
        return;
    }

    // Order matters: this has to come after rustc's own
    // `-Wl,--no-undefined-version`, and cargo appends build-script link
    // arguments after rustc's, so emitting it here is sufficient.  It is
    // required because zlib.map lists `local:` symbols that this port
    // deliberately never exports.
    cdylib_link_arg("-Wl,--undefined-version");
    cdylib_link_arg(&format!(
        "-Wl,--version-script={}",
        version_script.display()
    ));

    // Fires only when a caller has explicitly opted in, so the ordinary build
    // stays warning-free.  It is worth saying out loud, because on the default
    // toolchain the opt-in produces a library whose symbol versions were
    // silently dropped.
    println!(
        "cargo::warning={ENV_VERSION_SCRIPT}=1: passing {} to the cdylib link on {}. \
         On a toolchain where rustc supplies its own version script this is ignored with \
         linker warnings; verify with `nm -D --defined-only --extern-only` that the \
         expected symbol-version nodes are present.",
        version_script.display(),
        target.describe()
    );
}

// Emits one linker argument, scoped to the cdylib.
fn cdylib_link_arg(arg: &str) {
    println!("cargo::rustc-cdylib-link-arg={arg}");
}

// ---------------------------------------------------------------------------
//  4. The versioned symlink chain
// ---------------------------------------------------------------------------
//
// READ THIS BEFORE CHANGING ANYTHING BELOW.  The direction of the links is
// inverted relative to the C build, and the inversion is deliberate.
//
//   * In the C build the real file is `libz.so.1.3.2.1-motley`, and both
//     `libz.so` and `libz.so.1` are symlinks pointing at it.  That is the
//     `$(SHAREDLIBV)` recipe in Makefile.in: build the versioned file, then
//     `rm -f $(SHAREDLIB) $(SHAREDLIBM)` and `ln -s $@` twice.
//
//   * Cargo emits the real file as a bare `libz.so`.  So this script creates
//     `libz.so.1` and `libz.so.1.3.2.1-motley` as symlinks pointing *at*
//     `libz.so`.  The net effect is identical -- all three names resolve to the
//     same inode -- and that is the only thing the dynamic loader cares about.
//
// Why the chain is not optional, demonstrated rather than assumed.  The
// library's SONAME is `libz.so.1`, so a consumer asks the loader for that exact
// name.  Build the shared library, link a C probe against it with an rpath, and
// leave `libz.so.1` uncreated: `ldd` resolves the dependency to
// `/lib/x86_64-linux-gnu/libz.so.1` and the probe prints the *system* zlib's
// version.  Create the link and `ldd` resolves to the local file and the probe
// prints `1.3.2.1-motley`.  In other words, without this chain every "drop-in
// replacement" test passes while silently exercising the C library -- the worst
// possible failure mode, because it is invisible.  This was reproduced
// directly, and it is why every drop-in validation must independently assert
// via `ldd` which artifact actually got bound instead of trusting `-L`.
//
// A dangling link is deliberately fine.  A build script runs *before* the crate
// is linked, so `libz.so` does not exist yet at this point.
// `std::os::unix::fs::symlink` succeeds against a non-existent target and the
// link starts resolving the moment the real file appears.  The link target is
// kept RELATIVE (`libz.so`, never an absolute path) so the whole chain survives
// being copied or installed elsewhere.  Verified: while the link dangles the
// loader simply skips it and falls back to the system library, so an
// interrupted build leaves nothing harmful behind.
//
// The one cost, and the reason for the opt-out.  `cargo test` puts the artifact
// directory first on LD_LIBRARY_PATH (measured: `target/<profile>` then
// `target/<profile>/deps`).  `nm`, `readelf` and `ld.bfd` all carry
// `DT_NEEDED libz.so.1` with no RPATH of their own, so a `libz.so.1` sitting
// there is bound by them too -- confirmed with `ld.so --list /usr/bin/nm`.
// Once the facade is complete that is harmless, and is in fact the strongest
// drop-in proof available; against a partially built library it shows up as
// `nm: .../libz.so.1: no version information available`.  `ZLIB_RS_SONAME_LINKS=0`
// exists for exactly that situation and for nothing else.  Note that the
// `Makefile.in` `rust` target additionally stages the same three names into a
// separate directory of its own; that is a complementary mechanism, not a
// substitute, because a plain `cargo build` never runs it.
fn install_soname_aliases(target: &TargetInfo, version: &str, major: &str) {
    println!("cargo::rerun-if-env-changed={ENV_SONAME_LINKS}");

    if !parse_bool_env(ENV_SONAME_LINKS, true) {
        return;
    }

    // On an ELF target the chain is load-bearing, so any failure is loud.
    // Elsewhere it is a convenience and a failure is reported and tolerated.
    let required = target.is_elf();

    let (real_name, aliases) = match target.format {
        SharedObjectFormat::Elf | SharedObjectFormat::ElfDashH => {
            (ELF_SHARED_LIB, elf_aliases(version, major))
        }
        SharedObjectFormat::MachO => (MACHO_SHARED_LIB, macho_aliases(version, major)),
        // No `libz.so`/`libz.dylib` is produced for these, so there is nothing
        // to alias and a chain would only be misleading.
        SharedObjectFormat::Pe | SharedObjectFormat::Other => return,
    };

    // A build script is never told which crate types are being built, so the
    // manifest is the only available source.  The focused parser below reads
    // only `[lib].crate-type`, including multiline arrays and comments; that is
    // enough to avoid false positives without adding a TOML build dependency.
    // If the array omits `cdylib`, there is nothing to alias.
    if !cdylib_declared() {
        return;
    }

    let Some(artifact_dir) = cargo_artifact_dir() else {
        report(
            required,
            &format!(
                "could not locate cargo's artifact directory from OUT_DIR={}, so the \
                 {real_name} version aliases were not created. Existing binaries that \
                 record `DT_NEEDED {ELF_SHARED_LIB}.{major}` will bind the system zlib \
                 instead of this library. Set {ENV_SONAME_LINKS}=0 to silence this and \
                 stage the aliases from the build system instead.",
                env::var("OUT_DIR").unwrap_or_default()
            ),
        );
        return;
    };

    // Normally cargo has already created this; creating it is idempotent and
    // keeps the script working when the aliases are wanted before the first
    // artifact has ever been written there.
    if let Err(error) = fs::create_dir_all(&artifact_dir) {
        report(
            required,
            &format!(
                "could not create the artifact directory {}: {error}",
                artifact_dir.display()
            ),
        );
        return;
    }

    for alias in &aliases {
        if let Err(error) = link_alias(&artifact_dir, alias, real_name) {
            report(
                required,
                &format!(
                    "could not create the version alias {} -> {real_name} in {}: {error}. \
                     Without it the dynamic loader falls back to the system zlib and \
                     drop-in tests silently exercise the wrong library. Set \
                     {ENV_SONAME_LINKS}=0 if this platform cannot support symlinks.",
                    alias,
                    artifact_dir.display()
                ),
            );
            return;
        }
    }
}

// The ELF alias set, most significant first.
//
//   libz.so.1                 -- SHAREDLIBM, and the SONAME this library
//                                records.  This is the one the loader looks
//                                for; the rest are for humans and installers.
//   libz.so.1.3.2.1-motley    -- SHAREDLIBV as `configure` L481 computes it,
//                                `libz$shared_ext.$VER`, with VER scraped from
//                                ZLIB_VERSION.  This is the form that actually
//                                ships.
//   libz.so.1.3.2.1           -- the same name without the release suffix.
//                                Makefile.in L48 hardcodes `SHAREDLIBV=
//                                libz.so.1.3.2.1` and CMakeLists.txt L6 sets
//                                `VERSION 1.3.2.1`, so both of those disagree
//                                with what configure derives.  configure is
//                                what ships, so the suffixed name is the real
//                                one; this extra alias is created so that a
//                                consumer following either of the other two
//                                spellings still resolves.  It is skipped when
//                                ZLIB_VERSION carries no suffix and the two
//                                names would collide.
fn elf_aliases(version: &str, major: &str) -> Vec<String> {
    let mut aliases = vec![
        format!("{ELF_SHARED_LIB}.{major}"),
        format!("{ELF_SHARED_LIB}.{version}"),
    ];
    if let Some(numeric) = numeric_version(version) {
        aliases.push(format!("{ELF_SHARED_LIB}.{numeric}"));
    }
    aliases
}

// The Mach-O alias set.  configure L364-L366: SHAREDLIB is `libz.dylib`,
// SHAREDLIBM is `libz.$VER1.dylib` and SHAREDLIBV is `libz.$VER.dylib` -- the
// version goes before the extension on this platform, not after it.
fn macho_aliases(version: &str, major: &str) -> Vec<String> {
    let mut aliases = vec![
        format!("libz.{major}.dylib"),
        format!("libz.{version}.dylib"),
    ];
    if let Some(numeric) = numeric_version(version) {
        aliases.push(format!("libz.{numeric}.dylib"));
    }
    aliases
}

// `1.3.2.1-motley` -> `Some("1.3.2.1")`; `1.3.2.1` -> `None`, because the
// unsuffixed alias would then be the versioned name itself.
fn numeric_version(version: &str) -> Option<&str> {
    let (numeric, _suffix) = version.split_once('-')?;
    if numeric.is_empty() || numeric == version {
        None
    } else {
        Some(numeric)
    }
}

// Creates one alias, idempotently.
fn link_alias(dir: &Path, alias: &str, original: &str) -> io::Result<()> {
    let link = dir.join(alias);
    remove_existing(&link)?;

    match create_symlink(original, &link) {
        Ok(()) => Ok(()),
        // Two cargo jobs can race here -- a workspace build and a
        // `cargo test`, for instance, both running this script for different
        // crate types.  Whoever lost the race still ends up with the link that
        // was wanted, so this is a success, not a failure.
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error),
    }
}

// Clears whatever currently occupies a link path.  `symlink_metadata` does not
// follow links, so a dangling alias left by an earlier build is still seen here
// and removed, which is what makes the whole step idempotent.  A real directory
// in the way is removed non-recursively on purpose: if something has put a
// populated directory where `libz.so.1` belongs, failing loudly is far better
// than deleting it.
fn remove_existing(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.is_dir() {
                fs::remove_dir(path)
            } else {
                fs::remove_file(path)
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(unix)]
fn create_symlink(original: &str, link: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(original, link)
}

// Cross-compiling to a Unix target from a host without symlinks.  The error is
// surfaced through `report`, which turns it into a warning on the non-required
// platforms and a hard failure where the chain is load-bearing.
#[cfg(not(unix))]
fn create_symlink(_original: &str, _link: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "creating a symlink requires a Unix host",
    ))
}

// Fails the build where the chain is load-bearing, reports and continues where
// it is a convenience.
fn report(required: bool, message: &str) {
    assert!(!required, "{message}");
    println!("cargo::warning={message}");
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

// Cargo's artifact directory -- `target/<profile>`, or
// `target/<triple>/<profile>` when `--target` is in play -- which is where
// cargo writes the cdylib and where the install rules and CI steps look for it.
//
// It is derived from OUT_DIR, whose layout is
// `<artifact dir>/build/<pkg>-<hash>/out`.  Rather than assume a fixed depth,
// which differs between host and `--target` builds and would silently produce a
// wrong path if cargo ever changed it, this walks up to the first ancestor
// actually named `build` and takes that directory's parent.  If no such
// ancestor exists the derivation fails and says so, which is the whole point of
// verifying instead of assuming.
//
// CARGO_TARGET_DIR plus PROFILE was considered and rejected: PROFILE only ever
// reports `debug` or `release`, so it names the wrong directory for any custom
// profile, and CARGO_TARGET_DIR is frequently unset.  OUT_DIR is always set for
// a build script and always absolute.
//
// OUT_DIR itself is deliberately not used as the destination.  It is a
// per-crate scratch directory that no loader ever searches and that nothing
// downstream inspects.
fn cargo_artifact_dir() -> Option<PathBuf> {
    let out_dir = PathBuf::from(require_env("OUT_DIR"));
    let mut cursor: &Path = out_dir.as_path();

    while let Some(parent) = cursor.parent() {
        if parent.file_name() == Some(OsStr::new("build")) {
            return parent.parent().map(Path::to_path_buf);
        }
        cursor = parent;
    }

    None
}

// Whether this crate's manifest asks for a cdylib at all.
fn cdylib_declared() -> bool {
    let manifest = PathBuf::from(require_env("CARGO_MANIFEST_DIR")).join("Cargo.toml");

    match fs::read_to_string(&manifest) {
        Ok(text) => manifest_declares_cdylib(&text),
        // An unreadable manifest is not this script's problem to diagnose --
        // cargo could not have got this far without parsing it -- so assume the
        // normal case rather than skipping a step the build needs.
        Err(_) => true,
    }
}

// Finds `cdylib` specifically in the `[lib]` table's `crate-type` array.
//
// A whole-file substring search is not sufficient: package names, comments and
// metadata are all allowed to contain the word `cdylib` without asking cargo to
// produce one.  This deliberately small parser handles the one TOML construct
// the build script needs, including a multiline array and comments, while
// preserving the crate's zero-build-dependency contract.
fn manifest_declares_cdylib(manifest: &str) -> bool {
    let mut in_lib_table = false;
    let mut collecting_crate_types = false;
    let mut crate_types = String::new();

    for raw_line in manifest.lines() {
        let line = toml_line_without_comment(raw_line).trim();
        if line.is_empty() {
            continue;
        }

        if line.starts_with('[') {
            if collecting_crate_types {
                break;
            }
            in_lib_table = line == "[lib]";
            continue;
        }

        if !in_lib_table {
            continue;
        }

        if collecting_crate_types {
            crate_types.push(' ');
            crate_types.push_str(line);
        } else {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            if key.trim() != "crate-type" {
                continue;
            }
            collecting_crate_types = true;
            crate_types.push_str(value.trim());
        }

        if crate_types.contains(']') {
            break;
        }
    }

    crate_types
        .split(['[', ']', ','])
        .map(str::trim)
        .any(|crate_type| matches!(crate_type, "\"cdylib\"" | "'cdylib'"))
}

// Removes a TOML comment without mistaking a `#` inside a quoted string for
// the start of one.  Escapes matter only in basic (`"..."`) strings; literal
// (`'...'`) strings take every character verbatim.
fn toml_line_without_comment(line: &str) -> &str {
    let mut in_basic_string = false;
    let mut in_literal_string = false;
    let mut escaped = false;

    for (index, character) in line.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }

        match character {
            '\\' if in_basic_string => escaped = true,
            '"' if !in_literal_string => in_basic_string = !in_basic_string,
            '\'' if !in_basic_string => in_literal_string = !in_literal_string,
            '#' if !in_basic_string && !in_literal_string => {
                return line.get(..index).unwrap_or(line);
            }
            _ => {}
        }
    }

    line
}

// Reads one of this script's `0`/`1` knobs.  Unset means the documented
// default; anything other than `0` or `1` is a malformed build knob, and
// failing the build is the right answer for that.
fn parse_bool_env(key: &str, default: bool) -> bool {
    match env::var(key) {
        Ok(value) => match value.trim() {
            "1" => true,
            "0" => false,
            other => {
                panic!("{key} must be `0` or `1`, got {other:?}. Unset it to use the default.")
            }
        },
        Err(env::VarError::NotPresent) => default,
        Err(env::VarError::NotUnicode(value)) => panic!(
            "{key} is set to a non-Unicode value ({value:?}). Set it to `0` or `1`, or unset \
             it to use the default."
        ),
    }
}

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
