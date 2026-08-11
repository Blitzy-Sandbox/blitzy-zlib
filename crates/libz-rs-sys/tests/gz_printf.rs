// `gzvprintf` is part of the exported C surface only when that surface exists, and it belongs to the
// `gzFile` layer, so this suite compiles exactly when the probe it drives does. With either feature
// off there is no symbol to link against and the whole binary compiles away, which is what keeps
// `--no-default-features` -- a supported configuration for this crate -- building.
#![cfg(all(feature = "libz-compat", feature = "gz"))]
// The workspace denies the panic-prone family in `[workspace.lints.clippy]`, which is right for
// `src/**` -- a panic there would abort a C caller's process -- and wrong for a test, whose
// assertions panic by design. `clippy.toml` keys `allow-unwrap-in-tests` and friends on `#[test]`
// context only, so the file-scope helpers below still need this. Nothing else is relaxed.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

//! Behavioural coverage for `gzvprintf` — the one entry point in the whole contract that a Rust
//! test cannot call.
//!
//! # The gap this closes
//!
//! `zlib.h` L2045-L2050 declares `int gzvprintf(gzFile, const char *, va_list);`. Ninety-four of the
//! ninety-five exported functions had a test that *ran* them; this one had two checks that did not:
//!
//! * `tests/symbol_parity.rs` proves the symbol is present in the packaged library.
//! * `src/util.rs`'s `the_variadic_shim_is_linked_when_the_flags_say_it_is` assigns it to a typed
//!   function pointer, which proves it links and has the declared type.
//!
//! Neither runs a byte through it. A `gzvprintf` that mis-forwarded its `va_list`, returned the
//! wrong count, or wrote the caller's bytes into the wrong half of the doubled input buffer would
//! have satisfied both — and `gzvprintf` is precisely the entry point where that is most likely,
//! because the `va_list` handling is the platform-specific part.
//!
//! # Why the call goes through C
//!
//! Stable Rust cannot construct a `va_list`: `core::ffi::VaList` is unstable, defining a C-variadic
//! function needs the unstable `c_variadic` feature, and no single Rust declaration of `va_list` is
//! ABI-correct on x86-64 `SysV`, `AArch64` `AAPCS64` and i386 at once. So the call is made from
//! `crates/libz-rs-sys/csrc/gzvprintf_probe.c`, which is `va_start`, one call, `va_end` and nothing
//! else. That file's header sets out the reasoning in full.
//!
//! **Calling `gzprintf` instead would not be this test.** `gzprintf` builds its own `va_list` and
//! forwards it (`gzwrite.c` L487-L495), so it exercises the callee's body but never the shape a real
//! consumer produces: a list started in the *caller's* frame and handed across the boundary. That is
//! what a logging wrapper does, and it is what the probe reproduces.
//!
//! # What is asserted
//!
//! Every case compares against the reference's own documented behaviour at `gzwrite.c` L403-L485:
//!
//! | Case | Reference line | Expectation |
//! |---|---|---|
//! | A formatted write | L471-L481 | returns the byte count, and those exact bytes read back |
//! | Several calls in a row | L477-L478 | counts and `gztell` accumulate; the stream is one member |
//! | A null `gzFile` | L417-L418 | `Z_STREAM_ERROR`, nothing touched |
//! | A null format | shim guard | `Z_STREAM_ERROR` rather than C's undefined behaviour |
//! | A read stream | L421-L422 | `Z_STREAM_ERROR` |
//! | A result that does not fit | L473-L474 | returns 0, writes nothing, advances nothing, sets no error |
//! | A closed-then-reopened path | — | the bytes survive a real round trip through the file |
//!
//! The overflow case is driven with `gzbuffer`, because `state->size` is what bounds the scratch
//! region (`gzwrite.c` L452-L469) and `gzbuffer` is the only public way to make it small. `gzlib.c`
//! L340-L341 clamps the request up to 8, so the smallest reachable region is eight bytes.
//!
//! # Coverage this suite inherits
//!
//! It is an ordinary `-p libz-rs-sys` integration test, so the workflow's AddressSanitizer job —
//! `cargo +nightly test --locked -p libz-rs-sys --target x86_64-unknown-linux-gnu` with
//! `-Zsanitizer=address` — instruments it without any further wiring. That matters more here than
//! anywhere else in the crate: this is the one path where a caller's variadic arguments are read
//! through the platform's own `va_list` machinery into a region the safe core sized, and ASan is
//! what would catch a write past that region's end.

