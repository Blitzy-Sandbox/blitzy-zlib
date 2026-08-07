//! Opening a `gzFile`, and resetting one: the Rust counterpart of the first third of `gzlib.c`.
//!
//! This module owns four things, in the order `gzlib.c` presents them:
//!
//! | This module | C origin |
//! |---|---|
//! | `gz_reset` | `gz_reset`, `gzlib.c` L69-L84 |
//! | [`GzOpenSpec::parse`] and `gz_open` | `gz_open`, `gzlib.c` L87-L285 |
//! | [`gzopen`] | `gzopen` and `gzopen64`, `gzlib.c` L288 and L293 |
//! | [`gzdopen`], [`gz_open_handle`] | `gzdopen`, `gzlib.c` L298-L312 |
//! | `gzopen_w` | `gzopen_w`, `gzlib.c` L316-L318, `#ifdef WIDECHAR` |
//!
//! It also supplies [`FileHandle`], the concrete [`GzHandle`] implementation that
//! `crate::gz::state` declares but deliberately does not define, because path-based opening is
//! this module's job and is the only part of the `gzFile` layer that must name a file API at all.
//!
//! # The mode string is a grammar, and it is caller-observable
//!
//! `gz_open` walks the mode string one byte at a time (`gzlib.c` L113-L171). Nothing about that
//! loop may be tidied up, because every one of its decisions is visible to a caller: through
//! `gzdirect`, through which strategy the compressor ends up using, through the compression level
//! the output was produced at, and -- for four of the characters -- through whether `gzopen`
//! returns a stream at all.
//!
//! | Byte | Effect | C origin |
//! |---|---|---|
//! | `0`-`9` | compression level `c - '0'` | L114-L115 |
//! | `r` | read | L118-L119 |
//! | `w` | write | L122-L123 |
//! | `a` | append | L125-L126 |
//! | `+` | **fail**: reading and writing one gzip file is unsupported | L129-L131 |
//! | `b` | ignored; binary is requested unconditionally | L132-L133 |
//! | `e` | close-on-exec (`O_CLOEXEC`) | L135-L136 |
//! | `x` | exclusive create (`O_EXCL`) | L140-L141 |
//! | `f` | `Z_FILTERED` | L144-L145 |
//! | `h` | `Z_HUFFMAN_ONLY` | L147-L148 |
//! | `R` | `Z_RLE` | L150-L151 |
//! | `F` | `Z_FIXED` | L153-L154 |
//! | `G` | `direct = -1`, gzip only | L156-L157 |
//! | `N` | non-blocking (`O_NONBLOCK`) | L160-L161 |
//! | `T` | `direct = 1`, transparent | L164-L165 |
//! | anything else | **silently ignored** | L167-L168 |
//!
//! Later characters override earlier ones, which is why the last `G` or `T` wins and why `"rw"`
//! opens for writing. The `default:` case carries the comment "could consider as an error, but
//! just ignore", and that leniency is load-bearing rather than sloppy: `test/minigzip.c` L511
//! builds the mode string `"wb6 "` and patches index 3 afterwards, so a mode string containing a
//! space reaches this parser and must be accepted. Rejecting unknown characters would break the
//! acceptance suite.
//!
//! # `direct`, and the two ways it can make an open fail
//!
//! After the walk, `gz_open` resolves `direct` (`gzlib.c` L173-L197) and the resolution decides
//! two of the four failure modes:
//!
//! * a mode string that never selected `r`, `w` or `a` fails (L174-L177);
//! * `T` while reading fails -- "can't force a transparent read" (L181-L185);
//! * reading with `direct` still 0 becomes `direct = 1`, because "default when reading is
//!   auto-detect of gzip vs. transparent -- start with a transparent assumption in case of an
//!   empty file" (L186-L189). This is what makes `gzdirect` report true for an empty file, and
//!   leaving it at 0 would be a silent behaviour change;
//! * `G` while writing or appending fails -- "`G` has no meaning when writing" (L191-L195).
//!
//! The surviving meaning is C's comment at L196-L197, reproduced verbatim: "if reading,
//! `direct == 1` for auto-detect, `-1` for gzip only; if writing or appending, `direct == 0` for
//! gzip, 1 for transparent (copy in to out)".
//!
//! # `int fd` becomes an injected handle
//!
//! C stores a descriptor and calls `open`, `read`, `write`, `LSEEK`, `close` and `fcntl` on it.
//! `std::os::fd::FromRawFd::from_raw_fd` is an `unsafe` function, and this crate carries
//! `#![forbid(unsafe_code)]`, so a caller-supplied descriptor cannot be adopted here at all. The
//! two ways a `gzFile` comes into being therefore split along that line:
//!
//! * **by path** -- `gz_open` opens the file itself through [`std::fs::OpenOptions`], which is
//!   entirely safe, and installs a [`FileHandle`];
//! * **by descriptor** -- [`gz_open_handle`] and [`gzdopen`] take an already-constructed
//!   `Box<dyn GzHandle>`. The planned `crates/libz-rs-sys/src/gz.rs` will build it from the raw
//!   descriptor inside its own audited `unsafe` block and hand it in.
//!
//! ## What the facade must do
//!
//! `crates/libz-rs-sys/src/gz.rs` does not exist at this checkpoint, and neither does any other
//! module of that crate except its build script and `panic_guard.rs`. Every "the facade does X"
//! in this file is therefore an obligation on that future file, never a description of code that
//! can be read today; nothing here has been exercised through a compiled C caller. The facade
//! author cannot see this file, so the contract is stated in full.
//!
//! 1. **`gzopen` / `gzopen64`.** Convert the caller's `const char *` to a byte slice excluding
//!    the terminating NUL, reject a null pointer with `NULL` before calling (a null pointer
//!    cannot be represented as a `&[u8]`, so the `path == NULL || mode == NULL` test of
//!    `gzlib.c` L96-L97 belongs on that side), then call [`gzopen`]. Map `Ok` to a heap-allocated
//!    state and every `Err` to `NULL`.
//! 2. **`gzdopen`.** Call [`GzOpenSpec::parse`] **first**. If it fails, return `NULL` without
//!    having touched the descriptor -- `zlib.h` L1415-L1416 promises that "`gzdopen` does not
//!    close `fd` if it fails". Only once the mode string is known good, adopt the descriptor and
//!    call [`gzdopen`]. Should that still fail, take [`GzOpenHandleError::handle`] and release it
//!    **without closing**.
//! 3. **A descriptor-backed `GzHandle` must not close on drop.** Close only in
//!    [`GzHandle::close`]. `GzState`'s teardown calls `close` explicitly on every path that ends
//!    a stream (`crate::gz::state::GzState::teardown`), so the descriptor is still closed when
//!    `gzclose` asks for it -- and a handle handed back out of a failed open is merely dropped,
//!    which is exactly C's behaviour. [`FileHandle`] does the opposite, and correctly so: the
//!    library owns a file it opened by path.
//! 4. **`O_CLOEXEC` on an adopted descriptor is the facade's job.** C applies it with
//!    `fcntl(fd, F_SETFD, ...)` (`gzlib.c` L258-L261); [`GzHandle`] has no descriptor-flag method,
//!    so read [`GzOpenSpec::cloexec`] and apply it while adopting. The `F_SETFL` half --
//!    `O_NONBLOCK`, `gzlib.c` L254-L257 -- *is* expressible, and [`gz_open_handle`] performs it
//!    through [`GzHandle::set_nonblocking`], ignoring the result exactly as C ignores `fcntl`'s.
//! 5. **`gzopen_w`.** Convert the `wchar_t *` yourself and pass the resulting bytes to
//!    `gzopen_w`; see that function's documentation for the encoding requirement.
//!
//! # KNOWN BEHAVIOUR DIVERGENCES FROM C -- the complete inventory
//!
//! Behaviour preservation is this port's governing constraint, so every observable departure from
//! the reference implementation is an **open item**, not a feature. They are collected here rather
//! than scattered, each with a status, so that a drop-in claim can be qualified against a list
//! instead of asserted in general. None of them may be described at its own site as hardening, as
//! "strictly better", or as stricter than C.
//!
//! Statuses used below:
//!
//! * **CLOSABLE AT THE BOUNDARY** -- the facade can restore C's behaviour; the hook exists.
//! * **FORCED, UNRESOLVED** -- reproducing C exactly would require `unsafe` or would reproduce a
//!   memory-safety defect. The difference stands and is recorded as unresolved.
//! * **COSMETIC** -- affects human-readable text only, and no documented value.
//!
//! ## Divergences owned by this file
//!
//! 1. **`close(2)`'s `errno` is not observable.** *(CLOSABLE AT THE BOUNDARY.)* Dropping a [`std::fs::File`] discards the
//!    result, and stable Rust 1.80 offers no safe way to see it. `gzclose_r` and `gzclose_w` must
//!    return `Z_ERRNO` when `close` fails (`gzread.c` L665-L667, `gzwrite.c` L696-L697), so
//!    [`GzHandle::close`] is a distinct operation rather than a drop: a facade handle can back it
//!    with a real `close(2)` and report the `errno`. [`FileHandle::close`] flushes and then drops
//!    the file, reporting any flush failure; it deliberately does **not** `fsync`, because C's
//!    `close` does not either and forcing one would change `gzclose`'s cost rather than its
//!    behaviour.
//! 2. **`io::Error`'s `Display` is not `strerror(errno)`.** *(COSMETIC.)* It appends
//!    `" (os error N)"`, whereas
//!    C feeds `zstrerror()` -- literally `strerror(errno)` (`gzguts.h` L131-L133) -- to
//!    `gz_error`. Nothing in `test/example.c`, `test/minigzip.c` or `test/infcover.c` asserts that
//!    text, so the difference is invisible to the acceptance suite; [`GzIoError::errno`] carries
//!    the number so a facade that wants byte-identical text can call `strerror` itself.
//! 3. **`EAGAIN`/`EWOULDBLOCK` are recognised by kind, not by number.** *(FORCED, UNRESOLVED --
//!    but no divergence has been demonstrated.)* [`std::io::ErrorKind::WouldBlock`] is the safe
//!    stand-in, and it is what sets [`GzIoError::would_block`] -- the flag `gz_avail` and `gz_comp`
//!    record in `state->again` (`gzread.c` L36, `gzwrite.c` L82 and L117). A platform whose
//!    `WouldBlock` mapping omits an errno C would have matched would behave differently here; that
//!    has not been observed and has not been proven impossible either.
//!
//! 4. **`O_CLOEXEC` is set on every path-based open, requested or not.** *(FORCED, UNRESOLVED.)*
//!    `std` sets it unconditionally and offers no safe way to clear it, whereas C sets `FD_CLOEXEC`
//!    only when the mode string contained `e` (`gzlib.c` L258-L261). A program that opens a gzip
//!    file and then `exec`s a child expecting to inherit the descriptor observes the difference.
//!    See [`GzOpenSpec::cloexec`]; for an adopted descriptor the facade applies the flag itself, so
//!    only the path-based case diverges.
//!
//! 5. **Boxing the handle cannot report allocation failure.** *(FORCED, UNRESOLVED.)*
//!    `GzState`'s handle slot is a `Box<dyn GzHandle>`, and stable Rust 1.80 has no fallible
//!    constructor for one, so an allocator that cannot serve three words **aborts the process**
//!    where C would return `Z_MEM_ERROR`. The window is narrow -- such an allocator could not have
//!    served the `GzState` being installed into -- but an abort is not a return value, and
//!    `test/infcover.c`'s `mem_limit` exists precisely to drive allocation failure deliberately.
//!
//! 6. **`gzdopen` has no allocation-failure path at all.** *(FORCED, UNRESOLVED.)* C `malloc`s a
//!    small buffer for the `"<fd:%d>"` label and returns `NULL` if that fails (`gzlib.c`
//!    L298-L312). Building the label in a fixed stack buffer removes the failure mode, so a caller
//!    cannot observe `gzdopen` failing for memory the way it can in C. The label text itself is
//!    reproduced exactly, because `gzerror` prefixes messages with it.
//!
//! ## Divergences owned by sibling modules
//!
//! Recorded here so the inventory is one list. Each is documented in full at its own site.
//!
//! 7. **`gz_zero` allocates the buffers before the loop and ties the zero-fill to the first
//!    non-empty chunk.** *(FORCED, UNRESOLVED -- it avoids a C defect.)* C's lazy allocation lets a
//!    `gzseek`-armed `gzputc` consume the `first` flag on a zero-length chunk and then fill the
//!    stream from uninitialised `malloc` memory. See `gz/write.rs`, `gz_zero`.
//!
//! 8. **`gz_write` clears both stream cursors when the direct path ends.** *(FORCED, UNRESOLVED --
//!    it avoids a C defect.)* C leaves `next_in` pointing into the caller's buffer; after a
//!    non-blocking stall the next call derives an underflowing length from it and writes out of
//!    bounds. See `gz/write.rs`, `gz_write`.
//!
//! 9. **`gz_fetch` publishes the output cursor and count together.** *(FORCED, UNRESOLVED -- it
//!    avoids a C defect.)* C can leave a non-zero `x.have` paired with a stale `x.next`, so the
//!    application receives bytes from the wrong place. See `gz/read.rs`, `gz_fetch`.
//!
//! Items 7-9 are cases where matching C exactly would mean reproducing an out-of-bounds access or
//! a read of uninitialised memory. They are therefore not going to be "fixed" by aligning with C;
//! they are recorded as differences so that no blanket claim of exact behavioural equivalence is
//! made on their behalf.
//!
//! # What this module expects of the crate root
//!
//! The whole `gz` subtree is reached through `#[cfg(feature = "std")] pub mod gz;`, so no item
//! below carries a redundant per-item feature gate and `--no-default-features` compiles the
//! subtree out entirely. The subtree is also the only part of `zlib-rs` permitted to name `std`
//! (AAP §0.4.2.1: `#include <stdio.h>` and `<fcntl.h>` become `use std::{fs, io}` behind that
//! gate). Because the crate is `#![no_std]`, `std` is not in the extern prelude, so this file
//! declares `extern crate std;` itself rather than depending on the root having done so; a second
//! declaration at the root is harmless. The items below are `pub` because the planned
//! `crates/libz-rs-sys/src/gz.rs` will call them, so the root's declaration of this subtree must
//! be `pub mod gz`.

// The C names are reproduced deliberately: a maintainer diffing this file against `gzlib.c` is
// looking for `gz_open`, `gz_reset` and `gzdopen`, and renaming them to satisfy a lint would cost
// exactly the traceability the implementation is judged on.
#![allow(clippy::module_name_repetitions)]

extern crate std;

use core::fmt;

use alloc::boxed::Box;

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

#[cfg(unix)]
use std::ffi::OsStr;
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

use crate::allocate::Allocator;
use crate::config::{
    Z_DEFAULT_COMPRESSION, Z_DEFAULT_STRATEGY, Z_FILTERED, Z_FIXED, Z_HUFFMAN_ONLY, Z_RLE,
};
use crate::error::ReturnCode;
use crate::gz::gz_error;
use crate::gz::state::{
    GzFileSlot, GzHandle, GzIoError, GzMode, GzSeekFrom, GzState, ZOff64, GZ_APPEND, GZ_NONE,
    GZ_READ, GZ_WRITE, LOOK,
};

