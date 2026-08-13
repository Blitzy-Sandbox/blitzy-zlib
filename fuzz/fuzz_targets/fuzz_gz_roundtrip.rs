#![no_main]
#![forbid(unsafe_code)]
//! `fuzz_gz_roundtrip` -- write a payload through the `gzFile` layer, read it back, and
//! require the bytes to match.
//!
//! Derived from the four C translation units that make up that layer, which are the
//! algorithmic oracle for everything asserted here:
//!
//! | C source | What this target drives |
//! |---|---|
//! | `gzlib.c` | `gz_open`'s mode-string grammar (L87-L285), `gzbuffer`, `gzseek64`, `gztell64`, `gzoffset64`, `gzrewind`, `gzeof`, `gzerror`, `gzclearerr` |
//! | `gzwrite.c` | `gz_init`/`gz_comp`/`gz_zero`/`gz_write` behind `gzwrite`, `gzfwrite`, `gzputc`, `gzputs`, `gzflush`, `gzsetparams`, `gzclose_w` |
//! | `gzread.c` | `gz_look`/`gz_avail`/`gz_decomp`/`gz_fetch`/`gz_skip`/`gz_read` behind `gzread`, `gzfread`, `gzgets`, `gzgetc`, `gzgetc_`, `gzungetc`, `gzdirect`, `gzclose_r` |
//! | `gzclose.c` | `gzclose`'s dispatch to `gzclose_r` / `gzclose_w` |
//!
//! # The three invariants
//!
//! 1. **The payload round-trips byte for byte.** Whatever sequence of `gzwrite`,
//!    `gzfwrite`, `gzputc`, `gzputs`, `gzflush` and `gzseek` calls produced the file, the
//!    bytes read back through any mixture of `gzread`, `gzfread`, `gzgets`, `gzgetc` and
//!    `gzungetc` must equal the bytes handed in. This harness tracks what it wrote exactly,
//!    including the fact that `gzputs` stops at the first NUL and that a forward seek on a
//!    write stream inserts zeros, so the comparison is against a computed expectation
//!    rather than against the input buffer.
//! 2. **The mode grammar's `NULL` cases are honoured.** [`predict_mode`] reimplements
//!    `gz_open`'s character loop and its four post-loop rejections, and every open is
//!    cross-checked against that prediction. A mode string the grammar rejects must yield
//!    `NULL`; one it accepts must not.
//! 3. **No scratch file is ever created inside the repository.** The single reusable
//!    scratch file lives in the OS temporary directory under a name qualified by the
//!    process id, so parallel `-jobs` runs cannot collide and nothing is written into
//!    `fuzz/` or anywhere else in the tree.
//!
//! # Where the `unsafe` is
//!
//! Not here: this file carries `#![forbid(unsafe_code)]`, which the compiler enforces.
//! Driving the C ABI does need raw pointers, and every one of them is formed inside
//! `zlib_rs_differential::port` -- the harness crate's documented FFI boundary -- which is
//! where the four kinds of site this target reaches are discharged and documented:
//!
//! * calling a `gz*` export through a handle, live or `Z_NULL`, which the gates' obligation
//!   (g) covers and which [`Handle`]'s type invariant below narrows further to "live, and a
//!   close is still owed";
//! * building NUL-terminated path and mode strings -- this file produces the bytes with
//!   [`nul_terminated`], which appends exactly one NUL and refuses an interior one, and
//!   [`as_cstr`] views them as the `&CStr` a gate takes;
//! * handing buffer/length pairs to `gzread`, `gzwrite`, `gzfread`, `gzfwrite` and `gzgets`
//!   -- the gates take slices and derive both halves from the same one;
//! * reading the `*const c_char` `gzerror` returns, and the caller-visible `gzFile_s` prefix
//!   that the `gzgetc` macro mutates in caller object code, which `port::GzFile::macro_take`
//!   reproduces because no amount of encapsulation on this side can change what a C caller's
//!   own object code does.
//!
//! Nothing anywhere in this path defines a function that C calls back into, so there is no
//! unwinding hazard: the `assert!` macros below are the intended libFuzzer crash-reporting
//! mechanism, and a failure is meant to abort the process with a reproducer.
//!
//! # Throughput is a design constraint
//!
//! This is the only one of the five targets that touches the filesystem, so it is the
//! slowest, and a 300-second budget buys nothing if each iteration pays for a create and
//! an unlink. One path is resolved per process and cached; the file is truncated and
//! reused; it is removed only when an exclusive-create (`x`) mode is being exercised on
//! its success path. Payload length, operation count, chunk size and buffer size are all
//! capped by the private constants below.
//!
//! # What is deliberately absent
//!
//! * **No tracking allocator.** `gz_open` allocates its state with plain `malloc`
//!   (`gzlib.c` L100) and `gzguts.h` L120 states that the `gz*` functions always use the
//!   library's own allocation functions, so there is no caller-supplied `zalloc` to
//!   instrument. An `infcover`-style tracking allocator would give this layer zero
//!   coverage, which is why it appears in the deflate and inflate targets and not here.
//! * **No assertions on compressed bytes.** Byte-identity against the C encoder is the
//!   job of `crates/zlib-rs-differential/tests/byte_identical.rs`. This target asserts the
//!   round trip and the documented API contract, nothing about the encoding.

use std::env;
use std::ffi::{c_int, c_uint, c_void, CStr};
use std::fs::OpenOptions;
use std::mem::{align_of, offset_of, size_of};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;

#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
#[cfg(unix)]
use std::os::unix::io::{AsRawFd, IntoRawFd, OwnedFd};

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;

use libz_rs_sys::{
    gzFile_s, z_off64_t, z_off_t, z_size_t, Z_BEST_COMPRESSION, Z_BLOCK, Z_BUF_ERROR, Z_DATA_ERROR,
    Z_DEFAULT_COMPRESSION, Z_DEFAULT_STRATEGY, Z_ERRNO, Z_FILTERED, Z_FINISH, Z_FIXED,
    Z_FULL_FLUSH, Z_HUFFMAN_ONLY, Z_NO_COMPRESSION, Z_NO_FLUSH, Z_OK, Z_PARTIAL_FLUSH, Z_RLE,
    Z_STREAM_ERROR, Z_SYNC_FLUSH,
};
use zlib_rs_differential::port::GzFile;
use zlib_rs_fuzz::reached;

// ---------------------------------------------------------------------------
//  Bounds -- every one of these exists to keep the 300-second budget useful
// ---------------------------------------------------------------------------

/// Longest payload written in one invocation.
///
/// Small on purpose. The interesting behaviour of this layer is at buffer boundaries, in
/// the mode grammar and in the seek/flush interactions, none of which needs volume; and
/// libFuzzer's default RSS limit has to accommodate a `gzbuffer`-sized input buffer plus a
/// double-sized output buffer per open.
const MAX_PAYLOAD: usize = 4096;

/// Longest synthesised mode string, excluding the NUL and any appended direction or poison
/// character.
///
/// Eight is comfortably more than the longest mode string that means anything: the whole
/// alphabet is single-character flags, so beyond a handful the only new information is
/// which flag wins, and that is reached long before eight.
const MAX_MODE_SEED: usize = 8;

/// Ceiling on data-carrying operations in one pass.
///
/// Chunk sizes are raised so that the whole payload still fits inside this many
/// operations; see [`Plan::write_chunk`].
const MAX_DATA_OPS: usize = 32;

/// Largest chunk handed to a single write or read call.
const MAX_CHUNK: usize = 512;

/// Largest value passed to `gzbuffer` on its success path.
///
/// `gz_look` allocates `want` and `want << 1` bytes, and `gz_init` allocates `want << 1`
/// and `want`, so the real cost is three times this number per open.
const MAX_BUFFER: c_uint = 1 << 16;

/// A `gzbuffer` size whose doubling overflows, which `gzlib.c` L338-L339 refuses.
///
/// `(size << 1) < size` is true for anything with the top bit set, so this must be
/// rejected with -1 no matter how early it is called.
const OVERFLOWING_BUFFER: c_uint = 1 << 31;

/// Longest run of zeros a forward seek on a write stream is allowed to insert.
const MAX_ZERO_RUN: usize = 256;

/// Buffer length ceiling for a single `gzgets` call.
const MAX_GETS: usize = 256;

/// `SEEK_SET`, `SEEK_CUR` and `SEEK_END` as POSIX fixes them.
///
/// Written as literals rather than imported: these are `<stdio.h>` values, not `Z_*`
/// constants, so there is nothing in `zlib.h` to take them from and no contract constant is
/// being shadowed. `crates/libz-rs-sys/src/gz.rs`'s own test module spells them out the same
/// way. `gzseek` accepts the first two and must refuse the third (`gzlib.c` L383-L385).
const SEEK_SET: c_int = 0;
/// See [`SEEK_SET`].
const SEEK_CUR: c_int = 1;
/// See [`SEEK_SET`].
const SEEK_END: c_int = 2;

/// A `whence` value POSIX does not define, which `gzseek` must also refuse.
///
/// `gzlib.c` L383-L385 tests for `SEEK_SET` and `SEEK_CUR` and rejects everything else, so
/// the specific value is immaterial as long as it is neither of those two nor `SEEK_END`.
const SEEK_NONSENSE: c_int = 77;

// ---------------------------------------------------------------------------
//  The ABI gate -- `struct gzFile_s` is the highest-risk layout in the port
// ---------------------------------------------------------------------------
//
// `zlib.h` L1966-L1968 defines `gzgetc` as a macro:
//
//     #define gzgetc(g) \
//           ((g)->have ? ((g)->have--, (g)->pos++, *((g)->next)++) : (gzgetc)(g))
//
// so loads and stores at offsets 0, 8 and 16 of whatever `gzFile` points at are compiled
// into CALLER object code that the library cannot see or recompile.  Phase F below
// exercises exactly that arithmetic from Rust, which is only meaningful if the offsets it
// uses are the offsets the contract fixes -- hence these assertions.
//
// Two tiers, following `crates/libz-rs-sys/src/layout_assertions.rs`: the measured numbers
// are asserted only where the pointer is 64 bits wide, and the relationships that hold on
// every target are asserted unconditionally.  Deleting the guard would turn a correct
// 32-bit layout (0, 4, 8; 16 bytes) into a build failure.

/// True where a pointer is eight bytes, which is the target the absolute numbers below were
/// measured on.
const IS_64BIT_POINTER: bool = size_of::<*const c_void>() == 8;

// Absolute tier, 64-bit pointers only: `zlib.h` L1956-L1960 measured at 24 bytes with
// `have` at 0, `next` at 8 and `pos` at 16.
const _: () = assert!(!IS_64BIT_POINTER || size_of::<gzFile_s>() == 24);
const _: () = assert!(!IS_64BIT_POINTER || align_of::<gzFile_s>() == 8);
const _: () = assert!(!IS_64BIT_POINTER || offset_of!(gzFile_s, have) == 0);
const _: () = assert!(!IS_64BIT_POINTER || offset_of!(gzFile_s, next) == 8);
const _: () = assert!(!IS_64BIT_POINTER || offset_of!(gzFile_s, pos) == 16);

// Relational tier, every target.  `have` must lead, because that is the field the macro
// tests first and the one `gz_error` clears to switch the macro off (`gzlib.c` L564-L565);
// the other two must follow in declaration order without overlapping.
const _: () = assert!(offset_of!(gzFile_s, have) == 0);
const _: () = assert!(offset_of!(gzFile_s, next) >= size_of::<c_uint>());
const _: () =
    assert!(offset_of!(gzFile_s, pos) >= offset_of!(gzFile_s, next) + size_of::<*mut u8>());
const _: () = assert!(size_of::<gzFile_s>() >= offset_of!(gzFile_s, pos) + size_of::<z_off64_t>());

// `pos` is `z_off64_t` and is 64 bits on every target with large-file support, which is
// what lets a 32-bit caller and a 64-bit library agree on the prefix at all.
const _: () = assert!(size_of::<z_off64_t>() == 8);

// The payload cap has to fit an `int`, because `gzread` and `gzwrite` return their counts in
// one and refuse a request that does not fit (`gzread.c` L411-L416, `gzwrite.c` L269-L273).
const _: () = assert!(MAX_PAYLOAD < c_int::MAX as usize);

// ---------------------------------------------------------------------------
//  Phase B -- one scratch path per process, resolved once and reused
// ---------------------------------------------------------------------------

/// The fixed part of every scratch directory name, which is what makes the sweep possible.
///
/// A name is `<prefix><pid>-<32 hex digits>`: the prefix identifies the target, the pid
/// identifies the owner, and the hex is the unguessable part.
const SCRATCH_PREFIX: &str = "zlib-rs-fuzz-gz-";

/// Removes scratch directories left behind by processes that no longer exist.
///
/// ★ WHY THIS IS NEEDED AT ALL: nothing here can remove its own directory on the way out.
/// This target carries `#![forbid(unsafe_code)]`, so `atexit` is unavailable; statics are
/// never dropped; and libFuzzer leaves through `exit()` on most of its paths anyway. So the
/// directory a run creates outlives the run, and a machine that fuzzes this target regularly
/// accumulates one per invocation -- each holding a compressed file, in a directory that on a
/// shared build host is often never cleaned.
///
/// Sweeping on the way IN is the mechanism that is actually available, and it is safe in the
/// one way that matters: it removes a directory only when the pid encoded in its name has no
/// entry in `/proc`, so a directory belonging to a *live* sibling -- `cargo fuzz run -jobs=N`
/// starts several at once -- is never touched. Anything unparseable, anything whose pid is
/// still alive, and anything this process cannot remove is left exactly where it is.
///
/// Failures are ignored on purpose, every one of them. A temporary directory this process
/// cannot enumerate, an entry another user owns, a race with a sibling doing the same sweep:
/// none of those is a defect in the library under test, and none may become a crash
/// reproducer. [`preflight`] is what fails loudly if the environment is unusable, and it runs
/// straight after this.
///
/// Only on Linux, and deliberately so. The liveness test is `/proc/<pid>`; there is no
/// portable, dependency-free way to ask the same question elsewhere, and the alternative --
/// deleting by age -- would eventually delete a long-running sibling's directory out from
/// under it. Elsewhere the directory is left for the platform's own temporary-file cleanup,
/// and the CI job removes it explicitly.
#[cfg(target_os = "linux")]
fn sweep_abandoned_scratch(base: &Path) {
    // No `/proc` means the liveness test cannot be made, and a sweep that cannot tell live
    // from dead must not run.
    if !Path::new("/proc/self").is_dir() {
        return;
    }
    let Ok(entries) = std::fs::read_dir(base) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(tail) = name.strip_prefix(SCRATCH_PREFIX) else {
            continue;
        };
        // `<pid>-<hex>`; anything else is not one of ours, whatever it looks like.
        let Some((pid, hex)) = tail.split_once('-') else {
            continue;
        };
        if pid.is_empty()
            || !pid.bytes().all(|b| b.is_ascii_digit())
            || hex.len() != 32
            || !hex.bytes().all(|b| b.is_ascii_hexdigit())
        {
            continue;
        }
        if Path::new("/proc").join(pid).is_dir() {
            // Its owner is still running. Not ours to remove.
            continue;
        }
        if !entry.path().is_dir() {
            continue;
        }
        // Recursive, and bounded to a path this target's own naming scheme produced: the
        // directory holds exactly one file, `round-trip.gz`.
        let _ = std::fs::remove_dir_all(entry.path());
    }
}

/// The sweep is Linux-only; see the Linux definition for why.
#[cfg(not(target_os = "linux"))]
fn sweep_abandoned_scratch(_base: &Path) {}

