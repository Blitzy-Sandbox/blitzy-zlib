// The staged drop-in only exists when the C ABI facade and the gz layer are both compiled --
// `Makefile.in`'s `rust' target refuses any other feature list -- so with either feature off there
// is no chain to verify and the whole binary compiles away rather than reporting a false failure.
// This mirrors `symbol_parity.rs`, which gates itself on the same pair for the same reason.
#![cfg(all(feature = "libz-compat", feature = "gz"))]
// The workspace denies the panic-prone family in `[workspace.lints.clippy]`, which is right for
// `src/**` -- a panic there would abort a C caller's process -- and wrong for a test, whose
// assertions panic by design. `clippy.toml`'s `allow-unwrap-in-tests` and friends key on `#[test]`
// context only, so the file-scope helpers below still need this. Nothing else is relaxed; in
// particular this file contains no `unsafe`.
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used
)]

//! The **drop-in chain** gate: the shared-library symlink topology `make rust` staged, verified
//! exactly as the producer staged it, and proven able to fail.
//!
//! # Why a chain rather than a file
//!
//! A drop-in replacement for `libz` is not a file, it is a chain. The staged library declares
//! `SONAME libz.so.1`, so at load time the dynamic loader searches for a file literally called
//! `libz.so.1`; if the link directory holds only `libz.so`, the loader falls through to
//! `/lib/<triple>/libz.so.1` and the consumer runs happily against the **system** zlib while `-L`
//! and `-rpath` both point at the Rust build. Every test passes and nothing under test was
//! exercised. AAP 0.3.1.2 records that being reproduced on this machine, and AAP 0.8.3 makes
//! reproducing the chain -- `libz.so.<version>` with `libz.so.1` and `libz.so` pointing at it --
//! an installation requirement rather than a convenience.
//!
//! # What this suite checks that [`symbol_parity`] does not
//!
//! `symbol_parity.rs` asserts the `SONAME` and the chain's topology from names it derives itself,
//! out of `zlib.h`. That is the right thing for a parity gate and it leaves one question open: does
//! the **producer's own record** of what it staged agree with what is on disk? `Makefile.in`'s
//! `rust` target writes `$(RUSTLIBDIR)/soname.stage` with three lines -- the real versioned file,
//! the major alias that becomes the `SONAME`, and the version-script mode -- and every later target
//! reads that descriptor rather than re-deriving the names. This suite does the same, so a producer
//! that renames its artifact cannot leave a verifier checking a name nobody staged, and a
//! descriptor that has drifted from the tree it describes is a failure here rather than a
//! disagreement discovered later by a packager.
//!
//! Four things are therefore unique to this file:
//!
//! * the descriptor is **read**, validated line by line, and used as the source of every name;
//! * the recorded major alias is cross-checked against the `SONAME` the object actually declares;
//! * the staged **archive** is required beside the shared object, because `test/infcover.c` links
//!   it (`inflate_table` is hidden in both implementations);
//! * an arbitrary directory can be verified, so a **copy** of the staged tree can be checked as a
//!   packager would receive it -- which is what proves that copying preserves the chain rather than
//!   flattening it.
//!
//! # ★ Nothing here creates, retargets or dereferences a link
//!
//! The drop-in CI job once staged its consumers' tree with `cp -L` -- which *dereferences* -- and
//! then `ln -sf`'d fresh aliases beside the copy. That is a repair, not a check: a producer that
//! staged a dangling alias, an absolute link target, a flattened regular file where a link belongs,
//! or no alias at all still yielded a green job, because the consumer never saw what the producer
//! made. Every name below is read from the descriptor, every link is asserted by its literal target
//! text **and** by its canonicalised destination, and a chain that does not hold is a failure of
//! this suite rather than something it papers over.
//!
//! # Where the tree comes from, and the arming switch
//!
//! `ZLIB_RS_DROPIN_DIR` names the directory to verify; unset, `target/dropin` is probed, which is
//! where `Makefile.in` stages it (`RUSTLIBDIR=$(RUSTTARGETDIR)/dropin`). When no staged tree exists
//! every test prints one `SKIP:` line naming the remedy instead of failing, because `cargo test`
//! alone does not stage one -- and `ZLIB_RS_REQUIRE_PACKAGED=1` turns every one of those skips into
//! a failure, which is what a CI job that *did* stage the tree sets. That is the same convention
//! `symbol_parity.rs` documents, deliberately: a job which stages an artifact and then runs a suite
//! that silently skips every assertion about it reports success while verifying nothing.