/// The numeric value of `O_NONBLOCK` on this target, or [`None`] when it is not known.
///
/// `gz_open` folds `O_NONBLOCK` into the flags it passes to `open` when the mode string contained
/// `N` (`gzlib.c` L159-L162), and [`OpenOptions`] has no portable setter for it, so the flag has to
/// travel through [`OpenOptionsExt::custom_flags`] as a raw number. That number is defined by the
/// platform's `fcntl.h` rather than by Rust, and this crate's `[dependencies]` table is empty by
/// design (AAP §0.7.1 (i)), so `libc` cannot be asked. The values are therefore transcribed, with
/// the family each one belongs to named so the table can be checked against a header:
///
/// * Linux and Android use the `asm-generic/fcntl.h` value `0o4000` on every architecture except
///   the MIPS family (`0o200`) and the SPARC family (`0x4000`), each of which predates the generic
///   header and kept its own value;
/// * Darwin and the BSDs use the historical 4.4BSD value `0x0004`;
/// * anything else yields [`None`], and the flag is then simply not applied.
///
/// Not applying it is a conservative failure: the file opens in blocking mode, which is what a
/// mode string without `N` would have produced, rather than opening with some unrelated flag set.
/// It costs nothing on any Tier-1 target, all of which are covered above, and the escape hatch is
/// documented on [`GzOpenSpec::nonblocking`]: a caller that needs the flag on an exotic target can
/// open the descriptor itself and go through [`gzdopen`], which is the route `zlib.h` L1398-L1401
/// recommends anyway.
///
/// The two sibling flags C also requests need no entry here, because Rust already supplies them:
/// `O_CLOEXEC` is set unconditionally by `std`'s own file opening, and on Linux `std` opens through
/// `open64`, which is what `O_LARGEFILE` exists to request. `O_BINARY` has no meaning on a
/// POSIX target and is a Windows concern, where `std` opens in binary mode regardless.
#[cfg(unix)]
const fn o_nonblock() -> Option<i32> {
    if cfg!(any(target_os = "linux", target_os = "android")) {
        if cfg!(any(
            target_arch = "mips",
            target_arch = "mips32r6",
            target_arch = "mips64",
            target_arch = "mips64r6"
        )) {
            Some(0o200)
        } else if cfg!(any(target_arch = "sparc", target_arch = "sparc64")) {
            Some(0x4000)
        } else {
            Some(0o4000)
        }
    } else if cfg!(any(
        target_vendor = "apple",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd",
        target_os = "dragonfly"
    )) {
        Some(0x0004)
    } else {
        None
    }
}

/// Adds `O_NONBLOCK` to `options` when the mode string asked for it.
///
/// The one bit of C's flag word (`gzlib.c` L160-L161 setting it, L229-L244 folding it in) that
/// [`OpenOptions`] cannot express directly. Split out as a function rather than written inline with
/// a `#[cfg]` on the statement so that the non-unix build has a real, compiled definition to call
/// instead of a conditionally absent block.
#[cfg(unix)]
fn apply_nonblocking(options: &mut OpenOptions, nonblocking: bool) {
    if nonblocking {
        if let Some(flag) = o_nonblock() {
            options.custom_flags(flag);
        }
    }
}

/// Non-unix counterpart of [`apply_nonblocking`], which does nothing.
///
/// Windows has no `O_NONBLOCK`; overlapped I/O is requested per handle at creation time and is not
/// reachable through [`OpenOptions`]'s portable surface. C reaches the same conclusion from the
/// other direction, guarding the `N` case with `#ifdef O_NONBLOCK` (`gzlib.c` L159-L163) so that
/// the character is simply ignored where the flag does not exist.
#[cfg(not(unix))]
fn apply_nonblocking(_options: &mut OpenOptions, _nonblocking: bool) {}

/// What to open, in the form the platform's own `open` call takes it.
///
/// C has two open calls and hands each one a *different representation of the name*:
///
/// | C | Call | Argument |
/// |---|---|---|
/// | `gzopen`, `gzopen64` (`gzlib.c` L288, L293) | `open(path, oflag, 0666)` (L248) | the caller's `const char *`, byte for byte |
/// | `gzopen_w` (`gzlib.c` L316) | `_wopen(path, oflag, 0666)` (L246) | the caller's `wchar_t *`, unit for unit |
///
/// Neither is converted before the call. `wcstombs` appears in `gz_open` for one purpose only --
/// building `state->path`, the string `gz_error` prefixes messages with (`gzlib.c` L200-L219) -- and
/// the narrowed copy is never what gets opened.
///
/// This enum is that distinction, made explicit so it cannot be collapsed by accident. The
/// diagnostic label travels separately, as its own `&[u8]` argument to [`gz_open_with`], which is
/// what keeps the two from being confused: a wide open keeps its `u16` units *and* gets a narrow
/// label, exactly as C does.
///
/// # Why `Wide` is not gated to Windows
///
/// `gzopen_w` exists only on Windows, and so does `_wopen`. The *variant* is nevertheless
/// unconditional, for two reasons. It keeps this type and the label split testable on the target CI
/// actually runs (there is no Windows job for `cargo test`), and it keeps one signature for
/// [`gz_open_with`] rather than a shape that changes with the target. Opening a `Wide` target
/// on a platform with no wide file API reports the same `UNREPRESENTABLE_PATH` failure rather than
/// encoding, which is the same answer this module already gives a non-UTF-8 narrow path on such a
/// platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GzPathTarget<'p> {
    /// A byte path, as `open(2)` takes it and as `gzopen` supplies it.
    Narrow(&'p [u8]),
    /// A wide path in the platform's `wchar_t` units, as `_wopen` takes it and as `gzopen_w`
    /// supplies it. On Windows these are UTF-16 code units, and unpaired surrogates are preserved.
    Wide(&'p [u16]),
}

/// Interprets `path` as a filesystem path, or reports that it cannot be one.
///
/// C needs no counterpart: it hands the caller's `const char *` straight to `open`
/// (`gzlib.c` L248), which takes bytes. Rust's file API takes a typed path, so the bytes have to be
/// re-typed, and that is all this does.
///
/// On unix a path is an arbitrary byte string, so the bytes are used exactly as C uses them and
/// this never fails. On Windows a narrow path must be valid UTF-8, because `std` converts a `&Path`
/// to UTF-16 before calling `CreateFileW` and UTF-8 is the only encoding it can convert from; C
/// instead hands the bytes to the CRT's `open`, which interprets them in the active ANSI code page.
/// The difference is unreachable for a caller that passes UTF-8 or ASCII, and a caller that must
/// name a path the ANSI code page cannot express should use `gzopen_w`, which now carries its
/// `wchar_t` units through untouched -- see [`GzPathTarget`].
// The `Option` is infallible on unix and clippy is right to notice, but the two definitions are one
// signature: collapsing this one to `&Path` would make the caller's `ok_or` conditional on the
// target, which is a worse trade than an always-`Some` return here.
#[allow(clippy::unnecessary_wraps)]
#[cfg(unix)]
fn os_path(path: &[u8]) -> Option<&Path> {
    Some(Path::new(OsStr::from_bytes(path)))
}

/// Non-unix counterpart of [`os_path`], which requires UTF-8.
#[cfg(not(unix))]
fn os_path(path: &[u8]) -> Option<&Path> {
    core::str::from_utf8(path).ok().map(Path::new)
}

/// Interprets wide units as a filesystem path: the `_wopen` half of [`GzPathTarget`].
///
/// `OsStringExt::from_wide` is the lossless conversion -- it preserves unpaired surrogates, which is
/// precisely what a Windows path may legally contain and precisely what a round trip through UTF-8
/// would destroy. That losslessness is the point of the whole [`GzPathTarget`] split: `_wopen`
/// receives the caller's units, and so does this.
// The `Option` is always `Some` here, and clippy's `unnecessary_wraps` says so under
// `--target *-windows-*`. It is kept because the `#[cfg(not(windows))]` twin below has no wide file
// API to open through and must answer `None`: one signature, two cfgs, and the wrapper is load-
// bearing in exactly one of them. `os_path` above carries the same allow for the same reason.
#[allow(clippy::unnecessary_wraps)]
#[cfg(windows)]
fn wide_path(units: &[u16]) -> Option<std::path::PathBuf> {
    // The intermediate is named because `PathBuf: From<_>` has several impls and inference cannot
    // pick one from `from_wide`'s return type alone.
    let wide: std::ffi::OsString = std::os::windows::ffi::OsStringExt::from_wide(units);
    Some(std::path::PathBuf::from(wide))
}

/// Non-Windows counterpart of [`wide_path`]: there is no wide file API to open through.
///
/// C reaches the same conclusion by declaring `gzopen_w` only under `WIDECHAR` (`gzguts.h` L54-L56),
/// so no portable caller can be here. Reporting a failure rather than transcoding is deliberate:
/// guessing an encoding for a name the platform has no way to represent would open the wrong file
/// rather than none.
// `units` is genuinely unused here; the parameter exists so both definitions share one signature.
#[cfg(not(windows))]
fn wide_path(_units: &[u16]) -> Option<std::path::PathBuf> {
    None
}

/// Converts a [`std::io::Error`] into the allocation-free [`GzIoError`] the layer reports.
///
/// Two pieces survive the conversion, which is exactly the two pieces C acts on: the platform
/// error number, so that a facade can render it as `strerror(errno)` would, and whether the
/// failure was the non-blocking stall `EAGAIN`/`EWOULDBLOCK` rather than a real error
/// (`gzwrite.c` L117-L118). An error that carries no OS number -- one Rust synthesised rather than
/// received from a syscall -- reports zero, which [`GzIoError::errno`] documents as "could not
/// determine one".
fn io_error(error: &io::Error) -> GzIoError {
    GzIoError::new(
        error.raw_os_error().unwrap_or(0),
        error.kind() == io::ErrorKind::WouldBlock,
    )
}

/// The failure reported when an operation is attempted on a handle that has already been closed.
///
/// C would be calling `read`, `write` or `lseek` on a closed descriptor and receiving `EBADF`. The
/// number cannot be produced from safe Rust without naming a platform constant, so zero is
/// reported and [`GzIoError::errno`]'s "could not determine one" contract applies. The distinction
/// does not reach a caller through anything but the text of `gzerror`, because every consumer of
/// these results turns a failure into `Z_ERRNO` regardless of the number.
const CLOSED_HANDLE: GzIoError = GzIoError::new(0, false);

/// The failure reported when a file position cannot be represented.
///
/// `lseek` rejects a negative absolute offset with `EINVAL`, and a position that does not fit the
/// offset type cannot be reported at all. Both are recognised before a syscall is attempted, so
/// there is no `errno` to read and zero is reported, exactly as for [`CLOSED_HANDLE`]. On every
/// target this implementation supports the second case is unreachable, because `ZOff64` and the platform's
/// own offset type are both 64-bit and signed.
const OUT_OF_RANGE_OFFSET: GzIoError = GzIoError::new(0, false);

/// The failure reported when a name cannot be expressed as a path on this platform.
///
/// Two cases, and neither reaches a syscall, so there is no `errno` and zero is reported exactly as
/// for [`CLOSED_HANDLE`]: a narrow path that is not valid UTF-8 on a platform where `std` can only
/// convert from UTF-8, and a [`GzPathTarget::Wide`] target on a platform with no wide file API. C
/// has no counterpart for either -- it hands bytes to `open` and units to `_wopen` and lets the
/// platform judge -- so the closest C analogue is the `EINVAL`-or-`ENOENT` the platform would have
/// produced from a name it could not use. Reported rather than guessed at: transcoding a name the
/// platform cannot represent risks opening a *different* file, which is worse than opening none.
const UNREPRESENTABLE_PATH: GzIoError = GzIoError::new(0, false);

/// A [`GzHandle`] backed by an owned [`std::fs::File`]: what `gz_open` installs in place of C's
/// `state->fd` when the stream was opened by path.
///
/// `crate::gz::state` declares [`GzHandle`] and the slot that holds one, and states that the
/// concrete implementation belongs here. This is it. A *boxed trait object* was chosen over an
/// enum with a `File` variant plus an injected variant for one decisive reason: the injected case
/// lives in `crates/libz-rs-sys`, and an enum would force this crate to enumerate -- and therefore
/// to name -- a case it is forbidden to construct, since adopting a descriptor needs `unsafe`. A
/// trait object lets the safe crate define the only variant it can build while the facade adds its
/// own without either side reaching into the other. The indirection costs one pointer hop per
/// buffer refill, against `GZBUFSIZE` bytes of work per hop.
///
/// # Ownership
///
/// A `FileHandle` owns its file and closes it when dropped, which is right for a file the library
/// opened by path: C's `gzclose_r`/`gzclose_w` close the descriptor they were given
/// (`gzread.c` L665, `gzwrite.c` L696), and `GzState`'s teardown does the same through
/// [`GzHandle::close`]. A facade handle wrapping a *caller's* descriptor must behave differently --
/// see this module's documentation, point 3.
///
/// # Constructing one from a descriptor
///
/// [`FileHandle::from_file`] is `pub` precisely so that the facade's `gzdopen` can adopt the
/// caller's descriptor with `File::from_raw_fd` -- in the crate that is allowed to write `unsafe`
/// -- and then reuse everything below instead of reimplementing it. That call is not merely
/// "audited"; it carries an ownership invariant the facade has to discharge, and it is spelled out
/// here because this side cannot check any of it:
///
/// * `fd` must be an **open** descriptor on the calling process, valid for the operations the mode
///   string asked for. A closed or never-opened descriptor is not a safety problem for the
///   `from_raw_fd` call itself, but every subsequent read or write on it is nonsense, so the facade
///   validates it before adopting it.
/// * Ownership of `fd` transfers to the resulting [`File`] **exactly once**. No other [`File`],
///   handle, or piece of code may own the same descriptor, because the first owner to be dropped
///   closes it and leaves every other owner holding a stale number that the operating system is
///   free to reuse for an unrelated file. A descriptor the caller still intends to use must be
///   duplicated first, which is what `zlib.h` L1414-L1416 tells callers to do.
/// * The descriptor must not be closed behind the [`File`]'s back for as long as the handle lives.
///
/// The recovery obligation follows from the same transfer. A `File` built this way closes the
/// descriptor when dropped, and `zlib.h` L1415-L1416 promises that "`gzdopen` does not close `fd`
/// if it fails" -- so on **every** failure path after adoption, the facade must take the handle
/// back out of [`GzOpenHandleError::handle`] and recover the descriptor with `IntoRawFd` before the
/// handle is dropped, or supply its own handle type whose `Drop` does not close. Failure paths
/// reached *before* adoption need nothing, which is why this module's documentation, point 2,
/// requires [`GzOpenSpec::parse`] to run first.
#[derive(Debug)]
pub struct FileHandle {
    /// The open file, or [`None`] once [`GzHandle::close`] has run.
    ///
    /// An [`Option`] rather than a bare [`File`] because [`GzHandle::close`] is contractually
    /// idempotent: `GzState`'s teardown may close a handle that a close path already closed, and
    /// the second call must succeed rather than fail or double-close.
    file: Option<File>,
    /// Whether the file was opened for writing, which decides whether [`GzHandle::close`] flushes.
    writable: bool,
    /// Whether `O_NONBLOCK` was requested for this file.
    ///
    /// Recorded rather than acted upon after the fact; see [`GzHandle::set_nonblocking`]'s
    /// implementation below for why the flag can only be applied at open time here.
    nonblocking: bool,
}

impl FileHandle {
    /// Wraps an already-open file.
    ///
    /// `writable` selects whether [`GzHandle::close`] flushes before dropping the file, and
    /// `nonblocking` records whether the file was opened with `O_NONBLOCK` so that
    /// [`FileHandle::is_nonblocking`] can report it. Neither is inferred from the file itself,
    /// because neither is retrievable from a [`File`] without `fcntl`.
    #[must_use]
    pub const fn from_file(file: File, writable: bool, nonblocking: bool) -> Self {
        Self {
            file: Some(file),
            writable,
            nonblocking,
        }
    }

    /// Whether the file is still open -- that is, whether [`GzHandle::close`] has not yet run.
    #[must_use]
    pub const fn is_open(&self) -> bool {
        self.file.is_some()
    }

    /// Whether the file was opened for writing.
    #[must_use]
    pub const fn is_writable(&self) -> bool {
        self.writable
    }

    /// Whether `O_NONBLOCK` was requested when the file was opened.
    #[must_use]
    pub const fn is_nonblocking(&self) -> bool {
        self.nonblocking
    }

    /// Borrows the open file, or reports that it has been closed.
    fn open_file(&mut self) -> Result<&mut File, GzIoError> {
        self.file.as_mut().ok_or(CLOSED_HANDLE)
    }
}

impl GzHandle for FileHandle {
    /// Implements `read(state->fd, buf + *have, len)` in `gz_load` (`gzread.c` L30).
    ///
    /// `Ok(0)` means end of file, as a `read` returning 0 does there. Interruptions are **not**
    /// retried, deliberately: `read` returning `-1` with `EINTR` is an error to `gz_load`
    /// (`gzread.c` L37-L42), and [`std::fs::File`]'s `read` likewise surfaces
    /// [`std::io::ErrorKind::Interrupted`] rather than looping, so the two implementations agree.
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, GzIoError> {
        self.open_file()?
            .read(buf)
            .map_err(|error| io_error(&error))
    }

    /// Implements `write(state->fd, state->x.next, put)` in `gz_comp` (`gzwrite.c` L115).
    ///
    /// A short write is normal rather than exceptional; `gz_comp` loops until the buffer is drained
    /// (`gzwrite.c` L110-L123), so no loop is added here.
    fn write(&mut self, buf: &[u8]) -> Result<usize, GzIoError> {
        self.open_file()?
            .write(buf)
            .map_err(|error| io_error(&error))
    }

    /// Implements the `LSEEK` macro (`gzlib.c` L8-L16), which resolves to `lseek64`, `_lseeki64`,
    /// `llseek` or `lseek` per platform.
    ///
    /// The offset type is [`ZOff64`], the core's stand-in for `z_off64_t`, and it is signed for all
    /// three origins. [`SeekFrom::Start`] is the one that cannot accept a negative offset, so a
    /// negative absolute position is rejected the way `lseek` rejects it -- with a failure rather
    /// than a wrapped, enormous unsigned offset. A resulting position too large for [`ZOff64`] is
    /// rejected for the same reason; on every target this implementation supports that is unreachable,
    /// because [`ZOff64`] and the platform's own offset type are both 64-bit signed.
    fn seek(&mut self, offset: ZOff64, whence: GzSeekFrom) -> Result<ZOff64, GzIoError> {
        let from = match whence {
            GzSeekFrom::Start => {
                SeekFrom::Start(u64::try_from(offset).map_err(|_| OUT_OF_RANGE_OFFSET)?)
            }
            GzSeekFrom::Current => SeekFrom::Current(offset),
            GzSeekFrom::End => SeekFrom::End(offset),
        };
        let position = self
            .open_file()?
            .seek(from)
            .map_err(|error| io_error(&error))?;
        ZOff64::try_from(position).map_err(|_| OUT_OF_RANGE_OFFSET)
    }

    /// Records the non-blocking flag; the flag itself was applied when the file was opened.
    ///
    /// C has two routes to `O_NONBLOCK`: it is part of the `open` call for a path
    /// (`gzlib.c` L160-L161, L229-L244) and an `fcntl(fd, F_SETFL, ...)` for an adopted descriptor
    /// (`gzlib.c` L254-L257). Only the first is reachable from safe Rust -- `fcntl` is neither in
    /// `std` nor expressible without `libc` -- and only the first can apply to a file this type
    /// opened, so this reports success without a syscall. [`GzHandle::set_nonblocking`] explicitly
    /// sanctions that: "an implementation that only ever opens by path may report success without
    /// doing anything". The descriptor-backed handle in `crates/libz-rs-sys` is where the `F_SETFL`
    /// half lives, and it is the handle `gzdopen` -- the only caller that needs it -- receives.
    fn set_nonblocking(&mut self, nonblocking: bool) -> Result<(), GzIoError> {
        self.nonblocking = nonblocking;
        Ok(())
    }

    /// Closes the file, flushing first if it was opened for writing. Idempotent.
    ///
    /// The port of `close(state->fd)` (`gzread.c` L665, `gzwrite.c` L696), whose failure becomes
    /// `Z_ERRNO`. Divergence 1 in this module's documentation applies to *this* implementation: the
    /// result of the underlying `close(2)` is unobservable from safe Rust, so what is reported is the
    /// flush's result. That is a property of this type, not of the layer -- a stream whose
    /// `close(2)` status must be exact takes an injected handle through [`gz_open_with`], and the
    /// result then travels unchanged through [`crate::gz::state::GzFileSlot::close`] to
    /// `gzclose_r`/`gzclose_w`. No `fsync` is performed here -- C's `close` does not sync either,
    /// and forcing one would make every `gzclose` pay for durability the reference implementation
    /// never promised.
    ///
    /// The file is taken out of the slot before the flush is attempted, so it is released even when
    /// the flush fails and a second call still succeeds.
    fn close(&mut self) -> Result<(), GzIoError> {
        let Some(mut file) = self.file.take() else {
            // Already closed. The contract requires this to succeed.
            return Ok(());
        };
        if self.writable {
            file.flush().map_err(|error| io_error(&error))?;
        }
        Ok(())
    }
}

/// Why a `gzFile` could not be opened.
///
/// C has one answer for all of these: `gz_open` frees whatever it had allocated and returns `NULL`
/// (`gzlib.c` L130-L131, L175-L176, L183-L184, L193-L194, L208-L209, L265-L267), and `zlib.h`
/// L1391-L1393 lists the reasons in prose -- "`gzopen` returns `NULL` if the file could not be
/// opened, if there was insufficient memory to allocate the `gzFile` state, or if an invalid mode
/// was specified". **Every variant below maps to that same `NULL`**; the facade needs no table to
/// consult, it maps `Err(_)` to `NULL` unconditionally.
///
/// The variants exist so that the *facade* can tell the cases apart without re-parsing the mode
/// string, which matters for one specific obligation: `zlib.h` L1415-L1416 promises that `gzdopen`
/// does not close the caller's descriptor when it fails, and distinguishing "your mode string was
/// invalid" from "the file system said no" is what lets the facade decide whether it ever needed to
/// adopt the descriptor at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GzOpenError {
    /// The mode string was rejected.
    ///
    /// One of four conditions, all of which C answers with `NULL`: it contained `+`
    /// (`gzlib.c` L129-L131), it never selected `r`, `w` or `a` (L174-L177), it asked for a
    /// transparent read with `T` (L181-L185), or it asked for gzip-only writing with `G`
    /// (L191-L195).
    InvalidMode,
    /// A descriptor of `-1` was offered to [`gzdopen`].
    ///
    /// C tests it first thing and returns `NULL` (`gzlib.c` L302-L303), and `zlib.h` L1424 records
    /// the behaviour: `gzdopen` "returns `NULL` ... or if `fd` is `-1`".
    InvalidDescriptor,
    /// The path copy could not be allocated.
    ///
    /// C's `malloc(len + 1)` for `state->path` failing (`gzlib.c` L206-L210).
    OutOfMemory,
    /// The file itself could not be opened.
    ///
    /// C's `state->fd == -1` after `open` (`gzlib.c` L264-L268). `zlib.h` L1394-L1398 promises that
    /// "`errno` can be checked to determine if the reason `gzopen` failed was that the file could
    /// not be opened", and specifically that with `N` in the mode "the `open()` itself can fail in
    /// order to not block ... and `errno` will be `EAGAIN` or `ENONBLOCK`. The call to `gzopen()`
    /// can then be re-tried." That is why the error is carried rather than flattened: the number
    /// travels in [`GzIoError::errno`] and the stall is flagged in [`GzIoError::would_block`], so
    /// the retry decision is available to the facade.
    ///
    /// **What is *not* promised: that the platform's thread-local `errno` still holds the failing
    /// value when the C caller looks.** This crate captures `io::Error::raw_os_error()` into
    /// [`GzIoError::errno`] and nothing more. Rust makes no guarantee about `errno` after a `std`
    /// call returns, and `std` may perform further syscalls -- path conversion, a retry, a
    /// destructor -- between the failing `open` and the return, any of which can overwrite it.
    /// So the number in [`GzIoError::errno`] is the reliable one, and satisfying `zlib.h`'s
    /// `errno` clause is the facade's job: it must set `errno` from that field before returning
    /// `NULL`, and verify it does so. Until then, treat the header's `errno` promise as met by the
    /// carried number and not by the thread-local.
    Io(GzIoError),
}