/// The scratch path, resolved on first use and cached for the life of the process.
///
/// [`OnceLock`] rather than `Once` guarding a `static mut`: it has been stable since 1.70,
/// so it is comfortably inside the declared MSRV of 1.80, and it needs no `unsafe` and
/// trips no `static_mut_refs` warning -- which matters because this crate is linted with
/// `-D warnings`. `LazyLock` is deliberately not used.
static SCRATCH_PATH: OnceLock<PathBuf> = OnceLock::new();

/// The one file this target reads and writes, inside a private directory of its own.
///
/// ★ THE DIRECTORY IS UNGUESSABLE AND OWNER-ONLY; THE FILE NAME INSIDE IT IS FIXED. The
/// earlier form was `<temp dir>/zlib-rs-fuzz-gz-<pid>.gz` opened non-exclusively with
/// truncation, and in a shared temporary directory that is two problems. A process id is
/// neither secret nor unpredictable -- it is visible in `/proc`, drawn from a small space and
/// reused -- so another user on the machine can precreate that path, or plant a symlink or a
/// directory at it, and `gzopen(.., "wb")` (an `open()` with `O_CREAT` and no `O_EXCL`) then
/// follows it and writes wherever it points. Worse for a fuzz target: a planted *directory* at
/// that path makes every open fail, which before [`preflight`] existed was a green
/// 300-second run that exercised nothing.
///
/// So the security property is carried by the directory, exactly as
/// `crates/libz-rs-sys/tests/c_api_parity.rs` carries it:
///
/// * **Unguessable** -- 128 bits from two OS-seeded [`RandomState`] draws, which also removes
///   the `-jobs=N` collision the process id was there for.
/// * **Owner-only in the same syscall that creates it** -- `mode(0o700)` on the
///   [`DirBuilder`], not a create followed by a `chmod` that leaves a window open.
/// * **Exclusively** -- `create` is the non-recursive form, so an existing path is refused
///   rather than adopted, and nothing here removes a path it did not itself create.
///
/// Nothing can pre-exist inside a directory this process just created empty, so the fixed file
/// name inside it is safe and the `"wb"` truncating open has nothing to be tricked by. The name
/// stays fixed rather than per-iteration because a create and an unlink per iteration would put
/// this target's throughput in the tens of executions per second instead of the hundreds.
///
/// This never resolves inside the repository: [`preflight`] asserts that, and the temporary
/// directory is where the draw is rooted.
///
/// Panicking on failure is right, and is the same judgement [`preflight`] documents: an
/// unusable temporary directory makes every iteration a no-op, and this runs on the
/// `fuzz_target!` body's own stack rather than inside a callback the library invokes, so
/// nothing unwinds across the FFI edge.
fn scratch_path() -> &'static Path {
    /// How many unguessable names to try before giving up.
    const ATTEMPTS: usize = 16;

    SCRATCH_PATH
        .get_or_init(|| {
            use std::hash::{BuildHasher, RandomState};
            #[cfg(unix)]
            use std::os::unix::fs::DirBuilderExt;

            let base = env::temp_dir();
            // Before creating this process's directory, remove the ones abandoned by
            // processes that are gone. See `sweep_abandoned_scratch`.
            sweep_abandoned_scratch(&base);
            for _ in 0..ATTEMPTS {
                // Two independent OS-seeded draws, so the name carries 128 bits rather than 64.
                let high = u128::from(RandomState::new().hash_one(0_u64));
                let low = u128::from(RandomState::new().hash_one(u64::MAX));
                // ★ THE PROCESS ID IS A PREFIX, and it buys the cleanup below without
                // costing any of the unguessability the 128 bits carry: an attacker who
                // knows the pid -- everyone does, it is in `/proc` -- still has to guess
                // 128 unbiased bits, so the security argument above is untouched. What the
                // prefix adds is the ability for a LATER process to tell whose directory
                // this is, which is the only thing standing between "one directory per
                // fuzzing run" and a temporary directory that grows without bound.
                let dir = base.join(format!(
                    "{SCRATCH_PREFIX}{pid}-{:032x}",
                    (high << 64) | low,
                    pid = std::process::id()
                ));

                let mut builder = std::fs::DirBuilder::new();
                builder.recursive(false);
                #[cfg(unix)]
                builder.mode(0o700);

                match builder.create(&dir) {
                    Ok(()) => return dir.join("round-trip.gz"),
                    // Someone holds that name. Draw another; nothing is removed.
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(error) => panic!(
                        "cannot create a private scratch directory under {base}: {error}; \
                         every iteration of this target would be a no-op, so set TMPDIR to a \
                         writable directory",
                        base = base.display()
                    ),
                }
            }
            panic!(
                "{ATTEMPTS} unguessable names under {base} were all taken",
                base = base.display()
            )
        })
        .as_path()
}

/// Brings the scratch file to a known state for one invocation.
///
/// `remove` is true only when an exclusive-create mode (`x`) is being exercised on its
/// success path, because `O_EXCL` requires the file to be absent; otherwise the file is
/// created if missing and truncated in place, which is one `openat` and no unlink.
///
/// Returns whether the file is present afterwards, or [`None`] when the temporary directory
/// refused the operation. A refusal is an environment problem -- a read-only or full
/// `/tmp` -- and the caller returns early rather than asserting, because reporting it as a
/// crash would produce a reproducer that says nothing about the library.
fn prepare_scratch(path: &Path, remove: bool) -> Option<bool> {
    if remove {
        match std::fs::remove_file(path) {
            Ok(()) => Some(false),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Some(false),
            Err(_) => None,
        }
    } else {
        // `truncate` needs `write`; the handle is dropped immediately and only the side
        // effect on the directory entry is wanted.
        match OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)
        {
            Ok(_) => Some(true),
            Err(_) => None,
        }
    }
}

/// Proof, obtained once per process, that this target can actually do the thing it fuzzes.
///
/// ★ Without this every environment failure is indistinguishable from a clean run.
/// [`attempt`]'s result is discarded by the `fuzz_target!` body -- deliberately, because a
/// fuzzer-chosen mode string that the grammar refuses is an ordinary outcome and not a crash.
/// But the same `None` also carries "the temporary directory is read-only", "it is full", and
/// "this path cannot be expressed as a C string", and those are not ordinary outcomes: they
/// make *every* iteration a no-op. A mandatory `-max_total_time=300` job would then run to
/// completion, report success, and have exercised no gz round trip whatsoever -- the worst
/// kind of green, because it is indistinguishable from the real thing.
///
/// So the environment is proven up front instead of being hoped for, and a failure here is a
/// panic rather than a `None`. That is a safe place to panic: this runs on the `fuzz_target!`
/// body's own stack, never inside a callback the library invokes, so nothing unwinds across
/// the FFI edge.
///
/// The check is deliberately not a partial one. It resolves the path, insists the path is
/// expressible, creates and truncates the file, then performs a complete `gzopen`/`gzwrite`/
/// `gzclose` -> `gzopen`/`gzread`/`gzclose` cycle through the library under test and compares
/// the bytes, and finally unlinks. Every filesystem operation the target depends on is
/// therefore exercised before the first fuzz iteration -- including the unlink that the
/// exclusive-create (`x`) path needs -- and passing it means at least one genuine gz round
/// trip has provably happened in this process.
///
/// Mid-run refusals are still tolerated the way [`Opened::Environment`] describes: a
/// filesystem that fills up half way through should not manufacture a crash reproducer that
/// says nothing about the library. What is no longer tolerated is starting from an
/// environment that never worked at all.
static PREFLIGHT: OnceLock<()> = OnceLock::new();

/// The bytes the preflight round-trips. Long enough to cross a deflate block boundary rather
/// than sit entirely in the pending buffer, and fixed so the comparison is exact.
const PREFLIGHT_PAYLOAD: &[u8] = b"zlib-rs gz round trip preflight; if you can read this back \
the scratch directory works and the gz layer is wired up.";

fn preflight() {
    PREFLIGHT.get_or_init(|| {
        let path = scratch_path();

        // The doc on `scratch_path` promises this never lands in the repository; nothing
        // enforced it until now. A scratch file inside the tree would be a dirty working
        // copy at best and a clobbered source file at worst.
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
        assert!(
            !path.starts_with(repo),
            "the scratch path {} resolved inside the repository at {}; \
             set TMPDIR to a writable directory outside the tree",
            path.display(),
            repo.display(),
        );

        let path_c = path_bytes(path).unwrap_or_else(|| {
            panic!(
                "the scratch path {} cannot be expressed as a NUL-terminated C string, so \
                 gzopen can never be handed it and every iteration would be a no-op; set \
                 TMPDIR to a path that is valid Unicode and free of interior NULs",
                path.display(),
            )
        });
        assert!(
            prepare_scratch(path, false).is_some(),
            "the scratch file {} could not be created or truncated; the temporary directory \
             is unwritable or full, so no iteration of this target could do any work -- fix \
             the environment or set TMPDIR",
            path.display(),
        );

        // The two mode strings and the payload go through the gates in
        // `zlib_rs_differential::port`, where the raw pointers and their invariants live. This
        // file carries `#![forbid(unsafe_code)]`, so that is not a style choice: a raw
        // `gzopen`/`gzwrite`/`gzread`/`gzclose` sequence here would not compile.
        let write_mode = c"wb6";
        let read_mode = c"rb";
        let path_cstr = CStr::from_bytes_with_nul(&path_c)
            .expect("path_bytes appends the one NUL and rejects interior ones");
        let written = i32::try_from(PREFLIGHT_PAYLOAD.len()).expect("the payload is small");

        let file = GzFile::open(path_cstr, write_mode);
        assert!(
            !file.is_null(),
            "gzopen({}, \"wb6\") returned Z_NULL during preflight; the temporary \
             directory is not usable for writing",
            path.display(),
        );
        let put = file.write(PREFLIGHT_PAYLOAD);
        assert_eq!(
            put, written,
            "gzwrite accepted {put} of {written} preflight bytes"
        );
        assert_eq!(file.close(), Z_OK, "gzclose after the preflight write");

        let file = GzFile::open(path_cstr, read_mode);
        assert!(
            !file.is_null(),
            "gzopen({}, \"rb\") returned Z_NULL during preflight; the file just written \
             cannot be read back",
            path.display(),
        );
        let mut back = vec![0_u8; PREFLIGHT_PAYLOAD.len() + 1];
        let got = file.read(&mut back);
        assert_eq!(
            got, written,
            "gzread returned {got} bytes for the {written} written during preflight"
        );
        assert_eq!(file.close(), Z_OK, "gzclose after the preflight read");
        assert!(
            back[..PREFLIGHT_PAYLOAD.len()] == *PREFLIGHT_PAYLOAD,
            "the preflight round trip did not reproduce its own payload"
        );

        // The unlink the exclusive-create path depends on, proven here rather than discovered
        // by an iteration that would have silently given up.
        assert!(
            prepare_scratch(path, true).is_some(),
            "the scratch file {} could not be removed; the `x` exclusive-create mode could \
             never reach its success path",
            path.display(),
        );
    });
}

/// The scratch path as the NUL-terminated bytes `gzopen` wants.
///
/// Unix hands the bytes over unchanged, exactly as C does -- a path is a byte string there
/// and need not be UTF-8. Elsewhere the only portable route to bytes is through `str`, and a
/// path that is not valid Unicode simply cannot be expressed, which is reported as [`None`]
/// rather than guessed at.
fn path_bytes(path: &Path) -> Option<Vec<u8>> {
    #[cfg(unix)]
    let raw: Vec<u8> = path.as_os_str().as_bytes().to_vec();
    #[cfg(not(unix))]
    let raw: Vec<u8> = path.to_str()?.as_bytes().to_vec();
    nul_terminated(&raw)
}

/// Copies `bytes` into a buffer with a trailing NUL, refusing an interior one.
///
/// The refusal is not fussiness. A C string stops at its first NUL, so an interior one would
/// silently truncate a mode string and make [`predict_mode`]'s answer describe a different
/// string from the one the library parsed -- turning a correct library into an apparent
/// oracle mismatch.
fn nul_terminated(bytes: &[u8]) -> Option<Vec<u8>> {
    if bytes.contains(&0) {
        return None;
    }
    let mut owned = Vec::with_capacity(bytes.len() + 1);
    owned.extend_from_slice(bytes);
    owned.push(0);
    Some(owned)
}

/// Views a buffer [`nul_terminated`] produced as the `&CStr` the boundary gates take.
///
/// Total for every buffer that function produces: it appends exactly one NUL and refuses an
/// interior one, which is precisely `CStr::from_bytes_with_nul`'s precondition. So a failure
/// here would be a defect in this file rather than in the library, and reporting a defect in
/// this file as a crash is what a fuzz target's body is for.
///
/// # Panics
///
/// Panics if `bytes` is not a single NUL-terminated C string.
fn as_cstr(bytes: &[u8]) -> &CStr {
    match CStr::from_bytes_with_nul(bytes) {
        Ok(text) => text,
        Err(error) => {
            panic!("nul_terminated produced a buffer that is not a C string: {error}")
        }
    }
}

/// Widens a length to the `z_off64_t` the gz layer reports positions in.
///
/// Saturating rather than panicking: every length here is bounded by [`MAX_PAYLOAD`] plus at
/// most [`MAX_ZERO_RUN`], so the conversion cannot actually fail, and a saturating form keeps
/// that fact from becoming a panic on some target where `usize` were wider.
fn as_off64(value: usize) -> z_off64_t {
    z_off64_t::try_from(value).unwrap_or(z_off64_t::MAX)
}

// ---------------------------------------------------------------------------
//  Phase C part 1 -- the mode-string grammar and its oracle
// ---------------------------------------------------------------------------

/// The direction a mode string selects: `gzguts.h` L159-L162's `GZ_READ`, `GZ_WRITE` and
/// `GZ_APPEND`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Direction {
    /// `r`.
    Read,
    /// `w`.
    Write,
    /// `a`. `gz_open` turns this into `GZ_WRITE` once the file is positioned
    /// (`gzlib.c` L269-L272), so it behaves as a write everywhere after the open.
    Append,
}

/// Everything `gz_open`'s character loop extracts from a mode string.
#[derive(Clone, Copy, Debug)]
struct ModeSpec {
    /// Set by the last `r`, `w` or `a`; absent means `gz_open` returns `NULL`.
    direction: Direction,
    /// Compression level, set by the last decimal digit.
    level: c_int,
    /// One of the five `Z_*` strategies, set by `f`, `h`, `R` or `F`.
    strategy: c_int,
    /// The tri-state at `gzguts.h` L182 and `gzlib.c` L179-L197: -1 after a `G`, 1 after a
    /// `T`, 0 otherwise, and "last `G` or `T` wins" because each simply overwrites it.
    ///
    /// After the post-loop fix-up a reading stream carries 1 for auto-detect or -1 for
    /// gzip-only, and a writing stream carries 0 for gzip or 1 for transparent.
    direct: c_int,
    /// Set by `x`. `O_EXCL` reaches `open()` only on the write branch
    /// (`gzlib.c` L238-L241), so this is inert for a reading stream and inert for
    /// `gzdopen`, which never opens anything.
    exclusive: bool,
}

impl ModeSpec {
    /// Whether this is a gzip-only reader: the `G` case, `direct == -1` while reading.
    fn gzip_only_read(&self) -> bool {
        self.direction == Direction::Read && self.direct == -1
    }

    /// Whether this is a transparent writer: the `T` case, `direct == 1` while writing.
    fn transparent_write(&self) -> bool {
        self.direction != Direction::Read && self.direct == 1
    }
}

