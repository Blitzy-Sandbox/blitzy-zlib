//! The `gzFile` layer: reading and writing gzip files through a `stdio`-like handle.
//!
//! This subtree implements the four C translation units that together implement zlib's file
//! interface, plus the private header they share:
//!
//! | C source | Lines | Rust module |
//! |---|---|---|
//! | `gzlib.c` | 609 | this file, [`open`] and [`state`] |
//! | `gzread.c` | 668 | [`read`] |
//! | `gzwrite.c` | 700 | [`mod@write`] and [`printf`] |
//! | `gzclose.c` | 23 | [`close`] |
//! | `gzguts.h` | 216 | [`state`] |
//!
//! Where the `deflate` and `inflate` engines work on caller-supplied buffers, this layer owns a
//! file: it opens it, buffers it, drives the matching engine over it, tracks the position in
//! *uncompressed* bytes, and reports errors as text rather than as codes alone. It is also the only
//! part of `zlib-rs` that needs `std`, for the reason given under *Feature gating* below.
//!
//! # What this file itself contains
//!
//! Two things, mirroring the two roles `gzlib.c` plays in the C build:
//!
//! 1. **The barrel.** It declares the seven sibling modules and re-exports their public surface, so
//!    that `crates/libz-rs-sys/src/gz.rs` has one import point (`zlib_rs::gz::*`) for all 32 of the
//!    `gz*` entry points it exports rather than seven.
//! 2. **The shared plumbing of `gzlib.c`** that every other module in the subtree reaches for:
//!    `gz_error` (`gzlib.c` L555-L590), `gz_intmax` (L596-L609) and `gt_off`
//!    (`gzguts.h` L216), together with the six public drivers that C also keeps outside the read and
//!    write files -- [`gzbuffer`], [`gzeof`], [`gzerror`], [`gzclearerr`] -- plus [`gzdirect`]
//!    (`gzread.c` L627-L642) and [`gzsetparams`] (`gzwrite.c` L630-L664), which C places with the
//!    read and write halves but which belong to the shared surface.
//!
//! # The exposed prefix, and the contract every entry point here honours
//!
//! `zlib.h` L1956-L1968 exposes the first three fields of the `gzFile` structure and then defines
//! `gzgetc` as a **macro** that decrements `have`, increments `pos` and post-increments `next`
//! (quoted in full at the head of [`state`], where the layout it constrains lives).
//!
//! That arithmetic is compiled into *caller* object code, which this implementation cannot change, so
//! [`state::GzFileExposed`] reproduces the three fields at offset 0 and every entry point in this
//! subtree begins with [`state::GzState::resync_from_exposed`] and ends with
//! [`state::GzState::refresh_exposed`]. `state` documents the pointer/index split in full; the part
//! that matters here is that a caller may have advanced `next` and decremented `have` behind the
//! library's back, so the very first thing any function below does is recover the cursor, and the
//! last thing it does is publish it again.
//!
//! An entry point whose resync *fails* has been handed a structure this library did not produce.
//! Each one then returns the same value C returns for `file == NULL`, because that is the answer C
//! gives for every structure it declines to work with (`gzguts.h` L158 calls the odd mode constants
//! "a little integrity check on the passed structure"), and it publishes nothing.
//!
//! # Feature gating
//!
//! There is **no `gz` feature on this crate**; the one of that name belongs to the facade crate and
//! switches on `zlib-rs/std`. The crate root reaches this subtree through a single
//! `#[cfg(feature = "std")] pub mod gz;`, so no per-item gate appears below and
//! `--no-default-features` compiles the whole subtree out.
//!
//! ★ The crate root's re-export of [`GzState`] must carry the same `#[cfg(feature = "std")]` as the
//! module declaration. An ungated `pub use gz::GzState;` names a module that does not exist in the
//! default `no_std` build and fails it outright.
//!
//! # The division of labour with `crates/libz-rs-sys/src/gz.rs`
//!
//! This is the contract between the two files, recorded on this side so that neither has to
//! reconstruct it from the other. Each "the facade must X" is a requirement that file satisfies,
//! and a requirement any replacement for it has to keep.
//!
//! * `gzFile` is an opaque `*mut GzState`. The facade must **resync the exposed prefix immediately
//!   on entry and refresh it before returning to C** for every one of the 28 `gz*` exports plus
//!   `gzopen64`, `gzseek64`, `gztell64` and `gzoffset64` -- 32 in total -- because the caller's
//!   `gzgetc` macro mutates `have`, `pos` and `next` in caller object code. The six drivers in this
//!   file do it themselves; the facade must still do it around the ones it composes.
//! * The facade owns every raw pointer and every C string: NUL termination, the `gzopen` path, and
//!   the `wchar_t` handling for `gzopen_w` (AAP §0.6.1 unsafe-site category 5). For `gzopen_w` that
//!   means passing the caller's `wchar_t` units through **unconverted**, as
//!   [`crate::gz::open::GzPathTarget::Wide`], and supplying the narrowed name separately as the
//!   error label -- the split C draws between `_wopen`'s argument and `state->path`. Narrowing the
//!   path and opening the narrowing would drop any character the target encoding cannot represent,
//!   which names a different file or none.
//! * Two C behaviours need a syscall safe Rust cannot make, and both are reachable through
//!   [`crate::gz::open::gz_open_with`]: C's exact `open(2)` flag word, whose `O_CLOEXEC` is
//!   conditional on the mode string while `std` sets it unconditionally and offers no way to clear
//!   it; and a `close(2)` whose result is observable, which dropping a `std::fs::File` discards.
//!   Supply an opener that performs both and returns a [`crate::gz::state::GzFileSlot::Boxed`];
//!   [`crate::gz::open::open_with_std`] is the default to wrap or fall back to.
//! * **`gzprintf` and `gzvprintf` are the one pair the facade does NOT define.** Stable Rust cannot
//!   declare a C-variadic function or a `va_list` parameter -- rustc 1.97.1 rejects both with E0658
//!   -- so those two symbols come from `crates/libz-rs-sys/csrc/gzprintf_shim.c`, the single C
//!   translation unit that facade's `build.rs` compiles into every artifact it produces. The
//!   facade must define **only** the two hidden
//!   helpers `_zlib_rs_gzprintf_begin` and `_zlib_rs_gzprintf_commit`; [`printf`] documents their
//!   exact contract, and the leading underscore is what `zlib.map`'s `local: _*` pattern hides
//!   them by. That is why [`printf`] exposes a bounded scratch buffer rather than a formatter.
//! * The facade owns the pointer guard on `gzFile` (category 3), and the order of its two parts
//!   matters. It must **first** establish that the pointer is non-null, aligned and safe to
//!   dereference for the duration of the call; nothing read *through* the pointer can establish
//!   that, so no tag can substitute for it. Only then does inspecting the state tell it anything,
//!   and what it can then tell is that the *live object* is not one this library produced, or is
//!   one that has already been closed. A pointer that has been freed is outside what any such check
//!   reaches: it may still hold a plausible tag, and reading it is already undefined behaviour.
//!   Every entry point here additionally re-checks the mode, which is the weaker half of the same
//!   defence.
//! * The facade owns the narrowing of each [`state::ZOff64`] result to the caller's `z_off_t`, using
//!   C's own idiom `ret == (z_off_t)ret ? ret : -1` (`gzlib.c` L440-L441, L470-L471, L491-L492).
//!   [`read::narrow_offset`] performs exactly that test.
//! * The facade owns file-descriptor adoption for `gzdopen`: `FromRawFd::from_raw_fd` is `unsafe`,
//!   so the facade builds the [`state::GzHandle`] and injects it into [`open::gzdopen`].
//! * **Both the unsuffixed and the `*64` families must be exported.** `zconf.h` redirects the
//!   unsuffixed names according to the *caller's* `_FILE_OFFSET_BITS` and `_LARGEFILE64_SOURCE` at
//!   the caller's own compile time (`zlib.h` L1976-L2018), so which name a given object file
//!   references is not this library's choice. The core provides exactly one
//!   [`state::ZOff64`]-based function per operation -- [`open::gzopen`], [`read::gzseek64`],
//!   [`read::gztell64`], [`read::gzoffset64`] -- and both exported names delegate to it.
//! * `crates/libz-rs-sys/src/layout_assertions.rs` asserts `sizeof(struct gzFile_s) == 24` on an
//!   LP64 target; [`state`] is the core-side half of that one contract and asserts the same
//!   numbers at compile time.
//! * Determinations this subtree makes for the **computed** `zlibCompileFlags`, which
//!   `crates/libz-rs-sys/src/util.rs` assembles: bit 16 (`NO_GZCOMPRESS`) is reported **clear**,
//!   because both the read and the write half are implemented; bits 25, 26 and 27 -- the
//!   `NO_snprintf`/`NO_vsnprintf`/`HAS_vsprintf_void` family -- are **clear**, because [`printf`]
//!   always provides a bounded formatter and there is no void-returning variant.
//!
//! # Tests
//!
//! Each module in this subtree carries its own inline `#[cfg(test)] mod tests`. The integration
//! suite for the layer as a whole lives under `crates/zlib-rs/tests/` and belongs to the crate
//! root's agent; nothing in this subtree creates it.
//!
//! [`close`]: crate::gz::close
//! [`gzbuffer`]: crate::gz::gzbuffer
//! [`gzclearerr`]: crate::gz::gzclearerr
//! [`gzdirect`]: crate::gz::gzdirect
//! [`gzeof`]: crate::gz::gzeof
//! [`gzerror`]: crate::gz::gzerror
//! [`gzsetparams`]: crate::gz::gzsetparams
//! [`open`]: crate::gz::open
//! [`open::gzdopen`]: crate::gz::open::gzdopen
//! [`open::gzopen`]: crate::gz::open::gzopen
//! [`printf`]: crate::gz::printf
//! [`read`]: crate::gz::read
//! [`read::gzoffset64`]: crate::gz::read::gzoffset64
//! [`read::gzseek64`]: crate::gz::read::gzseek64
//! [`read::gztell64`]: crate::gz::read::gztell64
//! [`read::narrow_offset`]: crate::gz::read::narrow_offset
//! [`state`]: crate::gz::state
//! [`state::GzFileExposed`]: crate::gz::state::GzFileExposed
//! [`state::GzHandle`]: crate::gz::state::GzHandle
//! [`state::GzState::refresh_exposed`]: crate::gz::state::GzState::refresh_exposed
//! [`state::GzState::resync_from_exposed`]: crate::gz::state::GzState::resync_from_exposed
//! [`state::ZOff64`]: crate::gz::state::ZOff64
//! [`write`]: crate::gz::write

// The definitions closing the module documentation above are link-reference definitions, not
// prose: a module whose `mod` declaration carries an outer doc comment has its `//!` block
// resolved in the scope of the DECLARING module, so an unqualified sibling name does not
// resolve. See "Documentation lints" in `src/lib.rs` for the rule and the gate.