impl GzOpenError {
    /// The `Z_*` code that corresponds to this failure.
    ///
    /// `gz_open` never reports a code -- it has no stream to report one *on*, which is the whole
    /// reason it answers with `NULL`. This mapping exists for the facade's benefit, so that a
    /// diagnostic path can name the failure with the library's own vocabulary: an unusable mode
    /// string is a `Z_STREAM_ERROR`, an exhausted allocator is a `Z_MEM_ERROR`, and a refusal from
    /// the file system is the `Z_ERRNO` that every other `gzFile` I/O failure reports
    /// (`gzread.c` L41, `gzwrite.c` L119).
    #[must_use]
    pub const fn as_return_code(self) -> ReturnCode {
        match self {
            Self::InvalidMode | Self::InvalidDescriptor => ReturnCode::STREAM_ERROR,
            Self::OutOfMemory => ReturnCode::MEM_ERROR,
            Self::Io(_) => ReturnCode::ERRNO,
        }
    }
}

/// A failed [`gz_open_handle`] or [`gzdopen`], carrying the handle back to its owner.
///
/// The reason this type exists is a single sentence of `zlib.h`, L1415-L1416: "The duplicated
/// descriptor should be saved to avoid a leak, since `gzdopen` does not close `fd` if it fails."
/// C gets that for free -- it never held anything but an `int`, so failing and returning `NULL`
/// leaves the descriptor untouched. This implementation is handed an owning handle instead, and dropping an
/// owning handle is precisely what must not happen, so the handle is handed straight back.
///
/// What the recipient should do with it depends on which side built it. A handle whose `Drop` does
/// not close -- which is what this module's documentation, point 3, requires of the facade's
/// descriptor-backed handle -- can simply be dropped. A handle that does close on drop, such as a
/// [`FileHandle`] built from `File::from_raw_fd`, must have its descriptor recovered with
/// `IntoRawFd` first.
pub struct GzOpenHandleError<'a> {
    /// Why the open failed. Interpreted exactly as a [`GzOpenError`] from any other entry point.
    pub error: GzOpenError,
    /// The handle that was passed in, returned unclosed and unused.
    pub handle: Box<dyn GzHandle + 'a>,
}

impl fmt::Debug for GzOpenHandleError<'_> {
    /// Reports the error and that a handle is present.
    ///
    /// Written by hand because [`GzHandle`] does not -- and should not -- require [`fmt::Debug`] of
    /// its implementors: a facade handle wraps a caller's descriptor, and there is nothing useful
    /// or safe to print about it.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GzOpenHandleError")
            .field("error", &self.error)
            .field("handle", &"<returned>")
            .finish()
    }
}

/// A parsed and resolved mode string: everything `gz_open` learns from `mode` before it touches the
/// file system.
///
/// This is the first half of `gz_open` (`gzlib.c` L108-L197) lifted into a value. C writes each
/// decision straight into the freshly allocated `gz_state` as it goes and frees the state again on
/// the four failing paths; separating the two halves changes nothing observable -- the fields are
/// established in the same way from the same bytes, and a rejected mode string still produces no
/// stream -- and it buys two things the implementation needs:
///
/// * the grammar becomes testable on its own, which is how the table-driven test at the end of this
///   file can assert the resulting mode, level, strategy and `direct` for every documented mode
///   string without creating a file;
/// * the facade can validate a mode string **before** adopting a descriptor in `gzdopen`, which is
///   what makes `zlib.h`'s "does not close `fd` if it fails" promise easy to keep rather than
///   delicate.
///
/// The full grammar, and the resolution rules applied after it, are tabulated in this module's
/// documentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GzOpenSpec {
    /// [`GZ_READ`], [`GZ_WRITE`] or [`GZ_APPEND`]; never [`GZ_NONE`], which is rejected.
    mode: i32,
    /// The compression level, including the negative `Z_DEFAULT_COMPRESSION` sentinel.
    level: i32,
    /// The compression strategy as a raw `Z_*` value.
    strategy: i32,
    /// Container handling: `-1` gzip only, `0` gzip, `1` transparent.
    direct: i32,
    /// Whether `e` requested `O_CLOEXEC`.
    cloexec: bool,
    /// Whether `x` requested an exclusive create.
    exclusive: bool,
    /// Whether `N` requested `O_NONBLOCK`.
    nonblocking: bool,
}

impl GzOpenSpec {
    /// Parses and resolves a mode string, the Rust counterpart of `gzlib.c` L108-L197.
    ///
    /// `mode` is the bytes of C's `const char *mode` **without** the terminating NUL. A NUL that
    /// does appear inside the slice ends the walk, exactly as C's `while (*mode)` does, so a
    /// caller that passes a whole fixed-size buffer still gets C's answer rather than a different
    /// one. The `path == NULL || mode == NULL` test of `gzlib.c` L96-L97 has no counterpart here,
    /// because a null pointer cannot be a `&[u8]`; it belongs to the facade, which is where the
    /// pointers are.
    ///
    /// Unknown characters are ignored rather than rejected. That is C's `default: ;` at
    /// `gzlib.c` L167-L168 -- "could consider as an error, but just ignore" -- and it is required
    /// rather than merely tolerated, because `test/minigzip.c` L511 builds the mode string `"wb6 "`
    /// and hands the space to this parser.
    ///
    /// # Errors
    ///
    /// [`GzOpenError::InvalidMode`] for each of C's four rejections: `+` anywhere in the string,
    /// no `r`/`w`/`a` at all, `T` while reading, or `G` while writing or appending.
    pub fn parse(mode: &[u8]) -> Result<Self, GzOpenError> {
        // The starting values of `gzlib.c` L109-L112, which are also the ones `GzState::new`
        // installs; they are repeated here because this value exists before any state does.
        let mut spec = Self {
            mode: GZ_NONE,
            level: Z_DEFAULT_COMPRESSION,
            strategy: Z_DEFAULT_STRATEGY,
            direct: 0,
            cloexec: false,
            exclusive: false,
            nonblocking: false,
        };

        // `gzlib.c` L113-L171. The order of the arms is immaterial -- they are disjoint -- but the
        // order of the *bytes* is not: a later character overrides an earlier one, which is what
        // makes the last `G` or `T` win and `"rw"` open for writing.
        for &byte in mode {
            if byte == 0 {
                // C's loop condition, `while (*mode)`: the string ends here.
                break;
            }
            if byte.is_ascii_digit() {
                // L114-L115. `byte - b'0'` is in 0..=9 by the guard, so the conversion is exact.
                spec.level = i32::from(byte - b'0');
                continue;
            }
            // `b` and the wildcard do the same nothing, and clippy would rather they were one arm.
            // They must not be: `case 'b': break;` (L132-L133) is a *documented* no-op -- "ignore
            // -- will request binary anyway" -- whereas the wildcard is a deliberate decision to
            // tolerate garbage (L167-L168). Merging them would erase the distinction between a
            // character the format defines and one it does not.
            #[allow(clippy::match_same_arms)]
            match byte {
                b'r' => spec.mode = GZ_READ,
                b'w' => spec.mode = GZ_WRITE,
                b'a' => spec.mode = GZ_APPEND,
                // L129-L131: "can't read and write at the same time". C frees and returns NULL
                // immediately, without looking at the rest of the string.
                b'+' => return Err(GzOpenError::InvalidMode),
                // L132-L133: "ignore -- will request binary anyway".
                b'b' => {}
                b'e' => spec.cloexec = true,
                b'x' => spec.exclusive = true,
                b'f' => spec.strategy = Z_FILTERED,
                b'h' => spec.strategy = Z_HUFFMAN_ONLY,
                b'R' => spec.strategy = Z_RLE,
                b'F' => spec.strategy = Z_FIXED,
                b'G' => spec.direct = -1,
                b'N' => spec.nonblocking = true,
                b'T' => spec.direct = 1,
                // L167-L168: "could consider as an error, but just ignore".
                _ => {}
            }
        }

        // L173-L177: "must provide an 'r', 'w', or 'a'".
        if spec.mode == GZ_NONE {
            return Err(GzOpenError::InvalidMode);
        }

        // L179-L197. `direct` is 0, 1 if "T", or -1 if "G" (last "G" or "T" wins).
        if spec.mode == GZ_READ {
            if spec.direct == 1 {
                // L181-L185: "can't force a transparent read".
                return Err(GzOpenError::InvalidMode);
            }
            if spec.direct == 0 {
                // L186-L189: "default when reading is auto-detect of gzip vs. transparent -- start
                // with a transparent assumption in case of an empty file". This is also why
                // `gzdirect` answers true for an empty file.
                spec.direct = 1;
            }
        } else if spec.direct == -1 {
            // L191-L195: "'G' has no meaning when writing -- disallow it".
            return Err(GzOpenError::InvalidMode);
        }

        Ok(spec)
    }

    /// The direction the mode string selected: [`GZ_READ`], [`GZ_WRITE`] or [`GZ_APPEND`].
    ///
    /// Raw, as `gz_state.mode` stores it, because [`GZ_APPEND`] is transient -- `gz_open` replaces
    /// it with [`GZ_WRITE`] once the file has been positioned (`gzlib.c` L269-L272) -- and callers
    /// of this accessor need to see it before that happens.
    #[must_use]
    pub const fn mode(&self) -> i32 {
        self.mode
    }