/// `gz_open`'s mode parser, reimplemented as the harness's oracle.
///
/// A faithful transcription of `gzlib.c` L108-L197 -- the character loop and all four
/// rejections -- so that every open below can be cross-checked against what the grammar
/// says should happen. [`None`] means `gz_open` returns `NULL`.
///
/// The four rejections, in the order the C code reaches them:
///
/// 1. a `+` anywhere: "can't read and write at the same time" (L129-L131), decided
///    immediately without looking at the rest of the string;
/// 2. no `r`, `w` or `a` at all (L173-L177);
/// 3. reading with a `T`: "can't force a transparent read" (L181-L185);
/// 4. writing or appending with a `G`: "'G' has no meaning when writing" (L191-L195).
///
/// The two subtleties that a reimplementation is most likely to lose are both here. Unknown
/// characters are **silently ignored** -- the source comment is "could consider as an error,
/// but just ignore" (L167-L168) -- so `"wbQZ"` opens successfully. And because `G` and `T`
/// both assign to the same field, the **last one wins**: `"rGT"` is rejected while `"rTG"`
/// is accepted.
///
/// # Verified exhaustively
///
/// This oracle was checked against the library rather than merely reasoned about. A throwaway
/// harness ran **1426 cases** -- every one-character and two-character string over
/// `"rwa+bexfhRFGNT09QZ"`, every three-character string over `"rwaGT+x"`, the whole of
/// [`CANONICAL_MODES`], and the specific strings `""`, `"9"`, `"b"`, `"rT"`, `"wG"`, `"r+"`,
/// `"rb9fT"`, `"wbQZ"`, `"rGT"` and `"rTG"` -- each with the target file both present and
/// absent, comparing this function composed with [`predict_open`] against what `gzopen`
/// actually answered. **All 1426 cases agreed.** The harness was removed once it had served its
/// purpose; the canonical table is what keeps the interesting cases running every day.
fn predict_mode(mode: &[u8]) -> Option<ModeSpec> {
    let mut direction: Option<Direction> = None;
    let mut level = Z_DEFAULT_COMPRESSION;
    let mut strategy = Z_DEFAULT_STRATEGY;
    let mut direct: c_int = 0;
    let mut exclusive = false;

    for &byte in mode {
        if byte == 0 {
            // C's loop condition is `while (*mode)`: the string ends at the first NUL.
            break;
        }
        if byte.is_ascii_digit() {
            // L114-L115. The guard makes `byte - b'0'` exact.
            level = c_int::from(byte - b'0');
            continue;
        }
        // `b`, `e`, `N` and the wildcard all do nothing *to the prediction*, and clippy
        // would rather they were one arm. They must not be. `case 'b'` is a documented
        // no-op ("ignore -- will request binary anyway", L132-L133); `e` and `N` set
        // `O_CLOEXEC` and `O_NONBLOCK`, which change the descriptor and not the outcome
        // this function predicts; and the wildcard is a deliberate decision to tolerate
        // garbage. Merging them would erase the distinction between characters the format
        // defines and characters it does not.
        #[allow(clippy::match_same_arms)]
        match byte {
            b'r' => direction = Some(Direction::Read),
            b'w' => direction = Some(Direction::Write),
            b'a' => direction = Some(Direction::Append),
            // Rejection 1, decided here and now.
            b'+' => return None,
            b'b' => {}
            b'e' => {}
            b'x' => exclusive = true,
            b'f' => strategy = Z_FILTERED,
            b'h' => strategy = Z_HUFFMAN_ONLY,
            b'R' => strategy = Z_RLE,
            b'F' => strategy = Z_FIXED,
            b'G' => direct = -1,
            b'N' => {}
            b'T' => direct = 1,
            _ => {}
        }
    }

    // Rejection 2.
    let direction = direction?;

    if direction == Direction::Read {
        if direct == 1 {
            // Rejection 3.
            return None;
        }
        if direct == 0 {
            // L186-L189: auto-detect gzip versus transparent, starting from a transparent
            // assumption "in case of an empty file" -- which is also why `gzdirect` answers
            // true for one.
            direct = 1;
        }
    } else if direct == -1 {
        // Rejection 4.
        return None;
    }

    Some(ModeSpec {
        direction,
        level,
        strategy,
        direct,
        exclusive,
    })
}

// ---------------------------------------------------------------------------
//  Phase C part 2 -- synthesising mode strings from fuzzer bytes
// ---------------------------------------------------------------------------

/// Alphabet for a synthesised **write** mode.
///
/// The whole meaningful set except `+` and `G`, both of which are guaranteed rejections for
/// a writer and are therefore held back for the shapes that go looking for rejections. `T`
/// stays in, because a transparent write is a legitimate and interesting configuration; `r`
/// and `a` stay in because the guaranteed `w` is appended last and last wins.
const WRITE_ALPHABET: &[u8] = b"rwabexfhRFNT0123456789QZ";

/// Alphabet for a synthesised **read** mode.
///
/// Mirror image: `T` is held back because it is a guaranteed rejection for a reader, and `G`
/// stays in because a gzip-only read is a real configuration with its own outcome.
const READ_ALPHABET: &[u8] = b"rwabexfhRFNG0123456789QZ";

/// Every character the grammar gives meaning to, plus three it must ignore.
///
/// Used by the shapes that deliberately go after the rejections and the contradictions.
const WILD_ALPHABET: &[u8] = b"rwa+bexfhRFGNT0123456789QZ.";

/// Mode strings whose outcome is worth guaranteeing rather than waiting for the fuzzer to
/// stumble on.
///
/// Each is annotated with the rejection it triggers or the reason it succeeds, and together
/// they cover all four `NULL` cases, the silent-ignore rule and both orderings of the
/// asymmetric `G`/`T` pair. Selected by one shape below, so they are exercised continuously
/// rather than only by a throwaway check.
///
/// `#[rustfmt::skip]` keeps one string per line so that each stays next to the comment naming
/// its outcome; the same device is used for the tables in `crates/zlib-rs/src/deflate/
/// config_table.rs` and `crates/zlib-rs/tests/error.rs`, and for the same reason.
#[rustfmt::skip]
const CANONICAL_MODES: &[&[u8]] = &[
    // Rejection 2: no `r`, `w` or `a`.
    b"",
    b"b9",
    b"TG",
    // Rejection 3: cannot force a transparent read.
    b"rT",
    b"rb9fT",
    // Rejection 4: `G` is meaningless when writing.
    b"wG",
    b"aG",
    // Rejection 1: cannot read and write at once.
    b"r+",
    b"w+",
    // Accepted: `Q` and `Z` are silently ignored.
    b"wbQZ",
    // Accepted: last `G` or `T` wins, so this is a gzip-only read where `"rGT"` is refused.
    b"rTG",
    // Refused, for the same reason read the other way round.
    b"rGT",
    // Accepted, one of each interesting flavour.
    b"wT",
    b"a",
    b"wx",
    b"rb",
    b"wb9",
    b"whR",
];

/// Builds one mode string from fuzzer bytes.
///
/// `shape` chooses the strategy, and the split is deliberate: eleven of sixteen shapes
/// produce a string that opens successfully, so most iterations reach the round trip, while
/// the remaining five spend their probability mass on the rejections, the canonical table
/// and the unrestricted alphabet.
///
/// The result is NUL-terminated and free of interior NULs, because every alphabet is
/// printable ASCII; [`nul_terminated`] enforces that rather than assuming it.
fn synth_mode(seed: &[u8], shape: u8, pick: u8, want_read: bool) -> Option<Vec<u8>> {
    let direction: u8 = if want_read { b'r' } else { b'w' };
    let poison: u8 = if want_read { b'T' } else { b'G' };
    let safe: &[u8] = if want_read {
        READ_ALPHABET
    } else {
        WRITE_ALPHABET
    };

    let mut mode = Vec::with_capacity(seed.len() + 2);
    match shape % 16 {
        // The common case: a plausible string with the direction pinned on the end.
        0..=10 => {
            map_onto(&mut mode, seed, safe);
            mode.push(direction);
        }
        // Guaranteed coverage of the interesting strings, whatever the fuzzer is doing.
        11 => {
            let table = CANONICAL_MODES;
            mode.extend_from_slice(table[usize::from(pick) % table.len()]);
        }
        // Unrestricted, and with nothing appended -- so most of these have no direction at
        // all and must be refused.
        12 => map_onto(&mut mode, seed, WILD_ALPHABET),
        // A valid string with the one character that makes it invalid tacked on.
        13 => {
            map_onto(&mut mode, seed, safe);
            mode.push(direction);
            mode.push(poison);
        }
        // Likewise for the `+` rejection, which is decided the moment it is reached.
        14 => {
            map_onto(&mut mode, seed, safe);
            mode.push(direction);
            mode.push(b'+');
        }
        // Unrestricted but directed: contradictions, garbage and a real direction together.
        _ => {
            map_onto(&mut mode, seed, WILD_ALPHABET);
            mode.push(direction);
        }
    }
    nul_terminated(&mode)
}

/// Maps each seed byte onto `alphabet`, appending the results to `out`.
fn map_onto(out: &mut Vec<u8>, seed: &[u8], alphabet: &[u8]) {
    for &byte in seed {
        out.push(alphabet[usize::from(byte) % alphabet.len()]);
    }
}

// ---------------------------------------------------------------------------
//  Phase C part 3 -- the structured input and the plan derived from it
// ---------------------------------------------------------------------------

/// Which optional operations an invocation performs.
///
/// A single bitfield rather than a wall of `bool` fields: it keeps the fuzzer's encoding
/// compact, lets one mutated byte flip several decisions at once, and avoids
/// `clippy::struct_excessive_bools`, which this crate denies through `clippy::pedantic`.
mod flag {
    /// Open the write stream with `gzopen64` instead of `gzopen`.
    pub const OPEN64: u32 = 1 << 0;
    /// Open the write stream by adopting a descriptor through `gzdopen`.
    pub const DOPEN_WRITE: u32 = 1 << 1;
    /// Open the read stream by adopting a descriptor through `gzdopen`.
    pub const DOPEN_READ: u32 = 1 << 2;
    /// Call `gzbuffer` before writing, and again afterwards to confirm it is refused.
    pub const SET_BUFFER: u32 = 1 << 3;
    /// Probe `gzbuffer`'s overflow rejection with [`super::OVERFLOWING_BUFFER`].
    pub const BUFFER_OVERFLOW: u32 = 1 << 4;
    /// Call `gzsetparams` before writing.
    pub const SET_PARAMS: u32 = 1 << 5;
    /// Close the write stream with `gzclose_w` rather than `gzclose`.
    pub const CLOSE_W: u32 = 1 << 6;
    /// Close the read stream with `gzclose_r` rather than `gzclose`.
    pub const CLOSE_R: u32 = 1 << 7;
    /// Run the null-handle sentinel sweep.
    pub const NULL_SWEEP: u32 = 1 << 8;
    /// Let an exclusive-create mode succeed by removing the file first; otherwise leave it
    /// in place so that `O_EXCL` is refused.
    pub const EXCLUSIVE_SUCCEEDS: u32 = 1 << 9;
    /// Insert a run of zeros into the write stream with a forward `gzseek`.
    pub const WRITE_SEEK: u32 = 1 << 10;
    /// Seek around the read stream after the round trip has been verified.
    pub const READ_SEEK: u32 = 1 << 11;
    /// Rewind the read stream and read the whole payload a second time.
    pub const REWIND: u32 = 1 << 12;
    /// Check `gzoffset` and `gzoffset64` bounds and monotonicity.
    pub const OFFSET: u32 = 1 << 13;
    /// Call the wrong `gzclose_*` first and confirm it is refused without freeing.
    pub const WRONG_CLOSE: u32 = 1 << 14;
    /// Push bytes back with `gzungetc` until it runs out of room.
    pub const UNGETC_SATURATE: u32 = 1 << 15;
    /// Read `gzerror`'s message and then clear it with `gzclearerr`.
    pub const ERROR_PROBE: u32 = 1 << 16;
    /// Attempt operations that are illegal for the stream's direction.
    pub const ILLEGAL_OPS: u32 = 1 << 17;
    /// Use `gzgetc_` rather than `gzgetc` for single-byte reads.
    pub const GETC_UNDERSCORE: u32 = 1 << 18;
    /// Report positions through the narrow `gztell`/`gzoffset` pair as well as the wide one.
    pub const NARROW_OFFSETS: u32 = 1 << 19;
}

/// The fuzzer's structured input.
#[derive(Arbitrary, Debug)]
struct GzRoundTrip {
    /// Selects the strategy [`synth_mode`] uses for the write mode.
    write_shape: u8,
    /// Selects the strategy [`synth_mode`] uses for the read mode.
    read_shape: u8,
    /// Indexes [`CANONICAL_MODES`] when a shape asks for it.
    canonical_pick: u8,
    /// Raw bytes mapped onto the write alphabet.
    write_seed: [u8; MAX_MODE_SEED],
    /// Raw bytes mapped onto the read alphabet.
    read_seed: [u8; MAX_MODE_SEED],
    /// Low nibble is the write seed length, high nibble the read seed length; both taken
    /// modulo one more than [`MAX_MODE_SEED`], so an empty seed is reachable.
    seed_lengths: u8,
    /// Requested `gzbuffer` size, clamped to [`MAX_BUFFER`].
    buffer_size: u16,
    /// Requested `gzsetparams` level, clamped to the documented range.
    level: i8,
    /// Requested `gzsetparams` strategy, mapped onto the five `Z_*` values.
    strategy: u8,
    /// Preferred write chunk, raised if necessary so the payload fits in
    /// [`MAX_DATA_OPS`] operations.
    write_chunk: u16,
    /// Preferred read chunk, raised the same way.
    read_chunk: u16,
    /// `gzgets` buffer length, clamped to [`MAX_GETS`] and floored at two so that the call
    /// can always make progress.
    gets_len: u8,
    /// `gzfread`/`gzfwrite` item size, mapped into `1..=4`.
    item_size: u8,
    /// Absolute position for the read-stream seek, clamped to the payload length.
    seek_target: u16,
    /// Relative forward distance for the follow-up `SEEK_CUR`.
    seek_delta: u16,
    /// Length of the zero run a forward write seek inserts, clamped to [`MAX_ZERO_RUN`].
    write_skip: u8,
    /// The [`flag`] bitfield.
    flags: u32,
    /// Operation selectors, cycled through by both passes.
    ops: Vec<u8>,
    /// The bytes to round-trip; truncated to [`MAX_PAYLOAD`].
    payload: Vec<u8>,
}

/// The clamped, validated form of [`GzRoundTrip`] that the passes actually run against.
struct Plan {
    /// NUL-terminated write mode.
    write_mode: Vec<u8>,
    /// NUL-terminated read mode.
    read_mode: Vec<u8>,
    /// What the grammar says the write mode does.
    write_spec: Option<ModeSpec>,
    /// What the grammar says the read mode does.
    read_spec: Option<ModeSpec>,
    /// Bytes to write.
    payload: Vec<u8>,
    /// `gzbuffer` argument.
    buffer_size: c_uint,
    /// `gzsetparams` level, in `Z_DEFAULT_COMPRESSION..=Z_BEST_COMPRESSION`.
    level: c_int,
    /// `gzsetparams` strategy, one of the five `Z_*` values.
    strategy: c_int,
    /// Raw write chunk preference.
    write_chunk: u16,
    /// Raw read chunk preference.
    read_chunk: u16,
    /// `gzgets` buffer length, at least two.
    gets_len: usize,
    /// `gzfread`/`gzfwrite` item size, at least one.
    item_size: usize,
    /// Raw read-seek target preference.
    seek_target: usize,
    /// Raw read-seek relative distance preference.
    seek_delta: usize,
    /// Zero-run length for a forward write seek.
    write_skip: usize,
    /// The [`flag`] bitfield.
    flags: u32,
    /// Operation selectors.
    ops: Vec<u8>,
}