// Every item in a module named `gz` that implements a C function named `gz_error`, `gz_intmax` or
// `gzbuffer` necessarily repeats the module's name. The C names are the ones a maintainer diffing
// this file against `gzlib.c` will search for, and renaming them to satisfy a lint would cost
// exactly the traceability the implementation is judged on.
#![allow(clippy::module_name_repetitions)]

/// The `gz_state` structure and the constants that describe it (`gzguts.h`).
pub mod state;

/// The RFC 1952 header and trailer, and the four-byte gzip detection heuristic.
///
/// Crate-private in its entirety: every item is `pub(crate)`, because the file layer's view of a
/// gzip member is an implementation detail. `deflateSetHeader`/`inflateGetHeader` expose the
/// caller-visible `gz_header` instead, and they belong to the engines.
pub(crate) mod header;

/// Opening, the mode-string grammar and the reset path (`gzlib.c` L69-L318).
pub mod open;

/// The read half: `gzread`, `gzgets`, positioning and `gzclose_r` (`gzread.c`).
pub mod read;

/// The write half: `gzwrite`, `gzputs`, `gzflush` and `gzclose_w` (`gzwrite.c`).
pub mod write;

/// The bounded scratch buffer behind `gzprintf` and `gzvprintf` (`gzwrite.c` L403-L465).
pub mod printf;

/// The `gzclose` dispatch to `gzclose_r` or `gzclose_w` (`gzclose.c`).
pub mod close;

// The state, its constants and the traits a caller must implement or inspect. `GzState` keeps its
// name exactly: the facade names it in the signature of every `gz*` export, reaching it here as
// `zlib_rs::gz::GzState`, and the crate root additionally flattens it to `zlib_rs::GzState` when
// both `rust-api` and `std` are on.
// `try_box_handle` is exported alongside them because the facade needs it: `gzdopen` builds a
// handle over an adopted descriptor and must hand it to `gz_open_handle` already boxed, and
// `Box::new` would abort the caller's process on a failed allocation rather than returning C's
// documented `NULL`.
pub use crate::gz::state::{
    try_box_handle, EngineBox, GzEngine, GzFileExposed, GzFileSlot, GzHandle, GzHandleRef, GzHow,
    GzIoError, GzMode, GzSeekFrom, GzState, GzStream, ZOff64, COPY, GZBUFSIZE, GZIP, GZ_APPEND,
    GZ_NONE, GZ_READ, GZ_WRITE, LOOK,
};

// The open family. There is deliberately no `gzopen64`: `gzopen` *is* the 64-bit core, exactly as
// C's `gzopen64` is a one-line forward to the same `gz_open` (`gzlib.c` L300-L312 and L2042), so the
// facade exports both names from this one function.
//
// `gz_open_with`, `open_with_std` and `GzPathTarget` are the path-side injection seam. The facade
// needs all three whenever C's behaviour requires a syscall safe Rust cannot make -- C's exact
// `open(2)` flag word, whose conditional `O_CLOEXEC` `std` cannot express, and a `close(2)` whose
// result is observable, which dropping a `std::fs::File` discards. `open_with_std` is the default
// opener, exported so a facade can wrap or fall back to it rather than reimplement it.
pub use crate::gz::open::{
    gz_open_handle, gz_open_with, gzdopen, gzopen, open_with_std, FileHandle, GzOpenError,
    GzOpenHandleError, GzOpenSpec, GzPathTarget,
};

// `gzopen_w` exists on Windows only, and the gate is not this module's choice: C guards the
// declaration with `#if defined(_WIN32) && !defined(Z_SOLO)` (`zlib.h` L2041-L2044) and the
// definition with `#ifdef WIDECHAR`, which `gzguts.h` L54-L56 defines for `_WIN32`. `open` carries
// the matching `#[cfg(windows)]`, so the re-export must carry it too -- an ungated one would be an
// unresolved import on every other target.
#[cfg(windows)]
pub use crate::gz::open::gzopen_w;

// The read family. `gzgetc_` is not a duplicate of `gzgetc`: it is the function the `gzgetc` macro
// falls back to when the buffer is empty, and `zlib.h` L1961 declares it separately "for backward
// compatibility", so both must be reachable and both must be exported.
pub use crate::gz::read::{
    gzclose_r, gzfread, gzgetc, gzgetc_, gzgets, gzread, gzungetc, narrow_offset,
};

// The write-only destination forms of the three reads that fill a caller's buffer. A C caller's
// `buf` is guaranteed writable and nothing more, so `crates/libz-rs-sys` reaches for these; the
// slice forms above are the Rust API and forward to them unchanged.
pub use crate::gz::read::{gzfread_into, gzgets_into, gzread_into};

// The positioning family, all four in their `ZOff64` form; the facade narrows each result.
pub use crate::gz::read::{gzoffset64, gzrewind, gzseek64, gztell64};

// The write family.
pub use crate::gz::write::{gzclose_w, gzflush, gzfwrite, gzputc, gzputs, gzwrite};

// The direction-agnostic close.
pub use crate::gz::close::gzclose;

// The formatting interface behind `gzprintf`. The C shim in `crates/libz-rs-sys/csrc` formats into
// the scratch region this hands it -- the one place a `va_list` is touched anywhere in the port --
// and then commits the byte count through the facade's two hidden helpers.
pub use crate::gz::printf::{
    printf_begin, printf_bytes, printf_commit, printf_with, PrintfScratch,
};

// The `gz` subtree is the only part of `zlib-rs` permitted to name `std` (AAP §0.4.2.1), and the
// crate is `#![no_std]`, so `std` is not in the extern prelude and each file in the subtree declares
// it. This module needs it for `io::Error`, which is how the platform's own error text is obtained
// in place of C's `strerror`.
extern crate std;
use core::ffi::{c_int, c_uint};
use core::fmt::Write as _;

use std::io;

use crate::allocate::{Allocator, Buffer};
use crate::deflate::{deflate_params, DeflateStream, Flush};
use crate::error::ReturnCode;
use crate::gz::read::gz_look;
use crate::gz::state::split_buffers;
use crate::gz::write::{gz_comp, gz_zero};

/// The message `gzerror` substitutes for `Z_MEM_ERROR` (`gzlib.c` L526).
///
/// C returns this literal *instead of* a stored message, because [`gz_error`] deliberately declines
/// to allocate one while reporting that allocation failed. The two halves are a pair: remove either
/// and an out-of-memory condition reports no text at all.
const OUT_OF_MEMORY: &[u8] = b"out of memory";

/// [`OUT_OF_MEMORY`] with the terminator [`gzerror_terminated`] promises.
const OUT_OF_MEMORY_TERMINATED: &[u8] = b"out of memory\0";

/// The message `gzerror` returns when there is none (`gzlib.c` L527).
///
/// C's `state->msg == NULL ? "" : state->msg` -- an empty string, which is emphatically not the same
/// as the null pointer it returns for an unusable handle.
const NO_MESSAGE: &[u8] = b"";

/// [`NO_MESSAGE`] with the terminator [`gzerror_terminated`] promises: a valid empty C string.
const NO_MESSAGE_TERMINATED: &[u8] = b"\0";

/// Capacity of the fixed-size buffer an `errno` message is rendered into.
///
/// Sized so that no platform's error text can be truncated, because truncation would cut into the
/// suffix [`errno_message`] has to remove. The longest glibc `strerror` string is under 60 bytes and
/// the longest Windows `FormatMessage` text for a file-I/O error is around 120; 256 leaves room for
/// the longest of those plus the ` (os error N)` suffix and still fits comfortably on the stack of
/// an error path.
const ERRNO_MESSAGE_CAPACITY: usize = 256;

/// The literal `std` appends to an OS error's `Display`, up to the number.
///
/// `std` renders a raw-OS error as `"{error_string(code)} (os error {code})"`. Removing exactly this
/// separator and everything after it therefore leaves `error_string(code)` -- which is what
/// `strerror_r` produced -- and that is precisely `zstrerror()`.
const OS_ERROR_SUFFIX: &[u8] = b" (os error ";

/// The fallback text when the platform supplies none.
///
/// **zlib's own** fallback, not an invention: `gzguts.h` L135 defines `zstrerror()` as
/// `"stdio error (consult errno)"` when `strerror` is unavailable. Reached only if the platform
/// renders an empty string for the error number, which no supported target does.
const NO_ERRNO_TEXT: &[u8] = b"stdio error (consult errno)";

/// A short, heap-free rendering of an operating-system error: this port of `zstrerror()`.
///
/// `gzguts.h` L131-L133 defines `zstrerror()` as `strerror(errno)`, and `gz_error` copies the result
/// into an allocated, path-prefixed message (`gzlib.c` L576-L584). This crate has no `libc`
/// dependency, so the text comes from [`io::Error`]'s own `Display`, which `std` produces with
/// `strerror_r` on unix and `FormatMessageW` on Windows.
///
/// # Why the buffer is fixed-size and on the stack
///
/// `String::from` and `format!` abort the process if the global allocator is exhausted, and the one
/// moment a library is most likely to be out of memory is while it is reporting a failure.
/// [`core::fmt::Write`] into a fixed buffer cannot fail, so a truncated message is the worst
/// outcome -- and [`ERRNO_MESSAGE_CAPACITY`] is sized so that even that cannot happen for a real
/// platform error string.
///
/// # Why the ` (os error N)` suffix is removed
///
/// `std` renders a raw-OS error as `error_string(code)` followed by `" (os error {code})"`, whereas
/// C stops at `strerror(errno)`. Keeping the suffix would put text in `gzerror`'s return value that
/// the reference implementation never puts there, and `gzerror`'s string is part of what a caller
/// observes. So [`ErrnoMessage::strip_os_error_suffix`] takes it off again, leaving exactly the bytes
/// `strerror` produced. This is a removal of *our own* addition, not a reinterpretation of the
/// platform's text: `std` composes the two pieces itself and `error_string` is not separately
/// exposed, so rendering-then-stripping is the only way to obtain the first piece alone.
#[derive(Debug)]
pub(crate) struct ErrnoMessage {
    /// The rendered bytes. Only the first [`ErrnoMessage::len`] of them are meaningful.
    bytes: [u8; ERRNO_MESSAGE_CAPACITY],
    /// How much of [`ErrnoMessage::bytes`] has been written.
    len: usize,
}

impl ErrnoMessage {
    /// An empty message.
    pub(crate) const fn new() -> Self {
        Self {
            bytes: [0; ERRNO_MESSAGE_CAPACITY],
            len: 0,
        }
    }

    /// The rendered text, as the bytes `gz_error` expects.
    pub(crate) fn as_bytes(&self) -> &[u8] {
        self.bytes.get(..self.len).unwrap_or(&[])
    }