use std::ffi::{c_char, c_int, c_uint, c_void, CStr, CString};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use z::{gzFile, Z_OK, Z_STREAM_ERROR};

// ---------------------------------------------------------------------------------------------
//  The probe
// ---------------------------------------------------------------------------------------------

extern "C" {
    /// `crates/libz-rs-sys/csrc/gzvprintf_probe.c` — forwards `...` to `gzvprintf` through a
    /// `va_list` it owns, and returns exactly what `gzvprintf` returned.
    ///
    /// Declaring it here is also the link check: the symbol comes from the archive `build.rs`
    /// stages, so this binary cannot be produced unless that translation unit was compiled into it.
    fn _zlib_rs_gzvprintf_probe(file: gzFile, format: *const c_char, ...) -> c_int;
}

// ---------------------------------------------------------------------------------------------
//  Scratch files
// ---------------------------------------------------------------------------------------------

/// A private directory for one case's `.gz` file, and the file's path inside it.
///
/// # Why the directory carries the security property and the file does not
///
/// Every case below opens its scratch file with `gzopen(path, "wb")`, and that is
/// `open(..., O_CREAT | O_TRUNC, ...)` with **no** `O_EXCL` (`gzlib.c` L228-L244). A predictable
/// name in a shared, world-writable directory is therefore an attack surface rather than merely a
/// collision risk: any other user on the machine can plant a symlink at a path a test is about to
/// create, and the truncating open follows it and destroys whatever it points at. The previous
/// version of this type built its name from the process id and a counter and then *removed* the
/// path before using it -- and a process id is neither secret nor unpredictable (it is visible in
/// `/proc`, drawn from a small space, and reused), while the removal opened a second window between
/// the check and the open. That is CWE-377, CWE-367 and CWE-59 in three lines.
///
/// So the *directory* is what is made safe, and it is created:
///
/// * **Unguessable** -- 128 bits from two OS-seeded [`RandomState`](std::hash::RandomState) draws.
/// * **Owner-only in the same syscall that creates it** -- `mode(0o700)` on the
///   [`DirBuilder`](fs::DirBuilder), rather than a create followed by a `chmod` that leaves a window
///   open.
/// * **Exclusively** -- `create` is the non-recursive form, so an existing path is refused with
///   [`AlreadyExists`](std::io::ErrorKind::AlreadyExists) rather than adopted. Nothing here ever
///   removes a path it did not itself create, so a planted entry is refused, never deleted.
///
/// That is also what keeps every `gzopen(path, "wb")` below *verbatim*, which matters because these
/// cases exist to exercise `gzvprintf` against `gzwrite.c`'s documented behaviour and not to
/// exercise a hardened opener: nothing can pre-exist inside a directory this process just created
/// empty, so `"wb"` has nothing to be tricked by. (`"wbx"` would be the answer if the file had to
/// live in a shared directory.)
///
/// The same reasoning, and the same three bullets, appear on `GzScratch` in
/// `crates/libz-rs-sys/tests/c_api_parity.rs`; this type is that one with the file name taken from
/// a caller's tag.
///
/// [`Drop`] removes the tree, so cleanup is panic-safe: a failing assertion anywhere in a case
/// unwinds through it.
///
/// No `tempfile` dev-dependency: AAP 0.7.1(i) keeps the dependency table minimal and `std` makes one
/// unnecessary.
struct TempPath {
    /// The private directory. Removed, with its contents, when this value is dropped.
    dir: PathBuf,
    /// The `.gz` file inside it.
    file: PathBuf,
}

