//! The `gzFile` layer: reading and writing gzip files through a `stdio`-like handle.
//!
//! This subtree is the port of the four C translation units that together implement zlib's file
//! interface, plus the private header they share:
//!
//! | C source | Lines | Rust module |
//! |---|---|---|
//! | `gzlib.c` | 609 | this file, [`open`] and [`state`] |
//! | `gzread.c` | 668 | [`read`] |
//! | `gzwrite.c` | 700 | [`write`] and [`printf`] |
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
//!    [`gz_error`] (`gzlib.c` L555-L590), [`gz_intmax`] (L596-L609) and [`gt_off`]
//!    (`gzguts.h` L216), together with the six public drivers that C also keeps outside the read and
//!    write files -- [`gzbuffer`], [`gzeof`], [`gzerror`], [`gzclearerr`] -- plus [`gzdirect`]
//!    (`gzread.c` L627-L642) and [`gzsetparams`] (`gzwrite.c` L630-L664), which C places with the
//!    read and write halves but which belong to the shared surface.
//!
//! # The exposed prefix, and the contract every entry point here honours
//!
//! `zlib.h` L1956-L1968 exposes the first three fields of the `gzFile` structure and then defines
//! `gzgetc` as a **macro** over them:
//!
//! ```c
//! #define gzgetc(g) \
//!       ((g)->have ? ((g)->have--, (g)->pos++, *((g)->next)++) : (gzgetc)(g))
//! ```
//!
//! That arithmetic is compiled into *caller* object code, which this port cannot change, so
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
//! `crates/zlib-rs/Cargo.toml` declares exactly four features -- `default`, `rust-api`, `simd` and
//! `std` -- and **there is no `gz` feature on this crate**. The `gz` feature belongs to
//! `crates/libz-rs-sys`, is on by default there, and switches on `zlib-rs/std`. The crate root
//! therefore reaches this subtree through a single `#[cfg(feature = "std")] pub mod gz;`, which is
//! why no per-item gate appears anywhere below: AAP §0.4.2.1 turns `#include <stdio.h>` and
//! `<fcntl.h>` into `use std::{fs::File, io}` behind that one gate, and `cargo build -p zlib-rs
//! --no-default-features` compiles the whole subtree out.
//!
//! ★ The crate root's re-export of [`GzState`] must carry the same `#[cfg(feature = "std")]` as the
//! module declaration. An ungated `pub use gz::GzState;` refers to a module that does not exist in
//! the default `no_std` build and fails it outright.
//!
//! # Notes for `crates/libz-rs-sys/src/gz.rs`
//!
//! The facade author cannot see this file, so the division of labour is recorded here.
//!
//! * `gzFile` is an opaque `*mut GzState`. The facade must **resync the exposed prefix immediately
//!   on entry and refresh it before returning to C** for every one of the 28 `gz*` exports plus
//!   `gzopen64`, `gzseek64`, `gztell64` and `gzoffset64` -- 32 in total -- because the caller's
//!   `gzgetc` macro mutates `have`, `pos` and `next` in caller object code. The six drivers in this
//!   file do it themselves; the facade must still do it around the ones it composes.
//! * The facade owns every raw pointer and every C string: NUL termination, the `gzopen` path, and
//!   the `wchar_t` conversion for `gzopen_w` (AAP §0.6.1 unsafe-site category 5). It also owns
//!   `va_list` handling for `gzprintf`/`gzvprintf` through `core::ffi::VaList`, which is why
//!   [`printf`] exposes a bounded scratch buffer rather than a formatter.
//! * The facade owns the tag-validation guard that rejects a foreign, stale or already-closed
//!   `gzFile` (category 3). Every entry point here additionally re-checks the mode, which is the
//!   weaker half of the same defence.
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
//! * `crates/libz-rs-sys/src/layout_assertions.rs` asserts `sizeof(struct gzFile_s) == 24`;
//!   [`state`] is the core-side half of that one contract and asserts the same numbers at compile
//!   time.
//! * Determinations this subtree makes for the **computed** `zlibCompileFlags`
//!   (`crates/libz-rs-sys/src/util.rs`, AAP §0.6.3.5): bit 16 (`NO_GZCOMPRESS`) is **clear**,
//!   because both the read and the write half are implemented; bits 25, 26 and 27 -- the
//!   `NO_snprintf`/`NO_vsnprintf`/`HAS_vsprintf_void` family -- are **clear**, because [`printf`]
//!   always provides a bounded formatter and there is no void-returning variant.
//!
//! # Tests
//!
//! Each module in this subtree carries its own inline `#[cfg(test)] mod tests`. The integration
//! suite for the layer as a whole lives under `crates/zlib-rs/tests/` and belongs to the crate
//! root's agent; nothing in this subtree creates it.