    /// Drops `std`'s ` (os error N)` suffix, leaving exactly what `strerror` returned.
    ///
    /// Two cases, and the second exists only for completeness:
    ///
    /// 1. the whole suffix is present, which is every real case, so the exact expected tail is
    ///    matched and removed;
    /// 2. the text was long enough to be truncated mid-suffix, in which case the last occurrence of
    ///    [`OS_ERROR_SUFFIX`] is found and everything from it removed. Unreachable for any platform
    ///    error string at [`ERRNO_MESSAGE_CAPACITY`], and handled anyway so that a partial
    ///    ` (os erro` can never reach a caller.
    ///
    /// Leaves the message untouched when no suffix is present, which is what happens for an error
    /// `std` synthesised rather than received from a syscall.
    fn strip_os_error_suffix(&mut self, code: i32) {
        // Case 1: the exact tail `std` would have appended.
        let mut expected = Self::new();
        // Cannot fail: `write_str` below always reports success, and the tail is far shorter than
        // the buffer.
        let _ = write!(expected, " (os error {code})");
        let tail = expected.as_bytes();
        if let Some(kept) = self.len.checked_sub(tail.len()) {
            if self.bytes.get(kept..self.len) == Some(tail) {
                self.len = kept;
                return;
            }
        }

        // Case 2: a truncated suffix. Search for the separator from the right.
        let body = self.as_bytes();
        let window = OS_ERROR_SUFFIX.len();
        if let Some(limit) = body.len().checked_sub(window) {
            for start in (0..=limit).rev() {
                if body.get(start..start.saturating_add(window)) == Some(OS_ERROR_SUFFIX) {
                    self.len = start;
                    return;
                }
            }
        }
    }
}

impl core::fmt::Write for ErrnoMessage {
    /// Appends as much of `text` as fits, and reports success either way.
    ///
    /// Truncation is silent and deliberate: the alternative is for the formatting machinery to
    /// report an error that the caller would have to handle on a path whose whole purpose is to
    /// report a different error. A partial write stops at a `char` boundary so that the result stays
    /// valid UTF-8, which matters because the facade publishes it as a C string.
    fn write_str(&mut self, text: &str) -> core::fmt::Result {
        let room = ERRNO_MESSAGE_CAPACITY.saturating_sub(self.len);
        // The largest prefix of `text` that fits without splitting a `char`.
        let mut take = 0;
        for (index, character) in text.char_indices() {
            let end = index.saturating_add(character.len_utf8());
            if end > room {
                break;
            }
            take = end;
        }
        let Some(source) = text.as_bytes().get(..take) else {
            return Ok(());
        };
        let end = self.len.saturating_add(take);
        if let Some(destination) = self.bytes.get_mut(self.len..end) {
            destination.copy_from_slice(source);
            self.len = end;
        }
        Ok(())
    }
}

/// Renders the message C would obtain from `zstrerror()`.
///
/// `error` is the failure just observed, when there is one. A [`GzIoError`] carrying a non-zero
/// `errno` is rendered from that number, which is the most faithful and the most robust source: it
/// is the value the failing call itself reported, whatever has happened to the thread's `errno`
/// since.
///
/// With no error object -- `gzread`'s stall report at `gzread.c` L429, which C writes as a bare
/// `zstrerror()` long after the read returned -- or with an `errno` the handle could not determine,
/// the text comes from [`io::Error::last_os_error`]. That is precisely what C does at that line: it
/// reads the thread's current `errno`, relying on nothing having overwritten it in between.
///
/// The result is `strerror`'s bytes and nothing else; see [`ErrnoMessage`] for why the suffix `std`
/// adds is removed again. If the platform renders no text at all, [`NO_ERRNO_TEXT`] -- zlib's own
/// `NO_STRERROR` fallback -- is substituted, so the message is never empty.
pub(crate) fn errno_message(error: Option<GzIoError>) -> ErrnoMessage {
    let source = match error {
        Some(error) if error.errno != 0 => io::Error::from_raw_os_error(error.errno),
        _ => io::Error::last_os_error(),
    };
    let mut message = ErrnoMessage::new();
    // Cannot fail: `write_str` above always reports success.
    let _ = write!(message, "{source}");
    // `last_os_error` and `from_raw_os_error` both carry a number, so this is always `Some`; the
    // `unwrap_or` covers a hypothetical error `std` synthesised, for which there is no suffix to
    // remove and `strip_os_error_suffix` correctly finds none.
    message.strip_os_error_suffix(source.raw_os_error().unwrap_or(0));
    if message.len == 0 {
        let fallback = NO_ERRNO_TEXT;
        if let Some(destination) = message.bytes.get_mut(..fallback.len()) {
            destination.copy_from_slice(fallback);
            message.len = fallback.len();
        }
    }
    message
}

/// Widens a C `unsigned` count into a `usize` index.
///
/// Every target this implementation supports has `usize` at least as wide as `c_uint`, so the conversion is
/// exact. Saturating rather than panicking on a hypothetical narrower target is the conservative
/// choice: a saturated value can only make a bounds check stricter, never looser. The same helper,
/// with the same reasoning, appears in [`read`], [`mod@write`] and [`state`].
fn to_index(value: c_uint) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}

/// Narrows a `usize` count into a C `unsigned`.
///
/// The counterpart of [`to_index`]. Both counts this is applied to shrank from values that already
/// fitted in a `c_uint`, so the saturation is unreachable; it exists so that the conversion cannot
/// panic on any input.
fn to_count(value: usize) -> c_uint {
    c_uint::try_from(value).unwrap_or(c_uint::MAX)
}

/// Records an error code and, optionally, a message; releases whatever was recorded before.
///
/// Implements `gz_error` (`gzlib.c` L555-L590), whose own comment states the policy.
///
/// `ZLIB_INTERNAL` in C and listed in the `local:` block of `zlib.map` (L17), so it is `pub(crate)`
/// here, is never `#[no_mangle]`, and is never re-exported from this module. The linker's
/// `--version-script` and the absence of `#[no_mangle]` are belt and braces for the same contract.
///
/// # The five steps, in C's order, and why each is load-bearing
///
/// 1. **Release the previous message** (L557-L561). C guards the `free` with
///    `state->err != Z_MEM_ERROR` and then nulls the slot unconditionally.
/// 2. **★ Clear `x.have` when the error is fatal** (L563-L565), which C's own comment explains as
///    "if fatal, set `state->x.have` to 0 so that the `gzgetc()` macro fails". This is the *only*
///    mechanism by which an error becomes visible to code that reaches the stream exclusively through
///    the macro: with `have` at zero the macro takes its `else` branch and calls the `gzgetc`
///    function, which performs the checks the macro cannot. Omitting it would let such a caller keep
///    consuming stale buffered bytes after a fatal error.
///    `Z_OK` and `Z_BUF_ERROR` are excluded because neither is fatal -- `Z_BUF_ERROR` on a read
///    stream means the input ended mid-member and may resume (`zlib.h` L1474-L1478) -- and `again` is
///    excluded because it marks a stall on a non-blocking file, where discarding already-buffered
///    output would lose data the caller has not yet been given.
/// 3. **Store the code** (L568), then return if there is no message.
/// 4. **Return without a message for `Z_MEM_ERROR`** (L571-L572). Allocating a string in order to
///    report that allocation failed is exactly the wrong thing to do, so C stores nothing and
///    [`gzerror`] substitutes the literal `"out of memory"` instead. Both halves are required: with
///    only this one, an out-of-memory condition would report no text at all.
/// 5. **Build `"{path}: {msg}"`** (L575-L588). C allocates `strlen(path) + strlen(msg) + 3` bytes --
///    the two of `": "` plus the terminating NUL -- and formats `"%s%s%s"`.
///    [`GzState::try_set_prefixed_msg`] performs that concatenation, reserving exactly two extra
///    bytes because this implementation carries a length rather than a NUL. A failed allocation becomes
///    `Z_MEM_ERROR` with no message, which is C's L577-L580.
///
/// # The one deviation from C's letter
///
/// Step 1's guard is not reproduced. C skips the `free` because a message recorded for
/// `Z_MEM_ERROR` would be a static string rather than an allocation; here it is always an owned
/// `Vec<u8>`, so releasing it is correct and required. The guard is unreachable in C in any case,
/// since step 4 stores nothing, and the observable outcome is identical.
///
/// # Allocation
///
/// Through the `Vec` behind [`GzState::try_set_prefixed_msg`], which is the faithful counterpart of
/// C's `malloc`: `gzlib.c` uses `malloc`/`free` directly here and never the caller's
/// `zalloc`/`zfree`, because the `gzFile` layer sets those three `z_stream` members to `Z_NULL`
/// (`gzread.c` L110-L112, `gzwrite.c` L32-L34). The injected [`Allocator`] is for the engine and its
/// buffers, which is where `test/infcover.c`'s tracking allocator observes this library.
pub(crate) fn gz_error<'a, A: Allocator<'a>>(
    state: &mut GzState<'a, A>,
    err: ReturnCode,
    msg: Option<&[u8]>,
) {
    // 1. L557-L561: `if (state->msg != NULL) { if (state->err != Z_MEM_ERROR) free(state->msg);
    //    state->msg = NULL; }`. See "The one deviation from C's letter" above for the guard.
    if state.msg().is_some() {
        state.clear_msg();
    }

    // 2. L563-L565: `if (err != Z_OK && err != Z_BUF_ERROR && !state->again) state->x.have = 0;`
    let fatal = err != ReturnCode::OK && err != ReturnCode::BUF_ERROR && !state.again();
    if fatal {
        state.clear_have();
    }

    // 3. L567-L570: `state->err = err; if (msg == NULL) return;`
    state.set_err(err.as_i32());
    if let Some(msg) = msg {
        // 4. L571-L572: `if (err == Z_MEM_ERROR) return;`
        if err != ReturnCode::MEM_ERROR {
            // 5. L575-L588: allocate and format, or convert the failure to out of memory.
            if state.try_set_prefixed_msg(msg).is_err() {
                state.set_err(ReturnCode::MEM_ERROR.as_i32());
            }
        }
    }

    // Step 2 moved the cursor, so the prefix the caller's macro reads is republished before control
    // leaves. `clear_have` already leaves a coherent pair -- a null `next` with `have` at zero is
    // the encoding [`GzState::resync_from_exposed`] accepts for "nothing available" -- and this
    // recomputes `next` from the zeroed index so that, exactly as in C, the pointer still denotes a
    // position inside the output buffer when one exists. Skipped entirely when nothing moved, so
    // that recording a non-fatal error costs no pointer arithmetic.
    if fatal {
        state.refresh_exposed();
    }
}