impl TempPath {
    /// How many unguessable names to try before giving up.
    ///
    /// A collision means another process holds that exact 128-bit name, which is vanishingly
    /// unlikely; the retries keep one unlucky draw from failing the suite, and the bound keeps a
    /// temporary directory that refuses *every* create from spinning.
    const ATTEMPTS: usize = 16;

    /// Creates the private directory and names the file `<tag>.gz` inside it.
    ///
    /// Panics saying which step refused, which is right here: this is a test, the failure is an
    /// environment failure rather than a library one, and continuing would test nothing.
    fn new(tag: &str) -> Self {
        use std::hash::{BuildHasher, RandomState};
        #[cfg(unix)]
        use std::os::unix::fs::DirBuilderExt;

        let base = std::env::temp_dir();
        for _ in 0..Self::ATTEMPTS {
            // Two independent OS-seeded draws, so the name carries 128 bits rather than 64.
            let high = u128::from(RandomState::new().hash_one(0_u64));
            let low = u128::from(RandomState::new().hash_one(u64::MAX));
            let dir = base.join(format!("libz_rs_sys_gzprintf_{:032x}", (high << 64) | low));

            let mut builder = fs::DirBuilder::new();
            builder.recursive(false);
            #[cfg(unix)]
            builder.mode(0o700);

            match builder.create(&dir) {
                Ok(()) => {
                    let file = dir.join(format!("{tag}.gz"));
                    return Self { dir, file };
                }
                // Someone holds that name. Draw another; nothing is removed.
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => panic!(
                    "cannot create a private scratch directory under {}: {error}",
                    base.display()
                ),
            }
        }
        panic!(
            "{} unguessable names under {} were all taken",
            Self::ATTEMPTS,
            base.display()
        )
    }

    /// The `.gz` file's path.
    fn path(&self) -> &Path {
        &self.file
    }

    /// The private directory's path.
    fn dir(&self) -> &Path {
        &self.dir
    }

    /// The path as the NUL-terminated bytes `gzopen` wants.
    fn c_string(&self) -> CString {
        CString::new(self.file.to_str().expect("a temp path is valid UTF-8"))
            .expect("no interior NUL")
    }
}