    /// The direction as an exhaustive [`GzMode`].
    ///
    /// Never [`None`] and never [`GzMode::None`]: [`GzOpenSpec::parse`] only ever yields one of the
    /// three real directions. The [`Option`] is [`GzMode::from_raw`]'s shape, kept rather than
    /// unwrapped because unwrapping would be a panic, and library code here does not panic.
    #[must_use]
    pub const fn mode_typed(&self) -> Option<GzMode> {
        GzMode::from_raw(self.mode)
    }

    /// The compression level, `Z_DEFAULT_COMPRESSION` if the mode string carried no digit.
    ///
    /// Raw, including the negative sentinel, because `gz_state.level` stores it raw so that
    /// `gzsetparams`' "no change requested" short-circuit compares identically (`gzwrite.c` L647).
    #[must_use]
    pub const fn level(&self) -> i32 {
        self.level
    }

    /// The compression strategy, `Z_DEFAULT_STRATEGY` if the mode string carried none of
    /// `f`, `h`, `R` or `F`.
    #[must_use]
    pub const fn strategy(&self) -> i32 {
        self.strategy
    }

    /// Container handling after resolution: `-1` gzip only, `0` gzip, `1` transparent.
    ///
    /// Reading yields `1` (auto-detect) or `-1` (gzip only); writing and appending yield `0` (gzip)
    /// or `1` (transparent copy). That is C's comment at `gzlib.c` L196-L197 verbatim.
    #[must_use]
    pub const fn direct(&self) -> i32 {
        self.direct
    }

    /// Whether `e` asked for the descriptor to be closed on `execve`.
    ///
    /// For a path-based open nothing needs doing, because `std` sets `O_CLOEXEC` on every file it
    /// opens -- so the request is satisfied **whether or not it was made**. That is divergence 4 in
    /// this module's inventory and it is an observable behaviour difference, not merely hardening:
    /// C sets `FD_CLOEXEC` only when the mode string contained `e` (`gzlib.c` L258-L261), so a
    /// program that opens a gzip file without `e` and then `exec`s a child expecting to inherit the
    /// descriptor gets different results under the two implementations. `std` offers no safe way to
    /// clear the flag, so the difference is forced and stands as unresolved.
    ///
    /// For an adopted descriptor this is the flag the facade must apply with
    /// `fcntl(fd, F_SETFD, ...)` (`gzlib.c` L258-L261), because [`GzHandle`] has no
    /// descriptor-flag method -- so the adopted case does not diverge.
    #[must_use]
    pub const fn cloexec(&self) -> bool {
        self.cloexec
    }

    /// Whether `x` asked for an exclusive create.
    ///
    /// Consulted only when writing or appending, exactly as C consults `exclusive` only inside the
    /// non-read branch of its flag computation (`gzlib.c` L236-L244), so `"rbx"` opens an existing
    /// file for reading and the `x` has no effect.
    #[must_use]
    pub const fn exclusive(&self) -> bool {
        self.exclusive
    }

    /// Whether `N` asked for non-blocking I/O.
    ///
    /// Applied at open time for a path (see `o_nonblock` for the one target class where the flag
    /// cannot be named and is therefore skipped) and through [`GzHandle::set_nonblocking`] for an
    /// adopted descriptor. `zlib.h` L1398-L1401 documents the second route as the recommended one:
    /// "If the application would like to block on opening the file, then it can use `open()`
    /// without `O_NONBLOCK`, and then `gzdopen()` with the resulting file descriptor and `N` in the
    /// mode, which will set it to non-blocking."
    #[must_use]
    pub const fn nonblocking(&self) -> bool {
        self.nonblocking
    }

    /// Writes the four caller-visible decisions into a fresh state.
    ///
    /// The assignments of `gzlib.c` L109-L112 as corrected by the resolution at L180-L195. Only
    /// these four reach `gz_state`; `cloexec`, `exclusive` and `nonblocking` are `open` flags in C
    /// (`gzlib.c` L229-L244) and are never stored.
    fn apply_to<'a, A: Allocator<'a>>(&self, state: &mut GzState<'a, A>) {
        state.set_mode(self.mode);
        state.set_level(self.level);
        state.set_strategy(self.strategy);
        state.set_direct(self.direct);
    }
}

/// Puts a `gzFile` back to its just-opened condition: the Rust counterpart of `gz_reset` (`gzlib.c` L69-L84).
///
/// Two callers, and they want different things from it. `gz_open` uses it to finish initialising a
/// brand-new stream (`gzlib.c` L281), where most of the assignments are redundant because
/// `GzState::new` already made them. `gzrewind` uses it after seeking back to `state->start`
/// (`gzlib.c` L360-L362), where none of them are: the stream has been read, so every field it
/// touches is dirty. `gzseek64` performs the same set inline for its within-raw-area fast path
/// (`gzlib.c` L401-L406) rather than calling this, and that is C's structure, not a simplification
/// available here.
///
/// The order of operations is C's, assignment for assignment. Nothing here can fail: `gz_error`
/// with `Z_OK` and no message allocates nothing (`gzlib.c` L569-L570), which is exactly why C can
/// treat the reset as infallible.
///
/// What is **not** reset is as load-bearing as what is. `want` survives, so a `gzbuffer` call made
/// before the first read is not undone by a `gzrewind`. `size`, `in` and `out` survive, so the
/// working buffers are reused rather than reallocated. `direct` survives, because it was resolved
/// from the mode string and rewinding does not change what the caller asked for -- though the read
/// path may still revise it when it looks at the data again, since `how` is back to [`LOOK`].
///
/// The trailing `refresh_exposed` has no C counterpart and is required by this implementation's
/// pointer/index split: `x.have` has just gone to zero, and the caller-visible `x.next` must be
/// re-derived before control can return to code that reads it through the `gzgetc` macro. See
/// `crate::gz::state` for the contract.
pub(crate) fn gz_reset<'a, A: Allocator<'a>>(state: &mut GzState<'a, A>) {
    // L70: no output data available.
    state.set_have(0);
    if state.mode() == GZ_READ {
        // L71-L76, "for reading ...".
        state.set_eof(false); // L72: not at end of file
        state.set_past(false); // L73: have not read past end yet
        state.set_how(LOOK); // L74: look for gzip header
        state.set_junk(-1); // L75: mark first member
    } else {
        // L77-L78, "for writing ...": no deflateReset pending.
        state.set_reset_pending(false);
    }
    state.set_again(false); // L79: no stalled i/o yet
    state.set_skip(0); // L80: no seek request pending
    gz_error(state, ReturnCode::OK, None); // L81: clear error
    state.set_pos(0); // L82: no uncompressed data yet
    state.stream_mut().avail_in = 0; // L83: no input data yet

    // No C counterpart; re-derives the caller-visible `x.next` from the index state above.
    state.refresh_exposed();
}

/// Opens the file the mode string calls for: the Rust counterpart of `gzlib.c` L228-L263, path branch.
///
/// The C flag computation (`gzlib.c` L227-L232) selects `O_RDONLY` for a read stream and otherwise
/// `O_WRONLY | O_CREAT`, adding `O_EXCL` when the mode string asked for exclusivity and `O_TRUNC`
/// or `O_APPEND` according to write versus append. [`OpenOptions`] expresses all of it:
///
/// | C flags | `OpenOptions` |
/// |---|---|
/// | `O_RDONLY` | `read(true)` |
/// | `O_WRONLY \| O_CREAT \| O_TRUNC` | `write(true).create(true).truncate(true)` |
/// | `O_WRONLY \| O_CREAT \| O_APPEND` | `append(true).create(true)` |
/// | `\| O_EXCL` | `create_new(true)` |
/// | `\| O_NONBLOCK` | `custom_flags`, see `o_nonblock` |
/// | `\| O_CLOEXEC`, `\| O_LARGEFILE`, `\| O_BINARY` | already applied by `std` |
///
/// `truncate` is set only in the non-exclusive case. With `O_EXCL` the file provably did not exist,
/// so C's `O_TRUNC` has nothing to truncate and [`OpenOptions`] documents that it ignores
/// `truncate` alongside `create_new` in any case; stating the intent rather than relying on the
/// second fact keeps the call valid if that documented interaction is ever tightened into an error.
///
/// C's mode argument to `open` is `0666`, and `std`'s default is the same, so a created file lands
/// with identical permissions before `umask`.
///
/// The one flag this cannot get right is `O_CLOEXEC`; [`open_with_std`], the public wrapper, carries
/// the full explanation and names the escape hatch.
///
/// # Errors
///
/// The [`GzIoError`] behind C's `state->fd == -1` test (`gzlib.c` L264). The platform's error
/// *number* is captured into [`GzIoError::errno`]; the platform's thread-local `errno` is **not**
/// promised to still hold it, because `std` gives no such guarantee. See [`GzOpenError::Io`] for why
/// that distinction matters and for what the facade owes `zlib.h` L1394-L1398.
/// [`UNREPRESENTABLE_PATH`] when the target cannot be named on this platform at all.
fn open_path(target: GzPathTarget<'_>, spec: &GzOpenSpec) -> Result<File, GzIoError> {
    // Held by value for the wide case, whose conversion produces an owned buffer, and borrowed for
    // the narrow one, whose bytes are reinterpreted in place exactly as C reinterprets them.
    let owned;
    let target: &Path = match target {
        GzPathTarget::Narrow(bytes) => os_path(bytes).ok_or(UNREPRESENTABLE_PATH)?,
        GzPathTarget::Wide(units) => {
            owned = wide_path(units).ok_or(UNREPRESENTABLE_PATH)?;
            owned.as_path()
        }
    };
    let mut options = OpenOptions::new();
    if spec.mode == GZ_READ {
        options.read(true);
    } else {
        if spec.mode == GZ_APPEND {
            options.append(true);
        } else {
            options.write(true);
            if !spec.exclusive {
                options.truncate(true);
            }
        }
        if spec.exclusive {
            options.create_new(true);
        } else {
            options.create(true);
        }
    }
    apply_nonblocking(&mut options, spec.nonblocking);
    options.open(target).map_err(|error| io_error(&error))
}

/// Positions a freshly opened stream and resets it: the Rust counterpart of `gzlib.c` L269-L284.
///
/// Shared by `gz_open` and [`gz_open_handle`] because C shares it -- both entry points fall
/// through to the same tail once `state->fd` has been established. Three steps, and two of them
/// tolerate failure on purpose:
///
/// 1. an appending stream seeks to the end "so that `gzoffset()` is correct" and then becomes
///    [`GZ_WRITE`] "to simplify later checks" (L269-L272). C ignores `LSEEK`'s return value here
///    outright, and so does this;
/// 2. a reading stream records its starting position for rewinding, and **substitutes zero if the
///    seek fails** (L275-L278). That fallback is not defensive padding, it is the documented path
///    for an unseekable descriptor: `test/minigzip.c` L549 opens `gzdopen(fileno(stdin), "rb")`,
///    and a pipe answers `lseek` with `ESPIPE`;
/// 3. `gz_reset` finishes the job (L281).
///
/// Nothing after this point can fail, which is why the handle is installed by the caller only once
/// every fallible step is behind it -- see `gz_open`.
fn finish_open<'a, A: Allocator<'a>>(state: &mut GzState<'a, A>) {
    if state.mode() == GZ_APPEND {
        if let Some(handle) = state.handle_mut() {
            // L270. C discards the result: `LSEEK(state->fd, 0, SEEK_END);`
            let _ = handle.seek(0, GzSeekFrom::End);
        }
        // L271: "simplify later checks".
        state.set_mode(GZ_WRITE);
    }
    if state.mode() == GZ_READ {
        let start = state
            .handle_mut()
            .and_then(|handle| handle.seek(0, GzSeekFrom::Current).ok())
            .unwrap_or(0);
        state.set_start(start);
    }
    gz_reset(state);
}

/// The opener [`gzopen`] uses: [`std::fs::OpenOptions`], wrapped in an inline [`FileHandle`].
///
/// Separated from [`gz_open_with`] so that it has a name a facade can compose with -- wrap it, fall
/// back to it, or replace it outright -- and so that "what the safe core can open by itself" is one
/// readable function rather than a closure buried in a call.
///
/// The handle comes back as [`GzFileSlot::Owned`], which is stored inline: no allocation, and
/// therefore no allocation that could abort. See [`GzFileSlot`] for why that shape was chosen over a
/// boxed trait object.
///
/// # C's flag word, and the one flag this cannot reproduce
///
/// [`OpenOptions`] expresses all of `gz_open`'s flag computation (`gzlib.c` L229-L244) but one:
///
/// | C flags | `OpenOptions` |
/// |---|---|
/// | `O_RDONLY` | `read(true)` |
/// | `O_WRONLY \| O_CREAT \| O_TRUNC` | `write(true).create(true).truncate(true)` |
/// | `O_WRONLY \| O_CREAT \| O_APPEND` | `append(true).create(true)` |
/// | `\| O_EXCL` | `create_new(true)` |
/// | `\| O_NONBLOCK` | `custom_flags`, see `o_nonblock` |
/// | `\| O_LARGEFILE`, `\| O_BINARY` | already applied by `std` |
/// | `\| O_CLOEXEC` when the mode contained `e` | **cannot be expressed** |
///
/// C sets `O_CLOEXEC` **only** when the mode string contained `e` (`gzlib.c` L135-L136). `std` sets
/// it **unconditionally** on every file it opens, and `OpenOptionsExt::custom_flags` cannot take it
/// away -- `std` computes its flag word as `O_CLOEXEC | access | creation | custom`, so a custom flag
/// can only add. There is therefore no way to reproduce C's flag word exactly through
/// [`OpenOptions`], and pretending otherwise would be the wrong kind of quiet.
///
/// The consequence is narrow and it errs in the safe direction: a stream opened without `e` is *not*
/// inherited across an `exec` here, where C would have let it be inherited. It cannot leak a
/// descriptor a caller expected to stay private; it can only withhold one a caller expected a child
/// to receive.
///
/// [`gz_open_with`] is the escape hatch, and it is why that function takes the opener as a parameter
/// rather than being an abstraction for its own sake. `crates/libz-rs-sys` may write `unsafe`, so it
/// can call `open(2)` with C's flag word verbatim -- conditional `O_CLOEXEC` included, read from
/// [`GzOpenSpec::cloexec`] -- wrap the descriptor, and hand the result back as a
/// [`GzFileSlot::Boxed`]. Everything after that is this module's ordinary code.
///
/// C's mode argument to `open` is `0666` and `std`'s default is the same, so a created file lands
/// with identical permissions before `umask`.
///
/// # Errors
///
/// The platform's `errno` from the failed `open`, or the crate-internal `UNREPRESENTABLE_PATH`
/// for a name this platform cannot express.
pub fn open_with_std<'a>(
    target: GzPathTarget<'_>,
    spec: &GzOpenSpec,
) -> Result<GzFileSlot<'a>, GzIoError> {
    let file = open_path(target, spec)?;
    // `writable` is C's `state->mode != GZ_READ`, and it selects whether `close` flushes first.
    let writable = spec.mode != GZ_READ;
    Ok(GzFileSlot::Owned(FileHandle::from_file(
        file,
        writable,
        spec.nonblocking,
    )))
}