/// The largest value a C `int` can hold.
///
/// Implements `gz_intmax` (`gzlib.c` L596-L609), `ZLIB_INTERNAL` in C and listed in the `local:`
/// block of `zlib.map` (L18), hence `pub(crate)`.
///
/// C returns `INT_MAX` when `<limits.h>` has provided it and otherwise computes the same value by
/// doubling until it wraps (`gzlib.c` L600-L607). Rust guarantees `c_int::MAX`, so only the first
/// branch has an analogue.
///
/// Its comment explains why that loop exists rather than the obvious `((unsigned)-1) >> 1`: "we need
/// to do this to cover cases where 2's complement not used, since C standard permits 1's complement
/// and sign-bit representations". Rust *guarantees* two's complement for every integer type, so the
/// computed fallback has nothing to cover and [`c_int::MAX`] is the whole answer.
///
/// Returned as a `c_uint` because that is C's return type and because its one caller, [`gt_off`],
/// compares it against an `unsigned`. The conversion is checked rather than cast: `c_int::MAX` is
/// positive by definition, so it always fits, and going through `try_from` keeps the function clear
/// of `clippy::cast_sign_loss` without an `allow`.
#[must_use]
pub(crate) fn gz_intmax() -> c_uint {
    c_uint::try_from(c_int::MAX).unwrap_or(c_uint::MAX)
}

/// Whether an `unsigned` value is too large to be compared against a [`ZOff64`] safely.
///
/// Implements the `GT_OFF` macro (`gzguts.h` L216):
///
/// ```c
/// #define GT_OFF(x) (sizeof(int) == sizeof(z_off64_t) && (x) > gz_intmax())
/// ```
///
/// with the header's own explanation: it "is true if x > maximum `z_off64_t` value -- needed when
/// comparing unsigned to `z_off64_t`, which is signed (possible `z_off64_t` types `off_t`, `off64_t`,
/// and `long` are all signed)".
///
/// **On every target this implementation supports the first conjunct is false**, because [`ZOff64`] is 8 bytes
/// and a C `int` is 4, so the whole predicate is a compile-time constant `false` and the guard costs
/// nothing. It is preserved rather than deleted because the comparisons it protects are C's: each of
/// its three call sites -- [`read::gz_skip`], [`write::gz_zero`] and [`read::gzseek64`] -- writes
/// `GT_OFF(n) || (z_off64_t)n > amount`, and on a target where `int` and `z_off64_t` were the same
/// width the second half alone would misread a large `unsigned` as negative.
///
/// `pub(crate)`, and reachable by nothing outside this subtree: `gz_skip`, `gz_zero` and `gzseek64`
/// are its only callers.
#[must_use]
pub(crate) fn gt_off(value: c_uint) -> bool {
    size_of::<c_int>() == size_of::<ZOff64>() && value > gz_intmax()
}

//  Two conventions hold across all six, both following the precedent the sibling modules set:
//
//  * `Option<&mut GzState>` is this implementation's spelling of C's `gzFile file`, and [`None`] is
//    `file == NULL`. The null test stays *here* rather than being left to the facade because each
//    function has its own documented answer for it -- -1, 0, a null `const char *`, nothing at all,
//    or `Z_STREAM_ERROR` -- and because a facade holding a `*mut GzState` produces exactly this
//    argument in one step with `as_mut`. [`close::gzclose`] takes the same shape for the same reason.
//  * A value that is a C `int` in the *return* position is a `c_int`, because it is C's answer and
//    needs no arithmetic; a value that is a C `int` in an *argument* or out-parameter position is an
//    `i32`, because it flows into crate APIs typed `i32` -- `ReturnCode` wraps an `i32`,
//    [`crate::deflate::deflate_params`] takes `i32` -- and an extra conversion at this boundary would
//    buy nothing. [`read::gzrewind`] and [`write::gzflush`] make the same two choices.

/// The recorded error as a [`ReturnCode`].
///
/// `gzsetparams` returns `state->err` verbatim on two of its paths (`gzwrite.c` L652 and L659), and
/// every value that field can hold was put there by [`gz_error`] from a [`ReturnCode`], so the
/// conversion cannot fail in practice. It is written as a total function rather than an assertion
/// because reporting an unrecognised status is a far better outcome than aborting a C caller's
/// process over one, and `unwrap_or` is not the denied `unwrap`: it cannot panic.
fn recorded_error<'a, A: Allocator<'a>>(state: &GzState<'a, A>) -> ReturnCode {
    state.err_code().unwrap_or(ReturnCode::STREAM_ERROR)
}

/// Sets the size of the internal buffers, before any read or write allocates them.
///
/// Implements `gzbuffer` (`gzlib.c` L322-L343), declared at `zlib.h` L1429.
///
/// "Three times that size" is the sum of the two allocations: the read path takes `want` for `in` and
/// `want << 1` for `out`, and the write path takes `want << 1` for `in` and `want` for `out`
/// (`gzread.c` L100-L110, `gzwrite.c` L20-L27). [`printf`] then caps a single `gzprintf` at
/// `want`, which is the second sentence's consequence.
///
/// # The four rejections, in C's order
///
/// 1. No stream, or a mode that is neither [`GZ_READ`] nor [`GZ_WRITE`] (L326-L331).
/// 2. **`size != 0`** -- the buffers already exist (L334-L335). This is precisely why the header says
///    the call must come before any read or write: `size` is set the moment they are allocated, and
///    from then on this function does nothing but fail.
/// 3. **`(size << 1) < size`** (L338-L339), C's comment being "need to be able to double it". An
///    overflow guard, not a size limit: both paths above allocate `want << 1` for one of the two
///    buffers, so the doubling must not wrap.
/// 4. Nothing else. A `size` below 8 is not rejected but **raised to 8** (L340-L341), C's comment
///    being "needed to behave well with flushing".
///
/// Returns 0 on success, or -1 on any of the three failures above.
///
/// Returning `-1` unnoticed would leave the caller believing a buffer size it never got, so the
/// answer is `#[must_use]`.
#[must_use]
pub fn gzbuffer<'a, A: Allocator<'a>>(state: Option<&mut GzState<'a, A>>, size: c_uint) -> c_int {
    // L326-L327: `if (file == NULL) return -1;`. Nothing has been touched.
    let Some(state) = state else {
        return -1;
    };
    if state.resync_from_exposed().is_err() {
        return -1;
    }

    if !state.has_valid_mode() {
        state.refresh_exposed();
        return -1;
    }

    if state.size() != 0 {
        state.refresh_exposed();
        return -1;
    }

    // L338-L339: `if ((size << 1) < size) return -1;`. `wrapping_shl` reproduces C's truncating
    // shift exactly and, unlike `<<`, states in the source that discarding the top bit is the
    // intent rather than an accident.
    if size.wrapping_shl(1) < size {
        state.refresh_exposed();
        return -1;
    }

    let size = size.max(8);

    state.set_want(size);
    state.refresh_exposed();
    0
}

/// Whether a read went past the end of the input and came up short.
///
/// Implements `gzeof` (`gzlib.c` L498-L510), declared at `zlib.h` L1711.
///
/// So this reports `past`, not `eof`: `eof` records that the *file* ended, while `past` records that
/// a caller's request could not be satisfied because of it (`gzread.c` L293-L294 and L347-L348).
/// [`gzclearerr`] resets both, which is what makes reading a concurrently written file possible.
///
/// A write stream always answers 0, as does a missing stream or an unrecognised mode.
///
/// `#[must_use]`: this function exists only to be asked, and answers nothing else.
#[must_use]
pub fn gzeof<'a, A: Allocator<'a>>(state: Option<&mut GzState<'a, A>>) -> c_int {
    // L502-L503 and L506-L507: `if (file == NULL) return 0;` then the mode check.
    let Some(state) = state else {
        return 0;
    };
    if state.resync_from_exposed().is_err() {
        return 0;
    }
    if !state.has_valid_mode() {
        state.refresh_exposed();
        return 0;
    }

    let past = state.is_reading() && state.past();
    state.refresh_exposed();
    c_int::from(past)
}

/// The message for the last error on this stream, and optionally its code.
///
/// Implements `gzerror` (`gzlib.c` L513-L528), declared at `zlib.h` L1775.
///
/// # The three answers, all of which are distinct
///
/// * [`None`] is C's `NULL`, returned for a missing stream and for a mode that is neither
///   [`GZ_READ`] nor [`GZ_WRITE`] (L517-L521). The facade publishes it as a null `const char *`.
/// * `Some("out of memory")` for `Z_MEM_ERROR` (L526). `gz_error` deliberately stores no message
///   for that code, so the literal is supplied here; the two are a matched pair.
/// * `Some(...)` -- the stored message, already prefixed with the path, or the **empty string** when
///   there is none (L527). An empty string is not a null pointer, and a caller distinguishing "no
///   error text" from "unusable handle" depends on that.
///
/// # Lifetimes
///
/// The returned slice borrows the state, which is how Rust expresses the header's three warnings at
/// once: it cannot be modified, it cannot outlive a later mutation of the stream, and it cannot
/// outlive the handle. This view excludes the terminating zero, because a Rust caller wants the
/// text; [`gzerror_terminated`] is the view a C facade wants, and it is a view of the *same*
/// storage rather than a copy of it.
///
/// `errnum` is an `i32` rather than a `c_int` for the reason given above this group of functions: a
/// zlib status is an `i32` everywhere in this crate, and the facade stores it into the caller's
/// `*mut c_int`.
///
/// `#[must_use]`: the message and the code are the whole result; `gzclearerr` is the call to make
/// when the intent is to discard an error rather than read it.
#[must_use]
pub fn gzerror<'s, 'a, A: Allocator<'a>>(
    state: Option<&'s mut GzState<'a, A>>,
    errnum: Option<&mut i32>,
) -> Option<&'s [u8]> {
    error_text(state, errnum, false)
}

/// [`gzerror`] with the terminating zero included, for a caller that needs a C string.
///
/// ★ The entry point `crates/libz-rs-sys` uses, and the reason the stored message carries a
/// terminator at all. `gzerror` returns a `const char *` whose contents belong to the stream and
/// stay valid until the next call or the close (`zlib.h` L1783-L1786), which is exactly the
/// lifetime of this borrow -- so the facade can publish the pointer directly. Without this the
/// facade would have to keep a second, terminated copy beside every stream, and an error would
/// cost two allocations where C's costs one.
///
/// Identical to [`gzerror`] in every other respect, including which streams answer [`None`] and
/// which `errnum` values are written. The two static answers -- the `Z_MEM_ERROR` literal and the
/// empty string -- come back terminated too, so the result is always a valid C string.
#[must_use]
pub fn gzerror_terminated<'s, 'a, A: Allocator<'a>>(
    state: Option<&'s mut GzState<'a, A>>,
    errnum: Option<&mut i32>,
) -> Option<&'s [u8]> {
    error_text(state, errnum, true)
}