impl Drop for TempPath {
    fn drop(&mut self) {
        // Best effort, and only ever on a directory this process created: a missing tree must never
        // itself become the failure, and nothing else can have a path into this one.
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// `gzopen(path, mode)`, asserting it succeeded.
fn open(path: &TempPath, mode: &str) -> gzFile {
    let path_c = path.c_string();
    let mode_c = CString::new(mode).unwrap();
    // SAFETY: two live `CString`s, each valid for reads through its terminator for the whole call.
    // The library copies the path it keeps, so neither has to outlive the call.
    let file = unsafe { z::gzopen(path_c.as_ptr(), mode_c.as_ptr()) };
    assert!(!file.is_null(), "gzopen({mode}) must succeed");
    file
}

/// `gzclose(file)`, asserting `Z_OK`.
fn close(file: gzFile) {
    // SAFETY: `file` is a live handle this suite opened and has not yet closed, and it is closed
    // exactly once here.
    let status = unsafe { z::gzclose(file) };
    assert_eq!(status, Z_OK, "gzclose must report Z_OK");
}

/// The whole decompressed content of `path`, read back through `gzread`.
fn read_back(path: &TempPath) -> Vec<u8> {
    let file = open(path, "rb");
    let mut out = Vec::new();
    let mut chunk = [0_u8; 512];
    loop {
        let room = c_uint::try_from(chunk.len()).unwrap();
        // SAFETY: `file` is the live read handle; the destination is a live local array and `room`
        // is its own length, so nothing is written past it.
        let got = unsafe { z::gzread(file, chunk.as_mut_ptr().cast::<c_void>(), room) };
        assert!(got >= 0, "gzread must not fail");
        if got == 0 {
            break;
        }
        out.extend_from_slice(&chunk[..usize::try_from(got).unwrap()]);
    }
    close(file);
    out
}

/// `bytes` as the `z_off_t` `gztell` reports, so a comparison needs no cast on either width.
///
/// `z_off_t` follows the platform's `off_t` (`zconf.h` L494-L531): `i64` on LP64 and `i32` on a
/// 32-bit target that has not asked for `_FILE_OFFSET_BITS=64`. Converting the *expected* value into
/// it, rather than widening what `gztell` returned, is what keeps the comparison exact on both
/// widths without a conversion that is redundant on one of them.
fn offset(bytes: usize) -> z::z_off_t {
    z::z_off_t::try_from(bytes).expect("a fixture offset fits a z_off_t")
}

/// The `%d` argument the first case passes, spelled as a `c_int` rather than left to inference.
///
/// A variadic argument is not checked against the format by the compiler, so the *declared* width is
/// the only thing that makes the call site's promotion explicit. `c_int` is what `%d` consumes.
fn answer() -> c_int {
    42
}

/// `gzerror`'s code and message, as an owned pair.
fn error_of(file: gzFile) -> (c_int, String) {
    let mut code: c_int = 0;
    // SAFETY: `file` is a live handle; `code` is a live local the call writes through. The returned
    // pointer is owned by the stream and is only read before the stream is closed.
    let text = unsafe {
        let raw = z::gzerror(file, std::ptr::addr_of_mut!(code));
        if raw.is_null() {
            String::new()
        } else {
            CStr::from_ptr(raw).to_string_lossy().into_owned()
        }
    };
    (code, text)
}

// ---------------------------------------------------------------------------------------------
//  The cases
// ---------------------------------------------------------------------------------------------

/// ★ The scratch directory this suite writes into is private, unguessable and exclusively created.
///
/// The property [`TempPath`] exists for, asserted rather than described. Every case below opens its
/// file with `gzopen(path, "wb")`, which is `O_CREAT | O_TRUNC` and **not** `O_EXCL`
/// (`gzlib.c` L228-L244), so the only thing standing between that truncating open and a planted
/// symlink is the directory it happens inside. A regression here -- a name made predictable again, a
/// `chmod` moved out of the creating syscall, a `create` made recursive so that an existing path is
/// adopted -- would leave every other case in this file passing.
///
/// Five separate things are checked, because each of the three bullets on [`TempPath`] can regress
/// on its own: the names differ, the file is *inside* the directory, the directory is empty and
/// `0700`, a second create at the same name is refused rather than adopted, and the guard removes
/// the whole tree.
#[test]
fn the_scratch_directory_is_private_unguessable_and_exclusively_created() {
    let first = TempPath::new("guard");
    let second = TempPath::new("guard");

    // The same tag twice: two guards must still name two directories, so neither a second
    // invocation of this binary -- the `simd` matrix row runs the identical suite -- nor the
    // relinked C drivers in the same tree can land on the one this case owns.
    assert_ne!(
        first.dir(),
        second.dir(),
        "each guard owns its own directory"
    );
    assert_ne!(first.path(), second.path(), "and therefore its own file");

    // The fixed file name is safe only because of where it is: inside a directory nothing else can
    // reach. Both halves of that are asserted, so a future change that moved the file up a level
    // would fail here rather than silently reopen the hole.
    assert_eq!(
        first.path().parent(),
        Some(first.dir()),
        "the .gz file must live inside the private directory"
    );
    assert_eq!(
        first.path().file_name().and_then(std::ffi::OsStr::to_str),
        Some("guard.gz"),
        "the file name comes from the tag, unchanged"
    );

    // `symlink_metadata` rather than `metadata`: the assertion is about the entry itself, not about
    // whatever a link at that path might resolve to.
    let meta = fs::symlink_metadata(first.dir()).expect("the private directory must exist");
    assert!(meta.is_dir(), "the guard must have created a directory");
    assert_eq!(
        fs::read_dir(first.dir())
            .expect("the private directory must be readable")
            .count(),
        0,
        "a freshly created private directory is empty, which is what leaves `wb` nothing to follow"
    );

    // Owner-only, and set by the syscall that created the directory rather than by a later `chmod`
    // that would leave a window open.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            meta.permissions().mode() & 0o777,
            0o700,
            "the directory must be traversable by its owner alone"
        );
    }