impl Plan {
    /// Whether `bit` -- one of the [`flag`] constants -- is set.
    fn has(&self, bit: u32) -> bool {
        self.flags & bit != 0
    }
}

/// Clamps and derives everything the passes need, so that no clamping decision is made
/// twice or forgotten at a call site.
///
/// Two clamps are load-bearing rather than defensive:
///
/// * **`level` is forced into `Z_DEFAULT_COMPRESSION..=Z_BEST_COMPRESSION`.** `gzsetparams`
///   validates neither argument -- C does not either (`gzwrite.c` L641-L665) -- and stores
///   whatever it is given. An out-of-range level therefore survives until `gz_init` calls
///   `deflateInit2`, which fails, and the stream is reported as `Z_MEM_ERROR` from then on.
///   That is faithful C behaviour and not a defect, but it would make the round trip depend
///   on it, so the level is kept inside the documented range and the behaviour is left to
///   the differential suite.
/// * **`gets_len` is floored at two.** `gzgets` reserves one byte for the terminating NUL,
///   so `len == 1` leaves nothing to read and consumes nothing (`gzread.c` L594-L596); a
///   read loop that chose it would make no progress.
fn build_plan(input: &GzRoundTrip) -> Option<Plan> {
    let modulus = MAX_MODE_SEED + 1;
    let write_len = usize::from(input.seed_lengths & 0x0f) % modulus;
    let read_len = usize::from(input.seed_lengths >> 4) % modulus;

    let write_mode = synth_mode(
        &input.write_seed[..write_len],
        input.write_shape,
        input.canonical_pick,
        false,
    )?;
    let read_mode = synth_mode(
        &input.read_seed[..read_len],
        input.read_shape,
        input.canonical_pick,
        true,
    )?;

    let mut payload = input.payload.clone();
    payload.truncate(MAX_PAYLOAD);

    Some(Plan {
        write_spec: predict_mode(&write_mode),
        read_spec: predict_mode(&read_mode),
        write_mode,
        read_mode,
        payload,
        buffer_size: c_uint::from(input.buffer_size) % (MAX_BUFFER + 1),
        level: c_int::from(input.level).clamp(Z_DEFAULT_COMPRESSION, Z_BEST_COMPRESSION),
        strategy: match input.strategy % 5 {
            0 => Z_DEFAULT_STRATEGY,
            1 => Z_FILTERED,
            2 => Z_HUFFMAN_ONLY,
            3 => Z_RLE,
            _ => Z_FIXED,
        },
        write_chunk: input.write_chunk,
        read_chunk: input.read_chunk,
        gets_len: (usize::from(input.gets_len) % MAX_GETS).max(2),
        item_size: usize::from(input.item_size % 4) + 1,
        seek_target: usize::from(input.seek_target),
        seek_delta: usize::from(input.seek_delta),
        write_skip: usize::from(input.write_skip) % (MAX_ZERO_RUN + 1),
        flags: input.flags,
        ops: input.ops.clone(),
    })
}

/// Chunk size that honours the fuzzer's preference while guaranteeing that `total` bytes fit
/// inside [`MAX_DATA_OPS`] operations.
///
/// Without the floor, a chunk of one against a four-kilobyte payload would silently stop
/// after thirty-two bytes and the round trip would only ever cover a prefix.
fn effective_chunk(raw: u16, total: usize) -> usize {
    let chosen = usize::from(raw) % MAX_CHUNK + 1;
    chosen.max(total.div_ceil(MAX_DATA_OPS)).max(1)
}

/// Cycles through the fuzzer's operation selectors, so a short `ops` vector still drives a
/// long pass.
struct OpStream<'a> {
    /// The fuzzer's bytes; may be empty.
    bytes: &'a [u8],
    /// How many selectors have been drawn.
    index: usize,
}

impl<'a> OpStream<'a> {
    /// Wraps `bytes`.
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, index: 0 }
    }

    /// The next selector, or zero when the fuzzer supplied none.
    fn next(&mut self) -> u8 {
        if self.bytes.is_empty() {
            return 0;
        }
        let byte = self.bytes[self.index % self.bytes.len()];
        self.index = self.index.wrapping_add(1);
        byte
    }
}

// ---------------------------------------------------------------------------
//  The handle: this target's one owned resource
// ---------------------------------------------------------------------------

/// A live `gzFile` owned by this harness.
///
/// # Type invariant
///
/// `raw` is non-null and came from one of this target's own `gzopen`, `gzopen64` or `gzdopen`
/// gates, and no `gzclose*` has yet succeeded on it. That is stronger than the gates' own
/// obligation (g), which also admits `Z_NULL`, and it is what makes every method below a call
/// on a stream that really exists. It is established at construction -- [`Handle::adopt`] is
/// the only constructor and it rejects null -- and preserved because the only thing that can
/// invalidate it, a successful close, consumes the right to call anything else.
///
/// `open` records whether a close is still owed. [`Handle::close_as`] clears it and hands
/// back the return code so it can be asserted on; [`Drop`] closes whatever is still open, so
/// no early return -- including one taken because the environment misbehaved -- can leak a
/// descriptor. That matters more than it looks: a leaked descriptor per iteration surfaces as
/// `EMFILE` a few thousand executions into a run and reads exactly like a library defect.
struct Handle {
    /// The handle itself, non-null by the type invariant.
    raw: GzFile,
    /// Whether a `gzclose*` is still owed.
    open: bool,
}

/// Which `gzclose*` form to finish with.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum CloseWith {
    /// `gzclose`, which dispatches on the mode (`gzclose.c` L11-L22).
    Dispatch,
    /// `gzclose_r`, valid only for a reading stream.
    Reader,
    /// `gzclose_w`, valid only for a writing stream.
    Writer,
}

impl Handle {
    /// Takes ownership of a handle a `gz*open` call produced, or [`None`] for `Z_NULL`.
    fn adopt(raw: GzFile) -> Option<Self> {
        if raw.is_null() {
            None
        } else {
            Some(Self { raw, open: true })
        }
    }

    // -- configuration ------------------------------------------------------

    /// `gzbuffer(file, size)` -- `gzlib.c` L322-L343. Zero on success, -1 on refusal.
    fn buffer(&self, size: c_uint) -> c_int {
        self.raw.buffer(size)
    }

    /// `gzsetparams(file, level, strategy)` -- `gzwrite.c` L630-L665.
    fn setparams(&self, level: c_int, strategy: c_int) -> c_int {
        self.raw.setparams(level, strategy)
    }

    // -- writing ------------------------------------------------------------

    /// `gzwrite(file, buf, len)` -- `gzwrite.c` L255-L277. Byte count, or zero on error.
    fn write(&self, bytes: &[u8]) -> c_int {
        self.raw.write(bytes)
    }

    /// `gzfwrite(buf, size, nitems, file)` -- `gzwrite.c` L279-L303. Full items written.
    fn fwrite(&self, bytes: &[u8], size: z_size_t) -> z_size_t {
        // `size * items` is `bytes.len()` rounded down, so the region the call reads is inside
        // the slice, which is what the gate requires of the two factors it is given.
        self.raw.fwrite_items(bytes, size, bytes.len() / size)
    }

    /// `gzputc(file, c)` -- `gzwrite.c` L305-L344. Returns `c & 0xff`, or -1 on error.
    fn putc(&self, byte: u8) -> c_int {
        self.raw.putc(c_int::from(byte))
    }

    /// `gzputs(file, s)` -- `gzwrite.c` L346-L369. Writes `strlen(s)` bytes.
    ///
    /// `text` must already be NUL-terminated; [`nul_terminated`] is the only producer of such
    /// buffers here and it refuses an interior NUL, so what the call writes is exactly what
    /// the caller believes it handed over.
    fn puts(&self, text: &[u8]) -> c_int {
        debug_assert_eq!(
            text.last(),
            Some(&0),
            "gzputs needs a NUL-terminated buffer"
        );
        self.raw.puts(as_cstr(text))
    }

    /// `gzflush(file, flush)` -- `gzwrite.c` L603-L627.
    fn flush(&self, flush: c_int) -> c_int {
        // The call validates `flush`'s range itself (`gzwrite.c` L616-L617).
        self.raw.flush(flush)
    }

    // -- reading ------------------------------------------------------------

    /// `gzread(file, buf, len)` -- `gzread.c` L395-L436. Byte count, zero at end, -1 on error.
    fn read(&self, into: &mut [u8]) -> c_int {
        self.raw.read(into)
    }

    /// `gzfread(buf, size, nitems, file)` -- `gzread.c` L438-L465. Full items read.
    fn fread(&self, into: &mut [u8], size: z_size_t) -> z_size_t {
        // `size * items` is at most `into.len()`, which is what the gate requires.
        let items = into.len() / size;
        self.raw.fread_items(into, size, items)
    }

    /// `gzgets(file, buf, len)` -- `gzread.c` L565-L624.
    ///
    /// Returns whether the call answered with the caller's own buffer. The contract admits
    /// only two answers, `buf` or `NULL` (L620-L623), and anything else would be a defect, so
    /// the identity is checked here where the pointer is still in scope.
    fn gets(&self, into: &mut [u8]) -> bool {
        // The gate holds the call to the two answers the contract admits -- the caller's own
        // buffer or `NULL` -- because the returned pointer is only comparable while it is still
        // in scope, which is inside the gate and not here.
        self.raw.gets(into).is_some()
    }

    /// `gzgetc(file)` -- `gzread.c` L473-L498, the function form of the macro.
    fn getc(&self) -> c_int {
        self.raw.getc()
    }

    /// `gzgetc_(file)` -- `gzread.c` L500-L502.
    ///
    /// The name exists because `zlib.h` L1961 keeps it for callers that took the function's
    /// address before the macro existed. Nothing else in the suite calls it, so it is called
    /// here.
    fn getc_(&self) -> c_int {
        self.raw.getc_()
    }

    /// `gzungetc(c, file)` -- `gzread.c` L504-L563.
    fn ungetc(&self, byte: c_int) -> c_int {
        self.raw.ungetc(byte)
    }

    // -- position and state -------------------------------------------------

    /// `gzseek64(file, offset, whence)` -- `gzlib.c` L367-L433.
    fn seek(&self, offset: z_off64_t, whence: c_int) -> z_off64_t {
        // The call validates `whence` itself.
        self.raw.seek64(offset, whence)
    }

    /// `gzseek(file, offset, whence)` -- `gzlib.c` L435-L441, the narrow form.
    fn seek_narrow(&self, offset: z_off_t, whence: c_int) -> z_off_t {
        self.raw.seek(offset, whence)
    }

    /// `gzrewind(file)` -- `gzlib.c` L346-L365.
    fn rewind(&self) -> c_int {
        self.raw.rewind()
    }

    /// `gztell64(file)` -- `gzlib.c` L443-L457.
    fn tell(&self) -> z_off64_t {
        self.raw.tell64()
    }

    /// `gztell(file)` -- `gzlib.c` L459-L465, the narrow form.
    fn tell_narrow(&self) -> z_off_t {
        self.raw.tell()
    }

    /// `gzoffset64(file)` -- `gzlib.c` L467-L483.
    fn offset(&self) -> z_off64_t {
        self.raw.offset64()
    }

    /// `gzoffset(file)` -- `gzlib.c` L485-L491, the narrow form.
    fn offset_narrow(&self) -> z_off_t {
        self.raw.offset()
    }

    /// `gzeof(file)` -- `gzlib.c` L493-L506. Reports `past`, not `eof`.
    fn eof(&self) -> c_int {
        self.raw.eof()
    }

    /// `gzdirect(file)` -- `gzread.c` L626-L640.
    fn direct(&self) -> c_int {
        self.raw.direct()
    }

    /// `gzerror(file, &errnum)` -- `gzlib.c` L508-L525.
    ///
    /// Returns the error number and the message bytes. The message is read as **bytes** up to
    /// the NUL rather than as UTF-8: it is built from the path (`gzlib.c` L577-L586) and a
    /// path is an arbitrary byte string on Unix.
    fn error(&self) -> Option<(c_int, Vec<u8>)> {
        // `Z_OK` as the sentinel the gate initialises `errnum` to, which is what this target
        // wants: it asserts on the code only where the contract fixes it, and a stream with no
        // error reports `Z_OK` either way. The message comes back copied, so nothing outlives
        // the window `zlib.h` L1779-L1789 gives it, and [`None`] is C's null return -- a null
        // or wrongly-moded handle -- which is distinct from `Some` with an empty message.
        let (errnum, message) = self.raw.error(Z_OK);
        message.map(|bytes| (errnum, bytes))
    }

    /// `gzerror(file, NULL)` -- the form that asks only for the message.
    ///
    /// `gzlib.c` L521-L522 makes the out-parameter optional, and nothing else exercises the
    /// null form.
    fn error_message_only(&self) -> bool {
        self.raw.error_message_only()
    }

    /// `gzclearerr(file)` -- `gzlib.c` L530-L547.
    fn clearerr(&self) {
        self.raw.clearerr();
    }

    // -- the caller-visible prefix -----------------------------------------

    /// The caller-visible `gzFile_s` prefix, as `(have, next_is_non_null, pos)`.
    ///
    /// The gate reports `next` as a boolean rather than as a pointer, which is all this target
    /// needs of it: what is asserted is that `have` and `pos` move together, and a raw address
    /// is not a property any assertion here could compare. Reading the prefix at all rests on
    /// the contract that makes the `gzgetc` macro legal -- `zlib.h` L1956-L1960 publishes the
    /// three fields and `gzguts.h` L169-L172 places them at offset 0 of the real state -- which
    /// the assertions at the top of this file pin independently.
    fn prefix(&self) -> Option<(c_uint, bool, z_off64_t)> {
        self.raw.prefix()
    }

    /// Consumes one byte exactly as the `gzgetc` **macro** does.
    ///
    /// `zlib.h` L1966-L1968 expands to `((g)->have--, (g)->pos++, *((g)->next)++)` inside the
    /// **caller's** object code, so this is not poking at an internal: it is what every
    /// program that uses `gzgetc` genuinely does to the state, and the library has to
    /// tolerate it and recover its own cursor on the next call. Returns [`None`] when `have`
    /// is zero, which is the branch where the macro defers to the function.
    ///
    /// Nothing past the 24-byte prefix is touched; everything after it is the port's private
    /// state and is out of contract.
    fn macro_take(&self) -> Option<u8> {
        self.raw.macro_take()
    }

    // -- closing ------------------------------------------------------------

    /// Closes with the requested form and returns the code, discharging the owed close.
    fn close_as(&mut self, which: CloseWith) -> c_int {
        assert!(self.open, "close_as must not be called twice on one handle");
        self.open = false;
        // `open` is cleared first, so neither `Drop` nor a second `close_as` can repeat the one
        // close the type invariant permits.
        match which {
            CloseWith::Dispatch => self.raw.close(),
            CloseWith::Reader => self.raw.close_r(),
            CloseWith::Writer => self.raw.close_w(),
        }
    }