/// The shared body of [`gzerror`] and [`gzerror_terminated`].
///
/// One implementation rather than two, because the difference between them is which *view* of one
/// stored message is handed back -- never which stream is recognised, which code is reported, or
/// when the exposed prefix is refreshed. Those are the parts that must not be allowed to drift.
fn error_text<'s, 'a, A: Allocator<'a>>(
    state: Option<&'s mut GzState<'a, A>>,
    errnum: Option<&mut i32>,
    terminated: bool,
) -> Option<&'s [u8]> {
    let state = state?;
    if state.resync_from_exposed().is_err() {
        return None;
    }

    if !state.has_valid_mode() {
        state.refresh_exposed();
        return None;
    }

    if let Some(slot) = errnum {
        *slot = state.err();
    }

    // L526-L527, with the mutation finished before the borrow that is handed back begins.
    let mem_error = state.err() == ReturnCode::MEM_ERROR.as_i32();
    state.refresh_exposed();
    if mem_error {
        return Some(if terminated {
            OUT_OF_MEMORY_TERMINATED
        } else {
            OUT_OF_MEMORY
        });
    }
    let stored = if terminated {
        state.msg_with_nul()
    } else {
        state.msg()
    };
    Some(stored.unwrap_or(if terminated {
        NO_MESSAGE_TERMINATED
    } else {
        NO_MESSAGE
    }))
}

/// Clears the error and end-of-file indicators.
///
/// Implements `gzclearerr` (`gzlib.c` L531-L547), declared at `zlib.h` L1792.
///
/// `eof` and `past` are cleared only on a read stream (L544-L547), because neither means anything on
/// a write stream; the error itself is cleared for both by `gz_error` with `Z_OK`, which also
/// releases the stored message. Note that `Z_OK` is not fatal, so `x.have` survives -- clearing an
/// error must not discard output the caller has not yet been given.
///
/// A missing stream or an unrecognised mode makes this a no-op, exactly as C's two early returns do.
pub fn gzclearerr<'a, A: Allocator<'a>>(state: Option<&mut GzState<'a, A>>) {
    // L535-L536 and L539-L540.
    let Some(state) = state else {
        return;
    };
    if state.resync_from_exposed().is_err() {
        return;
    }
    if !state.has_valid_mode() {
        state.refresh_exposed();
        return;
    }

    if state.is_reading() {
        state.set_eof(false);
        state.set_past(false);
    }

    gz_error(state, ReturnCode::OK, None);
    state.refresh_exposed();
}

/// Whether the stream is being copied through untouched rather than decompressed.
///
/// Implements `gzdirect` (`gzread.c` L627-L642), declared at `zlib.h` L1726.
///
/// # ★ It has a side effect, and that is the point
///
/// When the direction is reading, the decision has not been made (`how == LOOK`) and nothing is
/// buffered, this calls `read::gz_look` and **discards its result** (L637-L638), C's comment being
/// "if the state is not known, but we can find out, then do so (this is mainly for right after a
/// `gzopen()` or `gzdopen()`)". That call allocates both buffers and reads up to four bytes, which is
/// why the header insists [`gzbuffer`] be called first and why this function lives with the read
/// machinery even though the field it reports belongs to [`open`]. Discarding the result is
/// deliberate: a read error leaves `direct` at its previous value, and reporting the error is the
/// next read's job.
///
/// # Two things it deliberately does not do
///
/// * **No mode check.** Unlike its neighbours in `gzlib.c`, C performs none here (L631-L634), so a
///   write stream falls straight through to the last line and reports whether transparent writing was
///   requested -- which `zlib.h` L1745-L1748 documents as the answer for a write stream.
/// * **No normalisation of `direct`.** The final test is `state->direct == 1` (L641), not "non-zero":
///   the -1 that the `"G"` mode installs means *gzip only* and is emphatically not transparent
///   (`gzlib.c` L156-L166). [`GzState::is_transparent`] is exactly that test.
///
/// Returns 1 when transparent, 0 otherwise, including for a missing stream.
///
/// `#[must_use]`: discarding the answer while keeping the side effect would be a buffer allocation
/// dressed up as a question -- `gzbuffer` followed by a read is the way to do that deliberately.
#[must_use]
pub fn gzdirect<'a, A: Allocator<'a> + Copy>(state: Option<&mut GzState<'a, A>>) -> c_int {
    let Some(state) = state else {
        return 0;
    };
    if state.resync_from_exposed().is_err() {
        return 0;
    }

    // L637-L638: `if (state->mode == GZ_READ && state->how == LOOK && state->x.have == 0)
    //             (void)gz_look(state);`
    if state.mode() == GZ_READ && state.how() == LOOK && state.have() == 0 {
        // The result is discarded exactly as C's `(void)` cast discards it; see above.
        let _ = gz_look(state);
    }

    let direct = state.is_transparent();
    state.refresh_exposed();
    c_int::from(direct)
}

/// `deflateParams(strm, level, strategy)` with the `z_stream` view rebuilt around the layer's cursors.
///
/// Implements `gzwrite.c` L659. C hands the compressor the address of the `z_stream` embedded in
/// `gz_state` (`gzguts.h` L202), so the cursors and the five scalars are already where
/// [`crate::deflate::deflate_params`] expects them; this implementation keeps the compression state and the
/// caller-visible scalars apart, so the view is assembled here, handed over and read back. The shape
/// is [`mod@write`]'s `deflate_once` deliberately: `deflateParams` can itself call `deflate(strm,
/// Z_BLOCK)` to close an open block (`deflate.c` L791-L799), so it needs a real output window and a
/// real input window, and whatever it produces must be accounted for afterwards.
///
/// Both windows are the **whole** buffer truncated to the end of the valid region, never re-sliced
/// from the cursor: `deflate_stored` reads backwards from `next_in` and `flush_pending` writes at
/// `next_out`, so the already-consumed prefix has to stay addressable -- which is exactly what C's
/// advancing pointers leave in place.
///
/// Returns whatever `deflate_params` returned, or [`ReturnCode::STREAM_ERROR`] if the view could not
/// be assembled -- no compressor, no output buffer, or a cursor outside its buffer. C cannot reach
/// those states without having already corrupted memory.
fn params_once<'a, A: Allocator<'a>>(
    state: &mut GzState<'a, A>,
    level: i32,
    strategy: i32,
) -> ReturnCode {
    let next_in = state.strm.next_in;
    let next_out = state.strm.next_out;
    let in_end = next_in.saturating_add(to_index(state.strm.avail_in));
    let out_end = next_out.saturating_add(to_index(state.strm.avail_out));
    let total_in = state.strm.total_in;
    let total_out = state.strm.total_out;
    let msg = state.strm.msg;
    let adler = state.strm.adler;
    let data_type = state.strm.data_type;

    // Three simultaneous views into one `&mut GzState`, each reached by naming its field directly:
    // `input` read-only, `output` mutably and `strm.engine` mutably. The paths are disjoint, so the
    // borrow checker admits all three at once.
    let (ret, new_next_in, new_avail_in, new_next_out, new_avail_out, scalars) = {
        let GzState {
            buffer_slot, strm, ..
        } = state;
        let (staged_in, staged_out) = split_buffers(buffer_slot);
        let base: &[u8] = staged_in.map_or(&[][..], |buffer| buffer.as_slice());
        let Some(input) = base.get(..in_end) else {
            return ReturnCode::STREAM_ERROR;
        };
        let Some(output) = staged_out
            .map(Buffer::as_mut_slice)
            .and_then(|slice| slice.get_mut(..out_end))
        else {
            return ReturnCode::STREAM_ERROR;
        };
        let GzEngine::Deflate(engine) = &mut strm.engine else {
            return ReturnCode::STREAM_ERROR;
        };
        let Some(compressor) = engine.get_mut() else {
            return ReturnCode::STREAM_ERROR;
        };

        let mut stream = DeflateStream::new(input, output);
        stream.next_in = next_in;
        stream.next_out = next_out;
        stream.total_in = total_in;
        stream.total_out = total_out;
        stream.msg = msg;
        stream.adler = adler;
        stream.data_type = data_type;

        let ret = deflate_params(compressor, &mut stream, level, strategy);

        (
            ret,
            stream.next_in,
            stream.avail_in(),
            stream.next_out,
            stream.avail_out(),
            (
                stream.total_in,
                stream.total_out,
                stream.msg,
                stream.adler,
                stream.data_type,
            ),
        )
    };

    state.strm.next_in = new_next_in;
    state.strm.avail_in = to_count(new_avail_in);
    state.strm.next_out = new_next_out;
    state.strm.avail_out = to_count(new_avail_out);
    let (total_in, total_out, msg, adler, data_type) = scalars;
    state.strm.total_in = total_in;
    state.strm.total_out = total_out;
    state.strm.msg = msg;
    state.strm.adler = adler;
    state.strm.data_type = data_type;
    ret
}

/// Changes the compression level and strategy for the input that follows.
///
/// Implements `gzsetparams` (`gzwrite.c` L630-L664), declared at `zlib.h` L1445.
///
/// # The six steps, in C's order
///
/// 1. **The guard** (L644-L646): the direction must be [`GZ_WRITE`], the recorded error must be
///    `Z_OK` unless the stream is merely stalled on a non-blocking file, **and `direct` must be
///    clear** -- transparent writing has no parameters to change. Anything else is
///    [`ReturnCode::STREAM_ERROR`]. Note the error half is the narrower, write-side one:
///    a lingering `Z_BUF_ERROR` is *not* tolerated here, because on a write stream it means a
///    `gzprintf` stalled and must be retried.
/// 2. `gz_error(state, Z_OK, NULL)` (L647), which clears any stale message.
/// 3. **★ The short circuit** (L650-L651): if neither `level` nor `strategy` differs from what the
///    stream already carries, return `Z_OK` and touch nothing. The engine is not consulted, so no
///    block is flushed and not one byte is emitted -- which is why this must not be "simplified" into
///    an unconditional update.
/// 4. A pending seek is paid for first (L654-L655), because the zeros it writes belong to the old
///    parameters.
/// 5. **★ The `Z_BLOCK` pre-flush** (L657-L660), and the reason the header can promise that
///    "previously provided data is flushed before applying the parameter changes". Only when the
///    buffers exist (`state->size`) and only when something is actually buffered
///    (`strm->avail_in`), `write::gz_comp` is run with [`Flush::Block`] so that the bytes already
///    handed to this stream are compressed under the **old** level and strategy. This is the single
///    place in the whole `gzFile` layer that uses `Z_BLOCK`; it is byte-observable, so it may be
///    neither dropped nor reordered. `deflate_params` is then what performs the switch -- the
///    parameter change is never reimplemented here.
/// 6. The new values are recorded (L662-L663) whether or not the engine exists yet, so that a
///    `gzsetparams` before the first write is honoured by `write::gz_init` when it finally builds
///    the compressor.
///
/// ★ `deflate_params`' own status is **discarded**, exactly as C's bare `deflateParams(strm, level,
/// strategy);` discards it. That is safe rather than sloppy: step 5 has already drained `avail_in`
/// and flushed the block, so the `Z_BUF_ERROR` `deflateParams` returns for an incompletely flushed
/// block is unreachable, and the level and strategy are recorded regardless so the next
/// `write::gz_comp` applies them.
///
/// `level` and `strategy` are `i32` rather than `c_int` for the reason given above this group of
/// functions. They are deliberately **not** validated here: C validates neither, leaving the range
/// check to `deflate_params` (`deflate.c` L784-L788) or, for a stream whose compressor does not exist
/// yet, to `deflate_init2`.
///
/// `#[must_use]`: a discarded status here hides a refused parameter change and, worse, a `Z_ERRNO`
/// from the flush -- which means bytes the caller believes were written were not.
#[must_use]
pub fn gzsetparams<'a, A: Allocator<'a> + Copy>(
    state: Option<&mut GzState<'a, A>>,
    level: i32,
    strategy: i32,
) -> ReturnCode {
    let Some(state) = state else {
        return ReturnCode::STREAM_ERROR;
    };
    if state.resync_from_exposed().is_err() {
        return ReturnCode::STREAM_ERROR;
    }

    // 1. L644-L646: `if (state->mode != GZ_WRITE || (state->err != Z_OK && !state->again) ||
    //    state->direct) return Z_STREAM_ERROR;`
    if !state.is_writing()
        || (state.err() != ReturnCode::OK.as_i32() && !state.again())
        || state.direct() != 0
    {
        state.refresh_exposed();
        return ReturnCode::STREAM_ERROR;
    }

    // 2. L647.
    gz_error(state, ReturnCode::OK, None);

    // 3. L650-L651: `if (level == state->level && strategy == state->strategy) return Z_OK;`
    if level == state.level() && strategy == state.strategy() {
        state.refresh_exposed();
        return ReturnCode::OK;
    }

    // 4. L654-L655: `if (state->skip && gz_zero(state) == -1) return state->err;`
    if state.skip() != 0 && gz_zero(state).is_err() {
        let err = recorded_error(state);
        state.refresh_exposed();
        return err;
    }

    // 5. L657-L660.
    if state.size() != 0 {
        // `if (strm->avail_in && gz_comp(state, Z_BLOCK) == -1) return state->err;` -- the `&&`
        // short-circuits exactly as C's does, so `gz_comp` runs only when something is buffered.
        if state.strm.avail_in != 0 && gz_comp(state, Flush::Block).is_err() {
            let err = recorded_error(state);
            state.refresh_exposed();
            return err;
        }
        // `deflateParams(strm, level, strategy);` -- status discarded, see above.
        let _ = params_once(state, level, strategy);
    }

    // 6. L662-L664.
    state.set_level(level);
    state.set_strategy(strategy);
    state.refresh_exposed();
    ReturnCode::OK
}