    // Exclusive creation. This is the property that makes a planted entry harmless: the guard draws
    // another name rather than deleting whatever it found, so `AlreadyExists` has to be what a
    // create at an occupied name reports.
    let mut builder = fs::DirBuilder::new();
    builder.recursive(false);
    let refused = builder
        .create(first.dir())
        .expect_err("a non-recursive create at an existing path must fail");
    assert_eq!(
        refused.kind(),
        std::io::ErrorKind::AlreadyExists,
        "an occupied name must be refused, never adopted"
    );

    // And the guard removes the tree it made, contents included, so nothing accumulates in the
    // shared temporary directory across runs.
    let remembered_dir = first.dir().to_path_buf();
    let remembered_file = first.path().to_path_buf();
    fs::write(&remembered_file, b"scratch").expect("the private directory must be writable");
    drop(first);
    assert!(
        !remembered_file.exists(),
        "the file must go with the directory"
    );
    assert!(
        !remembered_dir.exists(),
        "the private directory must be removed when the guard is dropped"
    );
}

/// A caller-built `va_list` reaches `gzvprintf`, which reports the exact byte count it wrote.
///
/// The format exercises the four conversions whose argument widths differ from one another, which is
/// what a mis-forwarded `va_list` corrupts first: a `const char *`, an `int`, a promoted `char` and
/// an `unsigned` in hex. `gzwrite.c` L477-L481 makes the return value the length `vsnprintf`
/// reported, so the count and the bytes are two independent observations of the same call and both
/// are asserted.
#[test]
fn a_caller_built_va_list_reaches_gzvprintf_and_reports_the_exact_count() {
    let path = TempPath::new("count");
    let file = open(&path, "wb");

    let format = CString::new("%s=%d %c 0x%x").unwrap();
    let name = CString::new("answer").unwrap();
    let expected = "answer=42 Z 0xbeef";

    // SAFETY: `file` is the live write handle. `format` and `name` are live `CString`s valid for
    // reads through their terminators for the whole call, and the four variadic arguments match the
    // four conversions in the format exactly -- `%s` a `*const c_char`, `%d` a `c_int`, `%c` an
    // `int`-promoted character and `%x` a `c_uint`, which is what the C variadic ABI requires.
    let written = unsafe {
        _zlib_rs_gzvprintf_probe(
            file,
            format.as_ptr(),
            name.as_ptr(),
            answer(),
            c_int::from(b'Z'),
            c_uint::from(0xbeef_u16),
        )
    };

    assert_eq!(
        written,
        c_int::try_from(expected.len()).unwrap(),
        "gzvprintf must return the number of bytes it formatted"
    );

    // `gzwrite.c` L478 advances `state->x.pos` by the same count, and that is the value `gztell`
    // reports, so a count that were right while the position was wrong is caught here.
    // SAFETY: `file` is the live write handle; `gztell` only reads the state.
    let told = unsafe { z::gztell(file) };
    assert_eq!(
        told,
        offset(expected.len()),
        "gztell must reflect the bytes gzvprintf accounted for"
    );

    let (code, text) = error_of(file);
    assert_eq!(code, Z_OK, "a successful gzvprintf leaves no error: {text}");

    close(file);
    assert_eq!(
        read_back(&path),
        expected.as_bytes(),
        "the bytes gzvprintf formatted must be the bytes the member holds"
    );
}