// Every item in a module named `gz` that ports a C function named `gz_error`, `gz_intmax` or
// `gzbuffer` necessarily repeats the module's name. The C names are the ones a maintainer diffing
// this file against `gzlib.c` will search for, and renaming them to satisfy a lint would cost
// exactly the traceability the port is judged on.
#![allow(clippy::module_name_repetitions)]

// -----------------------------------------------------------------------------
//  The seven sibling modules
// -----------------------------------------------------------------------------

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

// -----------------------------------------------------------------------------
//  The subtree's public surface, gathered into one import point
// -----------------------------------------------------------------------------

// The state, its constants and the traits a caller must implement or inspect. `GzState` keeps its
// name exactly: the crate root re-exports it as `zlib_rs::GzState`, and the facade names it in the
// signature of every `gz*` export.
pub use crate::gz::state::{
    EngineBox, GzEngine, GzFileExposed, GzHandle, GzHow, GzIoError, GzMode, GzSeekFrom, GzState,
    GzStream, ZOff64, COPY, GZBUFSIZE, GZIP, GZ_APPEND, GZ_NONE, GZ_READ, GZ_WRITE, LOOK,
};

// The open family. There is deliberately no `gzopen64`: `gzopen` *is* the 64-bit core, exactly as
// C's `gzopen64` is a one-line forward to the same `gz_open` (`gzlib.c` L300-L312 and L2042), so the
// facade exports both names from this one function.
pub use crate::gz::open::{
    gz_open_handle, gzdopen, gzopen, FileHandle, GzOpenError, GzOpenHandleError, GzOpenSpec,
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

// The positioning family, all four in their `ZOff64` form; the facade narrows each result.
pub use crate::gz::read::{gzoffset64, gzrewind, gzseek64, gztell64};

// The write family.
pub use crate::gz::write::{gzclose_w, gzflush, gzfwrite, gzputc, gzputs, gzwrite};

// The direction-agnostic close.
pub use crate::gz::close::gzclose;

// The formatting interface behind `gzprintf`. The facade formats into the scratch buffer this hands
// it -- that is where its `VaList` handling ends -- and then commits the byte count.
pub use crate::gz::printf::{
    printf_begin, printf_bytes, printf_commit, printf_with, PrintfScratch,
};

// -----------------------------------------------------------------------------
//  Imports
// -----------------------------------------------------------------------------

use core::ffi::{c_int, c_uint};

use crate::allocate::{Allocator, Buffer};
use crate::deflate::{deflate_params, DeflateStream, Flush};
use crate::error::ReturnCode;
use crate::gz::read::gz_look;
use crate::gz::write::{gz_comp, gz_zero};

// -----------------------------------------------------------------------------
//  Literals `gzerror` hands back
// -----------------------------------------------------------------------------

/// The message `gzerror` substitutes for `Z_MEM_ERROR` (`gzlib.c` L526).
///
/// C returns this literal *instead of* a stored message, because [`gz_error`] deliberately declines
/// to allocate one while reporting that allocation failed. The two halves are a pair: remove either
/// and an out-of-memory condition reports no text at all.
const OUT_OF_MEMORY: &[u8] = b"out of memory";

/// The message `gzerror` returns when there is none (`gzlib.c` L527).
///
/// C's `state->msg == NULL ? "" : state->msg` -- an empty string, which is emphatically not the same
/// as the null pointer it returns for an unusable handle.
const NO_MESSAGE: &[u8] = b"";

// -----------------------------------------------------------------------------
//  Integer conversions
// -----------------------------------------------------------------------------

/// Widens a C `unsigned` count into a `usize` index.
///
/// Every target this port supports has `usize` at least as wide as `c_uint`, so the conversion is
/// exact. Saturating rather than panicking on a hypothetical narrower target is the conservative
/// choice: a saturated value can only make a bounds check stricter, never looser. The same helper,
/// with the same reasoning, appears in [`read`], [`write`] and [`state`].
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

// -----------------------------------------------------------------------------
//  gz_error -- gzlib.c L549-L590
// -----------------------------------------------------------------------------

/// Records an error code and, optionally, a message; releases whatever was recorded before.
///
/// The port of `gz_error` (`gzlib.c` L555-L590), whose own comment states the policy:
///
/// > Create an error message in allocated memory and set `state->err` and `state->msg` accordingly.
/// > Free any previous error message already there. Do not try to free or allocate space if the
/// > error is `Z_MEM_ERROR` (out of memory). Simply save the error message as a static string. If
/// > there is an allocation failure constructing the error message, then convert the error to out of
/// > memory.
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
///    bytes because this port carries a length rather than a NUL. A failed allocation becomes
///    `Z_MEM_ERROR` with no message, which is C's L577-L580.
///
/// # The one deviation from C's letter
///
/// Step 1's guard cannot be reproduced, and does not need to be. C skips the `free` because a
/// message recorded for `Z_MEM_ERROR` would be a static string rather than an allocation; this port
/// stores an owned `Vec<u8>`, so releasing it is both correct and required, and the distinction is
/// carried by the [`Option`] itself. [`GzState::clear_msg`] documents the same reasoning from the
/// other side. The guard is unreachable in C in any case: step 4 is what would have stored a static
/// string, and it stores nothing, so `err == Z_MEM_ERROR` together with a non-null `msg` never
/// arises. The observable outcome -- no message, and the new code recorded -- is identical.
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

// -----------------------------------------------------------------------------
//  gz_intmax and GT_OFF -- gzlib.c L592-L609, gzguts.h L212-L216
// -----------------------------------------------------------------------------

/// The largest value a C `int` can hold.
///
/// The port of `gz_intmax` (`gzlib.c` L596-L609), `ZLIB_INTERNAL` in C and listed in the `local:`
/// block of `zlib.map` (L18), hence `pub(crate)`.
///
/// C returns `INT_MAX` when `<limits.h>` has provided it and otherwise computes it by doubling:
///
/// ```c
/// unsigned p = 1, q;
/// do { q = p; p <<= 1; p++; } while (p > q);
/// return q >> 1;
/// ```
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
/// The port of the `GT_OFF` macro (`gzguts.h` L216):
///
/// ```c
/// #define GT_OFF(x) (sizeof(int) == sizeof(z_off64_t) && (x) > gz_intmax())
/// ```
///
/// with the header's own explanation: it "is true if x > maximum `z_off64_t` value -- needed when
/// comparing unsigned to `z_off64_t`, which is signed (possible `z_off64_t` types `off_t`, `off64_t`,
/// and `long` are all signed)".
///
/// **On every target this port supports the first conjunct is false**, because [`ZOff64`] is 8 bytes
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

// -----------------------------------------------------------------------------
//  The public drivers
// -----------------------------------------------------------------------------
//
//  Two conventions hold across all six, both following the precedent the sibling modules set:
//
//  * `Option<&mut GzState>` is this port's spelling of C's `gzFile file`, and [`None`] is
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
/// The port of `gzbuffer` (`gzlib.c` L322-L343), declared at `zlib.h` L1429 and documented at
/// L1430-L1444:
///
/// > Set the internal buffer size used by this library's functions for file to size. The default
/// > buffer size is 8192 bytes. This function must be called after `gzopen()` or `gzdopen()`, and
/// > before any other calls that read or write the file. The buffer memory allocation is always
/// > deferred to the first read or write. Three times that size in buffer space is allocated. [...]
/// > The new buffer size also affects the maximum length for `gzprintf()`.
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

    // L330-L331: `if (state->mode != GZ_READ && state->mode != GZ_WRITE) return -1;`
    if !state.has_valid_mode() {
        state.refresh_exposed();
        return -1;
    }

    // L334-L335: `if (state->size != 0) return -1;`
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

    // L340-L341: `if (size < 8) size = 8;`
    let size = size.max(8);

    // L342-L343: `state->want = size; return 0;`
    state.set_want(size);
    state.refresh_exposed();
    0
}