#[cfg(test)]
mod tests {
    // The crate denies the panic-prone lints in library code, which is the right policy there and
    // the wrong one here: an assertion that fails panics, and a fixture that cannot index is
    // unreadable. `clippy.toml` already relaxes them inside tests; restating it keeps the intent
    // local and visible. Scoped to this module, which ships in no build.
    #![allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

    use super::{
        errno_message, gt_off, gz_error, gz_intmax, gzbuffer, gzclearerr, gzdirect, gzeof, gzerror,
        gzsetparams, ErrnoMessage, ERRNO_MESSAGE_CAPACITY, NO_ERRNO_TEXT, OS_ERROR_SUFFIX,
    };
    use crate::allocate::GlobalAllocator;
    use crate::config::{Z_DEFAULT_COMPRESSION, Z_DEFAULT_STRATEGY, Z_FILTERED};
    use crate::error::ReturnCode;
    use crate::gz::open::gz_open_handle;
    use crate::gz::state::{
        GzHandle, GzIoError, GzSeekFrom, GzState, ZOff64, GZIP, GZ_NONE, GZ_READ, GZ_WRITE,
    };
    use crate::gz::write::{gz_init, gzwrite};
    use alloc::boxed::Box;
    use alloc::vec::Vec;
    use core::cell::RefCell;
    use core::ffi::{c_int, c_uint};
    // Needed for the `write!` calls that exercise `ErrnoMessage`; the parent's `use ... as _` is a
    // private import and is not inherited by this module.
    use core::fmt::Write as _;