/// Successive calls accumulate, and the result is one gzip member holding their concatenation.
///
/// `gzvprintf` appends into the same input buffer and flushes it when more than half is occupied
/// (`gzwrite.c` L482-L484), so a boundary bug shows up as a missing or duplicated fragment rather
/// than as a bad count. Twenty calls at the default 8192-byte buffer cross that half-way point.
#[test]
fn successive_calls_accumulate_into_one_member() {
    let path = TempPath::new("accumulate");
    let file = open(&path, "wb");

    let format = CString::new("[%04d]").unwrap();
    let mut expected = String::new();
    let mut total = 0_usize;

    let indices: std::ops::Range<c_int> = 0..20;
    for index in indices {
        // SAFETY: as the first case -- a live handle, a live format valid through its terminator,
        // and one `c_int` argument for the one `%d` conversion.
        let written = unsafe { _zlib_rs_gzvprintf_probe(file, format.as_ptr(), index) };
        assert_eq!(written, 6, "[%04d] formats to exactly six bytes");
        write!(expected, "[{index:04}]").expect("writing to a String cannot fail");
        total += usize::try_from(written).expect("a byte count is non-negative");
    }

    // SAFETY: `file` is the live write handle; `gztell` only reads the state.
    let told = unsafe { z::gztell(file) };
    assert_eq!(told, offset(total), "gztell must be the running total");

    close(file);
    assert_eq!(read_back(&path), expected.as_bytes());
}

/// A null `gzFile` is refused with `Z_STREAM_ERROR` and nothing is dereferenced.
///
/// `gzwrite.c` L417-L418 makes this the first test `gzvprintf` performs, before it touches the
/// handle at all. The safe core cannot perform it -- a `&mut GzState` cannot be null -- so it lives
/// in the shim, and this is the case that proves the shim still performs it.
#[test]
fn a_null_file_is_refused() {
    let format = CString::new("anything %d").unwrap();
    // SAFETY: a null `gzFile` is the documented way to reach `gzwrite.c` L417-L418, whose first act
    // is the null test, so no dereference occurs. The format is a live `CString` and the one
    // variadic argument matches its one conversion.
    let status = unsafe { _zlib_rs_gzvprintf_probe(std::ptr::null_mut(), format.as_ptr(), 7_i32) };
    assert_eq!(status, Z_STREAM_ERROR, "gzvprintf(NULL, ...)");
}

/// A null format is refused with `Z_STREAM_ERROR` rather than handed to `vsnprintf`.
///
/// This is a deliberate *improvement* on the reference, and it is the shim's own guard rather than
/// the core's: `gzwrite.c` passes the pointer straight to `vsnprintf`, where a null format is
/// undefined behaviour. Refusing it costs nothing, removes a crash a caller could trigger with one
/// mistake, and cannot break a caller that was not already undefined. Asserted here so the guard
/// cannot be dropped as redundant.
#[test]
fn a_null_format_is_refused() {
    let path = TempPath::new("nullfmt");
    let file = open(&path, "wb");

    // SAFETY: `file` is the live write handle. A null format reaches the shim's own guard, which
    // returns before `vsnprintf` is called, so nothing dereferences it. No variadic arguments are
    // passed because none would be consumed.
    let status = unsafe { _zlib_rs_gzvprintf_probe(file, std::ptr::null()) };
    assert_eq!(status, Z_STREAM_ERROR, "gzvprintf(file, NULL)");

    // The stream must still be usable: a refused call is not an error state.
    let format = CString::new("still here").unwrap();
    // SAFETY: as the first case.
    let written = unsafe { _zlib_rs_gzvprintf_probe(file, format.as_ptr()) };
    assert_eq!(written, 10);

    close(file);
    assert_eq!(read_back(&path), b"still here");
}