    /// Calls the `gzclose_*` form that does **not** match this stream's direction.
    ///
    /// `gzclose_r` refuses a writing stream and `gzclose_w` refuses a reading one, both with
    /// `Z_STREAM_ERROR` and both **before** freeing anything (`gzread.c` L648-L652,
    /// `gzwrite.c` L671-L678), so the handle stays live and the right close can still follow.
    /// That is why this takes `&self` and leaves `open` set.
    fn close_mismatched(&self, which: CloseWith) -> c_int {
        assert!(self.open, "close_mismatched needs a live handle");
        // The call is a refusal that returns before touching the state, so the type invariant
        // survives it and the owed close is still owed.
        match which {
            CloseWith::Reader => self.raw.close_r(),
            CloseWith::Writer => self.raw.close_w(),
            CloseWith::Dispatch => {
                unreachable!("gzclose dispatches on the mode and is never mismatched")
            }
        }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        if self.open {
            self.open = false;
            // `gzclose` is the dispatching form, so it is right for either direction. The code
            // is discarded because a drop reached by an early return has nothing to assert; the
            // point is that the descriptor and the state are released rather than leaked.
            let _ = self.raw.close();
        }
    }
}

// ---------------------------------------------------------------------------
//  Opening, and the cross-check against the grammar
// ---------------------------------------------------------------------------

/// Which entry point performs an open.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum OpenKind {
    /// `gzopen` -- `gzlib.c` L288-L290.
    Named,
    /// `gzopen64` -- `gzlib.c` L292-L294. The `*64` names are barely touched anywhere else,
    /// and both families have to be exported because which one a caller references depends on
    /// its own `_FILE_OFFSET_BITS` (`zlib.h` L1976-L2013).
    Named64,
    /// `gzdopen` on a descriptor this harness opened -- `gzlib.c` L298-L312. Better coverage
    /// than a repeated `gzopen`, because nothing but `test/minigzip.c` adopts a descriptor,
    /// and it skips `open()` entirely so `x`, exclusivity and existence stop mattering.
    Descriptor,
}

/// Why the grammar or the filesystem says an open must answer `Z_NULL`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Refusal {
    /// The mode string itself is invalid: one of `gz_open`'s four rejections.
    Mode,
    /// A reading open of a file that is not there.
    Missing,
    /// An exclusive create of a file that is.
    Exists,
}

/// The result of an open attempt.
enum Opened {
    /// A live handle, and the grammar agreed it should exist.
    Live(Handle),
    /// `Z_NULL`, and the grammar predicted it for the recorded reason.
    Refused,
    /// `Z_NULL` where success was predicted, and the standard library cannot do it either.
    ///
    /// That is an environment problem -- an unwritable or full temporary directory -- and the
    /// caller abandons the iteration instead of asserting. Reporting it as a crash would yield
    /// a reproducer that says nothing about the library.
    Environment,
}

/// What the contract says an open of `path` with `spec` through `kind` must answer.
///
/// [`None`] means it must succeed.
fn predict_open(spec: Option<ModeSpec>, kind: OpenKind, exists: bool) -> Option<Refusal> {
    let Some(spec) = spec else {
        return Some(Refusal::Mode);
    };
    if kind == OpenKind::Descriptor {
        // `gz_open` takes the `fd >= 0` branch and never calls `open()` (`gzlib.c` L247-L263),
        // so neither the file's existence nor `O_EXCL` can refuse it.
        return None;
    }
    match spec.direction {
        // `O_RDONLY` on a file that is not there.
        Direction::Read if !exists => Some(Refusal::Missing),
        Direction::Read => None,
        // `O_CREAT | O_EXCL` on a file that is (`gzlib.c` L238-L241).
        _ if spec.exclusive && exists => Some(Refusal::Exists),
        _ => None,
    }
}

/// Opens the scratch file and holds the answer against [`predict_open`].
///
/// The two directions of disagreement are treated differently on purpose. A `NULL` where
/// success was predicted may be the environment's doing, so it is re-checked through the
/// standard library before anything is asserted. A handle where `NULL` was predicted cannot
/// be: every reason in [`Refusal`] is decided from the mode string and from whether the file
/// exists, both of which this harness controls, so it is a defect in the mode parser and is
/// asserted immediately.
fn open_stream(
    path: &Path,
    path_c: &[u8],
    mode_c: &[u8],
    spec: Option<ModeSpec>,
    kind: OpenKind,
    exists: bool,
) -> Opened {
    let refusal = predict_open(spec, kind, exists);
    let raw = match kind {
        OpenKind::Descriptor => match descriptor_open(path, mode_c, spec) {
            Some(raw) => raw,
            // The descriptor could not be created at all: nothing was handed to the library.
            None => return Opened::Environment,
        },
        _ => named_open(path_c, mode_c, kind),
    };

    match (Handle::adopt(raw), refusal) {
        (Some(handle), None) => Opened::Live(handle),
        (Some(_), Some(reason)) => panic!(
            "gz{}open accepted mode {:?} that the contract refuses ({reason:?})",
            if kind == OpenKind::Descriptor {
                "d"
            } else {
                ""
            },
            String::from_utf8_lossy(mode_c)
        ),
        (None, Some(_)) => Opened::Refused,
        (None, None) => {
            // Predicted success, got NULL. Ask the standard library to do the equivalent: if
            // it can, the library refused something it had to accept; if it cannot, the
            // temporary directory is at fault and this iteration has nothing to say.
            let doable = spec.is_some_and(|spec| std_can_open(path, &spec));
            assert!(
                !doable,
                "gzopen refused mode {:?}, which the grammar accepts and the filesystem allows",
                String::from_utf8_lossy(mode_c)
            );
            Opened::Environment
        }
    }
}

/// `gzopen` or `gzopen64` on a NUL-terminated path.
fn named_open(path_c: &[u8], mode_c: &[u8], kind: OpenKind) -> GzFile {
    // Neither argument is retained past the call. Both buffers come from `nul_terminated`, so
    // each ends in a NUL and contains no interior one -- which is exactly what makes [`as_cstr`]
    // total for them -- and both outlive this call because the caller owns them for the whole
    // iteration.
    let path = as_cstr(path_c);
    let mode = as_cstr(mode_c);
    if kind == OpenKind::Named64 {
        GzFile::open64(path, mode)
    } else {
        GzFile::open(path, mode)
    }
}

/// Opens the file with `std` and hands the descriptor to `gzdopen`.
///
/// `zlib.h` L1408-L1416: `gzclose` closes the adopted descriptor, and a **failed** `gzdopen`
/// does not. So the descriptor is released here exactly once -- by `gzclose` on success, and
/// by reconstructing the [`File`] and dropping it on failure. Doing both, or neither, is the
/// classic way to turn this call into either a double close or a leak.
#[cfg(unix)]
fn descriptor_open(path: &Path, mode_c: &[u8], spec: Option<ModeSpec>) -> Option<GzFile> {
    let spec = spec?;
    let mut options = OpenOptions::new();
    match spec.direction {
        Direction::Read => {
            options.read(true);
        }
        Direction::Append => {
            options.append(true).create(true);
        }
        Direction::Write => {
            options.write(true).create(true).truncate(true);
        }
    }
    let file = options.open(path).ok()?;
    // Held as an `OwnedFd` rather than as a bare number, so that both outcomes are expressed by
    // ownership instead of by a raw reconstruction: on failure the `OwnedFd` is dropped and
    // closes the descriptor, and on success `into_raw_fd` releases ownership without closing it,
    // which is what hands it to the library exactly once.
    let owned = OwnedFd::from(file);
    let raw = GzFile::dopen(owned.as_raw_fd(), as_cstr(mode_c));
    if raw.is_null() {
        drop(owned);
    } else {
        let _ = owned.into_raw_fd();
    }
    Some(raw)
}

/// The non-Unix stand-in: `gzdopen` takes a CRT descriptor there, which [`File`] cannot
/// produce, so the descriptor path is simply unavailable and the caller falls back to a named
/// open.
#[cfg(not(unix))]
fn descriptor_open(_path: &Path, _mode_c: &[u8], _spec: Option<ModeSpec>) -> Option<GzFile> {
    None
}

/// Whether the standard library can perform the open `spec` describes.
///
/// Used on one path only: telling a real refusal apart from a hostile temporary directory. It
/// really does open the file, which is harmless because the caller either asserts immediately
/// -- ending the process -- or abandons the iteration.
fn std_can_open(path: &Path, spec: &ModeSpec) -> bool {
    let mut options = OpenOptions::new();
    match spec.direction {
        Direction::Read => {
            options.read(true);
        }
        Direction::Append => {
            options.append(true).create(true);
        }
        Direction::Write => {
            options.write(true).create(true).truncate(true);
        }
    }
    if spec.exclusive && spec.direction != Direction::Read {
        options.create_new(true);
    }
    options.open(path).is_ok()
}

/// Picks the entry point an open should use.
///
/// The descriptor form is chosen only when a named open would have succeeded too, so that the
/// refusal cases -- a missing file, an exclusive create over an existing one -- are still
/// exercised through the entry point that can actually produce them. `gzdopen` skips `open()`
/// altogether, so routing those cases through it would quietly delete the test.
fn choose_kind(plan: &Plan, spec: Option<ModeSpec>, exists: bool, dopen_bit: u32) -> OpenKind {
    let adoptable = spec.is_some() && predict_open(spec, OpenKind::Named, exists).is_none();
    if adoptable && plan.has(dopen_bit) && cfg!(unix) {
        OpenKind::Descriptor
    } else if plan.has(flag::OPEN64) {
        OpenKind::Named64
    } else {
        OpenKind::Named
    }
}

// ---------------------------------------------------------------------------
//  Phase D -- the write pass
// ---------------------------------------------------------------------------

/// What the scratch file holds once the write pass is over.
struct FileImage {
    /// The uncompressed bytes any reader must observe.
    logical: Vec<u8>,
    /// True when the file holds a gzip stream; false when it holds [`FileImage::logical`]
    /// verbatim, which is the case for a transparent (`T`) write and for an iteration where
    /// nothing was written at all.
    gzip: bool,
    /// Whether the file exists on disk. False only when an exclusive-create iteration removed
    /// it and then failed to open, which is the one path that leaves nothing behind.
    present: bool,
}

/// Opens the scratch file for writing, writes the payload, and reports what the file holds.
///
/// [`None`] means the environment refused something and the iteration has nothing to say.
fn write_pass(path: &Path, path_c: &[u8], plan: &Plan, exists: bool) -> Option<FileImage> {
    let spec = plan.write_spec;
    let kind = choose_kind(plan, spec, exists, flag::DOPEN_WRITE);
    let mut handle = match open_stream(path, path_c, &plan.write_mode, spec, kind, exists) {
        Opened::Live(handle) => handle,
        // The mode or the filesystem refused it, exactly as predicted. Nothing was written, so
        // the file is however `prepare_scratch` left it.
        Opened::Refused => {
            return Some(FileImage {
                logical: Vec::new(),
                gzip: false,
                present: exists,
            })
        }
        Opened::Environment => return None,
    };
    let Some(spec) = spec else {
        unreachable!("a live handle implies the grammar accepted the mode string")
    };

    if spec.direction == Direction::Read {
        read_only_slot(&mut handle, plan, &spec)?;
        return Some(FileImage {
            logical: Vec::new(),
            gzip: false,
            present: true,
        });
    }
    write_stream(&mut handle, path, plan, &spec)
}

/// Handles a "write" mode string that turned out to select reading.
///
/// The shapes that draw on the unrestricted alphabet can produce one, and it is worth keeping
/// rather than discarding: it opens a reading stream on a file that has just been truncated to
/// nothing, which is the empty-input corner of `gz_look` (`gzread.c` L139-L146), and it is the
/// natural place to confirm that the write entry points refuse a reading stream.
fn read_only_slot(handle: &mut Handle, plan: &Plan, spec: &ModeSpec) -> Option<()> {
    assert_eq!(
        handle.tell(),
        0,
        "a fresh reading stream is at position zero"
    );
    assert_eq!(handle.eof(), 0, "nothing has been read past the end yet");
    if plan.has(flag::ILLEGAL_OPS) {
        assert_write_ops_refused(handle);
    }
    let mut sink = [0_u8; 32];
    assert_eq!(
        handle.read(&mut sink),
        0,
        "an empty file has no bytes to deliver"
    );
    // `gz_look` leaves `direct` at 1 for an empty file under auto-detect -- the transparent
    // assumption at `gzread.c` L187-L189 -- and forces it to 0 when `G` asked for gzip only.
    assert_eq!(
        handle.direct(),
        c_int::from(!spec.gzip_only_read()),
        "gzdirect must agree with how an empty file is being read"
    );
    let code = handle.close_as(CloseWith::Dispatch);
    if code == Z_ERRNO {
        return None;
    }
    assert!(
        matches!(code, Z_OK | Z_BUF_ERROR),
        "closing a reading stream must report Z_OK or a pending Z_BUF_ERROR, got {code}"
    );
    Some(())
}

/// Writes the payload and everything around it, then closes.
fn write_stream(
    handle: &mut Handle,
    path: &Path,
    plan: &Plan,
    spec: &ModeSpec,
) -> Option<FileImage> {
    let transparent = spec.transparent_write();
    let mut content: Vec<u8> = Vec::with_capacity(plan.payload.len() + plan.write_skip);
    configure_write(handle, plan, spec);

    let chunk = effective_chunk(plan.write_chunk, plan.payload.len());
    let mut ops = OpStream::new(&plan.ops);
    let mut cursor = 0_usize;
    let mut performed = 0_usize;
    let mut buffer_refused = false;

    while cursor < plan.payload.len() && performed < MAX_DATA_OPS {
        let end = (cursor + chunk).min(plan.payload.len());
        let slice = &plan.payload[cursor..end];
        let selector = ops.next();
        let advanced = write_data_op(handle, selector, slice, plan.item_size)?;
        assert!(advanced > 0, "a write operation must always make progress");
        content.extend_from_slice(&slice[..advanced]);
        cursor += advanced;
        performed += 1;
        assert_eq!(
            handle.tell(),
            as_off64(content.len()),
            "gztell must equal the number of uncompressed bytes written"
        );
        if !buffer_refused && plan.has(flag::SET_BUFFER) {
            // `gzlib.c` L334-L335: once `state->size` is non-zero the buffers exist and the
            // request is too late. `zlib.h` L1440-L1441 spells the answer as -1.
            assert_eq!(
                handle.buffer(plan.buffer_size),
                -1,
                "gzbuffer must be refused once the working buffers exist"
            );
            buffer_refused = true;
        }
        if plan.has(flag::OFFSET) {
            assert_offset_sane(handle, path, plan.has(flag::NARROW_OFFSETS));
        }
        write_side_op(handle, selector, plan, transparent);
    }

    if plan.has(flag::WRITE_SEEK) {
        write_seek(handle, plan, &mut content);
    }
    if plan.has(flag::WRONG_CLOSE) {
        assert_eq!(
            handle.close_mismatched(CloseWith::Reader),
            Z_STREAM_ERROR,
            "gzclose_r must refuse a writing stream"
        );
    }
    let which = if plan.has(flag::CLOSE_W) {
        CloseWith::Writer
    } else {
        CloseWith::Dispatch
    };
    let code = handle.close_as(which);
    if code == Z_ERRNO {
        return None;
    }
    assert_eq!(code, Z_OK, "closing a writing stream must succeed");

    Some(FileImage {
        logical: content,
        gzip: !transparent,
        present: true,
    })
}