/// Whether a read went past the end of the input and came up short.
///
/// The port of `gzeof` (`gzlib.c` L498-L510), declared at `zlib.h` L1711 and documented at
/// L1712-L1724:
///
/// > Return true (1) if the end-of-file indicator for file has been set while reading, false (0)
/// > otherwise. Note that the end-of-file indicator is set only if the read tried to go past the end
/// > of the input, but came up short. Therefore, just like `feof()`, `gzeof()` may return false even
/// > if there is no more data to read, in the event that the last read request was for the exact
/// > number of bytes remaining in the input file. [...] If `gzeof()` returns true, then the read
/// > functions will return no more data, unless the end-of-file indicator is reset by `gzclearerr()`
/// > and the input file has grown since the previous end of file was detected.
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

    // L510: `return state->mode == GZ_READ ? state->past : 0;`
    let past = state.is_reading() && state.past();
    state.refresh_exposed();
    c_int::from(past)
}

/// The message for the last error on this stream, and optionally its code.
///
/// The port of `gzerror` (`gzlib.c` L513-L528), declared at `zlib.h` L1775 and documented at
/// L1776-L1790:
///
/// > Return the error message for the last error which occurred on file. If `errnum` is not `NULL`,
/// > `*errnum` is set to zlib error number. If an error occurred in the file system and not in the
/// > compression library, `*errnum` is set to `Z_ERRNO` and the application may consult `errno` to
/// > get the exact error code. [...] The application must not modify the returned string. Future
/// > calls to this function may invalidate the previously returned string. If file is closed, then
/// > the string previously returned by `gzerror` will no longer be available.
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
/// outlive the handle. The facade turns it into the `const char *` C expects and is the layer that
/// must supply the NUL terminator; nothing in this crate stores one.
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
    // L517-L518: `if (file == NULL) return NULL;`
    let state = state?;
    if state.resync_from_exposed().is_err() {
        return None;
    }

    // L520-L521: `if (state->mode != GZ_READ && state->mode != GZ_WRITE) return NULL;`
    if !state.has_valid_mode() {
        state.refresh_exposed();
        return None;
    }

    // L524-L525: `if (errnum != NULL) *errnum = state->err;`
    if let Some(slot) = errnum {
        *slot = state.err();
    }

    // L526-L527, with the mutation finished before the borrow that is handed back begins.
    let mem_error = state.err() == ReturnCode::MEM_ERROR.as_i32();
    state.refresh_exposed();
    if mem_error {
        return Some(OUT_OF_MEMORY);
    }
    Some(state.msg().unwrap_or(NO_MESSAGE))
}