use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{self, Command};
use std::time::{SystemTime, UNIX_EPOCH};

/// The immutable public contract, pinned into this binary at compile time.
///
/// Read for one value -- `ZLIB_VERSION` -- so that the versioned name the producer staged can be
/// cross-checked against the header's own idea of the library's version. AAP 0.8.1 freezes the
/// header; nothing here writes to it.
const ZLIB_H: &str = include_str!("../../../zlib.h");

// =================================================================================================
// Reading the contract and locating the tree
// =================================================================================================

/// `ZLIB_VERSION` as `zlib.h` L44 defines it -- `1.3.2.1-motley` on this tree.
///
/// Panics rather than skipping if the header does not define it: the header is compiled into this
/// binary, so its absence is a broken contract and not a missing artifact.
fn zlib_version() -> &'static str {
    for line in ZLIB_H.lines() {
        let Some(rest) = line.trim_start().strip_prefix("#define ZLIB_VERSION") else {
            continue;
        };
        if let Some((_, after_open)) = rest.split_once('"') {
            if let Some((value, _)) = after_open.split_once('"') {
                assert!(!value.is_empty(), "zlib.h defines an empty ZLIB_VERSION");
                return value;
            }
        }
    }
    panic!("zlib.h does not define ZLIB_VERSION; the pinned public contract is unrecognisable");
}