/// The calls that have to happen before the first byte moves.
fn configure_write(handle: &Handle, plan: &Plan, spec: &ModeSpec) {
    assert_eq!(
        handle.tell(),
        0,
        "a fresh writing stream is at position zero"
    );
    assert_eq!(
        handle.eof(),
        0,
        "gzeof is always false for a writing stream"
    );
    if plan.has(flag::BUFFER_OVERFLOW) {
        // `gzlib.c` L338-L339 refuses a size it could not double, whatever else is true, and
        // leaves the previous request in place.
        assert_eq!(
            handle.buffer(OVERFLOWING_BUFFER),
            -1,
            "gzbuffer must refuse a size whose doubling overflows"
        );
    }
    if plan.has(flag::SET_BUFFER) {
        assert_eq!(
            handle.buffer(plan.buffer_size),
            0,
            "gzbuffer must be accepted before the first write"
        );
    }
    // A **transparent** stream is refused outright: `gzwrite.c` L641-L642 tests `state->direct`
    // alongside the mode and the error, so a `T` stream has no compression parameters to set.
    // That test is specific to this development head rather than to upstream 1.3.1, which makes
    // it exactly the kind of thing a reimplementation drops, so it is asserted rather than
    // avoided.
    let expected = if spec.transparent_write() {
        Z_STREAM_ERROR
    } else {
        Z_OK
    };
    if plan.has(flag::SET_PARAMS) {
        assert_eq!(
            handle.setparams(plan.level, plan.strategy),
            expected,
            "gzsetparams answers Z_OK for a gzip stream and Z_STREAM_ERROR for a transparent one"
        );
    } else {
        // The no-change short circuit at L647-L648. The mode string's own level and strategy are
        // already installed, so this must be accepted and change nothing.
        assert_eq!(
            handle.setparams(spec.level, spec.strategy),
            expected,
            "gzsetparams must accept the parameters the mode string already installed"
        );
    }
}

/// Performs one data-carrying write and reports how many bytes of `slice` it consumed.
///
/// Always at least one, so the caller's loop cannot stall. The case that would otherwise stall
/// is `gzputs`: it writes `strlen(s)` bytes (`gzwrite.c` L360-L367), so a chunk that opens with
/// a zero byte contributes nothing, and that case falls back to `gzputc`.
///
/// [`None`] means a call reported fewer bytes than it was given, which on a regular file means
/// the filesystem refused the write rather than the library.
fn write_data_op(handle: &Handle, selector: u8, slice: &[u8], item: usize) -> Option<usize> {
    let first = *slice.first()?;
    match selector % 4 {
        0 => {
            let got = handle.write(slice);
            accept_count(handle, got, slice.len())
        }
        1 => {
            let size = item.min(slice.len()).max(1);
            let items = slice.len() / size;
            if items == 0 {
                return write_one(handle, first);
            }
            let bytes = items * size;
            let got = handle.fwrite(&slice[..bytes], size);
            if got == items {
                Some(bytes)
            } else {
                reject_short_write(handle, "gzfwrite")
            }
        }
        2 => write_one(handle, first),
        _ => {
            let stop = slice
                .iter()
                .position(|&byte| byte == 0)
                .unwrap_or(slice.len());
            if stop == 0 {
                return write_one(handle, first);
            }
            let text = nul_terminated(&slice[..stop])?;
            let got = handle.puts(&text);
            accept_count(handle, got, stop)
        }
    }
}

/// `gzputc` for exactly one byte.
fn write_one(handle: &Handle, byte: u8) -> Option<usize> {
    // `gzwrite.c` L336-L343: the return value is `c & 0xff`, which for a `u8` is the byte.
    if handle.putc(byte) == c_int::from(byte) {
        Some(1)
    } else {
        reject_short_write(handle, "gzputc")
    }
}

/// Accepts a write that reported the whole request, and rejects one that did not.
fn accept_count(handle: &Handle, got: c_int, want: usize) -> Option<usize> {
    if got >= 0 && usize::try_from(got).ok() == Some(want) {
        return Some(want);
    }
    reject_short_write(handle, "a write")
}

/// Decides whether a short write is the environment's fault or the library's.
///
/// The only way a regular file on a working filesystem shortens a write is an I/O error, and
/// `gz_comp` records that as `Z_ERRNO` (`gzwrite.c` L84 and L119). Anything else is the
/// library's own answer and is a defect, so it is asserted on rather than tolerated.
fn reject_short_write(handle: &Handle, what: &str) -> Option<usize> {
    let errnum = handle.error().map_or(Z_OK, |(code, _)| code);
    assert_eq!(
        errnum, Z_ERRNO,
        "{what} came up short without an operating-system error to explain it"
    );
    None
}

/// One non-data operation on the writing stream, chosen by the top bits of the same selector.
fn write_side_op(handle: &Handle, selector: u8, plan: &Plan, transparent: bool) {
    match selector >> 5 {
        // Every valid flush value in turn. `Z_FINISH` is the interesting one: `gz_comp` sets
        // `state->reset` (`gzwrite.c` L143-L144) so the next write starts a **new** gzip
        // member, and a multi-member file reads back concatenated -- which is why the round
        // trip still holds across it.
        1 => assert_flush(handle, Z_NO_FLUSH),
        2 => assert_flush(handle, Z_SYNC_FLUSH),
        3 => assert_flush(handle, Z_FULL_FLUSH),
        4 => assert_flush(handle, Z_PARTIAL_FLUSH),
        5 => assert_flush(handle, Z_FINISH),
        // Out of range. `gzwrite.c` L616-L617 rejects `flush < 0 || flush > Z_FINISH`, so
        // `Z_BLOCK` -- a perfectly good `deflate` flush -- is not one of `gzflush`'s.
        6 => {
            assert_eq!(
                handle.flush(Z_BLOCK),
                Z_STREAM_ERROR,
                "gzflush must refuse a flush value above Z_FINISH"
            );
            assert_eq!(
                handle.flush(-1),
                Z_STREAM_ERROR,
                "gzflush must refuse a negative flush value"
            );
        }
        7 => {
            assert_eq!(
                handle.eof(),
                0,
                "gzeof is always false for a writing stream"
            );
            assert_eq!(
                handle.direct(),
                c_int::from(transparent),
                "gzdirect must report whether the writing stream is transparent"
            );
        }
        _ => {
            if plan.has(flag::NARROW_OFFSETS) {
                let wide = handle.tell();
                assert_eq!(
                    z_off64_t::from(handle.tell_narrow()),
                    wide,
                    "gztell and gztell64 must agree"
                );
            }
            // A mid-stream parameter change, which is a different path from the one
            // `configure_write` takes: `state->size` is non-zero by now, so `gzsetparams`
            // flushes whatever is buffered with `Z_BLOCK` and then calls `deflateParams`
            // (`gzwrite.c` L655-L660). Switching to stored is the most disruptive change
            // available and must still leave the stream readable.
            assert_eq!(
                handle.setparams(Z_NO_COMPRESSION, Z_DEFAULT_STRATEGY),
                if transparent { Z_STREAM_ERROR } else { Z_OK },
                "a mid-stream gzsetparams must be accepted unless the stream is transparent"
            );
        }
    }
}

/// A flush that must be accepted.
fn assert_flush(handle: &Handle, flush: c_int) {
    assert_eq!(
        handle.flush(flush),
        Z_OK,
        "gzflush({flush}) must succeed on a healthy writing stream"
    );
}

/// A forward `gzseek` on a writing stream, which inserts zeros rather than moving anything.
///
/// `gzlib.c` L410-L433 records the request in `state->skip`, and `gz_zero` (`gzwrite.c`
/// L150-L182) compresses that many zero bytes the next time anything is written -- or at
/// `gzclose_w`, which resolves a pending skip itself (`gzwrite.c` L680-L682). The zeros are
/// therefore part of the stream and belong in the expected content.
///
/// The three documented refusals are checked here too: `SEEK_END` is not supported at all
/// (L383-L385), an unrecognised `whence` is refused by the same test, and a writing stream
/// cannot go backwards (L411-L413).
fn write_seek(handle: &Handle, plan: &Plan, content: &mut Vec<u8>) {
    assert_eq!(handle.seek(0, SEEK_END), -1, "gzseek must refuse SEEK_END");
    assert_eq!(
        handle.seek(0, SEEK_NONSENSE),
        -1,
        "gzseek must refuse an unrecognised whence"
    );
    assert_eq!(
        handle.seek(-1, SEEK_CUR),
        -1,
        "a writing stream cannot seek backwards"
    );
    assert_eq!(
        handle.tell(),
        as_off64(content.len()),
        "a refused gzseek must leave the position alone"
    );

    let skip = plan.write_skip;
    let landed = handle.seek(as_off64(skip), SEEK_CUR);
    assert_eq!(
        landed,
        as_off64(content.len() + skip),
        "a forward gzseek reports the position it will reach"
    );
    content.resize(content.len() + skip, 0);
    assert_eq!(
        handle.tell(),
        as_off64(content.len()),
        "gztell counts a pending skip as already covered"
    );
}

/// `gzoffset` and `gzoffset64` must agree, be non-negative, and never claim to be past the end
/// of the file.
///
/// `gzlib.c` L467-L483 computes it as `lseek(fd, 0, SEEK_CUR)` less any buffered input, so it
/// is the count of compressed bytes actually consumed or produced, and it cannot exceed what
/// is on disk.
fn assert_offset_sane(handle: &Handle, path: &Path, narrow: bool) -> z_off64_t {
    let wide = handle.offset();
    assert!(wide >= 0, "gzoffset64 must not fail on a live stream");
    if narrow {
        assert_eq!(
            z_off64_t::from(handle.offset_narrow()),
            wide,
            "gzoffset and gzoffset64 must agree for a file this small"
        );
    }
    if let Ok(meta) = std::fs::metadata(path) {
        let on_disk = meta.len();
        assert!(
            u64::try_from(wide).unwrap_or(u64::MAX) <= on_disk,
            "gzoffset64 reported {wide} for a file of {on_disk} bytes"
        );
    }
    wide
}

/// Every write entry point must refuse a reading stream with its documented sentinel.
fn assert_write_ops_refused(handle: &Handle) {
    assert_eq!(
        handle.write(b"x"),
        0,
        "gzwrite must refuse a reading stream (gzwrite.c L259-L263)"
    );
    assert_eq!(
        handle.fwrite(b"xy", 2),
        0,
        "gzfwrite must refuse a reading stream"
    );
    assert_eq!(handle.putc(b'x'), -1, "gzputc must refuse a reading stream");
    assert_eq!(
        handle.puts(b"x\0"),
        -1,
        "gzputs must refuse a reading stream"
    );
    assert_eq!(
        handle.flush(Z_FINISH),
        Z_STREAM_ERROR,
        "gzflush must refuse a reading stream"
    );
    assert_eq!(
        handle.setparams(Z_BEST_COMPRESSION, Z_DEFAULT_STRATEGY),
        Z_STREAM_ERROR,
        "gzsetparams must refuse a reading stream"
    );
    assert_eq!(
        handle.close_mismatched(CloseWith::Writer),
        Z_STREAM_ERROR,
        "gzclose_w must refuse a reading stream"
    );
}

/// Every read entry point must refuse a writing stream with its documented sentinel.
///
/// The sentinels differ by return type and that is part of the contract: the two counting
/// forms answer 0, the `int`-returning forms answer -1, `gzgets` answers `NULL`.
fn assert_read_ops_refused(handle: &Handle) {
    let mut sink = [0_u8; 4];
    assert_eq!(
        handle.read(&mut sink),
        -1,
        "gzread must refuse a writing stream (gzread.c L403-L404)"
    );
    assert_eq!(
        handle.fread(&mut sink, 1),
        0,
        "gzfread must refuse a writing stream"
    );
    assert!(
        !handle.gets(&mut sink),
        "gzgets must refuse a writing stream"
    );
    assert_eq!(handle.getc(), -1, "gzgetc must refuse a writing stream");
    assert_eq!(handle.getc_(), -1, "gzgetc_ must refuse a writing stream");
    assert_eq!(
        handle.ungetc(0),
        -1,
        "gzungetc must refuse a writing stream"
    );
    assert_eq!(handle.rewind(), -1, "gzrewind must refuse a writing stream");
    assert_eq!(
        handle.close_mismatched(CloseWith::Reader),
        Z_STREAM_ERROR,
        "gzclose_r must refuse a writing stream"
    );
}

// ---------------------------------------------------------------------------
//  Phase E -- the read pass and the round-trip assertion
// ---------------------------------------------------------------------------

/// What a reading stream must do with the file the write pass left behind.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ReadOutcome {
    /// The bytes read back must equal the expectation exactly. The headline case.
    Exact,
    /// Reading must fail cleanly: no payload byte delivered, and an error recorded.
    CleanFailure,
    /// The file's own bytes decide what happens, so only "does not crash" is asserted.
    Unspecified,
}

/// Classifies what a reader configured by `spec` must do with `image`.
///
/// The boundaries are exactly where `gz_look` (`gzread.c` L93-L170) puts them.
///
/// * A file the library wrote as gzip round-trips under either read mode: `G` asks for a gzip
///   member and finds one, and auto-detect recognises the signature and does the same.
/// * A file holding raw bytes -- a transparent (`T`) write, or an iteration where nothing was
///   written -- round-trips under auto-detect only. Under `G`, `gz_look` goes straight to
///   `GZIP` (L127-L133) and `inflate` rejects the content: `Z_DATA_ERROR` once two bytes have
///   contradicted the signature, `Z_BUF_ERROR` when there were fewer than two bytes to
///   contradict it with. Either way not one payload byte is delivered, and that is what is
///   asserted rather than the specific code.
/// * Raw bytes that themselves begin like a gzip member are the single content-dependent case.
///   Auto-detect applies the four-byte test at L151-L153, and a match sends never-compressed
///   data down the inflate path; `G` skips the test and needs only the first two bytes to look
///   plausible. Whether that ends in an error, a short read or a coincidence is a property of
///   the bytes, so nothing beyond survival is claimed.
fn classify(spec: &ModeSpec, image: &FileImage) -> ReadOutcome {
    if image.gzip {
        return ReadOutcome::Exact;
    }
    let raw = image.logical.as_slice();
    if spec.gzip_only_read() {
        if raw.len() >= 2 && raw[0] == 31 && raw[1] == 139 {
            ReadOutcome::Unspecified
        } else {
            ReadOutcome::CleanFailure
        }
    } else if raw.len() > 3 && raw[0] == 31 && raw[1] == 139 && raw[2] == 8 && raw[3] < 32 {
        ReadOutcome::Unspecified
    } else {
        ReadOutcome::Exact
    }
}

/// Reopens the scratch file for reading and verifies what comes back.
fn read_pass(path: &Path, path_c: &[u8], plan: &Plan, image: &FileImage) -> Option<()> {
    let spec = plan.read_spec;
    let kind = choose_kind(plan, spec, image.present, flag::DOPEN_READ);
    let mut handle = match open_stream(path, path_c, &plan.read_mode, spec, kind, image.present) {
        Opened::Live(handle) => handle,
        Opened::Refused => return Some(()),
        Opened::Environment => return None,
    };
    let Some(spec) = spec else {
        unreachable!("a live handle implies the grammar accepted the mode string")
    };

    if spec.direction == Direction::Read {
        read_stream(&mut handle, path, plan, image, &spec)
    } else {
        // A "read" mode string that selected writing has just truncated the file. There is
        // nothing left to round-trip, but the read entry points must still refuse it.
        if plan.has(flag::ILLEGAL_OPS) {
            assert_read_ops_refused(&handle);
        }
        finish_write_handle(&mut handle)
    }
}

/// Closes a writing handle and checks the code.
fn finish_write_handle(handle: &mut Handle) -> Option<()> {
    let code = handle.close_as(CloseWith::Dispatch);
    if code == Z_ERRNO {
        return None;
    }
    assert_eq!(code, Z_OK, "closing a writing stream must succeed");
    Some(())
}