/// Clears the error and end-of-file indicators.
///
/// The port of `gzclearerr` (`gzlib.c` L531-L547), declared at `zlib.h` L1792 and documented at
/// L1793-L1797:
///
/// > Clear the error and end-of-file flags for file. This is analogous to the `clearerr()` function
/// > in `stdio`. This is useful for continuing to read a gzip file that is being written
/// > concurrently.
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

    // L543-L546: `if (state->mode == GZ_READ) { state->eof = 0; state->past = 0; }`
    if state.is_reading() {
        state.set_eof(false);
        state.set_past(false);
    }

    // L547: `gz_error(state, Z_OK, NULL);`
    gz_error(state, ReturnCode::OK, None);
    state.refresh_exposed();
}

/// Whether the stream is being copied through untouched rather than decompressed.
///
/// The port of `gzdirect` (`gzread.c` L627-L642), declared at `zlib.h` L1726 and documented at
/// L1727-L1748:
///
/// > Return true (1) if file is being copied directly while reading, or false (0) if file is a gzip
/// > stream being decompressed. If the input file is empty, `gzdirect()` will return true, since the
/// > input does not contain a gzip stream. If `gzdirect()` is used immediately after `gzopen()` or
/// > `gzdopen()` it will cause buffers to be allocated to allow reading the file to determine if it
/// > is a gzip file. Therefore if `gzbuffer()` is used, it should be called before `gzdirect()`. If
/// > the input is being written concurrently or the device is non-blocking, then `gzdirect()` may
/// > give a different answer once four bytes of input have been accumulated, which is what is needed
/// > to confirm or deny a gzip header. Before this, `gzdirect()` will return true (1).
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
    // L631-L633: `if (file == NULL) return 0;`
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

    // L641: `return state->direct == 1;`
    let direct = state.is_transparent();
    state.refresh_exposed();
    c_int::from(direct)
}