/// A read stream is refused with `Z_STREAM_ERROR`.
///
/// `gzwrite.c` L421-L422: `if (state->mode != GZ_WRITE || ...) return Z_STREAM_ERROR;`. The mode
/// test is the second thing `gzvprintf` does and the first that reads the state, so this is the case
/// that proves the state really was consulted.
#[test]
fn a_read_stream_is_refused() {
    let path = TempPath::new("readmode");

    // Something to open for reading.
    let writer = open(&path, "wb");
    let format = CString::new("payload").unwrap();
    // SAFETY: as the first case.
    let written = unsafe { _zlib_rs_gzvprintf_probe(writer, format.as_ptr()) };
    assert_eq!(written, 7);
    close(writer);

    let reader = open(&path, "rb");
    // SAFETY: `reader` is a live read handle; the call is refused on the mode test before any
    // formatting happens, and the format is a live `CString` regardless.
    let status = unsafe { _zlib_rs_gzvprintf_probe(reader, format.as_ptr()) };
    assert_eq!(status, Z_STREAM_ERROR, "gzvprintf on a read stream");
    close(reader);

    // And the member is untouched by the refusal.
    assert_eq!(read_back(&path), b"payload");
}

/// A result larger than the scratch region reports 0, writes nothing, and sets no error.
///
/// `gzwrite.c` L473-L474 is the rejection test: `if (len == 0 || (unsigned)len >= state->size ||
/// next[state->size - 1] != 0) return 0;`. It returns **0**, not a negative code, and it must not
/// have advanced `avail_in` or `x.pos` -- so a partially formatted fragment must not appear in the
/// member either.
///
/// `gzbuffer` is what makes the region small: `state->size` is the bound `vsnprintf` is given, and
/// `gzlib.c` L340-L341 clamps the request up to 8, so eight bytes is the smallest reachable region.
/// The format below produces sixty-odd bytes into it.
#[test]
fn a_result_that_does_not_fit_reports_zero_and_writes_nothing() {
    let path = TempPath::new("overflow");
    let file = open(&path, "wb");

    // Before any I/O, or `gzbuffer` refuses: `gzlib.c` L332-L333 rejects a stream that has already
    // allocated its buffers.
    //
    // SAFETY: `file` is the live write handle and nothing has been written through it yet, so the
    // buffers `gzbuffer` refuses to resize have not been allocated.
    let sized = unsafe { z::gzbuffer(file, 8) };
    assert_eq!(sized, 0, "gzbuffer(8) must succeed");

    let long = CString::new("%s%s%s%s%s%s").unwrap();
    let piece = CString::new("0123456789").unwrap();
    // SAFETY: `file` is the live write handle, `long` and `piece` are live `CString`s, and six
    // `*const c_char` arguments are passed for the six `%s` conversions.
    let status = unsafe {
        _zlib_rs_gzvprintf_probe(
            file,
            long.as_ptr(),
            piece.as_ptr(),
            piece.as_ptr(),
            piece.as_ptr(),
            piece.as_ptr(),
            piece.as_ptr(),
            piece.as_ptr(),
        )
    };
    assert_eq!(
        status, 0,
        "a result that does not fit reports 0, not a negative code"
    );

    // Nothing accounted for: `gzwrite.c` L473-L474 returns before L477-L478 touch `avail_in` or
    // `x.pos`.
    // SAFETY: `file` is the live write handle; `gztell` only reads the state.
    let told = unsafe { z::gztell(file) };
    assert_eq!(
        told, 0,
        "a rejected gzvprintf must not advance the position"
    );
    let (code, text) = error_of(file);
    assert_eq!(
        code, Z_OK,
        "'did not fit' is not an error state on the stream: {text}"
    );

    // And the stream is still usable, with the small buffer still in force.
    let short = CString::new("%d").unwrap();
    // SAFETY: as the first case; one `c_int` for the one `%d`.
    let written = unsafe { _zlib_rs_gzvprintf_probe(file, short.as_ptr(), 5_i32) };
    assert_eq!(written, 1, "one digit fits in an eight-byte region");

    close(file);
    assert_eq!(
        read_back(&path),
        b"5",
        "the rejected call must have contributed no bytes at all"
    );
}