    /// A file that lives in memory: reads from a fixed source, appends writes to a shared sink.
    ///
    /// Enough to drive [`crate::gz::read::gz_look`] and [`crate::gz::write::gz_comp`] with no real
    /// file, which also keeps these tests runnable under Miri.
    struct MemFile<'c> {
        /// Where writes accumulate, so a test can inspect the bytes the stream emitted.
        sink: Option<&'c RefCell<Vec<u8>>>,
        /// What reads consume.
        source: Vec<u8>,
        /// How far into [`Self::source`] reading has got, and the answer to a `SEEK_CUR` of zero.
        position: usize,
    }

    impl<'c> MemFile<'c> {
        /// A write-only file over `sink`.
        fn writer(sink: &'c RefCell<Vec<u8>>) -> Self {
            Self {
                sink: Some(sink),
                source: Vec::new(),
                position: 0,
            }
        }

        /// A read-only file over `source`.
        fn reader(source: &[u8]) -> Self {
            Self {
                sink: None,
                source: source.to_vec(),
                position: 0,
            }
        }
    }

    impl GzHandle for MemFile<'_> {
        fn read(&mut self, buf: &mut [u8]) -> Result<usize, GzIoError> {
            let remaining = self.source.len().saturating_sub(self.position);
            let take = remaining.min(buf.len());
            let end = self.position.saturating_add(take);
            buf[..take].copy_from_slice(&self.source[self.position..end]);
            self.position = end;
            Ok(take)
        }

        fn write(&mut self, buf: &[u8]) -> Result<usize, GzIoError> {
            match self.sink {
                Some(sink) => {
                    sink.borrow_mut().extend_from_slice(buf);
                    self.position = self.position.saturating_add(buf.len());
                    Ok(buf.len())
                }
                None => Err(GzIoError::new(9, false)),
            }
        }

        fn seek(&mut self, offset: ZOff64, whence: GzSeekFrom) -> Result<ZOff64, GzIoError> {
            let base = match whence {
                GzSeekFrom::Start => 0,
                GzSeekFrom::Current => ZOff64::try_from(self.position).unwrap(),
                GzSeekFrom::End => ZOff64::try_from(self.source.len()).unwrap(),
            };
            let target = base.checked_add(offset).ok_or(GzIoError::new(22, false))?;
            let target = usize::try_from(target).map_err(|_| GzIoError::new(22, false))?;
            self.position = target;
            ZOff64::try_from(target).map_err(|_| GzIoError::new(22, false))
        }

        fn set_nonblocking(&mut self, _nonblocking: bool) -> Result<(), GzIoError> {
            Ok(())
        }

        fn close(&mut self) -> Result<(), GzIoError> {
            Ok(())
        }
    }

    /// A state as `GzState::new` leaves one: no direction, no buffers, no handle.
    fn fresh() -> GzState<'static, GlobalAllocator> {
        GzState::new(GlobalAllocator)
    }

    /// A read-configured state with both buffers allocated, so `x.have` and `x.next` are meaningful.
    fn with_read_buffers() -> GzState<'static, GlobalAllocator> {
        let mut state = fresh();
        state.set_mode(GZ_READ);
        state.set_want(64);
        state.allocate_read_buffers().unwrap();
        state.set_size(64);
        state
    }

    #[test]
    fn gz_error_prefixes_the_message_with_the_path() {
        let mut state = fresh();
        state.try_set_path(b"/tmp/hello.gz").unwrap();

        gz_error(
            &mut state,
            ReturnCode::DATA_ERROR,
            Some(b"compressed data error"),
        );

        // `snprintf(msg, ..., "%s%s%s", state->path, ": ", msg)` -- `gzlib.c` L585-L586.
        assert_eq!(
            state.msg(),
            Some(&b"/tmp/hello.gz: compressed data error"[..])
        );
        assert_eq!(state.err(), ReturnCode::DATA_ERROR.as_i32());
    }

    #[test]
    fn gz_error_releases_the_previous_message() {
        let mut state = fresh();
        state.try_set_path(b"p").unwrap();
        gz_error(&mut state, ReturnCode::DATA_ERROR, Some(b"first"));
        assert_eq!(state.msg(), Some(&b"p: first"[..]));

        // `gz_error(state, Z_OK, NULL)` is what `gzclearerr` and every entry-point guard perform.
        gz_error(&mut state, ReturnCode::OK, None);

        assert_eq!(state.msg(), None);
        assert_eq!(state.err(), ReturnCode::OK.as_i32());
    }

    #[test]
    fn gz_error_clears_a_message_latched_with_an_out_of_memory_error() {
        // C's `if (state->err != Z_MEM_ERROR) free(state->msg);` skips the release for a message it
        // believes to be a static string, then nulls the slot regardless. This implementation owns the
        // message, so the slot is emptied and the storage released together -- the observable
        // outcome, an empty slot and the new code recorded, is the same. The combination is
        // unreachable through `gz_error` itself (it stores nothing for `Z_MEM_ERROR`), so it is
        // constructed here directly.
        let mut state = fresh();
        state.try_set_path(b"p").unwrap();
        state.set_err(ReturnCode::MEM_ERROR.as_i32());
        state.try_set_prefixed_msg(b"stale").unwrap();

        gz_error(&mut state, ReturnCode::OK, None);

        assert_eq!(state.msg(), None);
        assert_eq!(state.err(), ReturnCode::OK.as_i32());
    }

    #[test]
    fn gz_error_stores_no_message_for_an_out_of_memory_error() {
        // `if (err == Z_MEM_ERROR) return;` -- `gzlib.c` L573-L574. The literal comes from
        // `gzerror` instead, which the pair of assertions below checks end to end.
        let mut state = fresh();
        state.set_mode(GZ_READ);
        state.try_set_path(b"p").unwrap();

        gz_error(&mut state, ReturnCode::MEM_ERROR, Some(b"out of memory"));

        assert_eq!(state.msg(), None);
        assert_eq!(state.err(), ReturnCode::MEM_ERROR.as_i32());
        assert_eq!(gzerror(Some(&mut state), None), Some(&b"out of memory"[..]));
    }

    #[test]
    fn gz_error_zeroes_have_for_a_fatal_error() {
        let mut state = with_read_buffers();
        state.set_output_window(0, 10).unwrap();
        assert_eq!(state.have(), 10);

        // "if fatal, set state->x.have to 0 so that the gzgetc() macro fails" -- `gzlib.c` L563.
        gz_error(&mut state, ReturnCode::DATA_ERROR, None);

        assert_eq!(state.have(), 0);
    }

    #[test]
    fn gz_error_keeps_have_for_a_stalled_stream() {
        let mut state = with_read_buffers();
        state.set_output_window(0, 10).unwrap();
        // `&& !state->again` -- a non-blocking stall is not fatal, and discarding buffered output
        // would lose bytes the caller has not been given yet.
        state.set_again(true);

        gz_error(&mut state, ReturnCode::ERRNO, None);

        assert_eq!(state.have(), 10);
        assert_eq!(state.err(), ReturnCode::ERRNO.as_i32());
    }

    #[test]
    fn gz_error_keeps_have_for_the_two_non_fatal_codes() {
        for code in [ReturnCode::OK, ReturnCode::BUF_ERROR] {
            let mut state = with_read_buffers();
            state.set_output_window(0, 7).unwrap();

            gz_error(&mut state, code, None);

            assert_eq!(state.have(), 7, "{code:?} must not discard buffered output");
        }
    }

    #[test]
    fn gz_intmax_is_the_c_int_maximum() {
        assert_eq!(gz_intmax(), c_uint::try_from(c_int::MAX).unwrap());
    }

    #[test]
    fn gt_off_is_constantly_false_on_this_target() {
        // The first conjunct of `GT_OFF` is `sizeof(int) == sizeof(z_off64_t)`, which no supported
        // target satisfies, so the predicate cannot be true for any input.
        assert_ne!(size_of::<c_int>(), size_of::<ZOff64>());
        assert!(!gt_off(0));
        assert!(!gt_off(gz_intmax()));
        assert!(!gt_off(c_uint::MAX));
    }

    #[test]
    fn gzbuffer_rejects_a_missing_stream() {
        assert_eq!(gzbuffer::<GlobalAllocator>(None, 4096), -1);
    }

    #[test]
    fn gzbuffer_rejects_an_unrecognised_mode() {
        let mut state = fresh();
        assert_eq!(state.mode(), GZ_NONE);
        assert_eq!(gzbuffer(Some(&mut state), 4096), -1);
    }

    #[test]
    fn gzbuffer_rejects_a_second_call_once_the_buffers_exist() {
        let mut state = fresh();
        state.set_mode(GZ_READ);
        assert_eq!(gzbuffer(Some(&mut state), 4096), 0);
        assert_eq!(state.want(), 4096);

        // `if (state->size != 0) return -1;` -- the buffers are allocated, so it is too late.
        state.set_size(4096);
        assert_eq!(gzbuffer(Some(&mut state), 8192), -1);
        assert_eq!(state.want(), 4096);
    }

    #[test]
    fn gzbuffer_rejects_a_size_that_cannot_be_doubled() {
        let mut state = fresh();
        state.set_mode(GZ_WRITE);
        let before = state.want();

        for size in [c_uint::MAX, c_uint::MAX ^ 1, 1 << (c_uint::BITS - 1)] {
            assert_eq!(gzbuffer(Some(&mut state), size), -1, "{size:#x}");
            assert_eq!(state.want(), before);
        }
    }

    #[test]
    fn gzbuffer_raises_a_small_size_to_eight() {
        for size in [0, 1, 7] {
            let mut state = fresh();
            state.set_mode(GZ_WRITE);
            assert_eq!(gzbuffer(Some(&mut state), size), 0);
            // "needed to behave well with flushing" -- `gzlib.c` L341.
            assert_eq!(state.want(), 8);
        }
    }

    #[test]
    fn gzbuffer_stores_a_size_of_eight_or_more_verbatim() {
        for size in [8, 9, 65536, 1 << (c_uint::BITS - 2)] {
            let mut state = fresh();
            state.set_mode(GZ_READ);
            assert_eq!(gzbuffer(Some(&mut state), size), 0);
            assert_eq!(state.want(), size);
        }
    }

    #[test]
    fn gzeof_reports_past_on_a_read_stream() {
        let mut state = fresh();
        state.set_mode(GZ_READ);
        assert_eq!(gzeof(Some(&mut state)), 0);

        state.set_past(true);
        assert_eq!(gzeof(Some(&mut state)), 1);

        // `eof` alone is not what `gzeof` reports: the file may have ended without any request
        // having come up short.
        state.set_past(false);
        state.set_eof(true);
        assert_eq!(gzeof(Some(&mut state)), 0);
    }

    #[test]
    fn gzeof_is_zero_on_a_write_stream() {
        let mut state = fresh();
        state.set_mode(GZ_WRITE);
        state.set_past(true);
        assert_eq!(gzeof(Some(&mut state)), 0);
    }

    #[test]
    fn gzeof_is_zero_for_a_missing_stream_or_an_unrecognised_mode() {
        assert_eq!(gzeof::<GlobalAllocator>(None), 0);
        let mut state = fresh();
        state.set_past(true);
        assert_eq!(gzeof(Some(&mut state)), 0);
    }

    #[test]
    fn gzerror_returns_the_stored_message_and_the_code() {
        let mut state = fresh();
        state.set_mode(GZ_READ);
        state.try_set_path(b"in.gz").unwrap();
        gz_error(&mut state, ReturnCode::DATA_ERROR, Some(b"data error"));

        let mut errnum = 0;
        assert_eq!(
            gzerror(Some(&mut state), Some(&mut errnum)),
            Some(&b"in.gz: data error"[..])
        );
        assert_eq!(errnum, ReturnCode::DATA_ERROR.as_i32());
    }

    #[test]
    fn gzerror_returns_an_empty_string_when_there_is_no_message() {
        let mut state = fresh();
        state.set_mode(GZ_WRITE);

        let mut errnum = -99;
        // `state->msg == NULL ? "" : state->msg` -- an empty string, and emphatically not the null
        // pointer an unusable handle produces.
        assert_eq!(gzerror(Some(&mut state), Some(&mut errnum)), Some(&b""[..]));
        assert_eq!(errnum, ReturnCode::OK.as_i32());
    }

    #[test]
    fn gzerror_returns_nothing_for_a_missing_stream_or_an_unrecognised_mode() {
        assert_eq!(gzerror::<GlobalAllocator>(None, None), None);

        let mut state = fresh();
        let mut errnum = -99;
        assert_eq!(gzerror(Some(&mut state), Some(&mut errnum)), None);
        // C returns before touching `*errnum`, so the caller's value survives.
        assert_eq!(errnum, -99);
    }

    #[test]
    fn gzclearerr_resets_the_error_and_the_two_read_flags() {
        let mut state = fresh();
        state.set_mode(GZ_READ);
        state.try_set_path(b"in.gz").unwrap();
        gz_error(
            &mut state,
            ReturnCode::BUF_ERROR,
            Some(b"unexpected end of file"),
        );
        state.set_eof(true);
        state.set_past(true);

        gzclearerr(Some(&mut state));

        assert_eq!(state.err(), ReturnCode::OK.as_i32());
        assert_eq!(state.msg(), None);
        assert!(!state.eof());
        assert!(!state.past());
        assert_eq!(gzeof(Some(&mut state)), 0);
    }

    #[test]
    fn gzclearerr_clears_the_error_on_a_write_stream_too() {
        let mut state = fresh();
        state.set_mode(GZ_WRITE);
        state.try_set_path(b"out.gz").unwrap();
        gz_error(&mut state, ReturnCode::ERRNO, Some(b"stdio error"));

        gzclearerr(Some(&mut state));

        assert_eq!(state.err(), ReturnCode::OK.as_i32());
        assert_eq!(state.msg(), None);
    }

    #[test]
    fn gzclearerr_ignores_a_missing_stream_or_an_unrecognised_mode() {
        gzclearerr::<GlobalAllocator>(None);

        let mut state = fresh();
        state.set_err(ReturnCode::DATA_ERROR.as_i32());
        gzclearerr(Some(&mut state));
        // The mode is `GZ_NONE`, so C returns before clearing anything.
        assert_eq!(state.err(), ReturnCode::DATA_ERROR.as_i32());
    }

    #[test]
    fn gzdirect_reports_true_for_an_empty_input() {
        // "If the input file is empty, gzdirect() will return true, since the input does not contain
        // a gzip stream" -- `zlib.h` L1731-L1732. `gz_open` starts a read stream at `direct = 1`
        // precisely so that this holds (`gzlib.c` L185-L189).
        let mut state = gz_open_handle(
            Box::new(MemFile::reader(&[])),
            b"empty",
            b"rb",
            GlobalAllocator,
        )
        .unwrap();

        assert_eq!(gzdirect(Some(&mut state)), 1);
        // The side effect the header warns about: the buffers now exist.
        assert!(state.has_buffers());
    }

    #[test]
    fn gzdirect_reports_false_for_gzip_input() {
        // The four bytes `gz_look` tests: ID1, ID2, CM = deflate, and FLG < 32 (`gzread.c` L151).
        let mut state = gz_open_handle(
            Box::new(MemFile::reader(&[31, 139, 8, 0])),
            b"in.gz",
            b"rb",
            GlobalAllocator,
        )
        .unwrap();

        assert_eq!(gzdirect(Some(&mut state)), 0);
        assert_eq!(state.how(), GZIP);
    }

    #[test]
    fn gzdirect_is_zero_for_a_missing_stream() {
        assert_eq!(gzdirect::<GlobalAllocator>(None), 0);
    }

    #[test]
    fn gzdirect_does_not_check_the_mode() {
        // Unlike its neighbours in `gzlib.c`, `gzdirect` performs no mode check (`gzread.c`
        // L631-L638), so a stream with no direction still reports its `direct` flag.
        let mut state = fresh();
        assert_eq!(state.mode(), GZ_NONE);
        state.set_direct(1);
        assert_eq!(gzdirect(Some(&mut state)), 1);

        // And -1, the `"G"` mode's "gzip only", is not transparent.
        state.set_direct(-1);
        assert_eq!(gzdirect(Some(&mut state)), 0);
    }

    #[test]
    fn gzsetparams_rejects_anything_but_a_healthy_write_stream() {
        assert_eq!(
            gzsetparams::<GlobalAllocator>(None, 9, Z_DEFAULT_STRATEGY),
            ReturnCode::STREAM_ERROR
        );

        // A read stream.
        let mut state = fresh();
        state.set_mode(GZ_READ);
        assert_eq!(
            gzsetparams(Some(&mut state), 9, Z_DEFAULT_STRATEGY),
            ReturnCode::STREAM_ERROR
        );

        // A write stream with an error recorded and no stall to excuse it. Note that even
        // `Z_BUF_ERROR` -- which the read side tolerates -- is refused here.
        let mut state = fresh();
        state.set_mode(GZ_WRITE);
        state.set_err(ReturnCode::BUF_ERROR.as_i32());
        assert_eq!(
            gzsetparams(Some(&mut state), 9, Z_DEFAULT_STRATEGY),
            ReturnCode::STREAM_ERROR
        );

        // The same stream, stalled rather than broken, is allowed through.
        state.set_again(true);
        assert_eq!(
            gzsetparams(Some(&mut state), 9, Z_DEFAULT_STRATEGY),
            ReturnCode::OK
        );
        assert_eq!(state.level(), 9);
    }

    #[test]
    fn gzsetparams_rejects_a_transparent_stream() {
        let sink = RefCell::new(Vec::new());
        let mut state = gz_open_handle(
            Box::new(MemFile::writer(&sink)),
            b"out",
            b"wbT",
            GlobalAllocator,
        )
        .unwrap();
        assert_eq!(state.direct(), 1);

        // "Transparent writing" has no compression parameters to change.
        assert_eq!(
            gzsetparams(Some(&mut state), 9, Z_DEFAULT_STRATEGY),
            ReturnCode::STREAM_ERROR
        );
        assert_eq!(state.level(), Z_DEFAULT_COMPRESSION);
    }

    #[test]
    fn gzsetparams_short_circuits_when_nothing_changes() {
        let sink = RefCell::new(Vec::new());
        let mut state = gz_open_handle(
            Box::new(MemFile::writer(&sink)),
            b"out",
            b"wb",
            GlobalAllocator,
        )
        .unwrap();
        assert_eq!(gzwrite(&mut state, b"hello, hello!"), 13);
        assert_eq!(state.stream().avail_in, 13);
        assert!(sink.borrow().is_empty());

        // Identical parameters: the engine must not be touched, so the buffered input stays
        // buffered and not one byte reaches the file.
        assert_eq!(
            gzsetparams(Some(&mut state), Z_DEFAULT_COMPRESSION, Z_DEFAULT_STRATEGY),
            ReturnCode::OK
        );
        assert_eq!(state.stream().avail_in, 13);
        assert!(sink.borrow().is_empty());
    }

    #[test]
    fn gzsetparams_flushes_buffered_input_before_switching() {
        let sink = RefCell::new(Vec::new());
        let mut state = gz_open_handle(
            Box::new(MemFile::writer(&sink)),
            b"out",
            b"wb",
            GlobalAllocator,
        )
        .unwrap();
        assert_eq!(gzwrite(&mut state, b"hello, hello!"), 13);
        assert_eq!(state.stream().avail_in, 13);
        assert!(sink.borrow().is_empty());

        // The `Z_BLOCK` pre-flush of `gzwrite.c` L657-L658: everything already handed to the stream
        // is compressed under the *old* level and strategy before the switch, which is what makes
        // `zlib.h` L1449-L1450's "previously provided data is flushed" true. It is byte-observable,
        // so the assertion is made on the file rather than on a cursor: before the call nothing had
        // reached it, and afterwards the member that the old parameters produced has.
        //
        // Draining `strm.avail_in` is `gz_comp`'s own contract and is asserted in `gz/write.rs`'s
        // suite; what is pinned here is that the flush happens at all, and that it happens *before*
        // the switch. The companion test below pins the other half of C's `&&`.
        assert_eq!(gzsetparams(Some(&mut state), 9, Z_FILTERED), ReturnCode::OK);

        assert!(!sink.borrow().is_empty());
        assert_eq!(state.level(), 9);
        assert_eq!(state.strategy(), Z_FILTERED);
    }

    #[test]
    fn gzsetparams_does_not_flush_when_nothing_is_buffered() {
        let sink = RefCell::new(Vec::new());
        let mut state = gz_open_handle(
            Box::new(MemFile::writer(&sink)),
            b"out",
            b"wb",
            GlobalAllocator,
        )
        .unwrap();
        // The buffers and the compressor exist, but no input is waiting.
        gz_init(&mut state).unwrap();
        assert_ne!(state.size(), 0);
        assert_eq!(state.stream().avail_in, 0);

        // C's `if (strm->avail_in && gz_comp(state, Z_BLOCK) == -1)` short-circuits, so `gz_comp` is
        // never entered and not one byte is written -- a parameter change on an idle stream must not
        // open a block.
        assert_eq!(gzsetparams(Some(&mut state), 9, Z_FILTERED), ReturnCode::OK);

        assert!(sink.borrow().is_empty());
        assert_eq!(state.level(), 9);
        assert_eq!(state.strategy(), Z_FILTERED);
    }

    #[test]
    fn gzsetparams_records_parameters_before_the_first_write() {
        let sink = RefCell::new(Vec::new());
        let mut state = gz_open_handle(
            Box::new(MemFile::writer(&sink)),
            b"out",
            b"wb",
            GlobalAllocator,
        )
        .unwrap();
        // No write yet, so no buffers and no engine: C skips the whole `if (state->size)` block and
        // records the values for `gz_init` to pick up.
        assert_eq!(state.size(), 0);

        assert_eq!(gzsetparams(Some(&mut state), 1, Z_FILTERED), ReturnCode::OK);

        assert_eq!(state.level(), 1);
        assert_eq!(state.strategy(), Z_FILTERED);
        assert_eq!(state.size(), 0);
        assert!(sink.borrow().is_empty());
    }
    // -------------------------------------------------------------------------
    //  zstrerror(): the message a caller reads back must be the platform's own
    // -------------------------------------------------------------------------

    /// The rendered message is exactly `strerror`'s bytes, with no ` (os error N)` tail.
    ///
    /// C's `gz_error(state, Z_ERRNO, zstrerror())` stores `strerror(errno)` and nothing more
    /// (`gzguts.h` L131-L133), and `gzerror` hands that string straight back to a caller. `std`
    /// composes its `Display` as `error_string(code)` plus that suffix, so the suffix has to come
    /// off again -- this asserts that it does, for a spread of real error numbers.
    ///
    /// Skipped under Miri, which has no platform `strerror` to render through: its shim answers
    /// with text that *already* ends in ` (os error N)`, `std` then appends its own copy, and
    /// [`ErrnoMessage::strip_os_error_suffix`] removes exactly the one `std` added -- correctly,
    /// and leaving the shim's behind. Measured under the interpreter, errno 1 comes back as
    /// `"Operation not permitted (os error 1) (os error 1)"`, so the assertion below would report a
    /// property of the interpreter rather than of this crate. Natively it holds for every errno
    /// tried here, and `read.rs` skips its two `Z_ERRNO` message tests in these same words.
    #[test]
    #[cfg_attr(miri, ignore = "renders a message through the platform's strerror")]
    fn the_errno_message_carries_no_os_error_suffix() {
        // ENOENT, EACCES, EBADF, EINVAL, ENOSPC on unix; whatever the platform maps them to
        // elsewhere. The point is not which text comes back but that no suffix is attached to it.
        for errno in [1, 2, 5, 9, 13, 22, 28] {
            let message = errno_message(Some(GzIoError::new(errno, false)));
            let bytes = message.as_bytes();
            assert!(
                !bytes.is_empty(),
                "errno {errno} must render some text, never an empty message"
            );
            assert!(
                !contains(bytes, OS_ERROR_SUFFIX),
                "errno {errno} rendered {:?}, which still carries std's suffix",
                core::str::from_utf8(bytes)
            );
            // And what remains is the leading part of what std would have produced, so the wording
            // is the platform's rather than something this crate invented.
            let full = alloc::format!("{}", std::io::Error::from_raw_os_error(errno));
            assert!(
                full.as_bytes().starts_with(bytes),
                "errno {errno}: {:?} is not a prefix of {full:?}",
                core::str::from_utf8(bytes)
            );
        }
    }

    /// Whether `needle` occurs anywhere in `haystack`; `[u8]::contains` matches a single byte only.
    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack
            .windows(needle.len())
            .any(|window| window == needle)
    }

    /// With no error object the text comes from the thread's current `errno`, as C's bare
    /// `zstrerror()` does (`gzread.c` L429).
    ///
    /// Skipped under Miri, whose isolation refuses `open` outright. The probe below is a real
    /// syscall made for its side effect alone -- putting a meaningful value in the thread's `errno`
    /// -- and an unsupported operation under interpretation is not a failing test but an abort of
    /// the whole binary, which took the 38 tests of [`mod@write`] and one of this module's own down
    /// with it before this guard existed. `open.rs` guards its file-opening tests the same way.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "filesystem access is unavailable under Miri's isolation"
    )]
    fn the_errno_message_falls_back_to_the_threads_errno() {
        // Provoke a real failure so the thread's errno is set to something meaningful.
        let _ = std::fs::File::open("/nonexistent/zlib-rs/errno/probe");
        let message = errno_message(None);
        assert!(!message.as_bytes().is_empty());
        assert!(!contains(message.as_bytes(), OS_ERROR_SUFFIX));
    }

    /// An error number the platform has no text for still yields a message, never an empty one.
    ///
    /// zlib has the same requirement and the same answer: `gzguts.h` L135 defines `zstrerror()` as
    /// `"stdio error (consult errno)"` when no `strerror` is available.
    #[test]
    fn an_unrenderable_errno_falls_back_to_zlibs_own_wording() {
        let message = errno_message(Some(GzIoError::new(i32::MAX, false)));
        let bytes = message.as_bytes();
        assert!(!bytes.is_empty());
        assert!(!contains(bytes, OS_ERROR_SUFFIX));
        // Either the platform produced something, or the documented fallback was substituted.
        let rendered = alloc::format!("{}", std::io::Error::from_raw_os_error(i32::MAX));
        assert!(
            rendered.as_bytes().starts_with(bytes) || bytes == NO_ERRNO_TEXT,
            "unexpected text {:?}",
            core::str::from_utf8(bytes)
        );
    }

    /// The buffer truncates rather than failing, and never splits a `char`.
    #[test]
    fn the_message_buffer_truncates_instead_of_failing() {
        let mut message = ErrnoMessage::new();
        for _ in 0..(ERRNO_MESSAGE_CAPACITY / 10 + 4) {
            write!(message, "0123456789").unwrap();
        }
        assert_eq!(message.as_bytes().len(), ERRNO_MESSAGE_CAPACITY);
        // A multi-byte character is never split.
        let mut narrow = ErrnoMessage::new();
        for _ in 0..(ERRNO_MESSAGE_CAPACITY) {
            write!(narrow, "é").unwrap();
        }
        assert_eq!(narrow.as_bytes().len() % 2, 0);
        assert!(core::str::from_utf8(narrow.as_bytes()).is_ok());
    }

    /// A message truncated mid-suffix loses the whole partial suffix, not just part of it.
    ///
    /// Unreachable for a real platform string at this capacity, and asserted anyway so that a
    /// dangling ` (os erro` can never reach `gzerror`.
    #[test]
    fn a_truncated_suffix_is_removed_whole() {
        let mut message = ErrnoMessage::new();
        let filler = ERRNO_MESSAGE_CAPACITY - OS_ERROR_SUFFIX.len() - 2;
        for _ in 0..filler {
            write!(message, "x").unwrap();
        }
        // Now write the suffix so that it is cut short by the capacity.
        write!(message, " (os error 12345)").unwrap();
        assert_eq!(message.as_bytes().len(), ERRNO_MESSAGE_CAPACITY);
        message.strip_os_error_suffix(12_345);
        assert_eq!(message.as_bytes().len(), filler);
        assert!(!contains(message.as_bytes(), b" ("));
    }

    /// Text with no suffix at all is left exactly as it was.
    #[test]
    fn text_without_a_suffix_is_untouched() {
        let mut message = ErrnoMessage::new();
        write!(message, "plain failure").unwrap();
        message.strip_os_error_suffix(7);
        assert_eq!(message.as_bytes(), b"plain failure");
    }
}