/// `deflateParams(strm, level, strategy)` with the `z_stream` view rebuilt around the layer's cursors.
///
/// The port of `gzwrite.c` L659. C hands the compressor the address of the `z_stream` embedded in
/// `gz_state` (`gzguts.h` L202), so the cursors and the five scalars are already where
/// [`crate::deflate::deflate_params`] expects them; this port keeps the compression state and the
/// caller-visible scalars apart, so the view is assembled here, handed over and read back. The shape
/// is [`write`]'s `deflate_once` deliberately: `deflateParams` can itself call `deflate(strm,
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
        let base: &[u8] = state.input.as_ref().map_or(&[][..], Buffer::as_slice);
        let Some(input) = base.get(..in_end) else {
            return ReturnCode::STREAM_ERROR;
        };
        let Some(output) = state
            .output
            .as_mut()
            .map(Buffer::as_mut_slice)
            .and_then(|slice| slice.get_mut(..out_end))
        else {
            return ReturnCode::STREAM_ERROR;
        };
        let GzEngine::Deflate(engine) = &mut state.strm.engine else {
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
/// The port of `gzsetparams` (`gzwrite.c` L630-L664), declared at `zlib.h` L1445 and documented at
/// L1446-L1454:
///
/// > Dynamically update the compression level and strategy for file. See the description of
/// > `deflateInit2` for the meaning of these parameters. Previously provided data is flushed before
/// > applying the parameter changes. `gzsetparams` returns `Z_OK` if success, `Z_STREAM_ERROR` if the
/// > file was not opened for writing, `Z_ERRNO` if there is an error writing the flushed data, or
/// > `Z_MEM_ERROR` if there is a memory allocation error.
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
    // L636-L638: `if (file == NULL) return Z_STREAM_ERROR;`
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

// -----------------------------------------------------------------------------
//  Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    // The crate denies the panic-prone lints in library code, which is the right policy there and
    // the wrong one here: an assertion that fails panics, and a fixture that cannot index is
    // unreadable. `clippy.toml` already relaxes them inside tests; restating it keeps the intent
    // local and visible. Scoped to this module, which ships in no build.
    #![allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

    use super::{
        gt_off, gz_error, gz_intmax, gzbuffer, gzclearerr, gzdirect, gzeof, gzerror, gzsetparams,
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

    // -------------------------------------------------------------------------
    //  gz_error
    // -------------------------------------------------------------------------

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
        // believes to be a static string, then nulls the slot regardless. This port owns the
        // message, so the slot is emptied and the storage released together -- the observable
        // outcome, an empty slot and the new code recorded, is the same. The combination is
        // unreachable through `gz_error` itself (it stores nothing for `Z_MEM_ERROR`), so it is
        // constructed here directly.
        let mut state = fresh();
        state.try_set_path(b"p").unwrap();
        state.set_err(ReturnCode::MEM_ERROR.as_i32());
        state.set_msg(Some(b"stale".to_vec()));

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

    // -------------------------------------------------------------------------
    //  gz_intmax and gt_off
    // -------------------------------------------------------------------------

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

    // -------------------------------------------------------------------------
    //  gzbuffer
    // -------------------------------------------------------------------------

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

    // -------------------------------------------------------------------------
    //  gzeof
    // -------------------------------------------------------------------------

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

    // -------------------------------------------------------------------------
    //  gzerror and gzclearerr
    // -------------------------------------------------------------------------

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

    // -------------------------------------------------------------------------
    //  gzdirect
    // -------------------------------------------------------------------------

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

    // -------------------------------------------------------------------------
    //  gzsetparams
    // -------------------------------------------------------------------------

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
}