/// `<repo>` from `<repo>/crates/libz-rs-sys`.
fn repo_root() -> PathBuf {
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

/// The environment variable that names the directory to verify.
///
/// Its reason for existing is the copy: `.github/workflows/rust.yml`'s `dropin` job stages the four
/// artifacts into a second directory with `cp -a` and verifies THAT, which is how a packager
/// receives them, and a suite that could only ever look at `target/dropin` could not make that
/// check at all.
const DROPIN_DIR_VAR: &str = "ZLIB_RS_DROPIN_DIR";

/// The staged drop-in directory, or `None` when nothing has been staged.
///
/// A value in `ZLIB_RS_DROPIN_DIR` is authoritative and is NOT silently replaced by the default
/// when it does not exist: a caller that named a directory and got the default verified instead
/// would be told a different tree is sound than the one it asked about.
fn dropin_dir() -> Option<PathBuf> {
    if let Some(named) = env::var_os(DROPIN_DIR_VAR) {
        if !named.is_empty() {
            return Some(PathBuf::from(named));
        }
    }
    [
        target_root().join("dropin"),
        repo_root().join("target").join("dropin"),
    ]
    .into_iter()
    .find(|candidate| candidate.is_dir())
}

// =================================================================================================
// Skips and the arming switch
// =================================================================================================

fn skip(reason: &str) {
    println!("SKIP: {reason}");
}

/// The switch a harness that staged the tree sets, spelled identically in `symbol_parity.rs`,
/// `Makefile.in` and `.github/workflows/rust.yml`.
const REQUIRE_PACKAGED_VAR: &str = "ZLIB_RS_REQUIRE_PACKAGED";

/// Whether the caller has declared that the staged drop-in **must** exist and be inspectable.
///
/// A value that is neither `0`, `1` nor empty is a hard error rather than a fall-through to
/// disarmed: a typo in a CI expression must not quietly turn the gate off, because the whole point
/// of the switch is that its absence is invisible.
fn packaged_required() -> bool {
    match env::var(REQUIRE_PACKAGED_VAR) {
        Err(_) => false,
        Ok(value) => match value.trim() {
            "" | "0" => false,
            "1" => true,
            other => panic!(
                "{REQUIRE_PACKAGED_VAR}={other:?} is not a recognised value: use 1 to require the \
                 drop-in staged by `make rust`, or 0 (or leave it unset) to let these gates skip \
                 when it has not been built. Anything else is refused rather than treated as 0, \
                 because a silently disarmed gate is exactly the failure this variable exists to \
                 prevent"
            ),
        },
    }
}

/// Reports that a gate could not run: a skip when disarmed, a failure when [`packaged_required`]
/// says the tree had to be there.
fn unavailable(reason: &str) {
    assert!(
        !packaged_required(),
        "{REQUIRE_PACKAGED_VAR}=1 declares that the drop-in was staged, but {reason}. The staged \
         chain is what an installation copies -- cargo's own `libz.so` carries no versioned \
         aliases at all -- so this is a failure rather than a skip: with it skipped, nothing in \
         this suite has looked at the chain"
    );
    skip(reason);
}

// =================================================================================================
// ELF inspection
// =================================================================================================

/// The ELF inspectors this suite will accept, in preference order.
///
/// Two rather than one, because `readelf` and `objdump` come from binutils and from LLVM in
/// different combinations on different hosts, and `Makefile.in`'s `RUSTELFREADERS` makes the same
/// pair configurable for exactly that reason.
const ELF_READERS: [&str; 2] = ["readelf", "objdump"];

/// Whether this host can be inspected at all.
///
/// Each condition is a legitimate skip rather than a failure, and each is turned into a failure by
/// the arming switch:
///
/// * Miri cannot spawn a process, so every inspector call would abort. `.github/workflows/rust.yml`
///   scopes Miri to `-p zlib-rs`, so this should never fire.
/// * A non-ELF host has no `SONAME` and a different library naming scheme, so the chain this suite
///   describes does not exist there. AAP 0.2.2.3 puts non-Tier-1 targets out of scope.
/// * An inspector may simply be absent from a minimal container.
fn elf_host() -> bool {
    if cfg!(miri) {
        skip("running under Miri, which cannot spawn an ELF inspector");
        return false;
    }
    if !cfg!(target_os = "linux") {
        skip("host is not ELF/Linux; the staged chain this suite describes is an ELF one");
        return false;
    }
    true
}

/// Runs a program and returns its stdout, or `None` when it could not be spawned.
///
/// A program that ran and *failed* yields `None` as well: for the inspectors below that means "this
/// one cannot answer", and the caller tries the next before concluding anything.
fn tool_stdout(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// The `DT_SONAME` a shared object records, and which inspector answered.
///
/// # ★ Why this returns an error rather than `None` when no inspector is available
///
/// The shell script this replaced wrapped its `SONAME` assertion in
/// `if command -v readelf; then ... fi`, so on a host without `readelf` the check did not run and
/// the verification still reported success. That is the one assertion whose absence is invisible:
/// the whole chain exists to satisfy the name the loader searches for, and that name is the
/// `SONAME`. So a missing inspector is an error here, and the two callers decide what to do with
/// it -- the tests turn it into a skip when disarmed and a failure when armed, which is the
/// crate's documented policy for an artifact that could not be inspected.
fn recorded_soname(library: &Path) -> Result<(String, &'static str), String> {
    let path = library.to_string_lossy().into_owned();
    let mut tried = Vec::new();
    for reader in ELF_READERS {
        let (args, marker): (&[&str], &str) = match reader {
            // `readelf -d` prints `0x...(SONAME)  Library soname: [libz.so.1]`.
            "readelf" => (&["-d", &path], "SONAME"),
            // `objdump -p` prints `  SONAME               libz.so.1`.
            _ => (&["-p", &path], "SONAME"),
        };
        let Some(text) = tool_stdout(reader, args) else {
            tried.push(reader);
            continue;
        };
        for line in text.lines() {
            if !line.contains(marker) {
                continue;
            }
            // `readelf` brackets the value; `objdump` does not.
            let value = match (line.find('['), line.rfind(']')) {
                (Some(open), Some(close)) if close > open + 1 => &line[open + 1..close],
                _ => line.split_whitespace().last().unwrap_or(""),
            };
            let value = value.trim();
            if !value.is_empty() && value != marker {
                return Ok((value.to_owned(), reader));
            }
        }
        // The inspector ran and the object declares nothing. That is a finding, not a
        // tooling gap, so it is reported instead of trying the next reader.
        return Err(format!(
            "{} declares no SONAME ({reader} read it and found none). Without one the loader \
             searches for the file name the consumer was linked against, and the versioned \
             aliases buy nothing",
            library.display()
        ));
    }
    Err(format!(
        "no ELF inspector could be run, so the SONAME of {} cannot be proven. Tried: {}. Install \
         binutils, or the LLVM equivalents. This is not skipped inside the verification because \
         the SONAME is the whole reason the chain exists",
        library.display(),
        tried.join(", ")
    ))
}

// =================================================================================================
// The producer's descriptor
// =================================================================================================

/// The three lines `Makefile.in`'s `rust` target writes into `soname.stage`.
#[derive(Debug, Clone)]
struct Descriptor {
    /// Line 1: the real versioned file, e.g. `libz.so.1.3.2.1-motley`.
    versioned: String,
    /// Line 2: the major alias, which is also the `SONAME`, e.g. `libz.so.1`.
    major: String,
    /// Line 3: the version-script mode.
    version_script: String,
}

/// A name is turned into a path in exactly one place, so that a descriptor carrying a directory
/// component cannot reach outside the staged directory.
fn is_plain_name(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains('/') && !name.contains('\\')
}

fn read_descriptor(directory: &Path) -> Result<Descriptor, String> {
    let path = directory.join("soname.stage");
    let text = fs::read_to_string(&path).map_err(|error| {
        format!(
            "{} could not be read ({error}), so the producer's own record of what it staged is \
             unavailable. Run `make rust` first; this suite deliberately does not guess the names",
            path.display()
        )
    })?;
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() < 3 {
        return Err(format!(
            "{} has {} line(s); it must have at least three -- the versioned file, the major alias \
             and the version-script mode",
            path.display(),
            lines.len()
        ));
    }
    let descriptor = Descriptor {
        versioned: lines[0].trim().to_owned(),
        major: lines[1].trim().to_owned(),
        version_script: lines[2].trim().to_owned(),
    };
    for (index, (label, value)) in [
        ("1 (the versioned file name)", &descriptor.versioned),
        ("2 (the major alias)", &descriptor.major),
        ("3 (the version-script mode)", &descriptor.version_script),
    ]
    .iter()
    .enumerate()
    {
        if value.is_empty() {
            return Err(format!(
                "{} line {label} is empty (line index {index})",
                path.display()
            ));
        }
    }
    for (label, value) in [
        ("versioned name", &descriptor.versioned),
        ("major alias", &descriptor.major),
    ] {
        if !is_plain_name(value) {
            return Err(format!(
                "the staged {label} {value:?} is not a plain file name; a descriptor carrying a \
                 path component is not something to resolve"
            ));
        }
    }

    // `required` and `ldshared` are the same fact recorded from two producers. `Makefile.in`
    // composes the shared-object link itself and writes `required` when it passes
    // `--version-script` on that command line; when the version script is ALREADY in
    // `$(LDSHARED)` -- which is exactly what `./configure` writes, so it is the state of every
    // configured tree -- it has nothing to add and records `ldshared` instead. Either way a
    // version script WAS applied, which is the only thing that matters here.
    match descriptor.version_script.as_str() {
        "required" | "ldshared" | "none" => {}
        "static" => {
            return Err(format!(
                "{} records a static-only build, so there is no shared object to verify. This gate \
                 needs the shared chain; build without ZLIB_RS_SHARED=0",
                path.display()
            ))
        }
        other => {
            return Err(format!(
                "unknown version-script mode {other:?} in {}",
                path.display()
            ))
        }
    }

    if descriptor.versioned == descriptor.major {
        return Err(format!(
            "the versioned file and the major alias are both named {:?}; the chain needs two \
             distinct names",
            descriptor.versioned
        ));
    }
    if descriptor.versioned == "libz.so" {
        return Err(
            "the versioned file is named libz.so, so there is no versioned name for the aliases to \
             point at"
                .to_owned(),
        );
    }
    Ok(descriptor)
}

// =================================================================================================
// The verification
// =================================================================================================

/// One line of the evidence block, so that a passing run says what it saw rather than only that it
/// was happy.
type Evidence = Vec<String>;

/// Asserts the chain in `directory`, optionally cross-checking the staged versioned name against
/// `ZLIB_VERSION`.
///
/// Returns the evidence on success and a diagnosis on failure. Returning rather than panicking is
/// what lets [`the_verifier_rejects_every_documented_perturbation`] call it eleven times in one
/// process and assert on the outcome each time -- a gate that cannot be shown to fail is not a
/// gate.
fn verify(directory: &Path, expect_version: Option<&str>) -> Result<Evidence, String> {
    if !directory.is_dir() {
        return Err(format!(
            "{} is not a directory; nothing was staged",
            directory.display()
        ));
    }
    let descriptor = read_descriptor(directory)?;

    // --- the real file: a regular file, NOT a link ---------------------------------------------
    let real = directory.join(&descriptor.versioned);
    let kind = real.symlink_metadata().map_err(|error| {
        format!(
            "{} is absent or unreadable ({error}), although the descriptor names it as the \
             versioned library the aliases point at",
            real.display()
        )
    })?;
    if kind.file_type().is_symlink() {
        return Err(format!(
            "{} is a symlink. It must be the real file: an installation copies it and points the \
             aliases at it, and a link here means the real object lives somewhere this tree does \
             not control",
            real.display()
        ));
    }
    if !kind.file_type().is_file() {
        return Err(format!("{} is not a regular file", real.display()));
    }
    let real_canonical = fs::canonicalize(&real)
        .map_err(|error| format!("{} could not be canonicalised ({error})", real.display()))?;

    // --- the archive --------------------------------------------------------------------------
    // `test/infcover.c` links the static archive because `inflate_table` is hidden in the shared
    // object of BOTH implementations, so a staged tree without it cannot build that driver.
    let archive = directory.join("libz.a");
    let archive_kind = archive.symlink_metadata().map_err(|error| {
        format!(
            "{} is absent or unreadable ({error}); infcover.c links the static archive because \
             inflate_table is hidden in both implementations",
            archive.display()
        )
    })?;
    if archive_kind.file_type().is_symlink() {
        return Err(format!(
            "{} is a symlink; it must be the real archive",
            archive.display()
        ));
    }
    if !archive_kind.file_type().is_file() {
        return Err(format!("{} is not a regular file", archive.display()));
    }

    // --- SONAME: what the loader will actually search for --------------------------------------
    let (soname, reader) = recorded_soname(&real)?;
    if soname != descriptor.major {
        return Err(format!(
            "{} declares SONAME {soname:?} but the descriptor names {:?} as the major alias. The \
             loader will search for {soname:?}, so the alias the producer staged is not the one \
             that gets used -- which is exactly how a consumer silently binds the system zlib",
            real.display(),
            descriptor.major
        ));
    }

    // --- the two aliases: literal target text AND canonical destination ------------------------
    //
    // The two assertions are not redundant. The text one is the stricter of the pair and catches a
    // dangling or retargeted alias first; the canonical one notices an alias that names the right
    // file yet resolves outside this tree -- a staged directory reached through a symlinked path,
    // or a target that only looks relative.
    let mut aliases: BTreeSet<&str> = BTreeSet::new();
    aliases.insert("libz.so");
    aliases.insert(descriptor.major.as_str());
    aliases.remove(descriptor.versioned.as_str());

    for alias in &aliases {
        let path = directory.join(alias);
        let Ok(metadata) = path.symlink_metadata() else {
            return Err(format!(
                "{} is absent. The loader searches for {:?} by SONAME, so without it a consumer \
                 linked against this directory falls through to the system zlib and passes while \
                 exercising nothing",
                path.display(),
                descriptor.major
            ));
        };
        if !metadata.file_type().is_symlink() {
            return Err(format!(
                "{} exists but is not a symlink. A regular file here is a DEREFERENCED copy -- the \
                 producer's chain was flattened rather than preserved, which is the defect this \
                 gate exists to catch",
                path.display()
            ));
        }
        let target = path
            .read_link()
            .map_err(|error| format!("{} cannot be read ({error})", path.display()))?;
        if target != Path::new(&descriptor.versioned) {
            return Err(format!(
                "{} points at {:?}; it must point at {:?} by that exact name. A path-bearing or \
                 absolute target does not survive being installed or copied, and pointing at \
                 anything else means the chain no longer converges on the object that was built",
                path.display(),
                target.display(),
                descriptor.versioned
            ));
        }
        let canonical = fs::canonicalize(&path).map_err(|error| {
            format!(
                "{} is a DANGLING symlink (it names {:?}, which does not resolve: {error}). The \
                 loader treats that as absent and falls through to the system zlib",
                path.display(),
                target.display()
            )
        })?;
        if canonical != real_canonical {
            return Err(format!(
                "{} canonicalises to {} rather than {}. The alias resolves outside this staged \
                 tree, so what a consumer loads is not what this tree contains",
                path.display(),
                canonical.display(),
                real_canonical.display()
            ));
        }
    }

    // --- optional cross-check against the immutable header -------------------------------------
    if let Some(version) = expect_version {
        let expected = format!("libz.so.{version}");
        if descriptor.versioned != expected {
            return Err(format!(
                "the producer staged {:?}, but ZLIB_VERSION is {version:?}, so the versioned name \
                 should be {expected:?}. Two independent derivations of the same name disagreeing \
                 means one of them is reading a stale value",
                descriptor.versioned
            ));
        }
    }

    // --- evidence -----------------------------------------------------------------------------
    let mut evidence = vec![format!("chain verified in {}", directory.display())];
    evidence.push(format!(
        "  {:<28} regular file, {} bytes",
        descriptor.versioned,
        fs::metadata(&real).map_or(0, |meta| meta.len())
    ));
    for alias in &aliases {
        let path = directory.join(alias);
        evidence.push(format!(
            "  {:<28} symlink -> {:<24} canonical {}",
            alias,
            path.read_link().unwrap_or_default().display(),
            fs::canonicalize(&path).unwrap_or_default().display()
        ));
    }
    evidence.push(format!(
        "  {:<28} regular file, {} bytes",
        "libz.a",
        fs::metadata(&archive).map_or(0, |meta| meta.len())
    ));
    evidence.push(format!(
        "  {:<28} {soname} (read with {reader})",
        "SONAME (searched for)"
    ));
    evidence.push(format!(
        "  {:<28} {}",
        "version-script mode", descriptor.version_script
    ));
    Ok(evidence)
}

// =================================================================================================
// A scratch directory that removes only what it created
// =================================================================================================

/// A directory this process created, removed when the guard drops.
///
/// # ★ Why the name is not derived from the process id alone
///
/// The shell script this replaced used `${TMPDIR:-/tmp}/dropin-chain-selftest.$$` and began by
/// `rm -rf`-ing it. A process id is reused, and a world-writable `/tmp` means another user can
/// create that exact path first -- so the sequence was "recursively delete a predictable path this
/// process may not own, then create it". Here the name carries the process id AND the nanosecond
/// clock AND a per-instance counter, and it is created with `create_dir`, which FAILS if the path
/// already exists rather than removing anything. Nothing is deleted that this process did not make.
struct Scratch {
    path: PathBuf,
}

impl Scratch {
    fn new(label: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default();
        let mut attempt = 0u32;
        loop {
            let path = env::temp_dir().join(format!(
                "zlib-rs-{label}.{}.{nanos}.{attempt}",
                process::id()
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Self { path },
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    attempt += 1;
                    assert!(
                        attempt < 64,
                        "could not find an unused scratch directory name under {}",
                        env::temp_dir().display()
                    );
                }
                Err(error) => panic!(
                    "cannot create the scratch directory {}: {error}",
                    path.display()
                ),
            }
        }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // Best effort: a failing test must not be turned into a different failure by cleanup, and
        // the path is one this process created under the temp directory.
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// Copies the five staged names into a fresh case directory **without dereferencing links**.
///
/// `fs::copy` follows symlinks, which is the very thing under test, so a link is recreated as a
/// link with the same literal target and a regular file is copied byte for byte.
fn stage_case(source: &Path, destination: &Path, descriptor: &Descriptor) {
    fs::create_dir_all(destination).expect("the case directory can be created");
    let mut names = vec![
        descriptor.versioned.clone(),
        descriptor.major.clone(),
        "libz.so".to_owned(),
        "libz.a".to_owned(),
        "soname.stage".to_owned(),
    ];
    names.dedup();
    for name in names {
        let from = source.join(&name);
        let to = destination.join(&name);
        let metadata = from
            .symlink_metadata()
            .unwrap_or_else(|error| panic!("{} cannot be inspected ({error})", from.display()));
        if metadata.file_type().is_symlink() {
            let target = from
                .read_link()
                .unwrap_or_else(|error| panic!("{} cannot be read ({error})", from.display()));
            std::os::unix::fs::symlink(&target, &to).unwrap_or_else(|error| {
                panic!("{} cannot be recreated as a link ({error})", to.display())
            });
        } else {
            fs::copy(&from, &to)
                .unwrap_or_else(|error| panic!("{} cannot be copied ({error})", to.display()));
        }
    }
}

// =================================================================================================
// The gates
// =================================================================================================

/// The staged chain holds, and the evidence says what was seen.
#[test]
fn staged_chain_verifies() {
    if !elf_host() {
        unavailable("this host cannot be inspected with an ELF reader");
        return;
    }
    let Some(directory) = dropin_dir() else {
        unavailable(&format!(
            "no staged drop-in directory; run `make rust` to stage it, or set {DROPIN_DIR_VAR}"
        ));
        return;
    };
    if !directory.join("soname.stage").is_file() {
        unavailable(&format!(
            "{} carries no soname.stage, so `make rust` has not staged this tree",
            directory.display()
        ));
        return;
    }
    match verify(&directory, None) {
        Ok(evidence) => {
            for line in evidence {
                println!("{line}");
            }
        }
        Err(diagnosis) => panic!("{diagnosis}"),
    }
}

/// The producer's descriptor and the immutable header derive the same versioned name.
///
/// Two independent derivations: `Makefile.in`'s `rust` target reads `ZLIB_VERSION` out of `zlib.h`
/// with `sed` at build time and writes the result into the descriptor, while this test reads the
/// same macro out of the header compiled into this binary. They can only disagree if one of them is
/// stale, and a stale library name is not a visible failure -- a consumer simply resolves a
/// different file.
#[test]
fn the_descriptor_agrees_with_zlib_h() {
    if !elf_host() {
        unavailable("this host cannot be inspected with an ELF reader");
        return;
    }
    let Some(directory) = dropin_dir() else {
        unavailable(&format!(
            "no staged drop-in directory; run `make rust` to stage it, or set {DROPIN_DIR_VAR}"
        ));
        return;
    };
    if !directory.join("soname.stage").is_file() {
        unavailable(&format!(
            "{} carries no soname.stage, so `make rust` has not staged this tree",
            directory.display()
        ));
        return;
    }
    let version = zlib_version();
    match verify(&directory, Some(version)) {
        Ok(_) => println!(
            "the descriptor's versioned name agrees with ZLIB_VERSION {version} from zlib.h"
        ),
        Err(diagnosis) => panic!("{diagnosis}"),
    }
}

/// One case's mutation: whatever it does to a freshly staged copy before the verifier sees it.
type Mutation = Box<dyn Fn(&Path)>;

/// ★ The verifier rejects every perturbation it documents.
///
/// Every case starts from a link-preserving copy of the REAL staged tree and differs from it by
/// exactly one mutation, so a case that fails does so for the reason it names and the `SONAME`
/// assertion runs against a genuine ELF object rather than a fixture. Case 0 is the control, and it
/// doubles as the proof that copying the tree preserves the chain -- which is what the drop-in CI
/// job relies on when it stages the tree for its consumers.
#[test]
fn the_verifier_rejects_every_documented_perturbation() {
    if !elf_host() {
        unavailable("this host cannot be inspected with an ELF reader");
        return;
    }
    let Some(source) = dropin_dir() else {
        unavailable(&format!(
            "no staged drop-in directory; run `make rust` to stage it, or set {DROPIN_DIR_VAR}"
        ));
        return;
    };
    if !source.join("soname.stage").is_file() {
        unavailable(&format!(
            "{} carries no soname.stage, so `make rust` has not staged this tree",
            source.display()
        ));
        return;
    }

    // The cases below are the good tree MINUS one thing each, so the good tree has to be good
    // first. Asserting that here rather than relying on test ordering means a broken staged tree is
    // reported as a broken staged tree, with the verifier's own diagnosis.
    let descriptor = match read_descriptor(&source) {
        Ok(descriptor) => descriptor,
        Err(diagnosis) => {
            panic!("the tree the self-test would mutate has no usable descriptor: {diagnosis}")
        }
    };
    if let Err(diagnosis) = verify(&source, None) {
        panic!(
            "the tree the self-test would mutate does not itself verify: {diagnosis}\nFix the \
             staged chain first; this test mutates a KNOWN-GOOD tree and has nothing to say about \
             a broken one"
        );
    }

    let scratch = Scratch::new("dropin-chain");
    let versioned = descriptor.versioned.clone();
    let major = descriptor.major.clone();

    // Each case is (name, expect_pass, mutation). The mutation runs against a freshly staged copy.
    let cases: Vec<(&str, bool, Mutation)> = vec![
        (
            "a link-preserving copy of the staged tree",
            true,
            Box::new(|_: &Path| {}),
        ),
        ("the major alias removed", false, {
            let major = major.clone();
            Box::new(move |case: &Path| {
                fs::remove_file(case.join(&major)).expect("the alias can be removed");
            })
        }),
        ("libz.so flattened into a regular file", false, {
            let versioned = versioned.clone();
            Box::new(move |case: &Path| {
                let path = case.join("libz.so");
                fs::remove_file(&path).expect("the alias can be removed");
                fs::copy(case.join(&versioned), &path).expect("the object can be copied over it");
            })
        }),
        ("the major alias retargeted to an absolute path", false, {
            let (major, versioned) = (major.clone(), versioned.clone());
            Box::new(move |case: &Path| {
                let path = case.join(&major);
                fs::remove_file(&path).expect("the alias can be removed");
                std::os::unix::fs::symlink(case.join(&versioned), &path)
                    .expect("an absolute link can be made");
            })
        }),
        ("the major alias left dangling", false, {
            let major = major.clone();
            Box::new(move |case: &Path| {
                let path = case.join(&major);
                fs::remove_file(&path).expect("the alias can be removed");
                std::os::unix::fs::symlink("libz.so.99.99.99", &path)
                    .expect("a dangling link can be made");
            })
        }),
        ("the versioned file removed, aliases kept", false, {
            let versioned = versioned.clone();
            Box::new(move |case: &Path| {
                fs::remove_file(case.join(&versioned)).expect("the object can be removed");
            })
        }),
        ("the versioned file replaced by a link elsewhere", false, {
            let versioned = versioned.clone();
            Box::new(move |case: &Path| {
                let path = case.join(&versioned);
                fs::remove_file(&path).expect("the object can be removed");
                std::os::unix::fs::symlink("/lib/x86_64-linux-gnu/libz.so.1", &path)
                    .expect("a link can be made");
            })
        }),
        (
            "the static archive removed",
            false,
            Box::new(|case: &Path| {
                fs::remove_file(case.join("libz.a")).expect("the archive can be removed");
            }),
        ),
        ("the descriptor truncated to two lines", false, {
            let (versioned, major) = (versioned.clone(), major.clone());
            Box::new(move |case: &Path| {
                fs::write(case.join("soname.stage"), format!("{versioned}\n{major}\n"))
                    .expect("the descriptor can be rewritten");
            })
        }),
        (
            "the descriptor naming an alias the SONAME contradicts",
            false,
            {
                let versioned = versioned.clone();
                Box::new(move |case: &Path| {
                    fs::write(
                        case.join("soname.stage"),
                        format!("{versioned}\nlibz.so.99\nrequired\n"),
                    )
                    .expect("the descriptor can be rewritten");
                })
            },
        ),
        (
            "the descriptor naming a path rather than a file name",
            false,
            {
                let (versioned, major) = (versioned.clone(), major.clone());
                Box::new(move |case: &Path| {
                    fs::write(
                        case.join("soname.stage"),
                        format!("../{versioned}\n{major}\nrequired\n"),
                    )
                    .expect("the descriptor can be rewritten");
                })
            },
        ),
        ("the descriptor recording a static-only build", false, {
            let (versioned, major) = (versioned.clone(), major.clone());
            Box::new(move |case: &Path| {
                fs::write(
                    case.join("soname.stage"),
                    format!("{versioned}\n{major}\nstatic\n"),
                )
                .expect("the descriptor can be rewritten");
            })
        }),
    ];

    let mut wrong = Vec::new();
    for (index, (name, expect_pass, mutate)) in cases.iter().enumerate() {
        let case = scratch.path().join(format!("case{index}"));
        stage_case(&source, &case, &descriptor);
        mutate(&case);
        let outcome = verify(&case, None);
        let passed = outcome.is_ok();
        if passed == *expect_pass {
            println!(
                "  ok       {name:<48} ({})",
                if passed { "pass" } else { "fail" }
            );
            if let Err(diagnosis) = outcome {
                println!("           | {diagnosis}");
            }
        } else {
            wrong.push(*name);
            println!(
                "  NOT OK   {name:<48} (wanted {}, got {})",
                if *expect_pass { "pass" } else { "fail" },
                if passed { "pass" } else { "fail" }
            );
            if let Err(diagnosis) = outcome {
                println!("           | {diagnosis}");
            }
        }
        // Removed as each case finishes so that the scratch directory holds one case at a time
        // rather than a copy of the library per case.
        let _ = fs::remove_dir_all(&case);
    }

    assert!(
        wrong.is_empty(),
        "the chain verifier did not behave as documented; {} case(s) went the wrong way: {}. Fix \
         the verifier -- a gate that cannot fail is not a gate",
        wrong.len(),
        wrong.join(", ")
    );
    println!(
        "self-test PASS -- {} cases, one control and {} perturbations",
        cases.len(),
        cases.len() - 1
    );
}