/// Drives the reading stream: the round trip, then the wider surface, then the close.
fn read_stream(
    handle: &mut Handle,
    path: &Path,
    plan: &Plan,
    image: &FileImage,
    spec: &ModeSpec,
) -> Option<()> {
    let outcome = classify(spec, image);
    assert_eq!(
        handle.tell(),
        0,
        "a fresh reading stream is at position zero"
    );
    assert_eq!(handle.eof(), 0, "nothing has been read past the end yet");

    // Before anything that allocates: `gz_look` assigns `state->size` and from then on
    // `gzbuffer` can only fail, and `gzdirect` calls `gz_look`.
    if plan.has(flag::SET_BUFFER) {
        assert_eq!(
            handle.buffer(plan.buffer_size),
            0,
            "gzbuffer must be accepted before the first read"
        );
    }
    if outcome != ReadOutcome::Unspecified {
        // `gz_look` forces `direct` to 0 when `G` asked for gzip only (`gzread.c` L131), finds
        // 0 for a real gzip member, and leaves the transparent assumption of 1 otherwise --
        // including for an empty file, which is why `gzdirect` answers true for one.
        assert_eq!(
            handle.direct(),
            c_int::from(!spec.gzip_only_read() && !image.gzip),
            "gzdirect must agree with how the file is being read"
        );
    }
    if plan.has(flag::SET_BUFFER) {
        assert_eq!(
            handle.buffer(plan.buffer_size),
            -1,
            "gzbuffer must be refused once the working buffers exist"
        );
    }

    match outcome {
        ReadOutcome::CleanFailure => assert_clean_failure(handle),
        ReadOutcome::Unspecified => drain_unchecked(handle),
        ReadOutcome::Exact => exact_round_trip(handle, path, plan, image),
    }

    if plan.has(flag::ERROR_PROBE) {
        error_probe(handle);
    }
    if plan.has(flag::ILLEGAL_OPS) {
        assert_write_ops_refused(handle);
    }
    if plan.has(flag::WRONG_CLOSE) {
        assert_eq!(
            handle.close_mismatched(CloseWith::Writer),
            Z_STREAM_ERROR,
            "gzclose_w must refuse a reading stream"
        );
    }
    let which = if plan.has(flag::CLOSE_R) {
        CloseWith::Reader
    } else {
        CloseWith::Dispatch
    };
    let code = handle.close_as(which);
    if code == Z_ERRNO {
        return None;
    }
    // `gzread.c` L659-L661 maps the stored error to exactly these two: a pending `Z_BUF_ERROR`
    // is passed through, and anything else -- including `Z_DATA_ERROR` -- closes as `Z_OK`.
    assert!(
        matches!(code, Z_OK | Z_BUF_ERROR),
        "closing a reading stream must report Z_OK or a pending Z_BUF_ERROR, got {code}"
    );
    Some(())
}

/// A gzip-only read of data that is not gzip: no bytes, and an error to explain it.
fn assert_clean_failure(handle: &Handle) {
    let mut sink = [0_u8; 64];
    let got = handle.read(&mut sink);
    assert!(
        got <= 0,
        "a gzip-only read of non-gzip data must not deliver bytes, got {got}"
    );
    let Some((errnum, _)) = handle.error() else {
        panic!("gzerror must answer for a live stream")
    };
    assert!(
        matches!(errnum, Z_DATA_ERROR | Z_BUF_ERROR),
        "a gzip-only read of non-gzip data must record Z_DATA_ERROR or Z_BUF_ERROR, got {errnum}"
    );
}

/// Reads until the stream stops giving, asserting nothing about the bytes.
///
/// For the one content-dependent case. The bound is what keeps a misbehaving library from
/// turning this into a hang, which libFuzzer would report as a timeout.
fn drain_unchecked(handle: &Handle) {
    let mut sink = [0_u8; 256];
    for _ in 0..=(MAX_PAYLOAD + MAX_ZERO_RUN) {
        if handle.read(&mut sink) <= 0 {
            break;
        }
    }
}

/// Reads the whole expectation back and compares it byte for byte.
///
/// **This is the target's headline assertion.** Everything above exists so that `expected` is
/// known exactly; everything below drives a fuzzer-chosen mixture of the five reading entry
/// points -- plus the `gzgetc` macro simulated from caller code -- and then holds the result
/// against it.
fn exact_round_trip(handle: &Handle, path: &Path, plan: &Plan, image: &FileImage) {
    let expected = image.logical.as_slice();
    let chunk = effective_chunk(plan.read_chunk, expected.len());
    let mut ops = OpStream::new(&plan.ops);
    let mut got: Vec<u8> = Vec::with_capacity(expected.len());
    let mut last_offset: z_off64_t = 0;

    for _ in 0..MAX_DATA_OPS {
        let selector = ops.next();
        let before = handle.tell();
        let produced = read_data_op(handle, selector, plan, chunk, expected, &mut got);
        let after = handle.tell();
        assert_eq!(
            after - before,
            as_off64(produced),
            "gztell must move by exactly what a read consumed"
        );
        assert_eq!(
            usize::try_from(after).unwrap_or(usize::MAX),
            got.len(),
            "gztell must equal the number of uncompressed bytes delivered"
        );
        if produced == 0 {
            break;
        }
        if plan.has(flag::OFFSET) {
            let offset = assert_offset_sane(handle, path, plan.has(flag::NARROW_OFFSETS));
            assert!(
                offset >= last_offset,
                "gzoffset64 must not go backwards while reading forwards"
            );
            last_offset = offset;
        }
    }

    // Finish with plain reads, in case the operation cap cut the loop short or the last chosen
    // operation happened to consume nothing.
    let mut sink = vec![0_u8; chunk];
    let mut reached_end = false;
    for _ in 0..=(expected.len() + 2) {
        let n = handle.read(&mut sink);
        assert!(
            n >= 0,
            "gzread must not fail while draining a healthy stream"
        );
        let n = usize::try_from(n).unwrap_or(0);
        if n == 0 {
            reached_end = true;
            break;
        }
        got.extend_from_slice(&sink[..n]);
    }
    assert!(reached_end, "reading a bounded file must terminate");

    assert_eq!(
        got.as_slice(),
        expected,
        "the gz layer must return exactly the bytes it was given"
    );
    // `gzeof` reports `past`, which is set only once a request could not be satisfied because
    // the input ended (`gzread.c` L388-L389) -- not merely because the position is at the end.
    // The drain above ends with exactly such a request.
    assert_ne!(
        handle.eof(),
        0,
        "gzeof must report true once a read has gone past the end"
    );

    if plan.has(flag::READ_SEEK) {
        read_seek_checks(handle, plan, expected);
    }
    if plan.has(flag::REWIND) {
        rewind_check(handle, expected);
    }
    if plan.has(flag::UNGETC_SATURATE) {
        ungetc_saturation(handle, plan);
    }
}

/// Performs one read and appends what it produced to `got`, returning the byte count.
///
/// Two branches cannot trust their return value to be a byte count and measure consumption
/// from `gztell` deltas instead. `gzfread` reports **whole items** and rounds a partial one
/// away while still having consumed and delivered its bytes (`gzread.c` L464), and `gzgets`
/// NUL-terminates a string that may itself contain NULs, so `strlen` is not its length.
fn read_data_op(
    handle: &Handle,
    selector: u8,
    plan: &Plan,
    chunk: usize,
    expected: &[u8],
    got: &mut Vec<u8>,
) -> usize {
    match selector % 6 {
        0 => {
            let mut sink = vec![0_u8; chunk];
            let n = handle.read(&mut sink);
            assert!(n >= 0, "gzread must not fail on a healthy stream");
            let n = usize::try_from(n).unwrap_or(0);
            got.extend_from_slice(&sink[..n]);
            n
        }
        1 => read_fread(handle, plan, chunk, got),
        2 => read_gets(handle, plan, expected, got),
        3 => {
            let byte = if plan.has(flag::GETC_UNDERSCORE) {
                handle.getc_()
            } else {
                handle.getc()
            };
            push_byte(byte, got)
        }
        4 => read_ungetc_cycle(handle, got),
        // Phase F: the `gzgetc` macro, performed the way caller object code performs it.
        _ => read_macro_take(handle, expected, got),
    }
}

/// `gzfread`, measured by position rather than by its item count.
fn read_fread(handle: &Handle, plan: &Plan, chunk: usize, got: &mut Vec<u8>) -> usize {
    let size = plan.item_size.min(chunk).max(1);
    let mut sink = vec![0_u8; chunk.max(size)];
    let before = handle.tell();
    let items = handle.fread(&mut sink, size);
    let after = handle.tell();
    let consumed = usize::try_from(after - before).unwrap_or(0);
    assert!(
        consumed <= sink.len(),
        "gzfread consumed {consumed} bytes for a {}-byte buffer",
        sink.len()
    );
    // `gzread.c` L464 divides the byte count by the item size, so the reported item count is
    // the floor of what was actually consumed.
    assert_eq!(
        items,
        consumed / size,
        "gzfread must report the whole items inside what it consumed"
    );
    got.extend_from_slice(&sink[..consumed]);
    consumed
}

/// `gzgets`, and its three contract clauses.
///
/// `gzread.c` L565-L624: it reads until a newline or `len - 1` bytes, NUL-terminates, and
/// answers `NULL` -- rather than the buffer -- exactly when it read nothing at all.
fn read_gets(handle: &Handle, plan: &Plan, expected: &[u8], got: &mut Vec<u8>) -> usize {
    let len = plan.gets_len;
    let mut sink = vec![0_u8; len];
    let before = handle.tell();
    let answered = handle.gets(&mut sink);
    let after = handle.tell();
    let consumed = usize::try_from(after - before).unwrap_or(0);
    assert_eq!(
        answered,
        consumed > 0,
        "gzgets answers NULL exactly when it read nothing"
    );
    assert!(
        consumed < len,
        "gzgets must leave room for its terminator: {consumed} bytes in a buffer of {len}"
    );
    if consumed > 0 {
        assert_eq!(sink[consumed], 0, "gzgets must NUL-terminate what it read");
        got.extend_from_slice(&sink[..consumed]);
        if consumed < len - 1 && got.len() < expected.len() {
            assert_eq!(
                sink[consumed - 1],
                b'\n',
                "gzgets stops short of its buffer only at a newline or at end of input"
            );
        }
    }
    consumed
}

/// `gzgetc`, then `gzungetc`, then `gzgetc` again: the byte pushed back must be the next one
/// delivered, and the position must come back with it.
///
/// `gzread.c` L532-L540 handles the push into an empty output buffer by parking the byte at the
/// very end of it, and L537 decrements `x.pos` -- which is a field of the caller-visible prefix,
/// so `gztell` has to reflect the push as well.
fn read_ungetc_cycle(handle: &Handle, got: &mut Vec<u8>) -> usize {
    let before = handle.tell();
    let first = handle.getc();
    if first < 0 {
        return 0;
    }
    assert_eq!(
        handle.ungetc(first),
        first,
        "gzungetc must answer with the byte it accepted"
    );
    assert_eq!(
        handle.tell(),
        before,
        "gzungetc must undo the position the byte advanced"
    );
    assert_eq!(
        handle.getc(),
        first,
        "the pushed-back byte must be the next one delivered"
    );
    push_byte(first, got)
}

/// Phase F: consume a byte through the caller-visible prefix, exactly as the `gzgetc` macro
/// does, and prove the stream is still coherent afterwards.
///
/// This is the only place in the suite that drives the macro's arithmetic from Rust, and it is
/// the reason the compile-time offset assertions at the top of this file exist. What it
/// validates is not the macro -- there is no macro here -- but the library's side of the
/// bargain: that the state really does begin with the 24-byte prefix, and that the next entry
/// point recovers its own cursor from the values the caller left behind rather than from a
/// private copy it kept.
///
/// When `have` is zero the macro takes its function branch, and `zlib.h` L1961 keeps `gzgetc_`
/// for precisely that; nothing else in the suite calls it, so it is called here.
fn read_macro_take(handle: &Handle, expected: &[u8], got: &mut Vec<u8>) -> usize {
    let Some((before_have, _, before_pos)) = handle.prefix() else {
        panic!("the exposed prefix must be readable for a live handle")
    };
    match handle.macro_take() {
        Some(byte) => {
            let position = got.len();
            match expected.get(position) {
                Some(&want) => assert_eq!(
                    byte, want,
                    "the gzgetc macro delivered {byte} at position {position}, expected {want}"
                ),
                None => {
                    panic!("the gzgetc macro delivered a byte past the end of the written data")
                }
            }
            let Some((after_have, _, after_pos)) = handle.prefix() else {
                panic!("the exposed prefix must still be readable after a macro take")
            };
            assert_eq!(
                after_have,
                before_have - 1,
                "the macro decrements have by one"
            );
            assert_eq!(after_pos, before_pos + 1, "the macro increments pos by one");
            got.push(byte);
            1
        }
        None => push_byte(handle.getc_(), got),
    }
}

/// Appends a byte a single-byte read produced, or reports end of input.
fn push_byte(value: c_int, got: &mut Vec<u8>) -> usize {
    if value < 0 {
        return 0;
    }
    let byte = u8::try_from(value & 0xff).unwrap_or(0);
    got.push(byte);
    1
}

// ---------------------------------------------------------------------------
//  Phase G -- the wider surface
// ---------------------------------------------------------------------------

/// Seeks around the reading stream and re-reads from where it landed.
///
/// Three distinct paths, all in `gzseek64` (`gzlib.c` L367-L433): the raw fast path at
/// L395-L409 for a transparent stream, a deferred `state->skip` resolved by `gz_skip`
/// (`gzread.c` L281-L309) for a forward move in a gzip stream, and `gzrewind` followed by a
/// skip for a backward one (L411-L419).
///
/// Targets are kept inside the payload on purpose. A seek past the end is legal and answers the
/// requested position, but the read that follows stops at the end and sets `past`, after which
/// `gztell` stops counting the unspent skip (`gzlib.c` L456-L457) -- so the position accounting
/// stops being a simple function of what the harness asked for, and asserting on it would be
/// asserting on the harness's own arithmetic rather than the library's.
fn read_seek_checks(handle: &Handle, plan: &Plan, expected: &[u8]) {
    assert_eq!(handle.seek(0, SEEK_END), -1, "gzseek must refuse SEEK_END");
    assert_eq!(
        handle.seek(0, SEEK_NONSENSE),
        -1,
        "gzseek must refuse an unrecognised whence"
    );
    assert_eq!(
        handle.seek(-1, SEEK_SET),
        -1,
        "gzseek must refuse a position before the start of the file"
    );

    let target = plan.seek_target % (expected.len() + 1);
    let landed = handle.seek(as_off64(target), SEEK_SET);
    assert_eq!(
        landed,
        as_off64(target),
        "gzseek must report the position it was asked for"
    );
    assert_eq!(
        handle.tell(),
        as_off64(target),
        "gztell must agree with the position gzseek reported"
    );
    if plan.has(flag::NARROW_OFFSETS) {
        assert_eq!(
            z_off64_t::from(handle.tell_narrow()),
            as_off64(target),
            "gztell and gztell64 must agree"
        );
    }
    assert_read_matches(handle, &expected[target..], "after an absolute seek");

    assert_eq!(
        handle.seek(0, SEEK_SET),
        0,
        "seeking back to the start must succeed"
    );
    let delta = plan.seek_delta % (expected.len() + 1);
    let landed = handle.seek(as_off64(delta), SEEK_CUR);
    assert_eq!(
        landed,
        as_off64(delta),
        "a relative seek from the start lands at the distance asked for"
    );
    assert_read_matches(handle, &expected[delta..], "after a relative seek");

    // The narrow form has to agree with the wide one for a file this small; `gzlib.c` L438-L440
    // only substitutes -1 when the value will not fit a `z_off_t`.
    assert_eq!(
        handle.seek_narrow(0, SEEK_SET),
        0,
        "gzseek must behave as gzseek64 does for representable positions"
    );
    assert_read_matches(handle, expected, "after a narrow seek to the start");
}