/// Opens a gzip file by name, with the opening itself supplied by the caller: the port of `gz_open`
/// (`gzlib.c` L87-L285) on its `fd == -1` path.
///
/// The steps are C's, in C's order, and the order matters because each one can fail and C's cleanup
/// differs at each point:
///
/// 1. parse and resolve the mode string ([`GzOpenSpec::parse`], L108-L197);
/// 2. build the state and write the four caller-visible decisions into it (L103-L112, L180-L195);
/// 3. copy the *label*, "for error messages" (L199-L226);
/// 4. open the file (L228-L263) -- through `open`, which is where this function differs from C;
/// 5. install the handle, then position and reset the stream (`finish_open`, L269-L284).
///
/// Step 5 orders the fallible work differently from C, and the ordering is not a strictness
/// claim -- it is what ownership requires. `GzState` releases its buffers, its path and -- through
/// [`GzHandle::close`] -- its file when it is dropped, so a handle installed before a fallible step
/// would be closed by the unwinding of that step. Every fallible step therefore happens *before*
/// the handle exists, which is also why nothing in `finish_open` is allowed to fail. C reaches the
/// same **observable** arrangement from the opposite direction, by hand: it frees `state->path` and
/// `state` on the failing path at L265-L267 and has nothing to close, because it only reached that
/// line by *not* getting a descriptor. So the two agree on what a caller sees; only the mechanism
/// differs, and that is not a divergence.
///
/// One allocation on this path is genuinely different, and it **is** a divergence: boxing the
/// handle. `GzState`'s handle slot is a `Box<dyn GzHandle>`, and stable Rust 1.80 has no fallible
/// way to build one, so a three-word allocation failing here **aborts the process** where C would
/// return `Z_MEM_ERROR`. That is divergence 5 in this module's inventory -- forced and unresolved.
/// The window is narrow, because an allocator that cannot serve three words could not have served
/// the `GzState` being installed into, but `test/infcover.c`'s `mem_limit` exists to drive exactly
/// this kind of failure on purpose, so narrowness is not the same as unreachable.
///
/// # Errors
///
/// [`GzOpenError::InvalidMode`] from the mode string, [`GzOpenError::OutOfMemory`] from the label
/// copy, or [`GzOpenError::Io`] from `open`. All three are C's `NULL`.
pub fn gz_open_with<'a, A, F>(
    target: GzPathTarget<'_>,
    label: &[u8],
    mode: &[u8],
    allocator: A,
    open: F,
) -> Result<GzState<'a, A>, GzOpenError>
where
    A: Allocator<'a>,
    F: FnOnce(GzPathTarget<'_>, &GzOpenSpec) -> Result<GzFileSlot<'a>, GzIoError>,
{
    let spec = GzOpenSpec::parse(mode)?;

    let mut state = GzState::new(allocator);
    spec.apply_to(&mut state);

    // L199-L226. Stored as bytes; a POSIX path need not be UTF-8, and `gz_error` concatenates it
    // with the message exactly as C's `snprintf("%s%s%s", path, ": ", msg)` does.
    state
        .try_set_path(label)
        .map_err(|_| GzOpenError::OutOfMemory)?;

    let handle = open(target, &spec).map_err(GzOpenError::Io)?;

    // The last fallible step is behind us. This one cannot fail: it is a move, not an allocation.
    state.set_handle(handle);

    finish_open(&mut state);
    Ok(state)
}

/// Opens a gzip file by byte path, using this crate's own opener.
///
/// [`gz_open_with`] with [`open_with_std`] and with the path serving as its own label, which is what
/// C does for the narrow entry points: `gz_open` copies the same `const char *` into `state->path`
/// that it hands to `open` (`gzlib.c` L222 and L248).
///
/// # Errors
///
/// As [`gz_open_with`].
pub(crate) fn gz_open<'a, A: Allocator<'a>>(
    path: &[u8],
    mode: &[u8],
    allocator: A,
) -> Result<GzState<'a, A>, GzOpenError> {
    gz_open_with(
        GzPathTarget::Narrow(path),
        path,
        mode,
        allocator,
        open_with_std,
    )
}

/// Opens a gzip file over a handle somebody else built: the port of `gz_open` on its adopted-`fd`
/// path (`gzlib.c` L253-L263, and the shared remainder L269-L284).
///
/// This is the injection point the safe core needs and cannot avoid needing. `FromRawFd` is
/// `unsafe`, so a descriptor that arrives from a C caller can only be turned into something usable
/// by `crates/libz-rs-sys`, inside its own audited block; the resulting object comes back here as a
/// `Box<dyn GzHandle>` and every line after that is ordinary safe code. [`gzdopen`] is the thin
/// wrapper that adds C's `"<fd:%d>"` label; call this directly when the label is already known --
/// for instance when adapting something that is not a descriptor at all.
///
/// `path_label` is stored verbatim as the state's path and is used for nothing but prefixing error
/// messages, exactly as C's `state->path` is (`gzguts.h` L179).
///
/// The `N` character reaches the handle through [`GzHandle::set_nonblocking`], which is the
/// `fcntl(fd, F_SETFL, fcntl(fd, F_GETFL) | O_NONBLOCK)` of `gzlib.c` L254-L257. Its result is
/// discarded, because C discards `fcntl`'s. The `e` character has no such route -- [`GzHandle`] has
/// no descriptor-flag method -- so the facade applies `F_SETFD` itself while adopting, reading
/// [`GzOpenSpec::cloexec`]; parsing the mode string with [`GzOpenSpec::parse`] before adopting is
/// what `zlib.h` L1415-L1416 requires anyway, so the flag is available at exactly the moment it is
/// needed. This is the descriptor-side counterpart of the path-side hook in [`gz_open_with`].
///
/// # Errors
///
/// [`GzOpenHandleError`], which carries `handle` back **unclosed**: that is `zlib.h` L1415-L1416's
/// "`gzdopen` does not close `fd` if it fails". The only two ways to get here are an unusable mode
/// string and a failed path-label allocation, and both are checked before the handle is installed,
/// so a returned handle has never been touched -- not read from, not written to, not sought, not
/// closed.
pub fn gz_open_handle<'a, A: Allocator<'a>>(
    mut handle: Box<dyn GzHandle + 'a>,
    path_label: &[u8],
    mode: &[u8],
    allocator: A,
) -> Result<GzState<'a, A>, GzOpenHandleError<'a>> {
    let spec = match GzOpenSpec::parse(mode) {
        Ok(spec) => spec,
        Err(error) => return Err(GzOpenHandleError { error, handle }),
    };

    let mut state = GzState::new(allocator);
    spec.apply_to(&mut state);

    if state.try_set_path(path_label).is_err() {
        return Err(GzOpenHandleError {
            error: GzOpenError::OutOfMemory,
            handle,
        });
    }

    if spec.nonblocking {
        // L254-L257, with C's unchecked `fcntl` reproduced as a discarded result.
        let _ = handle.set_nonblocking(true);
    }

    // L262: `state->fd = fd;`. Nothing after this point can fail.
    state.set_handle(GzFileSlot::Boxed(handle));

    finish_open(&mut state);
    Ok(state)
}

/// Opens a gzip file by path. The Rust counterpart of **both** `gzopen` (`gzlib.c` L288-L290) and `gzopen64`
/// (`gzlib.c` L293-L295).
///
/// The two C functions are byte-for-byte identical -- each one is `return gz_open(path, -1, mode);`
/// -- and they both exist only because of the large-file naming scheme. `zconf.h` redirects the
/// unsuffixed name to the suffixed one, or declares only the suffixed one, according to the
/// *caller's* `_FILE_OFFSET_BITS` and `_LARGEFILE64_SOURCE` at the caller's own compile time
/// (`zlib.h` L1976-L2018). Which name a given object file references is therefore decided long
/// before this library is reached, and **both must be exported** (AAP §0.6.3.4). That is a symbol
/// table question, not an implementation question, so there is one function here and the planned
/// `crates/libz-rs-sys/src/gz.rs` is to export two `#[no_mangle]` wrappers against it. Nothing in
/// this crate is `#[no_mangle]` or `extern "C"`.
///
/// The same reasoning forbids a fixed offset width anywhere on this path: the offset type is
/// `crate::gz::state::ZOff64`, and narrowing it to the caller's `z_off_t` is
/// `crates/libz-rs-sys`'s job, done the way `gzseek`, `gztell` and `gzoffset` do it -- compute in
/// 64 bits, then check `ret == (z_off_t)ret` before narrowing (`gzlib.c` L438-L442, L461-L465,
/// L490-L495).
///
/// `path` is the caller's `const char *` as bytes without the terminating NUL, and `mode` likewise;
/// the null-pointer rejection of `gzlib.c` L96-L97 belongs to the facade, which is where pointers
/// exist. An empty `path` is *not* rejected here, and must not be: C hands it to `open`, which
/// answers `ENOENT`, and that distinction is visible to a caller through `errno`.
///
/// # Errors
///
/// As `gz_open`: [`GzOpenError::InvalidMode`], [`GzOpenError::OutOfMemory`] or
/// [`GzOpenError::Io`]. The facade maps all of them to `NULL`.
#[doc(alias = "gzopen64")]
pub fn gzopen<'a, A: Allocator<'a>>(
    path: &[u8],
    mode: &[u8],
    allocator: A,
) -> Result<GzState<'a, A>, GzOpenError> {
    gz_open(path, mode, allocator)
}

/// Capacity of the buffer C's `gzdopen` label is built in.
///
/// C allocates `7 + 3 * sizeof(int)` bytes (`gzlib.c` L302 and L305), which is 19 on every target
/// where `int` is 32 bits. The longest label the format can actually produce is
/// `"<fd:-2147483648>"`, 16 bytes, so neither C's buffer nor this one ever truncates. 24 is used
/// here rather than 19 so that the bound is obviously slack and stays slack if `i32` is ever
/// formatted differently.
const FD_LABEL_CAPACITY: usize = 24;

/// A fixed-capacity byte buffer that [`fmt::Write`] can format into.
///
/// Exists so that [`gzdopen`] can build C's `"<fd:%d>"` label with neither an allocation nor a
/// panic. `alloc::format!` would satisfy the first requirement badly: it aborts the process if the
/// allocation fails -- and hand-rolling decimal conversion would satisfy the second while
/// duplicating logic `core` already has. Formatting into a fixed buffer through [`fmt::Write`] gets
/// both: the format string stays a format string, so it can be compared against C's by eye, and an
/// overrun is reported as [`fmt::Error`] instead of growing or aborting.
#[derive(Debug)]
struct FdLabel {
    /// The formatted bytes, valid up to [`FdLabel::len`].
    bytes: [u8; FD_LABEL_CAPACITY],
    /// How much of [`FdLabel::bytes`] has been written.
    len: usize,
}

impl FdLabel {
    /// An empty buffer.
    const fn new() -> Self {
        Self {
            bytes: [0; FD_LABEL_CAPACITY],
            len: 0,
        }
    }

    /// The bytes written so far, without a terminating NUL.
    ///
    /// `get` rather than an index expression: the crate denies panicking indexing, and a slice that
    /// somehow did not exist is better reported as empty than as an abort.
    fn as_bytes(&self) -> &[u8] {
        self.bytes.get(..self.len).unwrap_or(&[])
    }
}

impl fmt::Write for FdLabel {
    /// Appends `s`, reporting [`fmt::Error`] rather than truncating or growing if it does not fit.
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let source = s.as_bytes();
        let end = self.len.checked_add(source.len()).ok_or(fmt::Error)?;
        let slot = self.bytes.get_mut(self.len..end).ok_or(fmt::Error)?;
        slot.copy_from_slice(source);
        self.len = end;
        Ok(())
    }
}

/// Associates a gzip stream with a file somebody else opened: the Rust counterpart of `gzdopen`
/// (`gzlib.c` L298-L312).
///
/// C's body is four steps: reject `fd == -1`, `malloc` a small buffer, `snprintf` the label
/// `"<fd:%d>"` into it, call `gz_open(path, fd, mode)`, and free the label. The label format is
/// reproduced exactly, because it is caller-visible: it becomes `state->path`, and `gz_error`
/// prefixes every message with it, so `gzerror` on a stream opened this way reports text like
/// `"<fd:3>: out of memory"`.
///
/// Building the label in a fixed stack buffer rather than on the heap **removes C's
/// `malloc`-failure path**, which is divergence 6 in this module's inventory -- forced and
/// unresolved. A caller cannot observe `gzdopen` failing for want of memory the way it can in C,
/// because that failure mode no longer exists here; [`GzOpenError::OutOfMemory`] can only come from
/// the path copy inside [`gz_open_handle`]. Fewer failure modes is still different behaviour, and it
/// is recorded as such rather than as a simplification.
///
/// `fd` is used **only** to build that label. The actual I/O goes through `handle`, because a
/// descriptor cannot be adopted in safe Rust; see this module's documentation for the division of
/// labour and for what the facade owes on failure.
///
/// # Errors
///
/// [`GzOpenHandleError`], with `handle` returned unclosed:
///
/// * [`GzOpenError::InvalidDescriptor`] when `fd` is `-1`, C's first test (`gzlib.c` L302) and
///   `zlib.h` L1424's "or if `fd` is `-1`";
/// * whatever [`gz_open_handle`] reports otherwise.
pub fn gzdopen<'a, A: Allocator<'a>>(
    handle: Box<dyn GzHandle + 'a>,
    fd: i32,
    mode: &[u8],
    allocator: A,
) -> Result<GzState<'a, A>, GzOpenHandleError<'a>> {
    if fd == -1 {
        return Err(GzOpenHandleError {
            error: GzOpenError::InvalidDescriptor,
            handle,
        });
    }

    let mut label = FdLabel::new();
    // L305: `snprintf(path, 7 + 3 * sizeof(int), "<fd:%d>", fd)`. The buffer is provably large
    // enough, so the error arm is unreachable in practice; it exists because the alternative to
    // handling it is an `unwrap`, and this crate does not panic. C's corresponding failure is the
    // `malloc` at L302 returning NULL, which is also an out-of-space condition answered by NULL.
    if fmt::Write::write_fmt(&mut label, format_args!("<fd:{fd}>")).is_err() {
        return Err(GzOpenHandleError {
            error: GzOpenError::OutOfMemory,
            handle,
        });
    }

    // L309: `gz = gz_open(path, fd, mode);` -- and L310's `free(path)` needs no counterpart, since
    // the label lives on the stack and `gz_open_handle` copies it.
    gz_open_handle(handle, label.as_bytes(), mode, allocator)
}

/// Opens a gzip file named by a wide-character path: the Rust counterpart of `gzopen_w`
/// (`gzlib.c` L316-L318).
///
/// C guards both the declaration (`zlib.h` L2041-L2044, `#if defined(_WIN32) && !defined(Z_SOLO)`)
/// and the definition (`#ifdef WIDECHAR`, which `gzguts.h` L54-L56 defines for `_WIN32`) so the
/// symbol exists only on Windows, and this follows suit with `#[cfg(windows)]`. It is exported by
/// `crates/libz-rs-sys` on Windows targets only.
///
/// **The conversion is the facade's job, and it must produce UTF-8.** C converts inside `gz_open`
/// with `wcstombs` (`gzlib.c` L200-L219), which yields the current locale's multibyte encoding; the
/// AAP places that kind of C-string handling at the ABI boundary (§0.6.1, category 5), so what
/// arrives here is bytes. On Windows [`os_path`] must decode those bytes, and the only encoding it
/// can decode portably is UTF-8 -- so the facade should convert the `wchar_t *` with
/// `char::decode_utf16` or `String::from_utf16` rather than with a locale-dependent narrowing.
///
/// **This is a behaviour difference, and it is recorded as one rather than claimed as an
/// improvement.** `wcstombs` narrows to the active code page and fails for any character that page
/// cannot represent, so a UTF-8 conversion accepts paths C would reject. Wider acceptance is still
/// different acceptance: a caller that relied on C's failure for such a path sees success here.
/// The difference is forced -- [`os_path`] cannot decode a locale-dependent encoding portably from
/// safe Rust -- and it applies only to `gzopen_w`, which is Windows-only and outside the Tier-1
/// target set this port is verified on. It stands as unresolved.
///
/// # Errors
///
/// As `gz_open`.
#[cfg(windows)]
pub fn gzopen_w<'a, A: Allocator<'a>>(
    path: &[u16],
    label: &[u8],
    mode: &[u8],
    allocator: A,
) -> Result<GzState<'a, A>, GzOpenError> {
    gz_open_with(
        GzPathTarget::Wide(path),
        label,
        mode,
        allocator,
        open_with_std,
    )
}

#[cfg(test)]
mod tests {
    // The crate denies the panic-prone lints in library code, which is the right policy there and
    // the wrong one here: a test asserts, and a failed assertion panics. Scoped to this module, so
    // it relaxes nothing that ships.
    #![allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

    // `extern crate std;` binds `std` in the parent module only -- an `extern crate` binding is not
    // inherited by child modules -- so the binding is re-imported here rather than declared twice.
    use super::std;

    use super::{
        gz_open, gz_open_handle, gz_open_with, gz_reset, gzdopen, gzopen, open_with_std, os_path,
        FdLabel, FileHandle, GzOpenError, GzOpenSpec, GzPathTarget,
    };
    // `o_nonblock` is itself `#[cfg(unix)]`, so the import has to be too: an ungated one is an
    // unresolved import when checking a non-unix target, which `cargo check --all-targets --target
    // x86_64-pc-windows-gnu` does reach.
    #[cfg(unix)]
    use super::o_nonblock;
    use crate::allocate::GlobalAllocator;
    use crate::config::{
        Z_DEFAULT_COMPRESSION, Z_DEFAULT_STRATEGY, Z_FILTERED, Z_FIXED, Z_HUFFMAN_ONLY, Z_RLE,
    };
    use crate::error::ReturnCode;
    use crate::gz::state::{
        GzFileSlot, GzHandle, GzIoError, GzMode, GzSeekFrom, GzState, ZOff64, GZ_APPEND, GZ_READ,
        GZ_WRITE, LOOK,
    };
    use alloc::boxed::Box;
    use alloc::format;
    use alloc::string::String;
    use alloc::vec::Vec;
    use core::cell::Cell;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A [`GzHandle`] that records what was asked of it and answers with canned results.
    ///
    /// Enough to exercise every path through [`gz_open_handle`] and [`gzdopen`] with no real file,
    /// which also keeps those tests runnable under Miri.
    struct RecordingHandle<'c> {
        /// How often [`GzHandle::close`] was called.
        closes: &'c Cell<usize>,
        /// The last value passed to [`GzHandle::set_nonblocking`], or [`None`] if never called.
        nonblocking: &'c Cell<Option<bool>>,
        /// How often [`GzHandle::seek`] was called.
        seeks: &'c Cell<usize>,
        /// What [`GzHandle::seek`] answers: [`Some`] position, or [`None`] to fail like a pipe.
        seek_answer: Option<ZOff64>,
    }

    impl GzHandle for RecordingHandle<'_> {
        fn read(&mut self, _buf: &mut [u8]) -> Result<usize, GzIoError> {
            Ok(0)
        }

        fn write(&mut self, buf: &[u8]) -> Result<usize, GzIoError> {
            Ok(buf.len())
        }

        fn seek(&mut self, _offset: ZOff64, _whence: GzSeekFrom) -> Result<ZOff64, GzIoError> {
            self.seeks.set(self.seeks.get() + 1);
            self.seek_answer.ok_or(GzIoError::new(29, false))
        }

        fn set_nonblocking(&mut self, nonblocking: bool) -> Result<(), GzIoError> {
            self.nonblocking.set(Some(nonblocking));
            Ok(())
        }

        fn close(&mut self) -> Result<(), GzIoError> {
            self.closes.set(self.closes.get() + 1);
            Ok(())
        }
    }

    /// Every counter a [`RecordingHandle`] writes into, so one test can own them all.
    struct Recorder {
        /// Close count.
        closes: Cell<usize>,
        /// Last non-blocking request.
        nonblocking: Cell<Option<bool>>,
        /// Seek count.
        seeks: Cell<usize>,
    }

    impl Recorder {
        /// A recorder with everything at zero.
        fn new() -> Self {
            Self {
                closes: Cell::new(0),
                nonblocking: Cell::new(None),
                seeks: Cell::new(0),
            }
        }

        /// A handle reporting `seek_answer` from every seek.
        fn handle(&self, seek_answer: Option<ZOff64>) -> Box<dyn GzHandle + '_> {
            Box::new(RecordingHandle {
                closes: &self.closes,
                nonblocking: &self.nonblocking,
                seeks: &self.seeks,
                seek_answer,
            })
        }
    }

    /// A unique path under the system temporary directory.
    ///
    /// Built by hand rather than with a crate: `zlib-rs` has no dependencies and no
    /// dev-dependencies, by design (AAP §0.7.1 (i)), so `tempfile` is unavailable. The process id
    /// separates concurrent test binaries and the counter separates tests within one binary.
    fn temp_path(tag: &str) -> String {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut path = std::env::temp_dir();
        path.push(format!(
            "blitzy_zlib_rs_gz_open_{}_{tag}_{unique}",
            std::process::id()
        ));
        path.to_string_lossy().into_owned()
    }

    /// Best-effort cleanup; a leftover file must never fail a test.
    fn remove(path: &str) {
        let _ = std::fs::remove_file(path);
    }

    /// Writes every byte, looping over short writes exactly as `gz_comp` does.
    ///
    /// [`GzHandle::write`] is contractually allowed to report a short write
    /// (`gzwrite.c` L110-L123 loops for that reason), so a test that assumed one call moved
    /// everything would be testing the platform rather than the library. Miri's `write` shim makes
    /// the point concrete by returning short counts.
    fn write_all_through(handle: &mut dyn GzHandle, mut bytes: &[u8]) {
        while !bytes.is_empty() {
            let written = handle.write(bytes).unwrap();
            assert!(written > 0, "a successful write must make progress");
            bytes = &bytes[written..];
        }
    }

    /// Reads until the buffer is full or end of file, looping as `gz_load` does.
    ///
    /// Implements [`write_all_through`]: [`GzHandle::read`] may return fewer bytes than asked
    /// for, and `Ok(0)` is end of file (`gzread.c` L31-L46).
    fn read_fully(handle: &mut dyn GzHandle, buffer: &mut [u8]) -> usize {
        let mut filled = 0;
        while filled < buffer.len() {
            let read = handle.read(&mut buffer[filled..]).unwrap();
            if read == 0 {
                break;
            }
            filled += read;
        }
        filled
    }

    /// A state in the condition `gz_open` leaves one in for the given direction, with no file.
    fn state_for(mode: i32) -> GzState<'static, GlobalAllocator> {
        let mut state = GzState::new(GlobalAllocator);
        state.set_mode(mode);
        state
    }

    /// Every mode string documented in `zlib.h` L1356-L1401 and every one the test suite builds,
    /// with the four caller-visible decisions each must produce.
    #[test]
    fn mode_grammar_table() {
        // Abbreviated so that each row stays on one line and the table can be read as a table.
        const DEF_LVL: i32 = Z_DEFAULT_COMPRESSION;
        const DEF_STR: i32 = Z_DEFAULT_STRATEGY;

        // (mode string, mode, level, strategy, direct)
        let cases: &[(&[u8], i32, i32, i32, i32)] = &[
            // `test/example.c` L99 and L116.
            (b"rb", GZ_READ, DEF_LVL, DEF_STR, 1),
            (b"wb", GZ_WRITE, DEF_LVL, DEF_STR, 0),
            (b"ab", GZ_APPEND, DEF_LVL, DEF_STR, 0),
            // Levels: `zlib.h` L1361 ("wb9").
            (b"wb9", GZ_WRITE, 9, DEF_STR, 0),
            (b"wb0", GZ_WRITE, 0, DEF_STR, 0),
            // `test/minigzip.c` L511 builds exactly this, trailing space included.
            (b"wb6 ", GZ_WRITE, 6, DEF_STR, 0),
            // Strategies: `zlib.h` L1361-L1364 spells out all four examples.
            (b"wb6f", GZ_WRITE, 6, Z_FILTERED, 0),
            (b"wb1h", GZ_WRITE, 1, Z_HUFFMAN_ONLY, 0),
            (b"wb6h", GZ_WRITE, 6, Z_HUFFMAN_ONLY, 0),
            (b"wb1R", GZ_WRITE, 1, Z_RLE, 0),
            (b"wb6R", GZ_WRITE, 6, Z_RLE, 0),
            (b"wb9F", GZ_WRITE, 9, Z_FIXED, 0),
            // `T` requests a transparent write; `G` requests gzip-only reading.
            (b"wbT", GZ_WRITE, DEF_LVL, DEF_STR, 1),
            (b"abT", GZ_APPEND, DEF_LVL, DEF_STR, 1),
            (b"rbG", GZ_READ, DEF_LVL, DEF_STR, -1),
            // Flag characters do not disturb the four decisions.
            (b"rbe", GZ_READ, DEF_LVL, DEF_STR, 1),
            (b"rbN", GZ_READ, DEF_LVL, DEF_STR, 1),
            (b"wbx", GZ_WRITE, DEF_LVL, DEF_STR, 0),
            // Unknown characters are ignored, not rejected (`gzlib.c` L167-L168).
            (b"rbZq", GZ_READ, DEF_LVL, DEF_STR, 1),
            // A later direction overrides an earlier one, because the walk simply assigns.
            (b"rw", GZ_WRITE, DEF_LVL, DEF_STR, 0),
            (b"wr", GZ_READ, DEF_LVL, DEF_STR, 1),
            // The last digit wins for the same reason.
            (b"wb19", GZ_WRITE, 9, DEF_STR, 0),
            // As does the last strategy letter.
            (b"wb6fh", GZ_WRITE, 6, Z_HUFFMAN_ONLY, 0),
            // Everything at once.
            (b"wb9xeNF", GZ_WRITE, 9, Z_FIXED, 0),
        ];

        for &(mode, expected_mode, level, strategy, direct) in cases {
            let spec = GzOpenSpec::parse(mode)
                .unwrap_or_else(|error| panic!("{mode:?} must be accepted, got {error:?}"));
            assert_eq!(spec.mode(), expected_mode, "mode for {mode:?}");
            assert_eq!(spec.level(), level, "level for {mode:?}");
            assert_eq!(spec.strategy(), strategy, "strategy for {mode:?}");
            assert_eq!(spec.direct(), direct, "direct for {mode:?}");
            // `parse` never yields `GZ_NONE`, so the typed view is always a real direction.
            assert!(spec.mode_typed().is_some(), "mode_typed for {mode:?}");
            assert_ne!(spec.mode_typed(), Some(GzMode::None), "{mode:?}");
        }
    }

    /// The four rejections of `gzlib.c` L129-L131, L174-L177, L181-L185 and L191-L195.
    #[test]
    fn mode_grammar_rejections() {
        let cases: &[&[u8]] = &[
            // No `r`, `w` or `a` at all.
            b"", b"b", b"9", b"bT", b"eNx",
            // `+`: reading and writing one gzip file is unsupported.
            b"rb+", b"wb+", b"ab+", b"r+b", b"wb+9",
            // `+` short-circuits before the missing-direction test, and either way it fails.
            b"+", // `T` cannot force a transparent read.
            b"rbT", b"rT", // `G` has no meaning when writing or appending.
            b"wbG", b"abG",
        ];

        for &mode in cases {
            assert_eq!(
                GzOpenSpec::parse(mode),
                Err(GzOpenError::InvalidMode),
                "{mode:?} must be rejected"
            );
        }
    }

    /// `e`, `x` and `N` set the three `open`-flag requests and nothing else.
    #[test]
    fn mode_grammar_flags() {
        let plain = GzOpenSpec::parse(b"rb").unwrap();
        assert!(!plain.cloexec());
        assert!(!plain.exclusive());
        assert!(!plain.nonblocking());

        assert!(GzOpenSpec::parse(b"rbe").unwrap().cloexec());
        assert!(GzOpenSpec::parse(b"wbx").unwrap().exclusive());
        assert!(GzOpenSpec::parse(b"rbN").unwrap().nonblocking());

        let all = GzOpenSpec::parse(b"wb9xeN").unwrap();
        assert!(all.cloexec());
        assert!(all.exclusive());
        assert!(all.nonblocking());
        assert_eq!(all.level(), 9);
    }

    /// The last `G` or `T` wins, and which one it is decides whether the open is legal.
    #[test]
    fn last_g_or_t_wins() {
        // Reading: `G` last is gzip-only and legal; `T` last forces a transparent read and is not.
        assert_eq!(GzOpenSpec::parse(b"rbTG").unwrap().direct(), -1);
        assert_eq!(GzOpenSpec::parse(b"rbGT"), Err(GzOpenError::InvalidMode));
        // Writing: `T` last is a transparent copy and legal; `G` last is meaningless and is not.
        assert_eq!(GzOpenSpec::parse(b"wbGT").unwrap().direct(), 1);
        assert_eq!(GzOpenSpec::parse(b"wbTG"), Err(GzOpenError::InvalidMode));
    }

    /// A NUL ends the walk, exactly as C's `while (*mode)` does.
    ///
    /// A facade passing a `CStr`'s bytes can never contain one, but a caller passing a fixed-size
    /// buffer can, and C's answer to that is unambiguous.
    #[test]
    fn a_nul_ends_the_mode_string() {
        // The `w` is past the terminator and must not be seen.
        assert_eq!(GzOpenSpec::parse(b"rb\0w").unwrap().mode(), GZ_READ);
        // Neither must the `+`, which would otherwise reject the whole string.
        assert_eq!(GzOpenSpec::parse(b"rb\0+").unwrap().mode(), GZ_READ);
        // Nor a level.
        assert_eq!(
            GzOpenSpec::parse(b"wb\0 9").unwrap().level(),
            Z_DEFAULT_COMPRESSION
        );
        // A leading NUL leaves no direction at all.
        assert_eq!(GzOpenSpec::parse(b"\0rb"), Err(GzOpenError::InvalidMode));
    }

    /// Each failure names itself with the library's own vocabulary.
    #[test]
    fn as_return_code_maps_each_failure() {
        assert_eq!(
            GzOpenError::InvalidMode.as_return_code(),
            ReturnCode::STREAM_ERROR
        );
        assert_eq!(
            GzOpenError::InvalidDescriptor.as_return_code(),
            ReturnCode::STREAM_ERROR
        );
        assert_eq!(
            GzOpenError::OutOfMemory.as_return_code(),
            ReturnCode::MEM_ERROR
        );
        assert_eq!(
            GzOpenError::Io(GzIoError::new(2, false)).as_return_code(),
            ReturnCode::ERRNO
        );
    }

    /// `gz_reset` on a read stream restores every field C's L71-L76 branch restores.
    #[test]
    fn gz_reset_restores_a_read_stream() {
        let mut state = state_for(GZ_READ);
        // Dirty every field the reset is responsible for, as a read would have left them.
        state.set_have(7);
        state.set_pos(1234);
        state.set_eof(true);
        state.set_past(true);
        state.set_how(2);
        state.set_junk(0);
        state.set_again(true);
        state.set_skip(99);
        state.stream_mut().avail_in = 42;

        gz_reset(&mut state);

        assert_eq!(state.have(), 0, "L70");
        assert_eq!(state.pos(), 0, "L82");
        assert!(!state.eof(), "L72");
        assert!(!state.past(), "L73");
        assert_eq!(state.how(), LOOK, "L74");
        assert_eq!(state.junk(), -1, "L75: mark first member");
        assert!(!state.again(), "L79");
        assert_eq!(state.skip(), 0, "L80");
        assert_eq!(state.stream().avail_in, 0, "L83");
        assert_eq!(state.err(), ReturnCode::OK.as_i32(), "L81");
        assert!(state.msg().is_none(), "L81");
    }

    /// On a write stream `gz_reset` clears the pending-reset flag and leaves the read-only flags
    /// alone, because C's `if (state->mode == GZ_READ)` branch is not taken.
    #[test]
    fn gz_reset_takes_the_writing_branch_for_a_write_stream() {
        let mut state = state_for(GZ_WRITE);
        state.set_reset_pending(true);
        // These belong to the read path. C does not touch them when writing, and neither may this.
        state.set_eof(true);
        state.set_past(true);
        state.set_how(1);
        state.set_junk(0);

        gz_reset(&mut state);

        assert!(!state.reset_pending(), "L78");
        assert!(state.eof(), "L72 is inside the read-only branch");
        assert!(state.past(), "L73 is inside the read-only branch");
        assert_eq!(state.how(), 1, "L74 is inside the read-only branch");
        assert_eq!(state.junk(), 0, "L75 is inside the read-only branch");
    }

    /// `gz_reset` preserves what a `gzrewind` must preserve.
    ///
    /// `gzrewind` seeks back to `state->start` and calls `gz_reset` (`gzlib.c` L360-L362), so a
    /// buffer size chosen with `gzbuffer`, the resolved container handling and the recorded start
    /// must all survive.
    #[test]
    fn gz_reset_preserves_the_stream_configuration() {
        let mut state = state_for(GZ_READ);
        state.set_want(4096);
        state.set_direct(-1);
        state.set_start(8192);
        state.set_level(9);
        state.set_strategy(Z_RLE);

        gz_reset(&mut state);

        assert_eq!(state.want(), 4096);
        assert_eq!(state.direct(), -1);
        assert_eq!(state.start(), 8192);
        assert_eq!(state.level(), 9);
        assert_eq!(state.strategy(), Z_RLE);
    }

    /// The error is cleared through `gz_error`, message included.
    #[test]
    fn gz_reset_clears_a_recorded_error() {
        let mut state = state_for(GZ_READ);
        state.try_set_path(b"/tmp/whatever.gz").unwrap();
        state.set_err(ReturnCode::DATA_ERROR.as_i32());
        state.try_set_prefixed_msg(b"invalid block type").unwrap();
        assert!(state.msg().is_some());

        gz_reset(&mut state);

        assert_eq!(state.err(), ReturnCode::OK.as_i32());
        assert!(state.msg().is_none());
        // The path is *not* cleared: it identifies the stream for the rest of its life.
        assert_eq!(state.path(), b"/tmp/whatever.gz");
    }

    /// The label is byte-for-byte C's `snprintf(path, ..., "<fd:%d>", fd)` (`gzlib.c` L305).
    #[test]
    fn fd_label_matches_the_c_format() {
        let cases: &[(i32, &[u8])] = &[
            (0, b"<fd:0>"),
            (1, b"<fd:1>"),
            (3, b"<fd:3>"),
            (1024, b"<fd:1024>"),
            (i32::MAX, b"<fd:2147483647>"),
            // Not reachable through `gzdopen`, which rejects -1, but the format must still fit.
            (i32::MIN, b"<fd:-2147483648>"),
        ];
        for &(fd, expected) in cases {
            let mut label = FdLabel::new();
            core::fmt::Write::write_fmt(&mut label, format_args!("<fd:{fd}>")).unwrap();
            assert_eq!(label.as_bytes(), expected);
        }
    }

    /// An empty label reports an empty slice rather than the whole buffer.
    #[test]
    fn fd_label_starts_empty() {
        assert_eq!(FdLabel::new().as_bytes(), b"");
    }

    /// `gzdopen` rejects `fd == -1` and hands the handle straight back, unclosed.
    ///
    /// C's first test (`gzlib.c` L302) and `zlib.h` L1424. The handle must come back untouched,
    /// because `zlib.h` L1415-L1416 promises the descriptor is not closed on failure.
    #[test]
    fn gzdopen_rejects_minus_one_and_returns_the_handle() {
        let recorder = Recorder::new();
        let failure = gzdopen(recorder.handle(Some(0)), -1, b"rb", GlobalAllocator)
            .expect_err("fd == -1 must be rejected");
        assert_eq!(failure.error, GzOpenError::InvalidDescriptor);
        drop(failure.handle);
        assert_eq!(recorder.closes.get(), 0, "a returned handle is not closed");
        assert_eq!(recorder.seeks.get(), 0, "nor used");
    }

    /// An unusable mode string returns the handle just as untouched.
    #[test]
    fn gz_open_handle_returns_the_handle_on_an_invalid_mode() {
        let recorder = Recorder::new();
        let failure = gz_open_handle(recorder.handle(Some(0)), b"<fd:9>", b"rbT", GlobalAllocator)
            .expect_err("a forced transparent read must be rejected");
        assert_eq!(failure.error, GzOpenError::InvalidMode);
        drop(failure.handle);
        assert_eq!(recorder.closes.get(), 0);
        assert_eq!(recorder.seeks.get(), 0);
        assert_eq!(recorder.nonblocking.get(), None);
    }

    /// `gzdopen` labels the stream `"<fd:N>"`, which is what `gzerror` will prefix its messages
    /// with.
    #[test]
    fn gzdopen_labels_the_stream_with_the_descriptor() {
        let recorder = Recorder::new();
        let mut state = gzdopen(recorder.handle(Some(0)), 7, b"rb", GlobalAllocator)
            .expect("a valid descriptor and mode must open");
        assert_eq!(state.path(), b"<fd:7>");
        assert_eq!(state.mode(), GZ_READ);
        assert_eq!(state.direct(), 1);
        // The read branch of `finish_open` asked the handle where it was.
        assert_eq!(recorder.seeks.get(), 1);
        assert_eq!(state.start(), 0);
        // And the message really is prefixed with the label.
        state
            .try_set_prefixed_msg(b"unexpected end of file")
            .unwrap();
        assert_eq!(state.msg(), Some(&b"<fd:7>: unexpected end of file"[..]));
        drop(state);
        assert_eq!(recorder.closes.get(), 1, "teardown closes the handle");
    }

    /// An unseekable stream still opens, with `start` falling back to zero.
    ///
    /// This is the `gzdopen(fileno(stdin), "rb")` of `test/minigzip.c` L549: a pipe answers `lseek`
    /// with `ESPIPE`, and `gzlib.c` L277 substitutes zero rather than failing.
    #[test]
    fn an_unseekable_read_stream_starts_at_zero() {
        let recorder = Recorder::new();
        let state = gzdopen(recorder.handle(None), 0, b"rb", GlobalAllocator)
            .expect("an unseekable descriptor must still open");
        assert_eq!(recorder.seeks.get(), 1, "the seek was attempted");
        assert_eq!(state.start(), 0, "and its failure was tolerated");
        assert_eq!(state.how(), LOOK);
        drop(state);
    }

    /// A seekable read stream records where it started.
    #[test]
    fn a_seekable_read_stream_records_its_start() {
        let recorder = Recorder::new();
        let state = gzdopen(recorder.handle(Some(4096)), 5, b"rb", GlobalAllocator)
            .expect("a seekable descriptor must open");
        assert_eq!(state.start(), 4096);
        drop(state);
    }

    /// `N` reaches an adopted handle through `set_nonblocking`, and only then.
    ///
    /// C applies `fcntl(fd, F_SETFL, ... | O_NONBLOCK)` for an adopted descriptor
    /// (`gzlib.c` L254-L257) and folds the flag into `open` for a path, so a mode string without
    /// `N` must leave the handle alone.
    #[test]
    fn nonblocking_reaches_an_adopted_handle() {
        let recorder = Recorder::new();
        let state =
            gzdopen(recorder.handle(Some(0)), 3, b"rbN", GlobalAllocator).expect("rbN must open");
        assert_eq!(recorder.nonblocking.get(), Some(true));
        drop(state);

        let quiet = Recorder::new();
        let state =
            gzdopen(quiet.handle(Some(0)), 3, b"rb", GlobalAllocator).expect("rb must open");
        assert_eq!(quiet.nonblocking.get(), None);
        drop(state);
    }

    /// Appending over an adopted handle seeks to the end and then reports itself as writing.
    ///
    /// `gzlib.c` L269-L272: the seek is "so that `gzoffset()` is correct" and the mode change is
    /// "to simplify later checks", after which no entry point ever observes `GZ_APPEND`.
    #[test]
    fn appending_seeks_to_the_end_and_becomes_writing() {
        let recorder = Recorder::new();
        let state =
            gzdopen(recorder.handle(Some(512)), 4, b"ab", GlobalAllocator).expect("ab must open");
        assert_eq!(recorder.seeks.get(), 1, "one seek, to the end");
        assert_eq!(state.mode(), GZ_WRITE, "GZ_APPEND is transient");
        assert_eq!(state.direct(), 0);
        // The write branch of `gz_reset` ran, so nothing is pending.
        assert!(!state.reset_pending());
        drop(state);
    }

    /// A write stream never seeks and never looks at `start`.
    #[test]
    fn a_write_stream_does_not_seek() {
        let recorder = Recorder::new();
        let state =
            gzdopen(recorder.handle(Some(64)), 6, b"wb9", GlobalAllocator).expect("wb9 must open");
        assert_eq!(recorder.seeks.get(), 0);
        assert_eq!(state.start(), 0);
        assert_eq!(state.level(), 9);
        drop(state);
    }

    /// A file written through a `"wb"` stream reads back through an `"rb"` stream.
    ///
    /// The handle is exercised directly rather than through the compressor, because the compressor
    /// belongs to `write.rs` and `read.rs`; what is under test here is that `gz_open` produced a
    /// usable handle, positioned it correctly and recorded the right state.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "filesystem access is unavailable under Miri's isolation"
    )]
    fn a_file_written_by_path_reads_back() {
        let path = temp_path("round_trip");
        remove(&path);

        let mut writer =
            gzopen(path.as_bytes(), b"wb", GlobalAllocator).expect("wb must open a new file");
        assert_eq!(writer.mode(), GZ_WRITE);
        assert_eq!(writer.direct(), 0, "writing defaults to gzip");
        assert_eq!(writer.start(), 0, "start is only recorded when reading");
        assert!(writer.has_handle());
        write_all_through(writer.handle_mut().unwrap(), b"hello, hello!");
        // Dropping the state tears it down, which closes the file.
        drop(writer);

        let mut reader = gzopen(path.as_bytes(), b"rb", GlobalAllocator)
            .expect("rb must open the file just written");
        assert_eq!(reader.mode(), GZ_READ);
        assert_eq!(
            reader.direct(),
            1,
            "reading starts transparent, then detects"
        );
        assert_eq!(reader.start(), 0, "a fresh read starts at offset zero");
        assert_eq!(reader.how(), LOOK);
        let mut buffer = [0_u8; 32];
        let read = read_fully(reader.handle_mut().unwrap(), &mut buffer);
        assert_eq!(&buffer[..read], b"hello, hello!");
        drop(reader);

        remove(&path);
    }

    /// Truncation: `"wb"` over an existing file starts from empty, as `O_TRUNC` requires.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "filesystem access is unavailable under Miri's isolation"
    )]
    fn writing_truncates_an_existing_file() {
        let path = temp_path("truncate");
        std::fs::write(&path, b"0123456789").unwrap();

        let state = gzopen(path.as_bytes(), b"wb", GlobalAllocator).expect("wb must open");
        drop(state);

        assert_eq!(std::fs::metadata(&path).unwrap().len(), 0);
        remove(&path);
    }

    /// Appending by path positions at the end, so the existing contents survive.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "filesystem access is unavailable under Miri's isolation"
    )]
    fn appending_by_path_keeps_the_existing_contents() {
        let path = temp_path("append");
        std::fs::write(&path, b"first").unwrap();

        let mut state = gzopen(path.as_bytes(), b"ab", GlobalAllocator).expect("ab must open");
        assert_eq!(state.mode(), GZ_WRITE, "GZ_APPEND becomes GZ_WRITE");
        // `gz_open` sought to the end so that `gzoffset` is correct.
        assert_eq!(
            state
                .handle_mut()
                .unwrap()
                .seek(0, GzSeekFrom::Current)
                .unwrap(),
            5
        );
        write_all_through(state.handle_mut().unwrap(), b"second");
        drop(state);

        assert_eq!(std::fs::read(&path).unwrap(), b"firstsecond");
        remove(&path);
    }

    /// A missing file fails with the platform's own error, not a flattened one.
    ///
    /// `zlib.h` L1394-L1395: "`errno` can be checked to determine if the reason `gzopen` failed was
    /// that the file could not be opened."
    #[test]
    #[cfg_attr(
        miri,
        ignore = "filesystem access is unavailable under Miri's isolation"
    )]
    fn a_missing_file_reports_the_os_error() {
        let path = temp_path("missing");
        remove(&path);

        let error = gz_open(path.as_bytes(), b"rb", GlobalAllocator)
            .expect_err("reading a nonexistent file must fail");
        match error {
            GzOpenError::Io(io) => {
                assert_ne!(io.errno, 0, "the platform error number is preserved");
                assert!(!io.would_block);
            }
            other => panic!("expected an I/O failure, got {other:?}"),
        }
        assert_eq!(error.as_return_code(), ReturnCode::ERRNO);
    }

    /// `x` makes the create exclusive, so the second attempt fails with the platform's error.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "filesystem access is unavailable under Miri's isolation"
    )]
    fn exclusive_create_refuses_an_existing_file() {
        let path = temp_path("exclusive");
        remove(&path);

        let first = gzopen(path.as_bytes(), b"wbx", GlobalAllocator)
            .expect("the first exclusive create must succeed");
        drop(first);

        let error = gzopen(path.as_bytes(), b"wbx", GlobalAllocator)
            .expect_err("the second exclusive create must fail");
        match error {
            GzOpenError::Io(io) => assert_ne!(io.errno, 0),
            other => panic!("expected an I/O failure, got {other:?}"),
        }

        // Without `x` the same mode string simply truncates.
        let again = gzopen(path.as_bytes(), b"wb", GlobalAllocator);
        assert!(again.is_ok());
        drop(again);

        remove(&path);
    }

    /// `x` is a write-side flag only: `"rbx"` still opens an existing file for reading.
    ///
    /// C consults `exclusive` only inside the non-read branch of its flag computation
    /// (`gzlib.c` L236-L244).
    #[test]
    #[cfg_attr(
        miri,
        ignore = "filesystem access is unavailable under Miri's isolation"
    )]
    fn exclusive_is_ignored_when_reading() {
        let path = temp_path("exclusive_read");
        std::fs::write(&path, b"payload").unwrap();

        let state = gzopen(path.as_bytes(), b"rbx", GlobalAllocator)
            .expect("rbx must open an existing file for reading");
        assert_eq!(state.mode(), GZ_READ);
        drop(state);

        remove(&path);
    }

    /// `e` and `N` do not get in the way of a perfectly ordinary open.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "filesystem access is unavailable under Miri's isolation"
    )]
    fn cloexec_and_nonblocking_still_open() {
        let path = temp_path("flags");
        std::fs::write(&path, b"payload").unwrap();

        for mode in [&b"rbe"[..], b"rbN", b"rbeN"] {
            let state = gzopen(path.as_bytes(), mode, GlobalAllocator)
                .unwrap_or_else(|error| panic!("{mode:?} must open: {error:?}"));
            assert_eq!(state.mode(), GZ_READ);
            drop(state);
        }

        remove(&path);
    }

    /// The path is stored verbatim, because `gz_error` prefixes every message with it.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "filesystem access is unavailable under Miri's isolation"
    )]
    fn the_path_is_kept_for_error_messages() {
        let path = temp_path("error_prefix");
        std::fs::write(&path, b"payload").unwrap();

        let mut state = gzopen(path.as_bytes(), b"rb", GlobalAllocator).expect("rb must open");
        assert_eq!(state.path(), path.as_bytes());
        state.try_set_prefixed_msg(b"out of memory").unwrap();
        let mut expected = Vec::from(path.as_bytes());
        expected.extend_from_slice(b": out of memory");
        assert_eq!(state.msg(), Some(&expected[..]));
        drop(state);

        remove(&path);
    }

    /// An empty path is passed through to the file system rather than rejected early.
    ///
    /// C hands it to `open`, which answers `ENOENT`, and that answer is visible through `errno`.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "filesystem access is unavailable under Miri's isolation"
    )]
    fn an_empty_path_reaches_the_file_system() {
        let error =
            gzopen(b"", b"rb", GlobalAllocator).expect_err("an empty path cannot be opened");
        assert!(matches!(error, GzOpenError::Io(_)), "got {error:?}");
    }

    /// `close` is idempotent, and reports through the file only while one is held.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "filesystem access is unavailable under Miri's isolation"
    )]
    fn file_handle_close_is_idempotent() {
        let path = temp_path("close");
        remove(&path);

        let mut slot = {
            let state = gzopen(path.as_bytes(), b"wb", GlobalAllocator).expect("wb must open");
            let mut state = state;
            let slot = state.take_handle();
            assert!(slot.is_installed(), "a handle was installed");
            slot
        };
        assert!(slot.close().is_ok(), "the first close succeeds");
        assert!(slot.close().is_ok(), "and so does the second");
        // Every operation on a closed handle fails rather than panicking. The slot still holds
        // the handle -- closing does not empty the slot -- so it can be interrogated afterwards.
        let handle = slot
            .handle_mut()
            .expect("the slot still holds the closed handle");
        assert!(handle.read(&mut [0_u8; 4]).is_err());
        assert!(handle.write(b"x").is_err());
        assert!(handle.seek(0, GzSeekFrom::Current).is_err());

        remove(&path);
    }

    /// The three read-only accessors report what the constructor was told.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "filesystem access is unavailable under Miri's isolation"
    )]
    fn file_handle_reports_how_it_was_built() {
        let path = temp_path("accessors");
        std::fs::write(&path, b"payload").unwrap();

        let file = std::fs::File::open(&path).unwrap();
        let mut handle = FileHandle::from_file(file, false, true);
        assert!(handle.is_open());
        assert!(!handle.is_writable());
        assert!(handle.is_nonblocking());
        handle.set_nonblocking(false).unwrap();
        assert!(!handle.is_nonblocking());
        handle.close().unwrap();
        assert!(!handle.is_open());

        remove(&path);
    }

    /// A negative absolute seek is refused rather than wrapped into an enormous offset.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "filesystem access is unavailable under Miri's isolation"
    )]
    fn a_negative_absolute_seek_is_refused() {
        let path = temp_path("negative_seek");
        std::fs::write(&path, b"payload").unwrap();

        let file = std::fs::File::open(&path).unwrap();
        let mut handle = FileHandle::from_file(file, false, false);
        assert!(handle.seek(-1, GzSeekFrom::Start).is_err());
        // The three legal forms all work.
        assert_eq!(handle.seek(3, GzSeekFrom::Start).unwrap(), 3);
        assert_eq!(handle.seek(1, GzSeekFrom::Current).unwrap(), 4);
        assert_eq!(handle.seek(0, GzSeekFrom::End).unwrap(), 7);
        handle.close().unwrap();

        remove(&path);
    }

    /// The `O_NONBLOCK` value is known on every target the implementation is built for.
    ///
    /// Tier-1 unix targets are all covered by `o_nonblock`'s table; a target that is not would
    /// silently open in blocking mode, so the gap is asserted here rather than discovered later.
    #[cfg(unix)]
    #[test]
    fn o_nonblock_is_known_on_this_target() {
        assert!(
            o_nonblock().is_some(),
            "O_NONBLOCK is not tabulated for this target"
        );
    }

    /// Path bytes survive the trip through the platform's path type.
    #[test]
    fn os_path_round_trips_the_bytes() {
        let path = os_path(b"/tmp/example.gz").expect("an ASCII path is always valid");
        assert_eq!(path.to_string_lossy(), "/tmp/example.gz");
        // An empty path is a path; `open` is what rejects it.
        assert!(os_path(b"").is_some());
    }

    /// On unix a path is an arbitrary byte string and need not be UTF-8.
    ///
    /// This is why the state stores the path as bytes rather than as a `String`: insisting on UTF-8
    /// would make a legal filename unopenable.
    #[cfg(unix)]
    #[test]
    fn a_non_utf8_path_is_still_a_path() {
        assert!(os_path(&[b'/', b't', b'm', b'p', b'/', 0xFF, 0xFE]).is_some());
    }

    // -------------------------------------------------------------------------
    //  The injection seam: what `gz_open_with` makes reachable
    // -------------------------------------------------------------------------

    /// What an injected opener was asked to open, captured so a test can assert on it.
    ///
    /// A `Vec<u16>` rather than a borrowed slice because the recorder outlives the call.
    #[derive(Debug, PartialEq, Eq)]
    enum SeenTarget {
        /// [`GzPathTarget::Narrow`], with the exact bytes.
        Narrow(Vec<u8>),
        /// [`GzPathTarget::Wide`], with the exact `u16` units.
        Wide(Vec<u16>),
    }

    /// A [`GzHandle`] that answers everything successfully and reports a chosen `close` result.
    ///
    /// This is the shape a facade handle takes when it backs [`GzHandle::close`] with a real
    /// `close(2)`: the close result is a value the handle produces, so it can fail independently of
    /// every other operation. That is what makes divergence 1 closable.
    struct ClosableHandle<'c> {
        /// Whether [`GzHandle::close`] reports failure.
        close_fails: bool,
        /// How often [`GzHandle::close`] ran.
        closes: &'c Cell<usize>,
    }

    impl GzHandle for ClosableHandle<'_> {
        fn read(&mut self, _buf: &mut [u8]) -> Result<usize, GzIoError> {
            Ok(0)
        }

        fn write(&mut self, buf: &[u8]) -> Result<usize, GzIoError> {
            Ok(buf.len())
        }

        fn seek(&mut self, offset: ZOff64, _whence: GzSeekFrom) -> Result<ZOff64, GzIoError> {
            Ok(offset)
        }

        fn set_nonblocking(&mut self, _nonblocking: bool) -> Result<(), GzIoError> {
            Ok(())
        }

        fn close(&mut self) -> Result<(), GzIoError> {
            self.closes.set(self.closes.get() + 1);
            if self.close_fails {
                // What a facade handle reports when its `close(2)` failed. EIO is representative;
                // the number travels through untouched and the close paths turn any failure into
                // `Z_ERRNO`.
                Err(GzIoError::new(5, false))
            } else {
                Ok(())
            }
        }
    }

    /// F08: opening by path installs the handle **inline**, with no boxing to abort on.
    ///
    /// The property under test is structural, and it is the whole point of [`GzFileSlot`] having an
    /// `Owned` variant: `gz_open` must not perform an infallible `Box::new`, because `Box::new`
    /// aborts the process on allocation failure where C returns `NULL`. `Debug` names the variant,
    /// so the shape is observable without adding API that exists only for a test.
    #[test]
    #[cfg_attr(miri, ignore = "opens a real file")]
    fn opening_by_path_stores_the_handle_inline() {
        let path = temp_path("inline");
        remove(&path);

        let state = gzopen(path.as_bytes(), b"wb", GlobalAllocator).expect("wb must open");
        let shape = format!("{state:?}");
        assert!(
            shape.contains("handle: Owned("),
            "a path-opened handle must be stored inline, not boxed: {shape}"
        );
        assert!(
            !shape.contains("handle: Boxed"),
            "boxing here would abort on OOM instead of reporting it: {shape}"
        );

        drop(state);
        remove(&path);
    }

    /// F08 companion: an adopted descriptor is still boxed, because only the facade can build it.
    #[test]
    fn an_adopted_handle_is_boxed() {
        let closes = Cell::new(0);
        let state = gzdopen(
            Box::new(ClosableHandle {
                close_fails: false,
                closes: &closes,
            }),
            3,
            b"rb",
            GlobalAllocator,
        )
        .expect("rb must open");
        let shape = format!("{state:?}");
        assert!(
            shape.contains("handle: Boxed(..)"),
            "an injected handle is a type this crate cannot name, so it is boxed: {shape}"
        );
    }

    /// F09: the name to open and the name to report are two separate arguments.
    ///
    /// C keeps them separate -- `_wopen` receives the caller's `wchar_t *` while `state->path` holds
    /// a `wcstombs` narrowing of it (`gzlib.c` L200-L219, L246) -- and collapsing them would force a
    /// wide path through a narrow encoding. This asserts both halves at once: the opener sees the
    /// exact `u16` units, and the state's path is the label, not a narrowing of those units.
    #[test]
    fn a_wide_target_reaches_the_opener_unnarrowed_while_the_label_is_reported() {
        // Deliberately not representable in the narrow label: two astral-plane code points,
        // "\u{1F600}\u{1F601}", as UTF-16 surrogate pairs.
        let units: Vec<u16> = vec![0xD83D, 0xDE00, 0xD83D, 0xDE01];
        let label = b"<lossy label>";
        let seen: Cell<Option<SeenTarget>> = Cell::new(None);
        let closes = Cell::new(0);

        let state = gz_open_with(
            GzPathTarget::Wide(&units),
            label,
            b"rb",
            GlobalAllocator,
            |target, _spec| {
                seen.set(Some(match target {
                    GzPathTarget::Narrow(bytes) => SeenTarget::Narrow(bytes.to_vec()),
                    GzPathTarget::Wide(wide) => SeenTarget::Wide(wide.to_vec()),
                }));
                Ok(GzFileSlot::Boxed(Box::new(ClosableHandle {
                    close_fails: false,
                    closes: &closes,
                })))
            },
        )
        .expect("the injected opener succeeds");

        assert_eq!(
            seen.replace(None),
            Some(SeenTarget::Wide(units.clone())),
            "the opener must receive the original wide units, not a narrowing of them"
        );
        assert_eq!(
            state.path(),
            label,
            "the label is what error messages are prefixed with"
        );
    }

    /// F09 companion: a narrow target arrives as bytes, byte for byte, as `open(2)` takes it.
    #[test]
    fn a_narrow_target_reaches_the_opener_as_bytes() {
        // Not valid UTF-8, and legal on unix: exactly the case a `String` would have destroyed.
        let path: &[u8] = &[b'/', b't', b'm', b'p', 0xFF, 0xFE];
        let seen: Cell<Option<SeenTarget>> = Cell::new(None);
        let closes = Cell::new(0);

        let state = gz_open_with(
            GzPathTarget::Narrow(path),
            path,
            b"rb",
            GlobalAllocator,
            |target, _spec| {
                seen.set(Some(match target {
                    GzPathTarget::Narrow(bytes) => SeenTarget::Narrow(bytes.to_vec()),
                    GzPathTarget::Wide(wide) => SeenTarget::Wide(wide.to_vec()),
                }));
                Ok(GzFileSlot::Boxed(Box::new(ClosableHandle {
                    close_fails: false,
                    closes: &closes,
                })))
            },
        )
        .expect("the injected opener succeeds");

        assert_eq!(seen.replace(None), Some(SeenTarget::Narrow(path.to_vec())));
        assert_eq!(state.path(), path);
    }

    /// F09: a wide target cannot be opened where there is no wide file API, and says so.
    ///
    /// The alternative -- transcoding to some guessed encoding -- could name a different file. C
    /// never reaches this case, because it declares `gzopen_w` only under `WIDECHAR`.
    #[cfg(not(windows))]
    #[test]
    fn a_wide_target_is_unrepresentable_without_a_wide_file_api() {
        let units: Vec<u16> = vec![b'/'.into(), b'x'.into()];
        let failure = gz_open_with(
            GzPathTarget::Wide(&units),
            b"/x",
            b"rb",
            GlobalAllocator,
            open_with_std,
        )
        .expect_err("there is no wide open on this target");
        assert!(matches!(failure, GzOpenError::Io(_)));
        assert_eq!(failure.as_return_code(), ReturnCode::ERRNO);
    }

    /// F10: `GzOpenSpec::cloexec` reaches the opener, which is what makes C's flag word reachable.
    ///
    /// `std` sets `O_CLOEXEC` on every file it opens and offers no way to clear it, so
    /// `open_with_std` cannot honour the `e` character. An injected opener can, because it receives
    /// the resolved spec: this asserts that both the `e`-present and `e`-absent answers arrive,
    /// which is the difference C's `gzlib.c` L135-L136 makes.
    #[test]
    fn the_close_on_exec_request_reaches_an_injected_opener() {
        for (mode, expected) in [
            (&b"rb"[..], false),
            (&b"rbe"[..], true),
            (&b"wbe9"[..], true),
            (&b"wb9"[..], false),
        ] {
            let seen = Cell::new(None);
            let closes = Cell::new(0);
            let state = gz_open_with(
                GzPathTarget::Narrow(b"labelled"),
                b"labelled",
                mode,
                GlobalAllocator,
                |_target, spec| {
                    // Every mode-string decision is available without re-parsing.
                    seen.set(Some((spec.cloexec(), spec.exclusive(), spec.nonblocking())));
                    Ok(GzFileSlot::Boxed(Box::new(ClosableHandle {
                        close_fails: false,
                        closes: &closes,
                    })))
                },
            )
            .expect("the injected opener succeeds");
            drop(state);
            let (cloexec, exclusive, nonblocking) =
                seen.replace(None).expect("the opener ran exactly once");
            assert_eq!(
                cloexec, expected,
                "mode {mode:?} must report cloexec = {expected}"
            );
            assert!(!exclusive, "no mode here contained x");
            assert!(!nonblocking, "no mode here contained N");
        }
    }

    /// F11: a path-based stream can now report `close(2)`'s failure as `Z_ERRNO`.
    ///
    /// `gzclose_w` must answer `Z_ERRNO` when `close` fails (`gzwrite.c` L696-L697), and the safe
    /// default cannot see that result at all -- dropping a `std::fs::File` discards it. Supplying
    /// the handle through `gz_open_with` closes the gap, and this asserts the result travels the
    /// whole way: handle -> `GzFileSlot::close` -> `gzclose_w`'s return value.
    #[test]
    fn a_failing_close_from_an_injected_handle_becomes_z_errno() {
        for (close_fails, expected) in [(false, ReturnCode::OK), (true, ReturnCode::ERRNO)] {
            let closes = Cell::new(0);
            let mut state = gz_open_with(
                GzPathTarget::Narrow(b"closable.gz"),
                b"closable.gz",
                b"wb",
                GlobalAllocator,
                |_target, _spec| {
                    Ok(GzFileSlot::Boxed(Box::new(ClosableHandle {
                        close_fails,
                        closes: &closes,
                    })))
                },
            )
            .expect("the injected opener succeeds");

            let status = crate::gz::write::gzclose_w(&mut state);
            assert_eq!(
                status,
                expected.as_i32(),
                "close_fails = {close_fails} must yield {expected:?}"
            );
            assert_eq!(closes.get(), 1, "close runs exactly once on this path");
        }
    }

    /// F11 companion: the read close path propagates the same result.
    ///
    /// `gzclose_r` has its own `close` call (`gzread.c` L665-L667) and its own deferred error
    /// reporting, so it is asserted separately rather than assumed to behave like the write path.
    #[test]
    fn a_failing_close_on_the_read_path_becomes_z_errno() {
        for (close_fails, expected) in [(false, ReturnCode::OK), (true, ReturnCode::ERRNO)] {
            let closes = Cell::new(0);
            let mut state = gz_open_with(
                GzPathTarget::Narrow(b"closable.gz"),
                b"closable.gz",
                b"rb",
                GlobalAllocator,
                |_target, _spec| {
                    Ok(GzFileSlot::Boxed(Box::new(ClosableHandle {
                        close_fails,
                        closes: &closes,
                    })))
                },
            )
            .expect("the injected opener succeeds");

            // `gzclose_r` answers with a `ReturnCode`, where `gzclose_w` answers with a bare
            // `int`; both are C's `int` at the boundary and the facade narrows each the same way.
            let status = crate::gz::read::gzclose_r(&mut state);
            assert_eq!(status, expected);
            assert_eq!(closes.get(), 1);
        }
    }

    /// An opener that fails is C's `state->fd == -1`: no stream, and the `errno` survives.
    #[test]
    fn an_injected_opener_failure_is_reported_as_it_is() {
        let failure = gz_open_with(
            GzPathTarget::Narrow(b"denied.gz"),
            b"denied.gz",
            b"rb",
            GlobalAllocator,
            |_target, _spec| Err(GzIoError::new(13, false)),
        )
        .expect_err("the opener refused");
        match failure {
            GzOpenError::Io(error) => {
                assert_eq!(error.errno, 13, "the platform's errno is preserved");
                assert!(!error.would_block);
            }
            other => panic!("expected an I/O failure, got {other:?}"),
        }
    }

    /// A bad mode string is rejected before the opener is consulted, as C rejects it before `open`.
    #[test]
    fn an_invalid_mode_never_reaches_the_opener() {
        let ran = Cell::new(false);
        let failure = gz_open_with(
            GzPathTarget::Narrow(b"x.gz"),
            b"x.gz",
            b"r+",
            GlobalAllocator,
            |_target, _spec| {
                ran.set(true);
                Err(GzIoError::new(0, false))
            },
        )
        .expect_err("r+ is rejected");
        assert!(matches!(failure, GzOpenError::InvalidMode));
        assert!(!ran.get(), "the open must not be attempted at all");
    }

    /// `gzopen` is `gz_open_with` over `open_with_std`, so the two must agree exactly.
    #[test]
    #[cfg_attr(miri, ignore = "opens a real file")]
    fn gzopen_and_gz_open_with_the_default_opener_agree() {
        let path = temp_path("agree");
        remove(&path);

        {
            let mut direct = gzopen(path.as_bytes(), b"wb", GlobalAllocator).expect("wb opens");
            write_all_through(direct.handle_mut().unwrap(), b"first");
            assert_eq!(crate::gz::write::gzclose_w(&mut direct), 0);
        }

        let via_hook = gz_open_with(
            GzPathTarget::Narrow(path.as_bytes()),
            path.as_bytes(),
            b"rb",
            GlobalAllocator,
            open_with_std,
        )
        .expect("rb opens");
        assert_eq!(via_hook.path(), path.as_bytes());
        assert!(via_hook.has_handle());
        assert_eq!(via_hook.mode(), GZ_READ);
        drop(via_hook);

        remove(&path);
    }
}