/// `gzrewind` returns the stream to where the data started and resets it (`gzlib.c` L346-L365),
/// so what follows must reproduce the payload from the beginning.
fn rewind_check(handle: &Handle, expected: &[u8]) {
    assert_eq!(
        handle.rewind(),
        0,
        "gzrewind must succeed on a healthy reading stream"
    );
    assert_eq!(
        handle.tell(),
        0,
        "gzrewind must put the stream back at position zero"
    );
    assert_eq!(handle.eof(), 0, "gzrewind must clear the past-end flag");
    assert_read_matches(handle, expected, "after gzrewind");
}

/// Drains the stream with plain `gzread` and requires exactly `expected`.
fn assert_read_matches(handle: &Handle, expected: &[u8], context: &str) {
    let mut got: Vec<u8> = Vec::with_capacity(expected.len());
    let mut sink = [0_u8; 256];
    for _ in 0..=(expected.len() + 2) {
        let n = handle.read(&mut sink);
        assert!(n >= 0, "gzread failed {context}");
        let n = usize::try_from(n).unwrap_or(0);
        if n == 0 {
            break;
        }
        got.extend_from_slice(&sink[..n]);
    }
    assert_eq!(
        got.as_slice(),
        expected,
        "the stream must deliver the right bytes {context}"
    );
}

/// Pushes bytes back until `gzungetc` runs out of room.
///
/// `gzread.c` L532-L546: the first push into an empty output buffer parks the byte at the very
/// end of it so that more can follow, each later one slides the data down, and the limit is
/// `size << 1` bytes -- at which point the call records `Z_DATA_ERROR` and answers -1.
///
/// The default buffer makes that sixteen thousand pushes, so this runs only when `gzbuffer`
/// asked for a small one. `gzlib.c` L340-L341 raises anything below eight to eight, so the
/// floor is sixteen pushes and the ceiling here is a hundred and twenty-eight.
///
/// Deliberately last: it leaves the stream holding bytes that were never in the file, so it must
/// not precede anything that reads.
fn ungetc_saturation(handle: &Handle, plan: &Plan) {
    if !plan.has(flag::SET_BUFFER) {
        return;
    }
    let want = plan.buffer_size.max(8);
    if want > 64 {
        return;
    }
    let limit = usize::try_from(want).unwrap_or(usize::MAX) * 2;
    let mut accepted = 0_usize;
    let mut refused = false;
    for _ in 0..=limit {
        if handle.ungetc(c_int::from(b'Z')) == -1 {
            refused = true;
            break;
        }
        accepted += 1;
    }
    assert!(
        refused,
        "gzungetc must eventually run out of room; it took {accepted} pushes without refusing"
    );
    // The stream is drained at this point, so `x.have` was zero and the buffer had all
    // `size << 1` slots free.
    assert_eq!(
        accepted, limit,
        "gzungetc must accept exactly `size << 1` pushes into an empty output buffer"
    );
    let Some((errnum, _)) = handle.error() else {
        panic!("gzerror must answer for a live stream")
    };
    assert_eq!(
        errnum, Z_DATA_ERROR,
        "running out of room to push must be recorded as Z_DATA_ERROR"
    );
}

/// `gzerror` must answer for a live stream, in both its forms, and `gzclearerr` must put the
/// stream back to `Z_OK` with no message and no past-end flag.
///
/// The message is read as **bytes** up to the NUL rather than as text: `gz_error` builds it from
/// the path (`gzlib.c` L577-L586), and a path is an arbitrary byte string on Unix.
fn error_probe(handle: &Handle) {
    let Some((_, message)) = handle.error() else {
        panic!("gzerror must answer for a live stream")
    };
    assert!(
        message.len() <= MAX_PAYLOAD,
        "an error message must be bounded, got {} bytes",
        message.len()
    );
    assert!(
        handle.error_message_only(),
        "gzerror must answer without an errnum slot too"
    );
    handle.clearerr();
    let Some((errnum, cleared)) = handle.error() else {
        panic!("gzerror must answer after gzclearerr")
    };
    assert_eq!(errnum, Z_OK, "gzclearerr must reset the error number");
    assert!(
        cleared.is_empty(),
        "gzclearerr must leave an empty message, got {cleared:?}"
    );
    assert_eq!(
        handle.eof(),
        0,
        "gzclearerr must clear the past-end flag on a reading stream"
    );
}

/// Every entry point must answer a null handle with its documented sentinel rather than faulting.
///
/// One sweep covers the whole guard surface, and the sentinels are deliberately not uniform --
/// which is the point, because each one is what some caller tests against:
///
/// | Answer | Entry points |
/// |---|---|
/// | 0 | `gzeof` (`gzlib.c` L498-L499), `gzdirect` (`gzread.c` L631-L632), `gzfread`, `gzfwrite`, `gzwrite` |
/// | -1 | `gzbuffer`, `gzread`, `gzgetc`, `gzgetc_`, `gzungetc`, `gzputc`, `gzputs`, `gzseek`, `gzseek64`, `gzrewind`, `gztell`, `gztell64`, `gzoffset`, `gzoffset64` |
/// | `Z_STREAM_ERROR` | `gzsetparams`, `gzflush`, `gzclose`, `gzclose_r`, `gzclose_w` |
/// | `NULL` | `gzgets`, `gzerror`, `gzopen`, `gzopen64`, `gzdopen` |
/// | nothing | `gzclearerr`, which returns void |
fn null_handle_sweep() {
    // `Z_NULL` is documented input for every one of these, and the gate that carries it says so:
    // each entry point answers from a guard at its head without dereferencing anything, so a
    // null handle owes no close and needs no `Handle`.
    let null = GzFile::null();
    let mut sink = [0_u8; 4];
    let text = c"gz";

    assert_eq!(null.buffer(8), -1, "gzbuffer(NULL)");
    assert_eq!(null.setparams(1, 0), Z_STREAM_ERROR, "gzsetparams(NULL)");

    assert_eq!(null.read(&mut sink), -1, "gzread(NULL)");
    assert_eq!(null.fread_items(&mut sink, 1, 4), 0, "gzfread(NULL)");
    assert!(null.gets(&mut sink).is_none(), "gzgets(NULL)");
    assert_eq!(null.getc(), -1, "gzgetc(NULL)");
    assert_eq!(null.getc_(), -1, "gzgetc_(NULL)");
    assert_eq!(null.ungetc(0), -1, "gzungetc(NULL)");

    assert_eq!(null.write(text.to_bytes()), 0, "gzwrite(NULL)");
    assert_eq!(
        null.fwrite_items(text.to_bytes(), 1, 2),
        0,
        "gzfwrite(NULL)"
    );
    assert_eq!(null.putc(c_int::from(b'x')), -1, "gzputc(NULL)");
    assert_eq!(null.puts(text), -1, "gzputs(NULL)");
    assert_eq!(null.flush(Z_FINISH), Z_STREAM_ERROR, "gzflush(NULL)");

    assert_eq!(null.seek(0, SEEK_SET), -1, "gzseek(NULL)");
    assert_eq!(null.seek64(0, SEEK_SET), -1, "gzseek64(NULL)");
    assert_eq!(null.rewind(), -1, "gzrewind(NULL)");
    assert_eq!(null.tell(), -1, "gztell(NULL)");
    assert_eq!(null.tell64(), -1, "gztell64(NULL)");
    assert_eq!(null.offset(), -1, "gzoffset(NULL)");
    assert_eq!(null.offset64(), -1, "gzoffset64(NULL)");

    // The two that answer zero rather than -1, which is easy to get wrong in a rewrite.
    assert_eq!(null.eof(), 0, "gzeof(NULL) is 0, not -1");
    assert_eq!(null.direct(), 0, "gzdirect(NULL) is 0, not -1");

    // `Z_STREAM_ERROR` as the sentinel, so "the library wrote nothing" is distinguishable; the
    // message being `None` is C's null return, which is what `gzerror(Z_NULL, &errnum)` owes.
    let (errnum, message) = null.error(Z_STREAM_ERROR);
    assert!(message.is_none(), "gzerror(NULL)");
    assert_eq!(
        errnum, Z_STREAM_ERROR,
        "gzerror(NULL) must not write the errnum slot"
    );
    assert!(!null.error_message_only(), "gzerror(NULL, NULL)");
    null.clearerr();

    assert_eq!(null.close(), Z_STREAM_ERROR, "gzclose(NULL)");
    assert_eq!(null.close_r(), Z_STREAM_ERROR, "gzclose_r(NULL)");
    assert_eq!(null.close_w(), Z_STREAM_ERROR, "gzclose_w(NULL)");

    // The open family's own null arguments. `gzlib.c` L96-L97 refuses a null path or mode, and
    // L302 refuses `fd == -1` before it even looks at the mode -- which is why the descriptor
    // case is probed with -1 and not with a real descriptor: `gzdopen` adopts whatever it is
    // given, and there is no descriptor here that is safe to lose. A null *string* cannot be
    // spelled as a `&CStr`, so those three have gates of their own.
    assert!(GzFile::open_null_path(text).is_null(), "gzopen(NULL path)");
    assert!(GzFile::open_null_mode(text).is_null(), "gzopen(NULL mode)");
    assert!(
        GzFile::open64_null_path(text).is_null(),
        "gzopen64(NULL path)"
    );
    assert!(GzFile::dopen(-1, text).is_null(), "gzdopen(-1)");
}

// ---------------------------------------------------------------------------
//  The driver
// ---------------------------------------------------------------------------

/// One invocation: prepare the file, write, read back, compare.
///
/// [`None`] means the fuzzer's own choices led nowhere -- a mode string the grammar refuses, or a
/// transient filesystem refusal of the kind [`Opened::Environment`] documents -- rather than the
/// library misbehaving, and is deliberately not a failure. Everything that *is* a failure has
/// already asserted by the time this returns.
///
/// The one thing that is no longer allowed to be quiet is a scratch environment that never
/// worked: [`preflight`] settles that once, with a panic, before the first plan is built.
///
/// # ★ ONE POLICY FOR FILESYSTEM REFUSALS, AND ONLY ONE
///
/// There used to be two, and they contradicted each other. [`Opened::Environment`] documents
/// that a mid-run refusal is *tolerated* -- "reporting it as a crash would yield a reproducer
/// that says nothing about the library" -- while the step that prepares the scratch file said
/// the opposite by calling `.expect()` on it, so a `/tmp` that filled up half way through a
/// 300-second run produced a libFuzzer crash and an artifact that no amount of staring at
/// would explain. Both positions have a real concern behind them: a refusal must not be
/// silent forever, and it must not masquerade as a finding.
///
/// The policy that satisfies both, and is now the only one in this file:
///
/// * A refusal is **tolerated** and the iteration is abandoned, exactly as
///   [`Opened::Environment`] says. [`CONSECUTIVE_REFUSALS`] counts it.
/// * A **success resets the count**, so a transient full disk costs nothing once it clears.
/// * [`MAX_CONSECUTIVE_REFUSALS`] refusals in a row is no longer transient, and *that* fails
///   loudly -- with a message that names the environment as the cause and says in terms that
///   the artifact is not a reproducer, so nobody spends an afternoon on it.
///
/// What is never done is manufacturing a product reproducer out of an environment fault.
fn attempt(input: &GzRoundTrip) -> Option<()> {
    preflight();

    let plan = build_plan(input)?;
    if plan.has(flag::NULL_SWEEP) {
        null_handle_sweep();
    }

    let path = scratch_path();
    let path_c =
        path_bytes(path).expect("preflight proved the scratch path is expressible as a C string");

    // `O_EXCL` needs the file absent to succeed and present to be refused, and both are worth
    // exercising; the fuzzer picks which, and the prediction in `predict_open` follows from the
    // same fact.
    let exclusive_write = plan
        .write_spec
        .is_some_and(|spec| spec.exclusive && spec.direction != Direction::Read);
    let remove = exclusive_write && plan.has(flag::EXCLUSIVE_SUCCEEDS);

    let Some(exists) = prepare_scratch(path, remove) else {
        note_filesystem_refusal(path);
        return None;
    };
    note_filesystem_success();
    let image = write_pass(path, &path_c, &plan, exists)?;
    read_pass(path, &path_c, &plan, &image)
}

/// How many consecutive filesystem refusals are treated as transient before the run is failed.
///
/// Sized against what it has to distinguish. A genuinely transient refusal -- a disk that is
/// briefly full, a sibling job racing on the same directory -- clears within a handful of
/// iterations, and this target runs hundreds per second, so sixty-four in a row is far outside
/// that. A permanently broken environment reaches sixty-four in a fraction of a second, so the
/// diagnosis is prompt rather than deferred to the end of the budget.
const MAX_CONSECUTIVE_REFUSALS: usize = 64;

/// Consecutive filesystem refusals since the last success. See [`attempt`]'s policy note.
static CONSECUTIVE_REFUSALS: AtomicUsize = AtomicUsize::new(0);

/// Records one tolerated filesystem refusal, failing only once they stop looking transient.
fn note_filesystem_refusal(path: &Path) {
    let seen = CONSECUTIVE_REFUSALS.fetch_add(1, Ordering::Relaxed) + 1;
    if seen < MAX_CONSECUTIVE_REFUSALS {
        return;
    }
    // Deliberately a panic and deliberately worded. libFuzzer offers no other failure
    // channel -- `std::process::exit` is denied by this crate's lints and would be reported
    // as a crash anyway -- so the message has to carry what the mechanism cannot.
    panic!(
        "ENVIRONMENT FAILURE, NOT A LIBRARY FINDING. The scratch file {path} could not be \
         created or truncated {seen} times in a row, so this target has stopped doing any \
         work. preflight proved the temporary directory was usable when this process \
         started, so it has become unusable since -- full, unmounted, read-only, or removed \
         underneath us. Any artifact libFuzzer writes alongside this message is NOT a \
         reproducer and re-running it will prove nothing: fix the temporary directory or \
         point TMPDIR somewhere writable. A refusal that clears on its own is tolerated and \
         never gets here; see the policy note on `attempt`.",
        path = path.display(),
    )
}

/// Records that the filesystem is working, which is what makes a refusal streak transient.
fn note_filesystem_success() {
    // A relaxed store is right: the only reader is the counter above, the count is a
    // heuristic about the environment rather than a synchronisation point, and libFuzzer
    // drives this target from one thread per job.
    CONSECUTIVE_REFUSALS.store(0, Ordering::Relaxed);
}

fuzz_target!(|input: GzRoundTrip| {
    let outcome = attempt(&input);
    // Off unless `ZLIB_RS_FUZZ_REPORT` is set. `round_trip` means this execution wrote a gz
    // member and read it back with the bytes compared -- the only outcome that exercises the
    // gz layer end to end, and therefore the one a committed seed has to reach.
    // `incomplete` covers both ordinary cases: a mode string the grammar refuses, and a
    // tolerated filesystem refusal. See `zlib_rs_fuzz`.
    reached(
        "fuzz_gz_roundtrip",
        if outcome.is_some() {
            "round_trip"
        } else {
            "incomplete"
        },
        |fields| {
            fields
                .with("payload", input.payload.len())
                .with("level", input.level)
                .with("writeShape", input.write_shape)
                .with("readShape", input.read_shape)
                .with("ops", input.ops.len())
        },
    );
});
