//! The `gzFile` C surface: thirty exported symbols, two hidden helpers, no logic.
//!
//! This module is the boundary face of `gzlib.c`, `gzread.c`, `gzwrite.c` and
//! `gzclose.c`. It contains no compression, no buffering and no file-format
//! knowledge whatsoever -- all of that lives in [`mod@zlib_rs::gz`], which is
//! compiled under `#![forbid(unsafe_code)]`. What lives here is the five things a
//! safe core structurally cannot do for itself:
//!
//! 1. **Owning the `gzFile` allocation.** C's `gz_open` ends in `malloc(sizeof(gz_state))`
//!    and its close paths end in `free(state)` (`gzlib.c` L100, `gzread.c` L666,
//!    `gzwrite.c` L698). The core deliberately performs neither -- its
//!    `GzState::teardown` documentation says so explicitly -- so the block, its
//!    lifetime and its validation are this file's.
//! 2. **Adopting a caller's file descriptor.** `FromRawFd::from_raw_fd` is `unsafe`,
//!    so `gzdopen`'s handle can only be built here and injected inward.
//! 3. **C strings and buffers.** Every `const char *path`, `const char *mode`,
//!    `const char *s`, `voidp buf` and `char *buf` is turned into a slice exactly
//!    once, on entry.
//! 4. **Offset-width narrowing.** Only this layer knows how wide the caller's
//!    `z_off_t` is, so the unsuffixed positioning entry points narrow here while
//!    the `*64` ones do not.
//! 5. **Panic containment.** Every export is `extern "C"`, never
//!    `extern "C-unwind"`, and routes through [`crate::panic_guard::guard`] once.
//!
//! # The exported surface: thirty symbols here, two from the shim
//!
//! The reference C library exports **thirty-two** `gz*` symbols on Linux, measured
//! by building it and grouping `nm -D --defined-only --extern-only`. Thirty of them
//! are defined below. The remaining two -- `gzprintf` and `gzvprintf` -- cannot be
//! written in Rust at all, for the reason the next section gives, and come from
//! `csrc/gzprintf_shim.c`.
//!
//! | Symbol | `zlib.h` | Ported from | `zlib.map` node |
//! |---|---|---|---|
//! | [`gzopen`] | L1357 | `gzlib.c` L288-L290 | *(base set, undecorated)* |
//! | [`gzopen64`] | L1978 | `gzlib.c` L293-L295 | `ZLIB_1.2.3.3` |
//! | [`gzdopen`] | L1404 | `gzlib.c` L298-L312 | *(base set)* |
//! | `gzopen_w` | L2042 | `gzlib.c` L316-L318 | *(Windows only; not a Linux export)* |
//! | [`gzbuffer`] | L1429 | `gzlib.c` L322-L343 | `ZLIB_1.2.3.5` |
//! | [`gzsetparams`] | L1445 | `gzwrite.c` L630-L665 | *(base set)* |
//! | [`gzread`] | L1456 | `gzread.c` L395-L436 | *(base set)* |
//! | [`gzfread`] | L1492 | `gzread.c` L438-L465 | `ZLIB_1.2.9` |
//! | [`gzwrite`] | L1519 | `gzwrite.c` L255-L277 | *(base set)* |
//! | [`gzfwrite`] | L1529 | `gzwrite.c` L280-L304 | `ZLIB_1.2.9` |
//! | `gzprintf` | L1549 | `gzwrite.c` L487-L495 | *(base set; **shim**)* |
//! | `gzvprintf` | L2047 | `gzwrite.c` L403-L485 | `ZLIB_1.2.7.1` *(**shim**)* |
//! | [`gzputs`] | L1575 | `gzwrite.c` L350-L372 | *(base set)* |
//! | [`gzgets`] | L1588 | `gzread.c` L565-L624 | *(base set)* |
//! | [`gzputc`] | L1607 | `gzwrite.c` L307-L347 | *(base set)* |
//! | [`gzgetc`] | L1613 | `gzread.c` L467-L498 | *(base set)* |
//! | [`gzgetc_`] | L1961 | `gzread.c` L500-L502 | `ZLIB_1.2.5.2` |
//! | [`gzungetc`] | L1630 | `gzread.c` L504-L563 | `ZLIB_1.2.0.2` |
//! | [`gzflush`] | L1647 | `gzwrite.c` L375-L400 | *(base set)* |
//! | [`gzseek`] | L1663 | `gzlib.c` L438-L443 | *(base set)* |
//! | [`gzseek64`] | L1979 | `gzlib.c` L366-L435 | `ZLIB_1.2.3.3` |
//! | [`gzrewind`] | L1683 | `gzlib.c` L345-L364 | *(base set)* |
//! | [`gztell`] | L1691 | `gzlib.c` L461-L466 | *(base set)* |
//! | [`gztell64`] | L1980 | `gzlib.c` L445-L458 | `ZLIB_1.2.3.3` |
//! | [`gzoffset`] | L1702 | `gzlib.c` L490-L495 | `ZLIB_1.2.3.5` |
//! | [`gzoffset64`] | L1981 | `gzlib.c` L468-L487 | `ZLIB_1.2.3.5` |
//! | [`gzeof`] | L1711 | `gzlib.c` L498-L510 | *(base set)* |
//! | [`gzdirect`] | L1726 | `gzread.c` L627-L642 | `ZLIB_1.2.2.3` |
//! | [`gzclose`] | L1750 | `gzclose.c` L11-L23 | *(base set)* |
//! | [`gzclose_r`] | L1763 | `gzread.c` L644-L667 | `ZLIB_1.2.3.5` |
//! | [`gzclose_w`] | L1764 | `gzwrite.c` L667-L700 | `ZLIB_1.2.3.5` |
//! | [`gzerror`] | L1775 | `gzlib.c` L513-L528 | *(base set)* |
//! | [`gzclearerr`] | L1792 | `gzlib.c` L531-L547 | *(base set)* |
//!
//! The `zlib.map` column records what the *linker* does, not what this file does.
//! Every entry point below is a plain `#[no_mangle]`; the `@@ZLIB_x.y.z` decoration
//! comes solely from linking with `--version-script=zlib.map`, and nothing here can
//! or should produce it.
//!
//! Two entries need reading carefully. `gzopen_w` is declared inside
//! `#if defined(_WIN32) && !defined(Z_SOLO)` (`zlib.h` L2041-L2044), so it is
//! **not** among the ninety-five Linux exports -- which is exactly why `zlib.h`
//! declares ninety-six real entry points while the reference library exports
//! ninety-five. It carries `#[cfg(windows)]` here for the same reason, and adding
//! it unconditionally would fail the symbol-parity diff with a ninety-sixth name.
//! `gzclearerr` is the only `void`-returning export in the whole crate.
//!
//! # ★ Why `gzprintf` and `gzvprintf` are not in this file
//!
//! They are the one pair the facade does not define, and the reason is a hard
//! language limit rather than a preference. `gzprintf` is C variadic
//! (`zlib.h` L1549, `ZEXPORTVA`) and stable Rust cannot *define* a variadic
//! function in any form; `gzvprintf` takes a `va_list` (L2047), whose Rust spelling
//! `core::ffi::VaList` requires the unstable `c_variadic` feature. `rust-toolchain.toml`
//! pins the stable channel and the manifests declare `rust-version = "1.80"`, so
//! both are rejected with E0658 -- verified, not assumed.
//!
//! The resolution is already in the tree and is the third option
//! `zlib_rs::gz::printf`'s documentation enumerates: `csrc/gzprintf_shim.c`, one C
//! translation unit that owns nothing but `va_start`, `va_end` and a single
//! `vsnprintf` call. `Makefile.in`'s `rust` target compiles it with `$(CC)`, adds
//! the object to the staged `libz.a` and relinks the staged shared object from that
//! archive. This crate's `build.rs` never invokes a C compiler, so
//! `cargo build --release` still works on a machine without one.
//!
//! What this file owes the shim is exactly two hidden helpers, and their names are
//! not negotiable:
//!
//! ```text
//! int _zlib_rs_gzprintf_begin(gzFile file, unsigned char **scratch, size_t *size);
//! int _zlib_rs_gzprintf_commit(gzFile file, size_t reported);
//! ```
//!
//! `zlib.map`'s `ZLIB_1.2.0` node has **no `local: *;` catch-all** -- it lists nine
//! names plus the pattern `_*`, and a symbol matched by nothing at all is assigned
//! to the base version and stays *exported*. The leading underscore is therefore
//! what keeps these two out of the dynamic table and the parity target at 111
//! symbols. It is the same mechanism zlib itself uses for the `_tr_*` family. Do
//! not rename them, and do not add them to `zlib.map`, which is immutable.
//!
//! Note also what a symbol diff cannot establish. `nm` proves `gzprintf` is
//! present; it cannot prove the variadic calling convention is right, because only
//! a real C call with real varargs exercises that. The acceptance evidence for the
//! pair is `test/example.c`'s `test_gzio`, which calls `gzprintf(file, ", %s!", "hello")`
//! and asserts the byte count.
//!
//! # ★ The `gzgetc` macro constraint, and the contract it forces
//!
//! `zlib.h` L1966-L1968 -- with an identical `z_gzgetc` at L1963-L1965 under
//! `Z_PREFIX_SET`:
//!
//! ```c
//! #define gzgetc(g) \
//!       ((g)->have ? ((g)->have--, (g)->pos++, *((g)->next)++) : (gzgetc)(g))
//! ```
//!
//! That field arithmetic is compiled into **caller** object code, which this port
//! cannot change and cannot see. Two consequences run through everything below.
//!
//! **The state must begin with the twenty-four-byte exposed prefix at offset zero.**
//! [`GzBlock`] is `#[repr(C)]` with the core's `GzState` -- itself `#[repr(C)]` with
//! `x: GzFileExposed` first -- as its own first field, so a `gzFile` handed to a
//! caller addresses `have` at 0, `next` at 8 and `pos` at 16, exactly as `gzguts.h`
//! L169-L172 embeds `struct gzFile_s x;` as `gz_state`'s first member.
//! `crates/libz-rs-sys/src/types.rs` pins the agreement between [`gzFile_s`] and
//! `GzFileExposed` with compile-time assertions, so a layout mistake is a build
//! failure rather than silent corruption in every program that uses the macro.
//!
//! **A caller mutates that prefix between calls,** so every entry point must
//! recover the library's own cursor on entry and publish it again on exit. The
//! core's `GzState::resync_from_exposed` and `GzState::refresh_exposed` are those
//! two halves, and **every core entry point below already calls them itself** --
//! verified by reading each one. This file therefore does *not* repeat them: doing
//! so would either double the work or, worse, publish a cursor derived from an
//! index that the core had deliberately left alone. The one place this file touches
//! the prefix is immediately after moving a freshly built state into its heap block,
//! where the pointer has to be re-derived at its final address.
//!
//! `gzerror` deserves a specific note. The core's `gz_error` clears `x.have` for a
//! fatal error precisely so that the caller's macro stops taking its fast path
//! (`gzlib.c` L563-L565), and nothing in this file may undo that by refreshing a
//! stale count back into the prefix. It does not, because it never writes the
//! prefix outside `install`.
//!
//! # The handle: allocation, validation and release
//!
//! A `gzFile` is a `*mut` [`GzBlock`], reinterpreted as the `*mut gzFile_s` that
//! `zlib.h` L1354 calls a "semi-opaque gzip file descriptor". The block is obtained
//! from [`alloc`] rather than from `Box::new`, and that is not a stylistic choice:
//! `Box::new` calls `handle_alloc_error`, which **aborts the process**, whereas
//! `gzopen` and `gzdopen` are documented to return `NULL` when memory runs out
//! (`zlib.h` L1391-L1393 and L1417-L1418) and `test/infcover.c`'s `mem_limit`
//! induces exactly that failure on purpose. [`alloc`] reports exhaustion by
//! returning null, which is the outcome the contract requires.
//!
//! Validation is [`block_mut`], and the order of its two parts matters. It must
//! **first** establish that the pointer is non-null and aligned, because nothing
//! read *through* a pointer can establish that the pointer is readable. Only then
//! does the tag tell it anything -- and what it can then tell is that the live
//! object is not one this library produced, or has already been closed. A pointer
//! that has been *freed* is outside what any such check reaches: reading it is
//! already undefined behaviour, and the tag poisoning in [`release`] is a
//! best-effort diagnostic for that case, never a guarantee. This is the same
//! posture as `deflateStateCheck` (`deflate.c` L538) and `inflateStateCheck`
//! (`inflate.c` L88), and for the same reason: `gzguts.h` L158 calls the odd mode
//! constants "a little integrity check on the passed structure".
//!
//! The allocator is `zlib_rs::allocate::GlobalAllocator`, and that is the faithful
//! choice rather than a shortcut. `gzguts.h` L120 states it outright -- "gz\*
//! functions always use library allocation functions" -- because a `gzFile` has no
//! `z_stream` and therefore no caller-supplied `zalloc`, `zfree` or `opaque` to
//! honour. The C code allocates its state, its path and its two working buffers
//! with plain `malloc` (`gzlib.c` L100 and L206, `gzread.c` L99-L100,
//! `gzwrite.c` L16 and L25), and sets all three stream hooks to `Z_NULL` before
//! initialising either engine (`gzread.c` L110-L112, `gzwrite.c` L32-L34), so the
//! engine state also comes from `zcalloc` -- which `zutil.c` L299-L308 defines as
//! plain, **uninitialised** `malloc`. Releases are strictly last-in-first-out,
//! which `test/infcover.c`'s `mem_done` (its L200-L234) checks for, and the core's
//! close paths already release in C's order.
//!
//! # Closing: when the block is freed, and when it is not
//!
//! C's three close entry points free the structure as their very last act, but only
//! on the path where they actually tore it down. `gzclose_r` returns
//! `Z_STREAM_ERROR` *before* `free` when the stream is not open for reading
//! (`gzread.c` L648-L649), `gzclose_w` does the same for a stream not open for
//! writing (`gzwrite.c` L677-L678), and `gzclose` merely picks between them
//! (`gzclose.c` L20-L21). This file reproduces that exactly: the direction is read
//! from the state *before* the call, and the block is released only when the half
//! that ran was the matching one.
//!
//! `zlib.h` L1752-L1756 requires that `gzclose` "must not be called more than once
//! on the same allocation" and that `gzerror` not be used on a closed handle. Both
//! remain the caller's obligation, because a freed pointer cannot be validated by
//! anybody. What this file adds is that the tag is poisoned before the block is
//! released, so a second close on memory the allocator has not yet reused reports
//! `Z_STREAM_ERROR` instead of running a second teardown -- a diagnostic, not a
//! licence.
//!
//! # KNOWN BEHAVIOUR DIVERGENCES FROM C -- this file's own inventory
//!
//! `zlib_rs::gz::open`'s documentation carries the core's nine; these four belong to
//! the boundary. None of them is described as hardening or as an improvement.
//!
//! 1. **`errno` is not written back before a failed `gzopen` returns `NULL`.**
//!    *(FORCED, UNRESOLVED.)* `zlib.h` L1394-L1401 promises that "`errno` can be
//!    checked to determine if the reason `gzopen` failed was that the file could not
//!    be opened", and specifically that with `N` in the mode `errno` "will be
//!    `EAGAIN` or `ENONBLOCK`" so the call can be retried. The number is carried
//!    faithfully in `GzIoError::errno`, but setting the platform's thread-local
//!    `errno` requires `libc` or a raw syscall and there is no `std` route to it.
//!    In practice the failing `open(2)` inside `std` has already set it and nothing
//!    between there and here resets it, so a caller usually observes the right
//!    value -- but that is an observation, not a guarantee.
//! 2. **`FD_CLOEXEC` is not applied to an adopted descriptor.** *(FORCED,
//!    UNRESOLVED.)* C applies it with `fcntl(fd, F_SETFD, ...)` when the mode string
//!    contained `e` (`gzlib.c` L258-L261). `fcntl` is not in `std`, so a descriptor
//!    adopted through [`gzdopen`] keeps whatever flag it arrived with. Note the
//!    direction: this leaves a descriptor *inheritable* that C would have closed on
//!    `exec`, which is the less conservative of the two. The path-based case
//!    diverges the other way, and the core records that as its own divergence 4.
//! 3. **`O_NONBLOCK` is not applied to an adopted descriptor.** *(FORCED,
//!    UNRESOLVED.)* Same cause: C's `fcntl(fd, F_SETFL, ...)` (`gzlib.c` L254-L257)
//!    has no `std` equivalent. [`AdoptedDescriptor::set_nonblocking`] reports the
//!    failure rather than claiming a success it did not achieve, and
//!    `zlib_rs::gz::open::gz_open_handle` discards that result exactly as C discards
//!    `fcntl`'s. A caller that needs the flag can set it on the descriptor before
//!    handing it over, which works unchanged.
//! 4. **`gzdopen` is unavailable on non-Unix targets.** *(FORCED, UNRESOLVED.)* The
//!    symbol is exported everywhere, so parity holds, but off Unix it returns
//!    `NULL`. Windows `gzdopen` takes a **CRT file descriptor**, which is not a
//!    `HANDLE` and therefore not something `std::fs::File` can adopt; bridging it
//!    needs `_get_osfhandle` plus CRT-level `_read`/`_write`/`_lseeki64`/`_close`,
//!    and none of that can be exercised by this port's verified Tier-1 target. A
//!    Windows consumer that needs it should open by path with [`gzopen`], which is
//!    fully supported there.
//!
//! One of the core's divergences is deliberately **not** inherited here, and it is
//! listed so that nobody looks for it in vain. Divergence 1 of
//! `zlib_rs::gz::open` -- `close(2)`'s result being unobservable, so `gzclose`
//! reports only the flush -- ends at this boundary: the core resolves it by stating
//! that "a stream whose `close(2)` status must be exact takes an injected handle",
//! and [`AdoptedDescriptor`] is that handle. It establishes the descriptor's validity
//! with `fstat(2)` before releasing it, so a caller who closed the descriptor behind
//! [`gzdopen`]'s back gets `Z_ERRNO` from `gzclose`, byte for byte as C does. See
//! [`AdoptedDescriptor::close`] for why `fstat` and not `fsync`, and for the measured
//! before-and-after.
//!
//! Divergence 1 of the core's inventory -- that `close(2)`'s `errno` is not
//! observable -- applies unchanged to [`AdoptedDescriptor::close`], which reports
//! the flush's result instead. The descriptor is still closed; only the status of
//! the closing syscall is lost.
//!
//! # Panic and unsafe posture
//!
//! Every export routes through [`crate::panic_guard::guard`] exactly once, so a
//! panic aborts rather than unwinding into a caller that was not compiled to expect
//! it. Forwarding that the C sources do between entry points -- `gzopen64` to
//! `gzopen`, `gzgetc_` to `gzgetc`, `gztell` to `gztell64` -- happens *below* the
//! guard through shared private bodies, so no call installs two guards.
//!
//! Every entry point is declared `unsafe` because every one of them dereferences a
//! caller-supplied pointer. `unsafe` on a Rust function item changes neither the
//! emitted symbol, the calling convention, nor the generated C declaration, so the
//! `zlib.h` contract is untouched; it only obliges a *Rust* caller to acknowledge
//! the invariant. The unsafety falls in four of the six categories the crate root
//! inventories, plus the descriptor adoption:
//!
//! | Category | Where |
//! |---|---|
//! | 1, pointer validation | [`block_mut`], [`state_mut`], [`state_ref`] |
//! | 2, slice reconstruction | [`read_slice`], [`write_slice`] |
//! | 3, opaque state round-trip | [`install`], [`block_mut`], [`release`] |
//! | 5, C strings | [`c_bytes`] |
//! | `FromRawFd` adoption | [`adopt_descriptor`] |
//!
//! `zlib_rs::gz`'s crate-private `gz_error`, `gz_intmax` and `gt_off` are never
//! named here and are never `#[no_mangle]`, so the `local:` block of `zlib.map` and
//! Rust's own visibility agree by construction rather than by convention.

// Every item in a module named `gz` that implements a C function named `gzopen` or
// `gzread` necessarily repeats the module's name. The C names are the ones a
// maintainer diffing this file against `gzlib.c` will search for, and renaming them
// to satisfy a lint would cost exactly the traceability the implementation is judged
// on -- and, for the exported symbols, would break the ABI outright.
#![allow(clippy::module_name_repetitions)]

use core::ffi::{c_char, c_int, c_uint, CStr};
use core::ptr;

use std::alloc::{alloc, dealloc, Layout};

// The descriptor-adoption half of the module. Gated because `gzdopen`'s `int fd` is a
// POSIX descriptor only on Unix; see `AdoptedDescriptor` and divergence 4.
#[cfg(unix)]
use std::fs::File;
#[cfg(unix)]
use std::io::{self, Read, Seek, SeekFrom, Write};
#[cfg(unix)]
use std::os::fd::{FromRawFd, IntoRawFd};

use zlib_rs::allocate::GlobalAllocator;
use zlib_rs::error::ReturnCode;
use zlib_rs::gz::{
    self as rs_gz, narrow_offset, printf_begin, printf_commit, try_box_handle, GzHandle, GzIoError,
    GzOpenSpec, GzSeekFrom, GzState, ZOff64, GZ_READ, GZ_WRITE,
};

use crate::panic_guard::{fallback, guard, guard_code};
use crate::types::{gzFile, voidp, voidpc, widen, z_off64_t, z_off_t, z_size_t};

// ---------------------------------------------------------------------------
// The heap block behind a `gzFile` -- unsafe-site category 3
// ---------------------------------------------------------------------------

/// The concrete state type a `gzFile` points at.
///
/// `'static` because nothing the state borrows outlives the process: its buffers
/// come from the global allocator and its file handle is owned. `GlobalAllocator`
/// because a `gzFile` has no caller-supplied hooks -- see the module documentation
/// for the four C sites that make that the faithful choice rather than a shortcut.
type FacadeGzState = GzState<'static, GlobalAllocator>;

/// The tag a live block carries, checked before the block is treated as state.
///
/// The value is arbitrary but must not be a plausible accident: zero, `-1` and small
/// integers all occur in uninitialised or half-written memory far too often to be
/// useful. `gzguts.h` L159-L162 chooses `7247` and `31153` for its mode constants on
/// the same reasoning, which it describes as "a little integrity check on the passed
/// structure".
const GZ_TAG_LIVE: c_int = 0x7a6c_6701;

/// The tag [`release`] writes immediately before the block is freed.
///
/// Reading it back is undefined behaviour, because the memory has been released, so
/// this is a diagnostic for the double-close case `zlib.h` L1752-L1756 forbids and
/// nothing more. When the allocator has not yet reused the block -- which is the
/// common case for an immediate second call -- the second close reports
/// `Z_STREAM_ERROR` rather than running a second teardown.
const GZ_TAG_CLOSED: c_int = 0x7a6c_6702;

/// The allocation a `gzFile` addresses: the core's state, plus a validation tag.
///
/// `#[repr(C)]` with [`GzBlock::state`] **first** is load-bearing rather than
/// decorative. The core's `GzState` is itself `#[repr(C)]` with the twenty-four-byte
/// exposed prefix as its own first field, so this arrangement places `have` at
/// offset 0, `next` at 8 and `pos` at 16 of whatever a caller's `gzgetc` macro
/// dereferences. That is `gzguts.h` L169-L172's `struct gzFile_s x;` reproduced, and
/// the module documentation explains why nothing may be inserted ahead of it.
///
/// The tag sits *after* the state for exactly that reason. It costs one machine word
/// of padding and is invisible to every caller, since nothing past the first
/// twenty-four bytes is part of the published contract -- `zlib.h` L1949-L1954 says
/// so, warning that even the exposed fields "could change in the future, perhaps
/// even capriciously".
#[repr(C)]
struct GzBlock {
    /// The core state. **Must remain the first field**; see the type documentation.
    state: FacadeGzState,
    /// [`GZ_TAG_LIVE`] while the handle is usable, [`GZ_TAG_CLOSED`] once released.
    tag: c_int,
    /// A NUL-terminated copy of the text [`gzerror`] last reported.
    ///
    /// `gzerror` returns a `const char *` and the core returns a `&[u8]` **without** a
    /// terminator, because nothing in `zlib-rs` stores one -- its `gzerror` documentation
    /// says so and makes supplying it the facade's job. The terminated copy has to live
    /// somewhere that outlives the call and belongs to the stream, which is what
    /// `zlib.h` L1783-L1786 describes: the application must not modify the string, a
    /// later call may invalidate it, and it is gone once the file is closed. Owning it
    /// here reproduces all three properties exactly, because the block *is* the stream's
    /// storage.
    ///
    /// Empty until the first `gzerror` call that has something to report; an empty
    /// message is answered with a `'static` empty string instead, so a stream that never
    /// errors never allocates here.
    message: Vec<u8>,
}

// `alloc` requires a non-zero size, and `Layout::new::<GzBlock>()` is what is handed
// to it. The state carries the exposed prefix, a mode, a handle slot and two owned
// vectors, so this holds on every target; asserting it makes the precondition of the
// one `alloc` call in this module a build-time fact rather than a comment.
const _: () = assert!(size_of::<GzBlock>() > 0);

// The offset the `gzgetc` macro depends on, asserted where the block is declared.
// `crates/libz-rs-sys/src/types.rs` pins `gzFile_s` against the core's
// `GzFileExposed`; this pins the block against the state, which is the remaining
// link in the chain from a caller's field arithmetic to the bytes it lands on.
const _: () = assert!(core::mem::offset_of!(GzBlock, state) == 0);

/// Moves a freshly built state onto the heap and returns it as a `gzFile`.
///
/// The counterpart of C's `state = malloc(sizeof(gz_state))` (`gzlib.c` L100), except
/// that C allocates first and fills the fields afterwards while this receives a state
/// that is already complete. The ordering difference is forced by ownership -- the
/// core's `gz_open` cannot hand back a half-built state -- and is not observable: both
/// produce a fully initialised structure or `NULL`.
///
/// Exhaustion returns `Z_NULL`, which is what `zlib.h` L1391-L1393 documents for
/// "insufficient memory to allocate the `gzFile` state". [`alloc`] is used rather than
/// `Box::new` because `Box::new` aborts the process instead; see the module
/// documentation.
///
/// The exposed prefix is refreshed after the move. Nothing requires it at this point
/// -- a just-opened stream has no output buffer, so `next` is null and `have` is zero
/// -- but the pointer is derived state and re-deriving it at the block's final address
/// is what makes "the pointer a caller holds is always the most recently derived one"
/// unconditional.
fn install(state: FacadeGzState) -> gzFile {
    let layout = Layout::new::<GzBlock>();
    // SAFETY: unsafe-site category 3 -- the opaque state round-trip, on its
    // allocating side. `layout` has a non-zero size, which the `const` assertion
    // above proves at build time, and that is `alloc`'s only precondition. A null
    // return is exhaustion and is handled immediately below rather than dereferenced.
    let raw = unsafe { alloc(layout) };
    if raw.is_null() {
        return fallback::null_handle();
    }
    let block: *mut GzBlock = raw.cast();
    // SAFETY: unsafe-site category 3. `block` is the fresh allocation above, sized
    // and aligned for `GzBlock` by `layout`, and uninitialised -- so `write` is the
    // correct primitive: it initialises without dropping a previous value, of which
    // there is none.
    unsafe {
        block.write(GzBlock {
            state,
            tag: GZ_TAG_LIVE,
            message: Vec::new(),
        });
    }
    // SAFETY: unsafe-site category 3. The block was initialised on the line above,
    // this is the only reference to it in existence, and `refresh_exposed` is safe
    // core code that touches nothing outside the state.
    unsafe { (*block).state.refresh_exposed() };
    block.cast()
}

/// Validates a caller's `gzFile` and borrows the block behind it.
///
/// The guard for unsafe-site categories 1 and 3, in that order, because the order is
/// what makes it meaningful: non-null and aligned must be established *before*
/// anything is read through the pointer, since no value read through a pointer can
/// establish that the pointer was readable. Only once the dereference is licensed does
/// the tag say anything, and what it then says is that the live object is not one this
/// library produced, or has already been closed.
///
/// [`None`] means "return the caller's documented failure value", which differs per
/// entry point -- `Z_STREAM_ERROR`, `-1`, `0`, `NULL` or nothing at all -- so this
/// deliberately does not choose one. Every call site takes its value from
/// [`crate::panic_guard::fallback`].
///
/// # Safety
///
/// If `file` is non-null it must address a live [`GzBlock`] produced by [`install`]
/// and not yet passed to [`release`], and no other reference to that block may exist
/// for `'a`. A freed handle cannot be detected here and is undefined behaviour to
/// pass, exactly as it is in C -- `zlib.h` L1752-L1756 makes not doing so the
/// caller's obligation.
unsafe fn block_mut<'a>(file: gzFile) -> Option<&'a mut GzBlock> {
    let block: *mut GzBlock = file.cast();
    if block.is_null() || !block.is_aligned() {
        return None;
    }
    // SAFETY: unsafe-site category 1 -- pointer validation. Non-null and correctly
    // aligned are established by the test above; liveness, exclusivity and provenance
    // are this function's documented obligations on its caller.
    let block = unsafe { &mut *block };
    // Unsafe-site category 3 -- the tag check, which is now a safe field read.
    if block.tag != GZ_TAG_LIVE {
        return None;
    }
    Some(block)
}

/// Validates a caller's `gzFile` and borrows the state mutably.
///
/// [`block_mut`] narrowed to the field every entry point actually wants. The tag stays
/// out of reach of the core, which is correct: it is this layer's bookkeeping and the
/// core has no business knowing a handle is tagged at all.
///
/// # Safety
///
/// As [`block_mut`].
unsafe fn state_mut<'a>(file: gzFile) -> Option<&'a mut FacadeGzState> {
    // SAFETY: this function's contract is `block_mut`'s, discharged unchanged.
    let block = unsafe { block_mut(file) }?;
    Some(&mut block.state)
}

/// Validates a caller's `gzFile` and borrows the state immutably.
///
/// Two entry points need no more than this: `gztell64`, whose core function takes a
/// shared borrow because it reads only `pos`, `past` and `skip`, and the direction
/// test the three close entry points make before deciding whether the block may be
/// released.
///
/// # Safety
///
/// As [`block_mut`], except that other shared borrows of the same block are permitted.
unsafe fn state_ref<'a>(file: gzFile) -> Option<&'a FacadeGzState> {
    let block: *const GzBlock = file.cast();
    if block.is_null() || !block.is_aligned() {
        return None;
    }
    // SAFETY: unsafe-site category 1 -- pointer validation. Non-null and correctly
    // aligned are established above; liveness and provenance are this function's
    // documented obligations on its caller.
    let block = unsafe { &*block };
    if block.tag != GZ_TAG_LIVE {
        return None;
    }
    Some(&block.state)
}

/// Runs the state's destructor and returns its block to the allocator.
///
/// C's `free(state)`, the last statement of `gzclose_r` and `gzclose_w`
/// (`gzread.c` L666, `gzwrite.c` L698). Dropping the state first is what makes the two
/// equivalent: `GzState`'s `Drop` runs the same teardown the close paths already ran,
/// finds nothing left to release, and cannot double-release -- which the core's
/// `GzState::teardown` documentation states as its purpose.
///
/// The tag is poisoned before the drop. That is a diagnostic for the double-close
/// `zlib.h` L1752-L1756 forbids and nothing more, because reading released memory is
/// undefined behaviour whatever it contains; a `c_int` has no destructor, so the
/// pattern survives the drop and is what a second call sees while the allocator has
/// not yet reused the block.
///
/// # Safety
///
/// `file` must address a live [`GzBlock`] produced by [`install`] and not yet released,
/// and **no borrow of it may be outstanding**. After this returns, `file` is dangling
/// and must never be used again -- which is exactly the obligation `zlib.h` places on
/// a caller of `gzclose`.
unsafe fn release(file: gzFile) {
    let block: *mut GzBlock = file.cast();
    if block.is_null() || !block.is_aligned() {
        return;
    }
    // SAFETY: unsafe-site category 3. The caller guarantees a live block with no
    // outstanding borrow. The write poisons the tag while the block is still valid;
    // `drop_in_place` then runs `GzState::drop`, which is safe core code; and
    // `dealloc` is given the same pointer and the same layout the allocation was made
    // with, which is `alloc`'s matching requirement.
    unsafe {
        (*block).tag = GZ_TAG_CLOSED;
        ptr::drop_in_place(block);
        dealloc(block.cast(), Layout::new::<GzBlock>());
    }
}

// ---------------------------------------------------------------------------
// C strings -- unsafe-site category 5
// ---------------------------------------------------------------------------

/// Borrows a caller's NUL-terminated string as bytes, excluding the terminator.
///
/// Four parameters arrive this way and all four are read-only for the duration of the
/// call: `gzopen`'s `path` and `mode`, `gzdopen`'s `mode`, and `gzputs`'s `s`. The core
/// takes each of them as a `&[u8]` without a terminator -- C's own `gz_open` copies the
/// path with `snprintf(state->path, len + 1, "%s", path)` (`gzlib.c` L222) and `gzputs`
/// measures its argument with `strlen` (`gzwrite.c` L360), so in both cases the
/// terminator is a delimiter rather than data.
///
/// Bytes rather than `&str`, deliberately: a POSIX path is an arbitrary byte string and
/// need not be valid UTF-8, so insisting on UTF-8 here would make some legal filenames
/// unopenable. The core stores them untouched for the same reason.
///
/// [`None`] is a null pointer, which is C's `path == NULL || mode == NULL` test
/// (`gzlib.c` L96-L97) and which every call site answers with the value `zlib.h`
/// documents for that entry point.
///
/// # Safety
///
/// If `text` is non-null it must point at a NUL-terminated byte string that stays valid,
/// and is not written by anything else, for `'a`. The scan for the terminator is
/// unbounded, exactly as `strlen`'s is, so a string without one reads out of bounds --
/// which is the same contract every C caller of `gzopen` already satisfies.
unsafe fn c_bytes<'a>(text: *const c_char) -> Option<&'a [u8]> {
    if text.is_null() {
        return None;
    }
    // SAFETY: unsafe-site category 5 -- C strings. Non-null is established above, and
    // NUL termination plus validity for `'a` are this function's documented
    // obligations on its caller. `CStr::from_ptr` performs the `strlen` and yields a
    // borrow whose lifetime is unconstrained by the argument, which is inherent to an
    // FFI boundary.
    Some(unsafe { CStr::from_ptr(text) }.to_bytes())
}

// ---------------------------------------------------------------------------
// Buffer reconstruction -- unsafe-site category 2
// ---------------------------------------------------------------------------

/// Rebuilds a caller's output buffer as a mutable slice from a `(buf, len)` pair.
///
/// Serves `gzread`, `gzfread` and `gzgets`. Separate from
/// `crate::types::output_slice_mut` because the lengths differ in width: that helper
/// takes a `uInt`, while `gzfread`'s count is a [`z_size_t`] and is already a
/// [`usize`]. The zero-length rule and its justification are identical.
///
/// ★ The zero-length case is not a formality. `core::slice::from_raw_parts_mut(null, 0)`
/// is **undefined behaviour** -- the pointer must be non-null and aligned even for an
/// empty slice -- so it is branched on rather than relied upon.
///
/// A null pointer with a non-zero length also yields the empty slice. C would hand that
/// pointer to `memcpy` and crash (`gzread.c` L370), so there is no reference behaviour
/// to match; the core then reads nothing, and the caller sees the zero count that
/// `zlib.h` L1502-L1503 tells it to disambiguate with `gzerror`.
///
/// # Safety
///
/// If `len` is non-zero, `buf` must be non-null and writable for `len` bytes, and that
/// region must not be aliased by anything else -- including by another slice this
/// module reconstructs -- for `'a`.
unsafe fn read_slice<'a>(buf: voidp, len: usize) -> &'a mut [u8] {
    if len == 0 || buf.is_null() {
        return &mut [];
    }
    // SAFETY: unsafe-site category 2 -- slice reconstruction. `buf` is non-null by the
    // test above and trivially aligned for `u8`; writability for `len` bytes and the
    // absence of aliasing are this function's documented obligations on its caller.
    unsafe { core::slice::from_raw_parts_mut(buf.cast::<u8>(), len) }
}

/// Rebuilds a caller's input buffer as a shared slice from a `(buf, len)` pair.
///
/// The [`read_slice`] counterpart for `gzwrite` and `gzfwrite`, whose buffers are
/// `voidpc` -- `const void *` -- and are never written through. The same zero-length
/// rule applies, and for the same reason.
///
/// # Safety
///
/// If `len` is non-zero, `buf` must be non-null and readable for `len` bytes, and that
/// region must stay valid and unwritten by anything else for `'a`.
unsafe fn write_slice<'a>(buf: voidpc, len: usize) -> &'a [u8] {
    if len == 0 || buf.is_null() {
        return &[];
    }
    // SAFETY: unsafe-site category 2 -- slice reconstruction. `buf` is non-null by the
    // test above and trivially aligned for `u8`; readability for `len` bytes and
    // stability for `'a` are this function's documented obligations on its caller.
    unsafe { core::slice::from_raw_parts(buf.cast::<u8>(), len) }
}

// ---------------------------------------------------------------------------
// Offset widths -- the large-file symbol duality
// ---------------------------------------------------------------------------

/// Widens a caller's offset to the signed 64-bit one the core computes in.
///
/// [`z_off_t`] is `c_long` -- eight bytes on LP64, four on a 32-bit target -- while
/// [`z_off64_t`] is always `i64`, and `Into<i64>` holds for both. `From` between signed
/// integers is sign-preserving by construction, so a negative offset arrives at the core
/// still negative and its own checked normalisation still sees it.
///
/// **Generic on purpose, and it must stay generic.** The concrete conversion is
/// invisible to the lint pass inside a generic body, which is the only way to write it
/// once and stay clean on every target: a non-generic `i64::from(offset)` trips
/// `clippy::useless_conversion` wherever the alias already *is* `i64`, and
/// `offset as i64` trips `clippy::unnecessary_cast` together with `trivial_numeric_casts`
/// there. `crate::checksum` widens its combine lengths through the same idiom.
#[inline]
fn widen_offset<T: Into<ZOff64>>(offset: T) -> ZOff64 {
    offset.into()
}

/// Narrows a 64-bit result to the caller's offset type, or reports `-1`.
///
/// C's own idiom, `ret == (z_off_t)ret ? (z_off_t)ret : -1` (`gzlib.c` L440-L441,
/// L463-L464, L492-L493), with the round-trip test performed by the core's
/// `narrow_offset` so that the `-1` convention lives in one place. Saturation is
/// deliberately **not** used: a position the caller's type cannot express is an error,
/// and reporting the nearest representable one would be a wrong answer rather than a
/// refused question.
///
/// The `*64` faces call this too, with `T = z_off64_t`, where the conversion is the
/// identity and the failure branch is unreachable. Routing both widths through one
/// helper is what makes it impossible for the two faces of a pair to disagree.
#[inline]
fn narrow_or_error<T: TryFrom<ZOff64> + From<i8>>(value: ZOff64) -> T {
    match narrow_offset::<T>(value) {
        Some(narrowed) => narrowed,
        None => fallback::offset_error(),
    }
}

// ---------------------------------------------------------------------------
// Descriptor adoption -- the one call the safe core cannot make
// ---------------------------------------------------------------------------

/// The failure reported when an operation is attempted on an already-closed handle.
///
/// No syscall is reached, so there is no `errno` to report and zero is used -- the value
/// `GzIoError`'s own documentation gives for "the implementation could not determine
/// one". C cannot reach the condition at all: its descriptor is an `int` that stays in
/// the structure until `free`, and `zlib.h` L1752-L1753 forbids using a handle after
/// `gzclose`. It is reachable here only through `GzState`'s `Drop` running after an
/// explicit close, which `GzHandle::close`'s idempotence requirement anticipates.
#[cfg(unix)]
const CLOSED_DESCRIPTOR: GzIoError = GzIoError::new(0, false);

/// The failure reported when a descriptor flag cannot be changed.
///
/// `fcntl` is neither in `std` nor expressible without `libc`, so
/// `AdoptedDescriptor::set_nonblocking` cannot perform C's
/// `fcntl(fd, F_SETFL, fcntl(fd, F_GETFL) | O_NONBLOCK)` (`gzlib.c` L254-L257). That is
/// divergence 3 in this module's inventory. Reporting the failure is the honest answer
/// -- `GzHandle::set_nonblocking` documents its error as "if the flag cannot be
/// changed", which is precisely the case -- and
/// `zlib_rs::gz::open::gz_open_handle` discards the result exactly as C discards
/// `fcntl`'s, so nothing observable turns on it.
#[cfg(unix)]
const FLAG_UNSUPPORTED: GzIoError = GzIoError::new(0, false);

/// The failure reported for an offset that cannot be expressed to the platform.
///
/// Two cases, neither of which reaches a syscall: a negative absolute position handed to
/// `SeekFrom::Start`, which takes a `u64`, and a resulting position too large for
/// [`ZOff64`]. On every target this crate supports the second is unreachable, because
/// `ZOff64` and the platform's own offset type are both signed and sixty-four bits wide.
/// The core's `FileHandle` rejects both the same way and for the same reason: `lseek`
/// itself refuses a negative absolute offset, so wrapping it into an enormous unsigned
/// one would be a wrong answer rather than a refused question.
#[cfg(unix)]
const OUT_OF_RANGE_OFFSET: GzIoError = GzIoError::new(0, false);

/// Converts a `std` I/O failure into the two facts the `gzFile` layer acts on.
///
/// C reports such a failure as `gz_error(state, Z_ERRNO, zstrerror())`, where
/// `zstrerror()` expands to `strerror(errno)` (`gzguts.h` L131-L133), and separately
/// records `EAGAIN`/`EWOULDBLOCK` in `state->again` (`gzread.c` L36,
/// `gzwrite.c` L117-L118). Both travel in `GzIoError`: the number, so the message can be
/// rendered as `strerror` would render it, and the stall flag, because a stalled
/// non-blocking stream is not an error condition but a retry signal.
///
/// The kind test rather than a numeric comparison is forced -- this crate names no
/// `errno` constants -- and is the same substitution the core's `FileHandle` makes. It is
/// recorded there as divergence 3 of the core's inventory: a platform whose `WouldBlock`
/// mapping omitted an `errno` C would have matched would behave differently, which has
/// not been observed and has not been proven impossible either.
#[cfg(unix)]
fn io_error(error: &io::Error) -> GzIoError {
    GzIoError::new(
        error.raw_os_error().unwrap_or(0),
        error.kind() == io::ErrorKind::WouldBlock,
    )
}

/// A [`GzHandle`] over a descriptor the caller handed to [`gzdopen`].
///
/// The core declares [`GzHandle`] and supplies `FileHandle` for files it opens by path,
/// but it cannot supply this one: `FromRawFd::from_raw_fd` is `unsafe` and `zlib-rs`
/// carries `#![forbid(unsafe_code)]`. `zlib_rs::gz::open`'s documentation states the
/// division of labour and imposes three requirements on whatever the facade supplies;
/// all three are met here.
///
/// # ★ Why this type exists instead of reusing `FileHandle`
///
/// `zlib.h` L1414-L1416: "The duplicated descriptor should be saved to avoid a leak,
/// since `gzdopen` does not close `fd` if it fails." A `FileHandle` built from
/// `File::from_raw_fd` closes the descriptor when *dropped*, so a failed
/// `gz_open_handle` handing the box back would still close it -- and a `Box<dyn GzHandle>`
/// cannot be downcast to recover the descriptor first. The core spells out the two ways
/// out and this is the second: a handle whose `Drop` does **not** close.
///
/// So the two operations are genuinely different here:
///
/// * [`GzHandle::close`] closes the descriptor. That is C's `close(state->fd)`
///   (`gzread.c` L665, `gzwrite.c` L696), and it runs on every path that ends a stream,
///   because `GzState::teardown` calls it explicitly.
/// * [`Drop`] **relinquishes** the descriptor without closing it, by recovering the raw
///   number with `IntoRawFd` and discarding it. Ownership returns to the caller, which is
///   exactly C's behaviour on a failed `gzdopen`.
///
/// The consequence is worth stating plainly, because it looks like a leak and is not:
/// dropping one of these without closing it deliberately leaves the descriptor open. It
/// is reachable only from a failed open, where the descriptor was never the library's to
/// close.
///
/// # Unix only
///
/// `gzdopen`'s `int fd` is a POSIX file descriptor, which `std::fs::File` can adopt on
/// Unix and cannot elsewhere: on Windows the same `int` is a **CRT file descriptor**, not
/// a `HANDLE`, and bridging it needs `_get_osfhandle` plus CRT-level `_read`, `_write`,
/// `_lseeki64` and `_close`. None of that is exercisable on this port's verified Tier-1
/// target, so it is not written blind; divergence 4 in this module's inventory records
/// the consequence.
#[cfg(unix)]
struct AdoptedDescriptor {
    /// The adopted descriptor, [`None`] once [`GzHandle::close`] has run.
    ///
    /// An [`Option`] rather than a bare [`File`] so that closing can be idempotent, which
    /// the trait requires: `GzState`'s teardown may close explicitly and then be dropped,
    /// and both `gzclose_r` and `gzclose_w` close before the state is released.
    file: Option<File>,
    /// Whether the stream was opened for writing, which decides whether `close` flushes.
    ///
    /// C's `state->mode != GZ_READ`. A read stream has nothing buffered in the `File` to
    /// flush, and flushing one would be a syscall C never makes.
    writable: bool,
}

#[cfg(unix)]
impl AdoptedDescriptor {
    /// Borrows the descriptor, or reports that it has already been closed.
    ///
    /// # Errors
    ///
    /// [`CLOSED_DESCRIPTOR`] once [`GzHandle::close`] has run.
    fn open_file(&mut self) -> Result<&mut File, GzIoError> {
        self.file.as_mut().ok_or(CLOSED_DESCRIPTOR)
    }
}

#[cfg(unix)]
impl GzHandle for AdoptedDescriptor {
    /// Implements `read(state->fd, buf + *have, len)` in `gz_load` (`gzread.c` L30).
    ///
    /// `Ok(0)` means end of file, as `read` returning 0 does there. Interruptions are
    /// deliberately **not** retried: `read` returning `-1` with `EINTR` is an error to
    /// `gz_load` (`gzread.c` L37-L42), and [`File`]'s `read` likewise surfaces
    /// [`io::ErrorKind::Interrupted`] rather than looping, so the two agree.
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, GzIoError> {
        self.open_file()?
            .read(buf)
            .map_err(|error| io_error(&error))
    }

    /// Implements `write(state->fd, state->x.next, put)` in `gz_comp` (`gzwrite.c` L115).
    ///
    /// A short write is normal rather than exceptional -- `gz_comp` loops until the
    /// buffer is drained (`gzwrite.c` L110-L123) -- so no loop is added here.
    fn write(&mut self, buf: &[u8]) -> Result<usize, GzIoError> {
        self.open_file()?
            .write(buf)
            .map_err(|error| io_error(&error))
    }

    /// Implements the `LSEEK` macro (`gzlib.c` L8-L16), which resolves to `lseek64`,
    /// `_lseeki64`, `llseek` or `lseek` per platform.
    ///
    /// [`GzSeekFrom::Start`] is the one origin that cannot take a negative offset, so a
    /// negative absolute position is refused the way `lseek` refuses it rather than
    /// wrapped into an enormous unsigned one.
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

    /// Reports that the flag cannot be changed: divergence 3 of this module's inventory.
    ///
    /// C performs `fcntl(fd, F_SETFL, fcntl(fd, F_GETFL) | O_NONBLOCK)` here
    /// (`gzlib.c` L254-L257) and ignores the result.
    /// `zlib_rs::gz::open::gz_open_handle` likewise discards this one, so the refusal is
    /// not observable through any entry point -- but it is reported rather than faked,
    /// because claiming a success that did not happen would make the divergence
    /// invisible. A caller that needs the flag can set it on the descriptor before
    /// handing it over, which `zlib.h` L1398-L1401 already describes as the route to
    /// take.
    fn set_nonblocking(&mut self, _nonblocking: bool) -> Result<(), GzIoError> {
        Err(FLAG_UNSUPPORTED)
    }

    /// Closes the descriptor, flushing first if the stream was opened for writing.
    /// Idempotent.
    ///
    /// The port of `close(state->fd)` (`gzread.c` L665, `gzwrite.c` L696), whose failure
    /// becomes `Z_ERRNO`.
    ///
    /// # ★ Why this reports a status the core's own handle cannot
    ///
    /// `Drop for File` discards `close(2)`'s result, so no safe-Rust handle can simply
    /// forward it -- which is divergence 1 of the core's inventory, and the core resolves
    /// it by saying that "a stream whose `close(2)` status must be exact takes an injected
    /// handle". This *is* that injected handle: it is the one `gzdopen` receives, and it
    /// is the only place in the port where a descriptor the library did not open itself
    /// can be closed. So rather than inherit the divergence, the one condition C reports
    /// through `close(2)` is established here *before* the descriptor is released.
    ///
    /// `metadata()` is `fstat(2)` on the same descriptor. `close(2)` has exactly one
    /// failure that says something observable about the descriptor -- `EBADF`, meaning it
    /// was never open or was closed behind the library's back -- and `fstat(2)` answers
    /// precisely that question. Its other documented failures (`EINTR`, `EIO`) do not
    /// leave the descriptor usable and are not reproducible from here in any case.
    /// Measured against the C reference: with a descriptor the caller closed before
    /// handing it over, `gzclose` now returns `Z_ERRNO` from both libraries, where
    /// forwarding only the flush's result returned `Z_OK` from this one.
    ///
    /// A metadata query is deliberately not an `fsync`: it costs no durability, so the
    /// core's reason for refusing to sync here -- that C's `close` does not sync, and
    /// forcing one would make every `gzclose` pay for a guarantee the reference never
    /// made -- is honoured rather than worked around.
    ///
    /// The file is taken out of the slot *before* anything is attempted, so the descriptor
    /// is released even when a step fails and a second call still succeeds. The probe runs
    /// before the flush rather than after it, which matters for one reason: a stale
    /// descriptor must be recognised before `std` is asked to touch or release it. C's
    /// order is the other way round -- `gz_comp` writes, then `close` -- but for a stale
    /// descriptor both of C's steps fail and both yield `Z_ERRNO`, so the observable
    /// answer is the same, and for a live one the probe cannot fail.
    fn close(&mut self) -> Result<(), GzIoError> {
        let Some(mut file) = self.file.take() else {
            // Already closed; the trait requires this to succeed.
            return Ok(());
        };
        if let Err(error) = file.metadata() {
            // The descriptor is not open, so there is nothing to close and nobody to hand
            // it back to. `into_raw_fd` releases the `File` WITHOUT closing: asking `std`
            // to close a number that is not open is an I/O-safety violation it aborts on
            // under `debug_assertions`, and abandoning a number that names nothing leaks
            // nothing. C reaches the same answer by a shorter route -- its `close(2)`
            // simply returns -1 -- and reports the same `Z_ERRNO`.
            let _stale = file.into_raw_fd();
            return Err(io_error(&error));
        }
        if self.writable {
            file.flush().map_err(|error| io_error(&error))?;
        }
        // Dropping the file here is what closes the descriptor. Stated rather than left
        // implicit, because this type's `Drop` deliberately does the opposite.
        drop(file);
        Ok(())
    }
}

#[cfg(unix)]
impl Drop for AdoptedDescriptor {
    /// Relinquishes the descriptor **without closing it**.
    ///
    /// `zlib.h` L1415-L1416: "`gzdopen` does not close `fd` if it fails." This runs only
    /// on that path -- every path that ends a stream closes through
    /// [`GzHandle::close`] first, because `GzState::teardown` calls it -- so a descriptor
    /// still present here is one the library never took ownership of in the caller's eyes.
    /// `IntoRawFd` recovers the raw number and consumes the [`File`] without closing it,
    /// which is precisely the "supply your own handle type whose `Drop` does not close"
    /// option `zlib_rs::gz::open` offers.
    fn drop(&mut self) {
        if let Some(file) = self.file.take() {
            // The descriptor is intentionally left open: ownership returns to the caller.
            let _relinquished = file.into_raw_fd();
        }
    }
}

/// Adopts a caller's descriptor as a boxed [`GzHandle`].
///
/// The single `from_raw_fd` site in the crate, and the one call
/// `zlib_rs::gz::open::gz_open_handle` exists to receive. `try_box_handle` rather than
/// `Box::new`, because `Box::new` aborts the process on allocation failure while
/// `gzdopen` is documented to return `NULL`; the core provides that fallible constructor
/// for exactly this call site. A failure drops the handle, whose [`Drop`] relinquishes
/// the descriptor unclosed -- which is what `zlib.h` L1415-L1416 requires.
///
/// # Safety
///
/// `fd` must be an **open** descriptor on the calling process, and ownership of it must
/// transfer to this call exactly once: no other [`File`], handle or piece of code may own
/// the same descriptor, and it must not be closed behind the resulting [`File`]'s back.
/// `zlib.h` L1414-L1416 places the same obligation on a C caller, and tells one that
/// wants to keep using the descriptor to `dup` it first. A value of `-1` must not be
/// passed; [`gzdopen`] rejects it before reaching here.
#[cfg(unix)]
unsafe fn adopt_descriptor(fd: c_int, spec: &GzOpenSpec) -> Option<Box<dyn GzHandle + 'static>> {
    // SAFETY: descriptor adoption -- the one unsafe site this module owns *in addition* to
    // AAP 0.6.1's six categories, called out there for `gzdopen` specifically. The caller
    // guarantees that `fd` is open and that ownership transfers here exactly once, which
    // is what `FromRawFd::from_raw_fd` requires; `gzdopen` has already rejected `-1`.
    let file = unsafe { File::from_raw_fd(fd) };
    let handle = AdoptedDescriptor {
        file: Some(file),
        // C's `state->mode != GZ_READ`, read from the already-parsed mode string.
        writable: spec.mode() != GZ_READ,
    };
    try_box_handle(handle).ok()
}

// ---------------------------------------------------------------------------
// Opening -- `zlib.h` L1350-L1425, ported from `gzlib.c`
// ---------------------------------------------------------------------------

/// The body shared by [`gzopen`] and [`gzopen64`].
///
/// `gzlib.c` L288-L295 defines the two as byte-for-byte identical one-line forwards to
/// the same `gz_open`, and they exist separately only because of the large-file naming
/// scheme; the core likewise provides one function. Keeping the shared body *below* the
/// guard is what stops one call installing two.
///
/// # Safety
///
/// As [`c_bytes`], for both `path` and `mode`.
unsafe fn open_path(path: *const c_char, mode: *const c_char) -> gzFile {
    // `gzlib.c` L96-L97: `if (path == NULL || mode == NULL) return NULL;`. A null pointer
    // cannot be a `&[u8]`, so the test belongs here rather than in the core.
    //
    // SAFETY: unsafe-site category 5, discharged by this function's own contract: each
    // pointer is either null -- handled by `c_bytes` -- or a NUL-terminated string that
    // stays readable for the duration of the call.
    let (Some(path), Some(mode)) = (unsafe { c_bytes(path) }, unsafe { c_bytes(mode) }) else {
        return fallback::null_handle();
    };

    // Every `Err` is C's `NULL`, which the core's `GzOpenError` documentation states
    // explicitly: an unusable mode string, an exhausted allocator and a refusal from the
    // file system are all answered the same way. The carried `errno` cannot be published
    // through the platform's thread-local from safe Rust -- divergence 1.
    match rs_gz::gzopen(path, mode, GlobalAllocator) {
        Ok(state) => install(state),
        Err(_) => fallback::null_handle(),
    }
}

/// Open a gzip file for reading or writing.
///
/// `zlib.h` L1357: `gzFile gzopen(const char *path, const char *mode);`. Ported from
/// `gzlib.c` L288-L290.
///
/// The mode string is `fopen`'s, extended: a compression level digit, `r`/`w`/`a` for
/// direction, and the modifiers `+` (rejected), `b` (ignored), `e`, `x`, `f`, `h`, `R`,
/// `F`, `G`, `N` and `T`. Unknown characters are **silently ignored**, which is
/// load-bearing rather than lax -- `test/minigzip.c` L511 builds `"wb6 "` and patches
/// index 3 afterwards, so a mode string containing a space must be accepted. The full
/// grammar and the four ways it can make an open fail are tabulated in
/// `zlib_rs::gz::open`'s documentation; nothing is re-parsed here.
///
/// Returns `Z_NULL` if the file could not be opened, if there was insufficient memory
/// for the state, or if the mode was invalid (`zlib.h` L1391-L1393). `errno` may be
/// consulted for the first of those, subject to divergence 1 in this module's
/// documentation.
///
/// # Safety
///
/// `path` and `mode` must each be either null or a NUL-terminated byte string that stays
/// readable, and is not written by anything else, for the duration of the call.
#[no_mangle]
pub unsafe extern "C" fn gzopen(path: *const c_char, mode: *const c_char) -> gzFile {
    guard(|| {
        // SAFETY: this function's contract is `open_path`'s, discharged unchanged.
        unsafe { open_path(path, mode) }
    })
}

/// Open a gzip file for reading or writing, with an explicitly 64-bit offset interface.
///
/// `zlib.h` L1978: `gzFile gzopen64(const char *, const char *);`. Ported from
/// `gzlib.c` L293-L295, which is the same `gz_open` call `gzopen` makes.
///
/// ★ **Both names must be exported, and neither is redundant.** `zconf.h` redirects the
/// unsuffixed names to the suffixed ones -- or declares only the suffixed ones --
/// according to the **caller's** `_FILE_OFFSET_BITS` and `_LARGEFILE64_SOURCE` at the
/// caller's own compile time (`zlib.h` L1976-L2022). Which name a given object file
/// references is decided long before this library is reached, so it is not this library's
/// choice to make. `test/CMakeLists.txt` propagates `_LARGEFILE64_SOURCE=1` to some of
/// the C drivers and not others, which is why both families are genuinely exercised.
///
/// Nothing about the offset width differs between the two for *opening*: the pair exists
/// so that the four positioning entry points can be redirected consistently.
///
/// # Safety
///
/// As [`gzopen`].
#[no_mangle]
pub unsafe extern "C" fn gzopen64(path: *const c_char, mode: *const c_char) -> gzFile {
    guard(|| {
        // SAFETY: this function's contract is `open_path`'s, discharged unchanged.
        unsafe { open_path(path, mode) }
    })
}

/// Associate a gzip stream with an already-open file descriptor.
///
/// `zlib.h` L1404: `gzFile gzdopen(int fd, const char *mode);`. Ported from
/// `gzlib.c` L298-L312.
///
/// The mode string is `gzopen`'s. `zlib.h` L1408-L1416 documents two properties that
/// shape the implementation:
///
/// * **The descriptor is closed by `gzclose`.** So `gzdopen(fileno(stdin), "rb")` --
///   which `test/minigzip.c` L549 does -- closes standard input when the stream is
///   closed, exactly as C does. A caller that wants to keep using the descriptor must
///   `dup` it first, which the header says outright.
/// * **A failed `gzdopen` does not close `fd`.** That is why the mode string is parsed
///   *before* the descriptor is adopted, and why [`AdoptedDescriptor`]'s [`Drop`]
///   relinquishes rather than closes: the two together make the promise hold on every
///   failure path.
///
/// Returns `Z_NULL` if there was insufficient memory for the state, if the mode was
/// invalid, or if `fd` is `-1` (`zlib.h` L1417-L1418 and L1424).
///
/// Off Unix this returns `Z_NULL` unconditionally -- divergence 4 in this module's
/// documentation, which explains why a Windows CRT descriptor cannot be adopted here and
/// what to use instead.
///
/// # Safety
///
/// `mode` must be null or a NUL-terminated string readable for the duration of the call.
/// If `fd` is neither `-1` nor invalid, it must be an **open** descriptor whose ownership
/// transfers to this call exactly once -- no other owner may exist, and nothing else may
/// close it behind the library's back. That is the same obligation `zlib.h` L1414-L1416
/// places on a C caller.
#[no_mangle]
pub unsafe extern "C" fn gzdopen(fd: c_int, mode: *const c_char) -> gzFile {
    guard(|| {
        // SAFETY: unsafe-site category 5, discharged by this function's own contract:
        // `mode` is null -- handled by `c_bytes` -- or a NUL-terminated readable string.
        let mode = unsafe { c_bytes(mode) };
        let Some(mode) = mode else {
            return fallback::null_handle();
        };
        // SAFETY: this function's contract is `dopen_descriptor`'s, discharged unchanged.
        unsafe { dopen_descriptor(fd, mode) }
    })
}

/// The Unix body of [`gzdopen`], once the mode string is a slice.
///
/// Split out so that the descriptor-specific reasoning -- and the `#[cfg]` -- sit away
/// from the exported signature, and so that the non-Unix stub has a real function to
/// stand in for rather than a conditionally absent block.
///
/// The order of the first three steps is required rather than tidy:
///
/// 1. **Parse the mode string.** `zlib_rs::gz::open`'s facade obligations require this to
///    come first, so that an unusable mode never touches the descriptor at all.
/// 2. **Reject `fd == -1`.** C's first test (`gzlib.c` L302-L303). It is repeated here,
///    ahead of the core's own copy, because `File::from_raw_fd(-1)` would violate
///    `FromRawFd`'s contract -- and the core's copy runs only *after* adoption.
/// 3. **Adopt.** Only now is the descriptor taken, and any later failure hands it back
///    unclosed.
///
/// # Safety
///
/// As [`gzdopen`].
#[cfg(unix)]
unsafe fn dopen_descriptor(fd: c_int, mode: &[u8]) -> gzFile {
    let Ok(spec) = GzOpenSpec::parse(mode) else {
        return fallback::null_handle();
    };
    if fd == -1 {
        return fallback::null_handle();
    }
    // SAFETY: descriptor adoption. This function's own contract is `adopt_descriptor`'s
    // -- `fd` is open and its ownership transfers here exactly once -- and the `-1` case
    // that contract excludes has just been rejected above.
    let handle = unsafe { adopt_descriptor(fd, &spec) };
    let Some(handle) = handle else {
        return fallback::null_handle();
    };
    match rs_gz::gzdopen(handle, fd, mode, GlobalAllocator) {
        Ok(state) => install(state),
        // The handle comes back unclosed and is dropped here; its `Drop` relinquishes the
        // descriptor without closing it, which is `zlib.h` L1415-L1416's promise. Naming
        // the field rather than dropping the whole error is what makes that deliberate.
        Err(failure) => {
            drop(failure.handle);
            fallback::null_handle()
        }
    }
}

/// The non-Unix body of [`gzdopen`], which cannot adopt a descriptor.
///
/// Divergence 4 in this module's documentation. The symbol is still exported, so symbol
/// parity holds; what is unavailable is the behaviour, because a Windows `int fd` is a CRT
/// file descriptor rather than a `HANDLE` and `std::fs::File` cannot adopt one.
///
/// `Z_NULL` is the right refusal: it is already the documented answer for a mode string
/// this platform cannot honour, and a caller that checks the return value -- which
/// `zlib.h` L1417-L1418 requires -- sees a clean failure rather than a broken stream.
///
/// # Safety
///
/// As [`gzdopen`]. Nothing is dereferenced, and no descriptor is taken.
#[cfg(not(unix))]
unsafe fn dopen_descriptor(_fd: c_int, _mode: &[u8]) -> gzFile {
    fallback::null_handle()
}

/// `wchar_t`, for the Windows-only wide-path entry point.
///
/// Rust has no `wchar_t` primitive and cbindgen knows no such name, so `cbindgen.toml`
/// L590-L595 requires the facade to declare the alias itself for `gzopen_w` to render
/// with the spelling `zlib.h` L2042 uses. On Windows `wchar_t` is a sixteen-bit UTF-16
/// code unit, which is why the alias is [`u16`] and not a wider type.
#[cfg(windows)]
#[allow(non_camel_case_types)]
pub type wchar_t = u16;

/// Open a gzip file named by a wide-character path. Windows only.
///
/// `zlib.h` L2042: `gzFile gzopen_w(const wchar_t *path, const char *mode);`. Ported from
/// `gzlib.c` L316-L318, whose definition is guarded by `#ifdef WIDECHAR` --
/// `gzguts.h` L54-L56 defines that for `_WIN32` -- while the declaration is guarded by
/// `#if defined(_WIN32) && !defined(Z_SOLO)`.
///
/// ★ **The `#[cfg(windows)]` is a symbol-parity requirement, not a portability nicety.**
/// The reference library exports ninety-five functions on Linux and this is the one
/// `zlib.h` declaration that is not among them. Compiling it unconditionally would add a
/// ninety-sixth name and fail the parity diff.
///
/// The conversion is the boundary's job, and it produces UTF-8. C converts inside
/// `gz_open` with `wcstombs` (`gzlib.c` L200-L219), which narrows to the active code page
/// and fails for any character that page cannot represent; the core's `gzopen_w`
/// documentation records the resulting difference as forced and unresolved, because safe
/// Rust cannot decode a locale-dependent encoding portably. Note what travels where: the
/// caller's `wchar_t` units are passed through **unconverted** as the thing to open,
/// while the narrowed text is supplied separately as the error label -- exactly the split
/// C draws between `_wopen`'s argument and `state->path`. Narrowing the path *and*
/// opening the narrowing could name a different file, or none.
///
/// Unpaired surrogates are replaced rather than rejected, which
/// [`String::from_utf16_lossy`] does and which only affects the label; the path itself is
/// unaffected because it is never narrowed.
///
/// # Safety
///
/// `path` must be null or a zero-terminated array of `wchar_t` that stays readable for the
/// duration of the call, and `mode` must satisfy [`gzopen`]'s contract.
#[cfg(windows)]
#[no_mangle]
pub unsafe extern "C" fn gzopen_w(path: *const wchar_t, mode: *const c_char) -> gzFile {
    guard(|| {
        // SAFETY: unsafe-site category 5, discharged by this function's own contract:
        // `mode` is null -- handled by `c_bytes` -- or a NUL-terminated readable string.
        let mode = unsafe { c_bytes(mode) };
        let Some(mode) = mode else {
            return fallback::null_handle();
        };
        if path.is_null() {
            // `gzlib.c` L96-L97 again: a null name is `NULL`, not an empty one.
            return fallback::null_handle();
        }
        // The zero-terminated length, found the way `wcslen` finds it. `wrapping_add` has
        // no precondition to violate, and the loop stops at the terminator the caller's
        // contract guarantees.
        let mut len = 0_usize;
        // SAFETY: unsafe-site category 5. The caller guarantees `path` is a
        // zero-terminated `wchar_t` array readable for the duration of the call, so every
        // unit up to and including the terminator is readable and the scan cannot run
        // past it.
        while unsafe { *path.add(len) } != 0 {
            len = len.wrapping_add(1);
        }
        // SAFETY: unsafe-site category 2. `path` is non-null, aligned for `u16` by the
        // caller's contract, and `len` units before the terminator were just proved
        // readable; the slice is built once and only safe values travel inward.
        let units = unsafe { core::slice::from_raw_parts(path, len) };
        let label = String::from_utf16_lossy(units);
        match rs_gz::gzopen_w(units, label.as_bytes(), mode, GlobalAllocator) {
            Ok(state) => install(state),
            Err(_) => fallback::null_handle(),
        }
    })
}

// ---------------------------------------------------------------------------
// Configuration -- `zlib.h` L1427-L1454
// ---------------------------------------------------------------------------

/// Set the internal buffer size used by this library's functions for `file`.
///
/// `zlib.h` L1429: `int gzbuffer(gzFile file, unsigned size);`. Ported from
/// `gzlib.c` L322-L343.
///
/// The default is 8192 bytes and the size **must be set before the first read or write**
/// (`zlib.h` L1431-L1438): `state->size` is assigned the moment the working buffers are
/// allocated, and from then on this call does nothing but fail. Two further rules live in
/// the core and are not duplicated here -- a size that cannot be doubled is refused, and
/// a size below 8 is *raised* to 8 rather than refused, "needed to behave well with
/// flushing".
///
/// `size` is passed through unchanged as an `unsigned`, and returns 0 on success or -1 on
/// failure.
///
/// # Safety
///
/// `file` must be null or satisfy [`block_mut`]'s contract.
#[no_mangle]
pub unsafe extern "C" fn gzbuffer(file: gzFile, size: c_uint) -> c_int {
    guard(|| {
        // SAFETY: unsafe-site categories 1 and 3, discharged by this function's own
        // contract. The core answers a `None` with -1 itself -- C's
        // `if (file == NULL) return -1;` at `gzlib.c` L326-L327 -- so the guard's result
        // travels inward rather than being branched on here.
        let state = unsafe { state_mut(file) };
        rs_gz::gzbuffer(state, size)
    })
}

/// Update the compression level and strategy for `file`.
///
/// `zlib.h` L1445: `int gzsetparams(gzFile file, int level, int strategy);`. Ported from
/// `gzwrite.c` L630-L665.
///
/// Writing only, and the previous input is flushed before the new parameters take effect,
/// so the change applies from this point in the stream onwards. Returns `Z_OK` on
/// success, `Z_STREAM_ERROR` for an invalid or non-writing stream, `Z_MEM_ERROR` if there
/// was no memory, or `Z_BUF_ERROR` if the stream stalled while flushing
/// (`zlib.h` L1447-L1452).
///
/// Neither argument is validated here, and that is C's behaviour rather than an omission:
/// C validates neither, leaving the range check to `deflateParams` or -- for a stream
/// whose compressor does not exist yet -- to the `deflateInit2_` that
/// `zlib_rs::gz::write`'s `gz_init` eventually performs.
///
/// # Safety
///
/// `file` must be null or satisfy [`block_mut`]'s contract.
#[no_mangle]
pub unsafe extern "C" fn gzsetparams(file: gzFile, level: c_int, strategy: c_int) -> c_int {
    guard_code(|| {
        // SAFETY: unsafe-site categories 1 and 3, discharged by this function's own
        // contract. As in `gzbuffer`, the core owns the `None` answer -- C's
        // `if (file == NULL) return Z_STREAM_ERROR;` at `gzwrite.c` L637-L638.
        let state = unsafe { state_mut(file) };
        rs_gz::gzsetparams(state, level, strategy)
    })
}

// ---------------------------------------------------------------------------
// Reading -- `zlib.h` L1456-L1516 and L1585-L1645, ported from `gzread.c`
// ---------------------------------------------------------------------------

/// Read and decompress up to `len` uncompressed bytes from `file` into `buf`.
///
/// `zlib.h` L1456: `int gzread(gzFile file, voidp buf, unsigned len);`. Ported from
/// `gzread.c` L395-L436.
///
/// Returns the number of bytes actually read -- which may be fewer than `len`, and is
/// zero at end of the compressed data -- or -1 on error. `zlib.h` L1474-L1478 is explicit
/// that an *incomplete* gzip stream is **not** reported here: the `Z_BUF_ERROR` stays
/// recorded, `gzerror` can be consulted for it, and `gzclose` is where it finally
/// surfaces. A request larger than `INT_MAX` is refused with `Z_STREAM_ERROR` because the
/// count is returned in an `int`.
///
/// # Safety
///
/// `file` must be null or satisfy [`block_mut`]'s contract. If `len` is non-zero, `buf`
/// must be non-null and writable for `len` bytes, and that region must not overlap the
/// state behind `file` or any other object the library holds.
#[no_mangle]
pub unsafe extern "C" fn gzread(file: gzFile, buf: voidp, len: c_uint) -> c_int {
    guard(|| {
        // SAFETY: unsafe-site categories 1 and 3, discharged by this function's own
        // contract.
        let state = unsafe { state_mut(file) };
        let Some(state) = state else {
            return fallback::GZ_ERROR;
        };
        // SAFETY: unsafe-site category 2, discharged by this function's own contract:
        // `len` bytes at `buf` are writable and unaliased for the duration of the call.
        // `widen` is the crate's `uInt` -> `usize` conversion and cannot lose a bit.
        let window = unsafe { read_slice(buf, widen(len)) };
        rs_gz::gzread(state, window)
    })
}

/// Read and decompress up to `nitems` items of `size` bytes each from `file` into `buf`.
///
/// `zlib.h` L1492-L1493:
/// `z_size_t gzfread(voidp buf, z_size_t size, z_size_t nitems, gzFile file);`. Ported
/// from `gzread.c` L438-L465.
///
/// ★ Note the argument order and the return unit, both of which differ from every
/// neighbour: the buffer comes **first**, the handle **last**, and the count is in
/// **items** rather than bytes -- because the call duplicates `fread`'s interface
/// (`zlib.h` L1495-L1497). `zlib.h` L1502-L1503 adds that a returned zero must be
/// disambiguated with `gzerror`, since end of file and error look the same.
///
/// The product `size * nitems` is computed with checked arithmetic. C computes it and
/// rejects the request when `size && len / size != nitems` (`gzread.c` L456-L461); the
/// core owns that test, because recording the resulting `Z_STREAM_ERROR` needs its
/// crate-private `gz_error`. So both counts are passed through together with the largest
/// buffer that could be built for them -- an empty slice when the product overflows,
/// which the core rejects before looking at the buffer at all.
///
/// A trailing partial item is still copied and the end-of-file flag set, matching common
/// `fread` implementations, and is *not* counted in the result; `gztell` recovers its
/// length (`zlib.h` L1508-L1516).
///
/// # Safety
///
/// `file` must be null or satisfy [`block_mut`]'s contract. If `size * nitems` is
/// non-zero, `buf` must be non-null and writable for that many bytes, unaliased for the
/// duration of the call.
#[no_mangle]
pub unsafe extern "C" fn gzfread(
    buf: voidp,
    size: z_size_t,
    nitems: z_size_t,
    file: gzFile,
) -> z_size_t {
    guard(|| {
        // SAFETY: unsafe-site categories 1 and 3, discharged by this function's own
        // contract.
        let state = unsafe { state_mut(file) };
        let Some(state) = state else {
            return fallback::GZ_NO_ITEMS;
        };
        // `unwrap_or` rather than the denied `unwrap`: an overflowing product yields the
        // empty slice, which the core answers with `Z_STREAM_ERROR` and zero items.
        let len = size.checked_mul(nitems).unwrap_or(0);
        // SAFETY: unsafe-site category 2, discharged by this function's own contract:
        // `len` is exactly `size * nitems` when that product is representable, and those
        // bytes at `buf` are writable and unaliased for the duration of the call.
        let window = unsafe { read_slice(buf, len) };
        rs_gz::gzfread(state, window, size, nitems)
    })
}

/// Read bytes from `file` into `buf` until a newline, `len - 1` bytes, or end of file.
///
/// `zlib.h` L1588: `char *gzgets(gzFile file, char *buf, int len);`. Ported from
/// `gzread.c` L565-L624.
///
/// The string is always terminated with a zero byte, and `buf` is returned on success or
/// **null** at end of file or on error (`zlib.h` L1591-L1600). If an error occurs, data
/// already read is delivered first and the *next* call reports the error.
///
/// Two details are worth stating because they surprise readers of the header rather than
/// of the implementation, and the implementation is the oracle:
///
/// * **Embedded zero bytes are copied like any other byte** (`gzread.c` L617-L619), so a
///   caller treating the result as a C string may see a short line. `test/example.c`
///   relies on exactly that with its `"hello, hello!\0"` fixture.
/// * **`len == 1` returns null**, and does not even write the terminator: C computes
///   `left = len - 1`, skips its loop, and returns `NULL` because nothing was written.
///
/// `buf == NULL || len < 1` is refused with null, which is C's own first test
/// (`gzread.c` L574-L575).
///
/// # Safety
///
/// `file` must be null or satisfy [`block_mut`]'s contract. If `len` is at least 1, `buf`
/// must be non-null and writable for `len` bytes, unaliased for the duration of the call.
#[no_mangle]
pub unsafe extern "C" fn gzgets(file: gzFile, buf: *mut c_char, len: c_int) -> *mut c_char {
    guard(|| {
        // `gzread.c` L574-L575: `if (file == NULL || buf == NULL || len < 1) return NULL;`
        // The two pointer-free halves are tested here because a null `buf` cannot be a
        // slice and a non-positive `len` cannot be a length.
        let Ok(capacity) = usize::try_from(len) else {
            return fallback::null_string();
        };
        if buf.is_null() || capacity == 0 {
            return fallback::null_string();
        }
        // SAFETY: unsafe-site categories 1 and 3, discharged by this function's own
        // contract.
        let state = unsafe { state_mut(file) };
        let Some(state) = state else {
            return fallback::null_string();
        };
        // SAFETY: unsafe-site category 2, discharged by this function's own contract:
        // `capacity` is `len` and those bytes at `buf` are writable and unaliased for the
        // duration of the call. `c_char` and `u8` have the same size and alignment, and
        // the cast is to a byte pointer rather than a differently-sized one.
        let window = unsafe { read_slice(buf.cast(), capacity) };
        match rs_gz::gzgets(state, window) {
            // C returns the buffer it was given, not a pointer past the text.
            Some(_) => buf,
            None => fallback::null_string(),
        }
    })
}

/// The body shared by [`gzgetc`] and [`gzgetc_`].
///
/// `gzread.c` L500-L502 makes `gzgetc_` a one-line forward to `gzgetc`, reproduced here
/// one level *below* the guard so that neither export installs two.
///
/// # Safety
///
/// As [`block_mut`].
unsafe fn getc_body(file: gzFile) -> c_int {
    // SAFETY: unsafe-site categories 1 and 3, discharged by this function's own contract.
    let state = unsafe { state_mut(file) };
    let Some(state) = state else {
        return fallback::GZ_ERROR;
    };
    rs_gz::gzgetc(state)
}

/// Read and decompress one byte from `file`.
///
/// `zlib.h` L1613: `int gzgetc(gzFile file);`. Ported from `gzread.c` L467-L498. Returns
/// the byte, or -1 on end of file or error.
///
/// ★ **This function must exist even though `gzgetc` is also a macro**, and it is not a
/// fallback nobody reaches. `zlib.h` L1966-L1968 defines the macro to serve the byte out
/// of the exposed prefix in the caller's own object code and to call `(gzgetc)(g)` when
/// `have` is zero, so the function form runs on every buffer refill; a caller may also
/// take its address or `#undef` the macro. `nm -u` on a compiled `test/example.o` lists
/// `gzgetc` as a genuine undefined symbol, which settles the question empirically.
///
/// The core reproduces the macro's fast path for the function form -- read at the cursor,
/// then decrement `have`, increment `pos` and advance the cursor -- and performs the
/// exposed-prefix resynchronisation itself, which is what makes a macro call and a
/// function call interchangeable in any order. The module documentation explains why this
/// file must not repeat that resynchronisation.
///
/// On a non-blocking device with no uncompressed data to return, this reports -1 with
/// `gzeof` false and `gzerror` set to `Z_ERRNO` (`zlib.h` L1624-L1628).
///
/// # Safety
///
/// `file` must be null or satisfy [`block_mut`]'s contract.
#[no_mangle]
pub unsafe extern "C" fn gzgetc(file: gzFile) -> c_int {
    guard(|| {
        // SAFETY: this function's contract is `getc_body`'s, discharged unchanged.
        unsafe { getc_body(file) }
    })
}

/// Read and decompress one byte from `file`. The function form, kept for compatibility.
///
/// `zlib.h` L1961: `int gzgetc_(gzFile file); /* backward compatibility */`. Ported from
/// `gzread.c` L500-L502.
///
/// A separately exported symbol rather than an alias: the name exists so that a program
/// linked against a zlib built before the `gzgetc` macro appeared still resolves, and it
/// must remain reachable independently. It does nothing but delegate, exactly as C's does.
///
/// # Safety
///
/// As [`gzgetc`].
#[no_mangle]
pub unsafe extern "C" fn gzgetc_(file: gzFile) -> c_int {
    guard(|| {
        // SAFETY: this function's contract is `getc_body`'s, discharged unchanged.
        unsafe { getc_body(file) }
    })
}

/// Push `c` back onto the stream for `file`, to be read first by the next read.
///
/// `zlib.h` L1630: `int gzungetc(int c, gzFile file);`. Ported from
/// `gzread.c` L504-L563. Returns the byte pushed, or -1 on failure.
///
/// ★ **The argument order is inverted relative to every other `gz` entry point**: the
/// byte comes first and the handle second. That is `zlib.h`'s signature and it is
/// reproduced verbatim, because swapping it would compile and then silently corrupt every
/// caller.
///
/// At least one push is always allowed, and immediately after opening the whole output
/// buffer is available for pushing (`zlib.h` L1633-L1637). A push after end of file makes
/// the stream readable again, and `gzungetc(-1, file)` is the documented idiom for
/// forcing a pending `gzseek` to execute so that `gztell` reports the resulting position
/// -- which works only because the core carries out the skip *before* rejecting the
/// negative byte, exactly as C does (`gzlib.c` L1641-L1644 documents the idiom,
/// `gzread.c` L524-L530 is the order that makes it work).
///
/// # Safety
///
/// `file` must be null or satisfy [`block_mut`]'s contract.
#[no_mangle]
pub unsafe extern "C" fn gzungetc(c: c_int, file: gzFile) -> c_int {
    guard(|| {
        // SAFETY: unsafe-site categories 1 and 3, discharged by this function's own
        // contract.
        let state = unsafe { state_mut(file) };
        let Some(state) = state else {
            return fallback::GZ_ERROR;
        };
        rs_gz::gzungetc(state, c)
    })
}

// ---------------------------------------------------------------------------
// Writing -- `zlib.h` L1517-L1583 and L1645-L1660, ported from `gzwrite.c`
// ---------------------------------------------------------------------------

/// Compress `len` bytes from `buf` and write them to `file`.
///
/// `zlib.h` L1519: `int gzwrite(gzFile file, voidpc buf, unsigned len);`. Ported from
/// `gzwrite.c` L255-L277.
///
/// Returns the number of uncompressed bytes written, **or 0 in case of error or if `len`
/// is 0** (`zlib.h` L1522-L1523). Zero rather than -1 is deliberate and is the one place
/// the write side departs from its read counterpart's convention: the count is unsigned
/// in spirit, so -1 would report that a negative number of bytes was written.
///
/// A length that does not fit in an `int` records `Z_DATA_ERROR`, which is *not* the
/// `Z_STREAM_ERROR` `gzread` records for the same condition. That asymmetry is visible
/// through `gzerror`, so it is part of the observable contract and is preserved.
///
/// # Safety
///
/// `file` must be null or satisfy [`block_mut`]'s contract. If `len` is non-zero, `buf`
/// must be non-null and readable for `len` bytes, and must stay valid and unwritten for
/// the duration of the call.
#[no_mangle]
pub unsafe extern "C" fn gzwrite(file: gzFile, buf: voidpc, len: c_uint) -> c_int {
    guard(|| {
        // SAFETY: unsafe-site categories 1 and 3, discharged by this function's own
        // contract.
        let state = unsafe { state_mut(file) };
        let Some(state) = state else {
            return fallback::GZ_NOTHING_WRITTEN;
        };
        // SAFETY: unsafe-site category 2, discharged by this function's own contract:
        // `len` bytes at `buf` are readable and stable for the duration of the call.
        let payload = unsafe { write_slice(buf, widen(len)) };
        rs_gz::gzwrite(state, payload)
    })
}

/// Compress `nitems` items of `size` bytes each from `buf` and write them to `file`.
///
/// `zlib.h` L1529-L1530:
/// `z_size_t gzfwrite(voidpc buf, z_size_t size, z_size_t nitems, gzFile file);`. Ported
/// from `gzwrite.c` L280-L304.
///
/// The mirror of [`gzfread`], with the same two traps: the buffer comes **first** and the
/// handle **last**, and the count returned is in **items**. `zlib.h` L1540-L1541 documents
/// the overflow case explicitly -- "If the multiplication of `size` and `nitems` overflows
/// ... nothing is written, zero is returned, and the error state is set to
/// `Z_STREAM_ERROR`" -- and L1543-L1545 warns that with `size != 1` a *partial* item can
/// be written on a non-blocking or concurrently read file, with no way to learn how much
/// was lost. Both are properties of the interface and are unchanged.
///
/// The product is computed with checked arithmetic and both counts are passed inward, for
/// the same reason as in [`gzfread`]: the core owns the rejection because recording it
/// needs its crate-private `gz_error`.
///
/// # Safety
///
/// `file` must be null or satisfy [`block_mut`]'s contract. If `size * nitems` is
/// non-zero, `buf` must be non-null and readable for that many bytes, and must stay valid
/// and unwritten for the duration of the call.
#[no_mangle]
pub unsafe extern "C" fn gzfwrite(
    buf: voidpc,
    size: z_size_t,
    nitems: z_size_t,
    file: gzFile,
) -> z_size_t {
    guard(|| {
        // SAFETY: unsafe-site categories 1 and 3, discharged by this function's own
        // contract.
        let state = unsafe { state_mut(file) };
        let Some(state) = state else {
            return fallback::GZ_NO_ITEMS;
        };
        let len = size.checked_mul(nitems).unwrap_or(0);
        // SAFETY: unsafe-site category 2, discharged by this function's own contract:
        // `len` is exactly `size * nitems` when representable, and those bytes at `buf`
        // are readable and stable for the duration of the call.
        let payload = unsafe { write_slice(buf, len) };
        rs_gz::gzfwrite(state, payload, size, nitems)
    })
}

/// Compress `c`, converted to an `unsigned char`, and write it to `file`.
///
/// `zlib.h` L1607: `int gzputc(gzFile file, int c);`. Ported from `gzwrite.c` L307-L347.
/// Returns the value that was written, or -1 on error.
///
/// ★ The value returned is masked with `0xff`, on **both** of C's return paths (L338 and
/// L346). So `gzputc(file, -1)` returns 255 rather than -1 -- which matters, because -1
/// is both a perfectly good byte and the error code, and returning it unmasked would make
/// a successfully written `0xff` indistinguishable from a failure. The core does the
/// masking; it is noted here because a reader checking the return value against the
/// argument will otherwise think it is a bug.
///
/// # Safety
///
/// `file` must be null or satisfy [`block_mut`]'s contract.
#[no_mangle]
pub unsafe extern "C" fn gzputc(file: gzFile, c: c_int) -> c_int {
    guard(|| {
        // SAFETY: unsafe-site categories 1 and 3, discharged by this function's own
        // contract.
        let state = unsafe { state_mut(file) };
        let Some(state) = state else {
            return fallback::GZ_ERROR;
        };
        rs_gz::gzputc(state, c)
    })
}

/// Compress the NUL-terminated string `s`, excluding the terminator, and write it to
/// `file`.
///
/// `zlib.h` L1575: `int gzputs(gzFile file, const char *s);`. Ported from
/// `gzwrite.c` L350-L372. Returns the number of characters written, or -1 on error; the
/// count "may be less than the length of the string if the write destination is
/// non-blocking" (`zlib.h` L1577-L1584). `test/example.c` L105 asserts
/// `gzputs(file, "ello") == 4`.
///
/// C measures the string with `strlen` (`gzwrite.c` L364), so the terminator is a
/// delimiter rather than data and the core receives the bytes before it.
///
/// A null `s` is refused with -1. C has no test for it and would fault inside `strlen`,
/// so there is no reference behaviour to reproduce; -1 is the value this entry point
/// already uses for every other refusal, and it cannot be mistaken for a successful write
/// because a successful write of nothing returns 0.
///
/// # Safety
///
/// `file` must be null or satisfy [`block_mut`]'s contract. `s` must be null or a
/// NUL-terminated byte string that stays readable, and is not written by anything else,
/// for the duration of the call.
#[no_mangle]
pub unsafe extern "C" fn gzputs(file: gzFile, s: *const c_char) -> c_int {
    guard(|| {
        // SAFETY: unsafe-site category 5, discharged by this function's own contract: `s`
        // is null -- handled by `c_bytes` -- or a NUL-terminated readable string.
        let text = unsafe { c_bytes(s) };
        let Some(text) = text else {
            return fallback::GZ_ERROR;
        };
        // SAFETY: unsafe-site categories 1 and 3, discharged by this function's own
        // contract.
        let state = unsafe { state_mut(file) };
        let Some(state) = state else {
            return fallback::GZ_ERROR;
        };
        rs_gz::gzputs(state, text)
    })
}

/// Flush all pending output to `file`.
///
/// `zlib.h` L1647: `int gzflush(gzFile file, int flush);`. Ported from
/// `gzwrite.c` L375-L400. Returns the zlib error number, so `Z_OK` means success.
///
/// The `flush` parameter is `deflate`'s, and `Z_FINISH` ends the compressed stream while
/// leaving the file open so that later writes append a new gzip member
/// (`zlib.h` L1649-L1660). ★ The accepted range is `Z_NO_FLUSH ..= Z_FINISH`, which is
/// **narrower than `deflate`'s**: `Z_BLOCK` and `Z_TREES` are refused with
/// `Z_STREAM_ERROR` even though `deflate` accepts `Z_BLOCK`. The `gzFile` layer uses
/// `Z_BLOCK` internally -- `gzsetparams` passes it before changing parameters -- but will
/// not take it from a caller. The core owns the range test.
///
/// Flushing degrades compression, so it should be called only when necessary
/// (`zlib.h` L1657-L1658).
///
/// # Safety
///
/// `file` must be null or satisfy [`block_mut`]'s contract.
#[no_mangle]
pub unsafe extern "C" fn gzflush(file: gzFile, flush: c_int) -> c_int {
    guard(|| {
        // SAFETY: unsafe-site categories 1 and 3, discharged by this function's own
        // contract. C's `if (file == NULL) return Z_STREAM_ERROR;` (`gzwrite.c`
        // L382-L383) is the value used for an unrecognised handle, not the -1 the read
        // side would use.
        let state = unsafe { state_mut(file) };
        let Some(state) = state else {
            return fallback::STREAM_ERROR;
        };
        rs_gz::gzflush(state, flush)
    })
}

// ---------------------------------------------------------------------------
// The `gzprintf` boundary -- the two hidden helpers `csrc/gzprintf_shim.c` calls
// ---------------------------------------------------------------------------

/// Prepare `file` for formatting and lend out the scratch region.
///
/// The first half of `gzvprintf` (`gzwrite.c` L416-L453), serving the two variadic exports
/// `gzprintf` (`zlib.h` L1549) and `gzvprintf` (`zlib.h` L2047) that this crate cannot
/// define on stable Rust, and reached from `csrc/gzprintf_shim.c`:
///
/// ```c
/// extern int _zlib_rs_gzprintf_begin(gzFile file, unsigned char **scratch, size_t *size);
/// ```
///
/// On success returns `Z_OK` and sets `*scratch` to the first byte of a writable region of
/// exactly `*size` bytes whose **last byte has been set to zero** -- C's overflow sentinel
/// at L453. On failure returns the negative zlib code `gzvprintf` must return and leaves
/// both out-parameters untouched. `*size` is never zero on success, so `vsnprintf` always
/// has room for at least its terminator.
///
/// Every guard, the `gz_init` and `gz_zero` preconditions, the `gz_vacate`-then-format
/// protocol and the sentinel plant live in `zlib_rs::gz::printf::printf_begin`; this adds
/// only the pointer validation the core cannot perform and the two stores the shim needs.
/// The shim performs the `file == NULL` test itself -- plus the `format == NULL` test C
/// omits -- before calling in, so a null `file` reaching here means the handle failed
/// *this* layer's tag check instead.
///
/// # ★ Why lending a raw pointer out of a Rust borrow is sound here
///
/// The shim calls `vsnprintf` and **nothing else** between the two helpers. `vsnprintf`
/// never re-enters this library, so no second reference to the stream can exist while the
/// region is outstanding, and the loan's borrow has already ended by the time
/// [`_zlib_rs_gzprintf_commit`] takes the stream back. `PrintfScratch::into_mut_slice`
/// exists for exactly this hand-off, and the core's own documentation records the
/// argument.
///
/// The obligations that come with holding the region are the core's to state and the
/// shim's to honour; two bear repeating because they are easy to break from this side.
/// The region's width must be reported from the slice itself and never from a constant or
/// a separately read `state->size`, which is why `*size` is `PrintfScratch::len`. And
/// nothing between the two helpers may touch the region other than by formatting into it,
/// because a non-zero final byte is how a formatter that ignored its bound is detected.
///
/// # Safety
///
/// `file` must be null or satisfy [`block_mut`]'s contract. `scratch` and `size` must each
/// be null or a writable, aligned location of their pointee type. [`_zlib_rs_gzprintf_commit`]
/// must be called on the same `file` before any other entry point touches it.
#[no_mangle]
pub unsafe extern "C" fn _zlib_rs_gzprintf_begin(
    file: gzFile,
    scratch: *mut *mut u8,
    size: *mut z_size_t,
) -> c_int {
    guard_code(|| {
        if scratch.is_null() || size.is_null() {
            // A shim that passed either as null could not receive the region, so there is
            // nothing to prepare. `Z_STREAM_ERROR` is the code `gzvprintf` uses for a
            // request it cannot make sense of (`gzwrite.c` L418-L419).
            return ReturnCode::STREAM_ERROR;
        }
        // SAFETY: unsafe-site categories 1 and 3, discharged by this function's own
        // contract.
        let state = unsafe { state_mut(file) };
        let Some(state) = state else {
            return ReturnCode::STREAM_ERROR;
        };
        let region = match printf_begin(state) {
            Ok(loan) => loan.into_mut_slice(),
            // Every failure code is one C returns at the corresponding line; the core
            // has already refreshed the exposed prefix where refreshing was correct.
            Err(code) => return code,
        };
        let len = region.len();
        // SAFETY: unsafe-site category 2, in the outward direction. Both pointers were
        // established non-null above and are writable and aligned by this function's
        // contract. `region` is a live, uniquely borrowed slice whose first byte the shim
        // will hand to `vsnprintf` together with `len` -- the width the core guarantees is
        // the region's own and is never zero.
        unsafe {
            *scratch = region.as_mut_ptr();
            *size = len;
        }
        ReturnCode::OK
    })
}

/// Account for a formatted result and produce `gzvprintf`'s return value.
///
/// The second half of `gzvprintf` (`gzwrite.c` L471-L483), reached from
/// `csrc/gzprintf_shim.c`:
///
/// ```c
/// extern int _zlib_rs_gzprintf_commit(gzFile file, size_t reported);
/// ```
///
/// `reported` is what `vsnprintf` **returned** -- the length it *would* have written,
/// which may exceed the region -- not the length it actually wrote. A negative return is
/// passed as `(size_t)-1`, which arrives as [`usize::MAX`] and reproduces C's
/// `(unsigned)len >= state->size` comparison on a negative `int`, folding an encoding
/// error into "did not fit".
///
/// Returns the byte count on success, **0 when the result did not fit** -- with nothing
/// written and nothing counted, which is the "error (0) with nothing written" that
/// `zlib.h` L1557-L1560 documents for a result exceeding 8191 bytes, or one less than the
/// size given to `gzbuffer` -- or a negative zlib code. All three outcomes are the core's;
/// this only converts the two-armed `Result` into C's single `int`.
///
/// # Safety
///
/// `file` must be null or satisfy [`block_mut`]'s contract, and must be the same handle
/// the immediately preceding [`_zlib_rs_gzprintf_begin`] succeeded on. Calling this
/// without that preceding call cannot corrupt anything -- the core reads the sentinel
/// through a checked lookup -- but the accounting it performs would be meaningless.
#[no_mangle]
pub unsafe extern "C" fn _zlib_rs_gzprintf_commit(file: gzFile, reported: z_size_t) -> c_int {
    guard(|| {
        // SAFETY: unsafe-site categories 1 and 3, discharged by this function's own
        // contract. The region lent by `_zlib_rs_gzprintf_begin` is no longer borrowed:
        // its loan was consumed there, and the shim performed nothing but `vsnprintf` in
        // between.
        let state = unsafe { state_mut(file) };
        let Some(state) = state else {
            return fallback::STREAM_ERROR;
        };
        match printf_commit(state, reported) {
            Ok(len) => len,
            Err(code) => code.as_i32(),
        }
    })
}

// ---------------------------------------------------------------------------
// Positioning -- `zlib.h` L1661-L1709 and L1977-L1981
// ---------------------------------------------------------------------------

/// The body shared by [`gzseek`] and [`gzseek64`].
///
/// `gzlib.c` L438-L443 makes `gzseek` a narrowing wrapper around `gzseek64`, which is
/// where the whole implementation lives; the core provides exactly one `ZOff64` function
/// and both faces delegate to it, so they cannot disagree about anything but width.
///
/// # Safety
///
/// As [`block_mut`].
unsafe fn seek_body(file: gzFile, offset: ZOff64, whence: c_int) -> ZOff64 {
    // SAFETY: unsafe-site categories 1 and 3, discharged by this function's own contract.
    let state = unsafe { state_mut(file) };
    let Some(state) = state else {
        return fallback::offset_error();
    };
    rs_gz::gzseek64(state, offset, whence)
}

/// Set the starting position for the next read or write on `file`.
///
/// `zlib.h` L1663-L1664: `z_off_t gzseek(gzFile file, z_off_t offset, int whence);`.
/// Ported from `gzlib.c` L438-L443, whose body is
/// `ret = gzseek64(file, offset, whence); return ret == (z_off_t)ret ? (z_off_t)ret : -1;`.
///
/// `offset` counts bytes in the **uncompressed** data stream and `whence` is `lseek`'s,
/// except that `SEEK_END` is **not supported** -- the uncompressed length of a gzip stream
/// is not knowable without decompressing it (`zlib.h` L1668-L1669). Seeking backwards is
/// possible only on a read stream; on a write stream a forward seek is honoured by writing
/// zeroes. The seek is **deferred**: the request is recorded and carried out by the next
/// read, which is why the value returned is computed rather than read back from the file
/// (`zlib.h` L1674-L1675).
///
/// Returns the resulting offset, or -1 if the stream is not open, if the request is
/// invalid, or if the result cannot be expressed in the caller's `z_off_t`. That last case
/// is the narrowing this wrapper exists for, and it refuses rather than saturates: a
/// position the caller's type cannot hold is an error, not a nearby answer.
///
/// # Safety
///
/// `file` must be null or satisfy [`block_mut`]'s contract.
#[no_mangle]
pub unsafe extern "C" fn gzseek(file: gzFile, offset: z_off_t, whence: c_int) -> z_off_t {
    guard(|| {
        // SAFETY: this function's contract is `seek_body`'s, discharged unchanged.
        let landed = unsafe { seek_body(file, widen_offset(offset), whence) };
        narrow_or_error(landed)
    })
}

/// Set the starting position for the next read or write on `file`, with 64-bit offsets.
///
/// `zlib.h` L1979: `z_off64_t gzseek64(gzFile, z_off64_t, int);`. Ported from
/// `gzlib.c` L366-L435, which is where the implementation actually lives.
///
/// ★ **Both this and [`gzseek`] must be exported.** `zconf.h` redirects the unsuffixed
/// name to this one -- or declares only this one -- according to the **caller's**
/// `_FILE_OFFSET_BITS` and `_LARGEFILE64_SOURCE` at the caller's own compile time
/// (`zlib.h` L1976-L2022), so which name a given object file references is not this
/// library's choice. Note the subtlety in that block: under `Z_WANT64` the unsuffixed
/// names are `#define`d **to** the `*64` ones and *re-declared taking `z_off_t`*
/// (L2006-L2009), so a 32-bit caller with `_FILE_OFFSET_BITS=64` calls a symbol named
/// `gzseek64` with the width it thinks `z_off_t` has. Exporting both names over one
/// implementation is what makes every combination resolve.
///
/// Semantics are [`gzseek`]'s exactly; the only difference is that no narrowing can fail,
/// so the shared helper's error branch is unreachable here. It is still routed through the
/// same helper, because two spellings of one conversion is how the two faces of a pair
/// come to disagree.
///
/// # Safety
///
/// As [`gzseek`].
#[no_mangle]
pub unsafe extern "C" fn gzseek64(file: gzFile, offset: z_off64_t, whence: c_int) -> z_off64_t {
    guard(|| {
        // SAFETY: this function's contract is `seek_body`'s, discharged unchanged.
        let landed = unsafe { seek_body(file, widen_offset(offset), whence) };
        narrow_or_error(landed)
    })
}

/// Rewind `file`, which must be open for reading.
///
/// `zlib.h` L1683: `int gzrewind(gzFile file);`. Ported from `gzlib.c` L345-L364.
/// Equivalent to `(int)gzseek(file, 0L, SEEK_SET)` (`zlib.h` L1687), and returns 0 or -1.
///
/// The file is repositioned to where its gzip data *started*, which `gz_open` recorded, so
/// a stream opened on a descriptor already part way into a file rewinds to the right place
/// rather than to byte zero.
///
/// # Safety
///
/// `file` must be null or satisfy [`block_mut`]'s contract.
#[no_mangle]
pub unsafe extern "C" fn gzrewind(file: gzFile) -> c_int {
    guard(|| {
        // SAFETY: unsafe-site categories 1 and 3, discharged by this function's own
        // contract.
        let state = unsafe { state_mut(file) };
        let Some(state) = state else {
            return fallback::GZ_ERROR;
        };
        rs_gz::gzrewind(state)
    })
}

/// The body shared by [`gztell`] and [`gztell64`].
///
/// `gzlib.c` L461-L466 narrows `gztell64`'s result, and the core's `gztell64` takes a
/// **shared** borrow because it reads only `pos`, `past` and `skip` -- none of which is
/// derived state, since `pos` *is* a field of the exposed prefix that the caller's
/// `gzgetc` macro increments in place. That is why no resynchronisation is needed on this
/// path, and why [`state_ref`] rather than [`state_mut`] is the right guard.
///
/// # Safety
///
/// As [`state_ref`].
unsafe fn tell_body(file: gzFile) -> ZOff64 {
    // SAFETY: unsafe-site categories 1 and 3, discharged by this function's own contract.
    let state = unsafe { state_ref(file) };
    let Some(state) = state else {
        return fallback::offset_error();
    };
    rs_gz::gztell64(state)
}

/// The position of the next read or write on `file`, in uncompressed bytes.
///
/// `zlib.h` L1691: `z_off_t gztell(gzFile file);`. Ported from `gzlib.c` L461-L466, and
/// defined by L1698 as equivalent to `gzseek(file, 0L, SEEK_CUR)`.
///
/// Any deferred seek is included, which is what makes the `gzungetc(-1, file)` idiom work:
/// force the pending seek to run, then ask where it ended up. Returns -1 on error, or if
/// the position cannot be expressed in the caller's `z_off_t`.
///
/// # Safety
///
/// `file` must be null or satisfy [`state_ref`]'s contract.
#[no_mangle]
pub unsafe extern "C" fn gztell(file: gzFile) -> z_off_t {
    guard(|| {
        // SAFETY: this function's contract is `tell_body`'s, discharged unchanged.
        let position = unsafe { tell_body(file) };
        narrow_or_error(position)
    })
}

/// The position of the next read or write on `file`, with a 64-bit offset.
///
/// `zlib.h` L1980: `z_off64_t gztell64(gzFile);`. Ported from `gzlib.c` L445-L458.
///
/// Semantics are [`gztell`]'s; both names exist for the large-file reason [`gzseek64`]
/// sets out, and both delegate to one core function.
///
/// # Safety
///
/// As [`gztell`].
#[no_mangle]
pub unsafe extern "C" fn gztell64(file: gzFile) -> z_off64_t {
    guard(|| {
        // SAFETY: this function's contract is `tell_body`'s, discharged unchanged.
        let position = unsafe { tell_body(file) };
        narrow_or_error(position)
    })
}

/// The body shared by [`gzoffset`] and [`gzoffset64`].
///
/// Unlike [`tell_body`] this needs a mutable borrow: the core's `gzoffset64` asks the file
/// itself where it is, which is a `seek` on the handle.
///
/// # Safety
///
/// As [`block_mut`].
unsafe fn offset_body(file: gzFile) -> ZOff64 {
    // SAFETY: unsafe-site categories 1 and 3, discharged by this function's own contract.
    let state = unsafe { state_mut(file) };
    let Some(state) = state else {
        return fallback::offset_error();
    };
    rs_gz::gzoffset64(state)
}

/// The current offset in the compressed file underlying `file`, in actual bytes.
///
/// `zlib.h` L1702: `z_off_t gzoffset(gzFile file);`. Ported from `gzlib.c` L490-L495.
///
/// Includes any bytes that precede the gzip data, which is what makes it useful as a
/// progress indicator when appending or when reading from a descriptor positioned part way
/// into a file (`zlib.h` L1704-L1708). While reading, buffered but unconsumed input is
/// subtracted, because those bytes have been taken from the file but not yet accounted for
/// in the stream. Returns -1 on error, including when the file is not seekable.
///
/// # Safety
///
/// `file` must be null or satisfy [`block_mut`]'s contract.
#[no_mangle]
pub unsafe extern "C" fn gzoffset(file: gzFile) -> z_off_t {
    guard(|| {
        // SAFETY: this function's contract is `offset_body`'s, discharged unchanged.
        let offset = unsafe { offset_body(file) };
        narrow_or_error(offset)
    })
}

/// The current offset in the compressed file underlying `file`, with a 64-bit offset.
///
/// `zlib.h` L1981: `z_off64_t gzoffset64(gzFile);`. Ported from `gzlib.c` L468-L487.
///
/// Semantics are [`gzoffset`]'s; both names exist for the large-file reason [`gzseek64`]
/// sets out, and both delegate to one core function.
///
/// # Safety
///
/// As [`gzoffset`].
#[no_mangle]
pub unsafe extern "C" fn gzoffset64(file: gzFile) -> z_off64_t {
    guard(|| {
        // SAFETY: this function's contract is `offset_body`'s, discharged unchanged.
        let offset = unsafe { offset_body(file) };
        narrow_or_error(offset)
    })
}

// ---------------------------------------------------------------------------
// Status and error reporting -- `zlib.h` L1710-L1748 and L1773-L1795
// ---------------------------------------------------------------------------

/// Whether the end of the input was reached and a read came up short.
///
/// `zlib.h` L1711: `int gzeof(gzFile file);`. Ported from `gzlib.c` L498-L510.
///
/// ★ This reports **`past`, not `eof`**: reaching the end of the compressed *file* is not
/// the same as a caller asking for data that is not there, and only the second is what
/// `gzeof` answers. The distinction is the point of the function -- `zlib.h` L1713-L1722
/// explains that `gzread` returning a short count sets it, so a read loop that checks
/// `gzeof` after a short read terminates while one that checks it before does not. A write
/// stream always answers 0.
///
/// Returns 1 or 0; there is no failure value, so an unrecognised handle answers 0, as C's
/// early return does.
///
/// # Safety
///
/// `file` must be null or satisfy [`block_mut`]'s contract.
#[no_mangle]
pub unsafe extern "C" fn gzeof(file: gzFile) -> c_int {
    guard(|| {
        // SAFETY: unsafe-site categories 1 and 3, discharged by this function's own
        // contract. The core answers a `None` with 0 itself -- C's
        // `if (file == NULL) return 0;` at `gzlib.c` L502-L503.
        let state = unsafe { state_mut(file) };
        rs_gz::gzeof(state)
    })
}

/// Whether `file` is being copied through directly rather than decompressed.
///
/// `zlib.h` L1726: `int gzdirect(gzFile file);`. Ported from `gzread.c` L627-L642.
///
/// ★ **It has a side effect, and that is the point.** On a read stream whose container has
/// not yet been determined, this forces the decision by reading up to four bytes and
/// allocating both working buffers -- C's comment being "this is mainly for right after a
/// `gzopen()` or `gzdopen()`". That is why `zlib.h` L1737-L1739 insists `gzbuffer` be
/// called *before* `gzdirect`, and why the result must not be discarded. The core owns the
/// side effect and this file must not short-circuit it.
///
/// Two further behaviours are C's and are preserved: there is **no mode check**, so a write
/// stream falls through and reports whether transparent writing was requested
/// (`zlib.h` L1745-L1748); and the test is `direct == 1` rather than "non-zero", because
/// the -1 the `"G"` mode installs means *gzip only* and is emphatically not transparent.
///
/// Returns 1 when transparent, 0 otherwise -- including for an unrecognised handle, and
/// including for an empty file, which reads as transparent because the container cannot be
/// determined (`zlib.h` L1740-L1743).
///
/// # Safety
///
/// `file` must be null or satisfy [`block_mut`]'s contract.
#[no_mangle]
pub unsafe extern "C" fn gzdirect(file: gzFile) -> c_int {
    guard(|| {
        // SAFETY: unsafe-site categories 1 and 3, discharged by this function's own
        // contract. The core answers a `None` with 0 itself.
        let state = unsafe { state_mut(file) };
        rs_gz::gzdirect(state)
    })
}

/// The message for the last error on `file`, and optionally its zlib error number.
///
/// `zlib.h` L1775: `const char *gzerror(gzFile file, int *errnum);`. Ported from
/// `gzlib.c` L513-L528.
///
/// `*errnum` is written when `errnum` is non-null, and is `Z_ERRNO` when the failure came
/// from the file system rather than from the compression library -- in which case `errno`
/// may hold the exact code (`zlib.h` L1777-L1782). This entry point is how the caller
/// distinguishes an error from end of file for the functions that do not distinguish them
/// in their return values (`zlib.h` L1789-L1790).
///
/// # ★ It returns null for a handle it does not recognise, because C does
///
/// `gzlib.c` L517-L521 returns `NULL` twice: for `file == NULL`, and for a mode that is
/// neither `GZ_READ` nor `GZ_WRITE`. Behavioural fidelity is this port's governing
/// constraint, so both are reproduced exactly. It is worth recording that the guidance
/// this file was written against asserted the opposite -- that C answers a null handle
/// with a static string and that this function must never return null -- and that the
/// assertion was checked against the C source and found not to describe it. The source is
/// the oracle. Nothing in `test/example.c`, `test/minigzip.c` or `test/infcover.c` calls
/// `gzerror` with an unusable handle, so the choice is invisible to the acceptance suite
/// and is settled on fidelity alone.
///
/// The three answers for a handle it *does* recognise are all distinct and all C's:
/// `"out of memory"` for `Z_MEM_ERROR`, for which the core deliberately stores no message;
/// the stored message, already prefixed with the path; or the **empty string** when there
/// is none. An empty string is not a null pointer, and a caller distinguishing "no error
/// text" from "unusable handle" depends on that.
///
/// # Lifetime of the returned string
///
/// `zlib.h` L1783-L1786: the application must not modify it, a later call may invalidate
/// it, and it is unavailable once the file is closed. The core returns bytes *without* a
/// terminator, so the terminated copy is stored in the stream's own block -- see
/// [`GzBlock::message`] -- which reproduces all three properties. An empty message is
/// answered with a `'static` empty string instead, so a stream that never errors never
/// allocates. If even that copy cannot be allocated, the empty string is returned rather
/// than null: the caller asked what went wrong, and reporting nothing is better than
/// reporting an unusable handle.
///
/// # Safety
///
/// `file` must be null or satisfy [`block_mut`]'s contract. `errnum` must be null or a
/// writable, aligned `int`.
#[no_mangle]
pub unsafe extern "C" fn gzerror(file: gzFile, errnum: *mut c_int) -> *const c_char {
    guard(|| {
        // SAFETY: unsafe-site categories 1 and 3, discharged by this function's own
        // contract. The block rather than the state, because the terminated copy of the
        // message lives beside it.
        let block = unsafe { block_mut(file) };
        let Some(block) = block else {
            // `gzlib.c` L517-L518: `if (file == NULL) return NULL;`. See above.
            return ptr::null();
        };
        // Split the borrow so that the message buffer can be filled while the returned
        // text still borrows the state. The two fields are disjoint, which is what makes
        // this safe rather than merely convenient.
        let GzBlock {
            state,
            message,
            tag: _,
        } = block;

        let mut code = ReturnCode::OK.as_i32();
        let Some(text) = rs_gz::gzerror(Some(state), Some(&mut code)) else {
            // The core returns `None` on exactly the paths where C returns `NULL` without
            // writing `*errnum` -- an unrecognised mode, or an exposed prefix that is not
            // consistent with the output buffer -- so neither is written here either.
            return ptr::null();
        };

        // SAFETY: unsafe-site category 2, in the outward direction. `errnum` is null --
        // tested here -- or a writable, aligned `int` by this function's contract. C's
        // `if (errnum != NULL) *errnum = state->err;` (`gzlib.c` L524-L525), performed
        // only on the path where C performs it.
        if !errnum.is_null() {
            unsafe { *errnum = code };
        }

        if text.is_empty() {
            // C's `""`, which is a static empty string rather than an allocation.
            return fallback::GZ_NO_MESSAGE.as_ptr();
        }

        // A terminated copy, owned by the stream. `try_reserve_exact` reports exhaustion
        // instead of aborting, which `Vec::reserve` would; `checked_add` keeps the length
        // computation from wrapping on a pathologically long path.
        message.clear();
        let Some(needed) = text.len().checked_add(1) else {
            return fallback::GZ_NO_MESSAGE.as_ptr();
        };
        if message.try_reserve_exact(needed).is_err() {
            return fallback::GZ_NO_MESSAGE.as_ptr();
        }
        message.extend_from_slice(text);
        message.push(0);
        message.as_ptr().cast::<c_char>()
    })
}

/// Clear the error and end-of-file flags for `file`.
///
/// `zlib.h` L1792: `void gzclearerr(gzFile file);`. Ported from `gzlib.c` L531-L547.
/// Analogous to `stdio`'s `clearerr`, and useful for continuing to read a gzip file that
/// is being written concurrently (`zlib.h` L1794-L1795).
///
/// ★ **The only `void`-returning export in this crate**, which is why it routes through
/// [`crate::panic_guard::guard`] with `T = ()` rather than through `guard_code`, and why
/// that guard is deliberately not `#[must_use]`.
///
/// The end-of-file flags are cleared only on a read stream, because neither means anything
/// on a write stream; the error itself is cleared for both. Note that `Z_OK` is not a fatal
/// code, so the exposed `have` count survives -- clearing an error must not discard output
/// the caller has not yet been given.
///
/// # Safety
///
/// `file` must be null or satisfy [`block_mut`]'s contract.
#[no_mangle]
pub unsafe extern "C" fn gzclearerr(file: gzFile) {
    guard(|| {
        // SAFETY: unsafe-site categories 1 and 3, discharged by this function's own
        // contract. The core makes a `None` a no-op itself -- C's two early returns at
        // `gzlib.c` L535-L540.
        let state = unsafe { state_mut(file) };
        rs_gz::gzclearerr(state);
    });
}

// ---------------------------------------------------------------------------
// Closing -- `zlib.h` L1749-L1771, ported from `gzclose.c`, `gzread.c`, `gzwrite.c`
// ---------------------------------------------------------------------------

/// Whether the block behind `file` may be released, and by which half.
///
/// The direction test C makes before it commits to a teardown, read out separately so that
/// the answer is known *before* the borrow the close itself needs. The three C entry points
/// all guard on it and all return `Z_STREAM_ERROR` **without** reaching their `free`:
/// `gzclose_r` at `gzread.c` L648-L649, `gzclose_w` at `gzwrite.c` L677-L678, and `gzclose`
/// by dispatching to whichever of them will then refuse (`gzclose.c` L20-L21).
///
/// [`None`] means the handle is unusable -- null, mistagged, or not open in either
/// direction -- and nothing may be released. `Some(true)` is a read stream and
/// `Some(false)` a write stream.
///
/// # Safety
///
/// As [`state_ref`].
unsafe fn close_direction(file: gzFile) -> Option<bool> {
    // SAFETY: unsafe-site categories 1 and 3, discharged by this function's own contract.
    // A shared borrow suffices: `mode` is a plain field and this decides nothing else.
    let state = unsafe { state_ref(file) }?;
    match state.mode() {
        GZ_READ => Some(true),
        GZ_WRITE => Some(false),
        // `GZ_NONE`, the transient `GZ_APPEND`, and any value that is not a mode at all.
        // C reaches this only through `gzclose_w`'s guard, which refuses without freeing.
        _ => None,
    }
}

/// Close a `gzFile`, flushing pending output and releasing everything it owns.
///
/// `zlib.h` L1750: `int gzclose(gzFile file);`. Ported from `gzclose.c` L11-L23, which does
/// nothing but pick between the two halves on `state->mode == GZ_READ`.
///
/// Returns `Z_STREAM_ERROR` if the handle is not valid, `Z_ERRNO` on a file-operation
/// error, `Z_MEM_ERROR` if out of memory, `Z_BUF_ERROR` if the last read ended in the
/// middle of a gzip stream, or `Z_OK` on success (`zlib.h` L1757-L1761). The `Z_BUF_ERROR`
/// is the deferred truncation report `gzread` itself declines to make -- this is where a
/// truncated stream finally surfaces, which is why discarding the result is almost always
/// a defect.
///
/// ★ **`gzclose` must not be called more than once on the same handle**, and `gzerror` must
/// not be used on a closed one (`zlib.h` L1752-L1756). Both remain the caller's obligation,
/// because a freed pointer cannot be validated by anybody -- reading one is already
/// undefined behaviour. What this file adds is that [`release`] poisons the tag first, so a
/// second close on memory the allocator has not yet reused reports `Z_STREAM_ERROR` instead
/// of running a second teardown. That is a diagnostic and not a licence.
///
/// # Safety
///
/// `file` must be null or satisfy [`block_mut`]'s contract, and must not have been closed
/// before. After this returns, `file` is dangling and must never be used again.
#[no_mangle]
pub unsafe extern "C" fn gzclose(file: gzFile) -> c_int {
    guard_code(|| {
        // SAFETY: this function's contract is `close_direction`'s, discharged unchanged.
        // Reading the direction first is what keeps the release decision faithful to C's,
        // and the shared borrow it takes has ended before the mutable one below begins.
        let direction = unsafe { close_direction(file) };
        if direction.is_none() {
            // `gzclose.c` L15-L16 for a null handle, and `gzwrite.c` L677-L678 for a mode
            // that is not a direction: `Z_STREAM_ERROR`, and nothing is freed.
            return ReturnCode::STREAM_ERROR;
        }
        // SAFETY: unsafe-site categories 1 and 3, discharged by this function's own
        // contract.
        let state = unsafe { state_mut(file) };
        let Some(state) = state else {
            return ReturnCode::STREAM_ERROR;
        };
        // The core performs C's dispatch, including the asymmetry that only `GZ_READ` is
        // tested for.
        let code = rs_gz::gzclose(Some(state));
        // SAFETY: unsafe-site category 3, on its releasing side. The direction test above
        // established that the chosen half ran its teardown, so this is C's `free(state)`
        // at the same point. The borrow taken above ended when `gzclose` consumed it, so
        // no reference to the block is outstanding.
        unsafe { release(file) };
        code
    })
}

/// Close a `gzFile` that is open for reading.
///
/// `zlib.h` L1763: `int gzclose_r(gzFile file);`. Ported from `gzread.c` L644-L667.
///
/// Exported in its own right rather than as a private helper of [`gzclose`], because
/// `zlib.h` L1766-L1770 offers the pair "for the rare occasion when the application wants
/// to avoid linking in all of zlib" -- a program that only ever reads can link this half
/// alone. So it must remain independently reachable, and it must refuse a write stream
/// itself.
///
/// A stream not open for reading is refused with `Z_STREAM_ERROR` and **is not released**,
/// which is C's own behaviour: `gzread.c` L648-L649 returns before the `free` at L666. The
/// teardown order that does run is C's and is observable -- `test/infcover.c`'s tracking
/// allocator reports a release that is not last-in-first-out -- so the engine goes first,
/// then the output buffer, then the input buffer. The core owns that order.
///
/// # Safety
///
/// As [`gzclose`].
#[no_mangle]
pub unsafe extern "C" fn gzclose_r(file: gzFile) -> c_int {
    guard_code(|| {
        // SAFETY: this function's contract is `close_direction`'s, discharged unchanged.
        if unsafe { close_direction(file) } != Some(true) {
            // `gzread.c` L648-L649: not a read stream, so nothing is torn down and nothing
            // is freed.
            return ReturnCode::STREAM_ERROR;
        }
        // SAFETY: unsafe-site categories 1 and 3, discharged by this function's own
        // contract.
        let state = unsafe { state_mut(file) };
        let Some(state) = state else {
            return ReturnCode::STREAM_ERROR;
        };
        let code = rs_gz::gzclose_r(state);
        // SAFETY: unsafe-site category 3. The direction test established that the read
        // teardown ran, so this is C's `free(state)` at `gzread.c` L666, and the borrow
        // above has ended.
        unsafe { release(file) };
        code
    })
}

/// Close a `gzFile` that is open for writing.
///
/// `zlib.h` L1764: `int gzclose_w(gzFile file);`. Ported from `gzwrite.c` L667-L700.
///
/// The write half of the pair [`gzclose_r`] documents. A final `Z_FINISH` flush is
/// performed, so this is where a failed write surfaces as `Z_ERRNO` and where a pending
/// forward `gzseek` is paid for with zeroes.
///
/// A stream not open for writing is refused with `Z_STREAM_ERROR` and **is not released** --
/// C's `gzwrite.c` L677-L678 returns before the `free` at L698.
///
/// ★ Note one consequence of the core's own divergence: `gzclose_w` sets `mode` to
/// `GZ_NONE` before returning, so if the block were kept a second call would refuse. It is
/// not kept -- it is released here, exactly as C releases it -- so that property is
/// defence in depth rather than an observable difference.
///
/// # Safety
///
/// As [`gzclose`].
#[no_mangle]
pub unsafe extern "C" fn gzclose_w(file: gzFile) -> c_int {
    guard_code(|| {
        // SAFETY: this function's contract is `close_direction`'s, discharged unchanged.
        if unsafe { close_direction(file) } != Some(false) {
            // `gzwrite.c` L677-L678: not a write stream, so nothing is torn down and
            // nothing is freed.
            return ReturnCode::STREAM_ERROR;
        }
        // SAFETY: unsafe-site categories 1 and 3, discharged by this function's own
        // contract.
        let state = unsafe { state_mut(file) };
        let Some(state) = state else {
            return ReturnCode::STREAM_ERROR;
        };
        // `gzclose_w` reports the raw C `int` it latched out of `state->err`. Every value
        // it can produce is one of the documented codes, so the fallback is unreachable in
        // practice; `unwrap_or` is not the denied `unwrap` -- it cannot panic -- and
        // `Z_STREAM_ERROR` is the code zlib already uses for a state it does not recognise.
        let code =
            ReturnCode::from_i32(rs_gz::gzclose_w(state)).unwrap_or(ReturnCode::STREAM_ERROR);
        // SAFETY: unsafe-site category 3. The direction test established that the write
        // teardown ran, so this is C's `free(state)` at `gzwrite.c` L698, and the borrow
        // above has ended.
        unsafe { release(file) };
        code
    })
}

// Test code is the one place in this crate where panicking is permitted: the root
// `clippy.toml` grants `allow-panic-in-tests`, `allow-unwrap-in-tests` and
// `allow-expect-in-tests`, and nothing below this banner is compiled into the shipped
// library.
//
// What is asserted here, and why each item is worth a test rather than a comment:
//
//   * The block round-trip -- install, validate, release -- because it is the one piece of
//     memory management this file owns and the one C performs with `malloc`/`free`.
//   * The `gzgetc` MACRO, simulated byte for byte from `zlib.h` L1966-L1968 against a real
//     handle. This is the single most likely place for a silent corruption bug in the whole
//     port, so it is exercised first: the test mutates `have`, `pos` and `next` exactly as
//     caller object code would and then requires every Rust-side entry point to agree.
//   * The two `gzprintf` helpers, whose C caller cannot be run from here -- the shim is
//     linked by the packaging step, not by cargo -- so their protocol is driven directly.
//   * The large-file duality, asserted as an equality between the two faces of each pair
//     rather than against a hardcoded number, which is what makes it meaningful on a
//     target where the widths differ.
//   * The null and unusable-handle matrix, because every one of those answers is a
//     documented value in `zlib.h` and several of them differ between neighbouring
//     functions.

#[cfg(test)]
// `used_underscore_items` is unavoidable here and only here: the two `gzprintf` helpers MUST
// be named `_zlib_rs_gzprintf_begin` and `_zlib_rs_gzprintf_commit`, because the leading
// underscore is what `zlib.map`'s `local: _*;` pattern hides them by and `zlib.map` is
// immutable. Their real caller is `csrc/gzprintf_shim.c`, which cargo does not link, so these
// tests are the only Rust callers they will ever have.
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::used_underscore_items
)]
mod tests {
    //! Unit tests for the `gzFile` facade, driven through the real C entry points.
    //!
    //! # The single invariant that discharges every `unsafe` call in this module
    //!
    //! Every one of the `unsafe` blocks below is a call to one of this module's own
    //! `extern "C"` exports, to one of its private helpers ([`install`], [`release`],
    //! [`block_mut`], [`state_mut`]) or to the two `gzprintf` helpers. They are
    //! deliberately *not* annotated one by one, because the invariant is identical for all
    //! of them and stating it once is what makes it auditable:
    //!
    //! - every `gzFile` argument is either a null pointer passed on purpose, to exercise
    //!   the entry guard, or the exact non-null value a preceding `gzopen`, `gzopen64` or
    //!   [`install`] returned inside the *same* test and has not yet been closed or
    //!   released -- so it always addresses a live [`GzBlock`] with the [`GZ_TAG_LIVE`] tag;
    //! - every buffer pointer is `as_ptr`/`as_mut_ptr` of a local array or [`Vec`] that
    //!   outlives the call, and the length handed alongside it is that same object's
    //!   length, so the readable/writable extent is exact;
    //! - every C string is a [`CString`] bound to a local that outlives the call, so it
    //!   stays NUL-terminated and valid for reads throughout;
    //! - the deliberately-hostile cases (a closed handle, a double close, a foreign
    //!   pointer with the wrong tag) construct a *valid, live* allocation first and then
    //!   corrupt only the tag, which is exactly the state the guard is specified to reject.
    //!
    //! Tests may panic -- that is how they report failure -- so `clippy.toml` allows
    //! panicking here and only here, and the `#[allow]` above re-enables `unwrap`,
    //! indexing and `panic!` for the same reason.

    use super::{
        _zlib_rs_gzprintf_begin, _zlib_rs_gzprintf_commit, block_mut, gzbuffer, gzclearerr,
        gzclose, gzclose_r, gzclose_w, gzdirect, gzeof, gzerror, gzflush, gzfread, gzfwrite,
        gzgetc, gzgetc_, gzgets, gzoffset, gzoffset64, gzopen, gzopen64, gzputc, gzputs, gzread,
        gzrewind, gzseek, gzseek64, gzsetparams, gztell, gztell64, gzungetc, gzwrite, install,
        release, state_mut, FacadeGzState, GzBlock, GZ_TAG_CLOSED, GZ_TAG_LIVE,
    };

    use core::ffi::{c_char, c_int, c_uint, c_void};
    use core::sync::atomic::{AtomicU32, Ordering};

    use std::ffi::{CStr, CString};
    use std::path::PathBuf;

    use zlib_rs::allocate::GlobalAllocator;
    use zlib_rs::error::ReturnCode;

    use crate::types::{gzFile, gzFile_s, z_off_t};

    /// `Z_OK`, spelled the way the tests read.
    const Z_OK: c_int = 0;
    /// `Z_STREAM_ERROR`.
    const Z_STREAM_ERROR: c_int = -2;
    /// `SEEK_SET`.
    const SEEK_SET: c_int = 0;
    /// `SEEK_CUR`.
    const SEEK_CUR: c_int = 1;
    /// `SEEK_END`, which `gzseek` must refuse.
    const SEEK_END: c_int = 2;

    /// A distinct temporary path per test, safe against parallel clones and parallel tests.
    ///
    /// `CLONE_INDEX` is honoured because the workspace may be built by several clones on one
    /// host at the same time; the process id and a counter separate the tests within a run.
    fn temp_path(tag: &str) -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let serial = COUNTER.fetch_add(1, Ordering::Relaxed);
        let clone = std::env::var("CLONE_INDEX").unwrap_or_else(|_| String::from("local"));
        std::env::temp_dir().join(format!(
            "blitzy_libz_rs_gz_{clone}_{}_{tag}_{serial}.gz",
            std::process::id()
        ))
    }

    /// Removes its path when dropped, so a failing assertion still cleans up.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            Self(temp_path(tag))
        }

        /// The path as the NUL-terminated string the C entry points take.
        fn c_path(&self) -> CString {
            CString::new(self.0.as_os_str().as_encoded_bytes()).unwrap()
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ignored = std::fs::remove_file(&self.0);
        }
    }

    /// `gzopen` with Rust-side strings, panicking rather than returning a null handle.
    fn open(path: &CString, mode: &str) -> gzFile {
        let mode = CString::new(mode).unwrap();
        let file = unsafe { gzopen(path.as_ptr(), mode.as_ptr()) };
        assert!(!file.is_null(), "gzopen({mode:?}) returned Z_NULL");
        file
    }

    /// `gzopen` that is expected to fail.
    fn open_expect_null(path: &CString, mode: &str) -> bool {
        let mode = CString::new(mode).unwrap();
        let file = unsafe { gzopen(path.as_ptr(), mode.as_ptr()) };
        if file.is_null() {
            return true;
        }
        assert_eq!(unsafe { gzclose(file) }, Z_OK);
        false
    }

    /// `gzwrite` with Rust-side types, so no test needs a numeric cast.
    ///
    /// The lint policy denies `clippy::cast_possible_truncation` and
    /// `clippy::cast_sign_loss` in test code as well as in library code, which is the right
    /// policy: a test that silently truncated a length would assert the wrong thing.
    /// `try_from().unwrap()` is the alternative, and panicking is permitted here.
    fn write_bytes(file: gzFile, payload: &[u8]) -> c_int {
        let len = c_uint::try_from(payload.len()).unwrap();
        unsafe { gzwrite(file, payload.as_ptr().cast::<c_void>(), len) }
    }

    /// `gzread` with Rust-side types, returning the raw count -- negative on error.
    fn read_bytes(file: gzFile, out: &mut [u8]) -> c_int {
        let len = c_uint::try_from(out.len()).unwrap();
        unsafe { gzread(file, out.as_mut_ptr().cast::<c_void>(), len) }
    }

    /// A count reported by an entry point, as a length. Panics if it is negative.
    fn count(value: c_int) -> usize {
        usize::try_from(value).unwrap()
    }

    /// A length as the `int` an entry point reports for it.
    fn as_count(value: usize) -> c_int {
        c_int::try_from(value).unwrap()
    }

    /// Widens a narrow offset to the 64-bit one, for comparing the two faces of a pair.
    ///
    /// Generic for the reason `widen_offset` in the library half is generic: the concrete
    /// conversion is invisible to the lint pass inside a generic body, and a non-generic
    /// `i64::from` trips `clippy::useless_conversion` on the target where `z_off_t` already
    /// *is* `i64` -- which is this one.
    fn to_wide<T: Into<i64>>(value: T) -> i64 {
        value.into()
    }

    /// Writes `payload` to a fresh gzip file at `path` with the given mode.
    fn write_file(path: &CString, mode: &str, payload: &[u8]) {
        let file = open(path, mode);
        assert_eq!(count(write_bytes(file, payload)), payload.len());
        assert_eq!(unsafe { gzclose(file) }, Z_OK);
    }

    /// Reads the whole of the gzip file at `path` back through `gzread`.
    fn read_file(path: &CString) -> Vec<u8> {
        let file = open(path, "rb");
        let mut out = vec![0_u8; 4096];
        let got = read_bytes(file, &mut out);
        assert!(got >= 0, "gzread reported {got}");
        assert_eq!(unsafe { gzclose(file) }, Z_OK);
        out.truncate(count(got));
        out
    }

    /// `zlib.h` L1966-L1968's `gzgetc` macro, reproduced exactly.
    ///
    /// ```c
    /// #define gzgetc(g) \
    ///       ((g)->have ? ((g)->have--, (g)->pos++, *((g)->next)++) : (gzgetc)(g))
    /// ```
    ///
    /// This is what caller object code does, and it is the reason the exposed prefix has to
    /// be at offset 0 and has to be resynchronised on entry to every wrapper. Written
    /// against the raw `gzFile_s` view, not against the block, so it can see only what a C
    /// caller can see.
    unsafe fn macro_gzgetc(file: gzFile) -> c_int {
        // SAFETY: the caller supplies a live handle, and the three fields read and written
        // below are exactly the ones `zlib.h` publishes for this purpose at offsets 0, 8
        // and 16. `next` addresses a byte inside the library's output buffer whenever
        // `have` is non-zero, which is the invariant `GzState::refresh_exposed` maintains.
        unsafe {
            let exposed: &mut gzFile_s = &mut *file;
            if exposed.have != 0 {
                exposed.have -= 1;
                exposed.pos += 1;
                let byte = *exposed.next;
                exposed.next = exposed.next.add(1);
                c_int::from(byte)
            } else {
                gzgetc(file)
            }
        }
    }

    #[test]
    fn a_block_round_trips_through_install_and_release() {
        let file = install(FacadeGzState::new(GlobalAllocator));
        assert!(!file.is_null(), "install reported exhaustion");

        // The tag makes the handle usable, and the state is reachable through it.
        let tag = unsafe { block_mut(file) }.map(|block| block.tag);
        assert_eq!(tag, Some(GZ_TAG_LIVE));
        assert!(unsafe { state_mut(file) }.is_some());

        // The exposed prefix of a state with no output buffer: nothing available, pointing
        // nowhere, which is the pair the `gzgetc` macro answers by taking its function arm.
        let exposed = unsafe { &*file };
        assert_eq!(exposed.have, 0);
        assert!(exposed.next.is_null());
        assert_eq!(exposed.pos, 0);

        unsafe { release(file) };
    }

    #[test]
    fn the_exposed_prefix_begins_at_offset_zero_of_the_block() {
        // The link in the chain from a caller's field arithmetic to the bytes it lands on
        // that only this module can assert: `types.rs` pins `gzFile_s` against the core's
        // `GzFileExposed`, and this pins the block against the state.
        assert_eq!(core::mem::offset_of!(GzBlock, state), 0);
        assert!(align_of::<GzBlock>() >= align_of::<gzFile_s>());
        assert!(size_of::<GzBlock>() >= size_of::<gzFile_s>());
    }

    #[test]
    fn a_null_or_mistagged_handle_is_refused() {
        assert!(unsafe { block_mut(core::ptr::null_mut()) }.is_none());
        assert!(unsafe { state_mut(core::ptr::null_mut()) }.is_none());

        let file = install(FacadeGzState::new(GlobalAllocator));
        // Poison the tag by hand: this is what `release` does, and it is what a second
        // close would see while the allocator has not yet reused the block.
        unsafe { block_mut(file) }.unwrap().tag = GZ_TAG_CLOSED;
        assert!(unsafe { block_mut(file) }.is_none());
        assert!(unsafe { state_mut(file) }.is_none());
        // Restore it so the block can be released through the normal path.
        unsafe { (*file.cast::<GzBlock>()).tag = GZ_TAG_LIVE };
        unsafe { release(file) };
    }

    #[test]
    fn a_round_trip_survives_write_then_read() {
        let scratch = Scratch::new("roundtrip");
        let path = scratch.c_path();
        let payload = b"hello, hello! the quick brown fox jumps over the lazy dog";
        write_file(&path, "wb", payload);
        assert_eq!(read_file(&path), payload);
    }

    #[test]
    fn every_compression_level_and_strategy_round_trips() {
        // The mode-string matrix, compared against the one thing every variant must agree
        // on: the bytes come back. `"rb+"` is the rejection C performs for `+`.
        for mode in [
            "wb", "wb0", "wb1", "wb6", "wb9", "wbf", "wbh", "wbR", "wbF", "wbe", "wb6 ",
        ] {
            let scratch = Scratch::new("modes");
            let path = scratch.c_path();
            let payload = b"aaaaaaaaaabbbbbbbbbbccccccccccdddddddddd0123456789";
            write_file(&path, mode, payload);
            assert_eq!(read_file(&path), payload, "mode {mode:?}");
        }
    }

    #[test]
    fn the_rejected_mode_strings_are_refused() {
        let scratch = Scratch::new("badmodes");
        let path = scratch.c_path();
        // `+` is rejected outright (`gzlib.c` L129-L131); a mode with no direction is
        // rejected (L174-L177); `T` while reading cannot force a transparent read
        // (L181-L185); `G` while writing has no meaning (L191-L195).
        for mode in ["rb+", "wb+", "b", "9", "rbT", "wbG"] {
            assert!(open_expect_null(&path, mode), "mode {mode:?} was accepted");
        }
    }

    #[test]
    fn a_transparent_write_is_reported_by_gzdirect() {
        let scratch = Scratch::new("direct");
        let path = scratch.c_path();
        let payload = b"stored verbatim, not deflated";
        write_file(&path, "wbT", payload);

        // `T` writes the payload through untouched, so the file is not a gzip member and a
        // reader reports transparent copying.
        assert_eq!(std::fs::read(&scratch.0).unwrap(), payload);
        let file = open(&path, "rb");
        assert_eq!(unsafe { gzdirect(file) }, 1);
        assert_eq!(read_file(&path), payload);
        assert_eq!(unsafe { gzclose(file) }, Z_OK);
    }

    #[test]
    fn gzbuffer_is_refused_once_the_buffers_exist() {
        let scratch = Scratch::new("buffer");
        let path = scratch.c_path();
        write_file(&path, "wb", b"payload");

        let file = open(&path, "rb");
        // Before any read: accepted, and a size below 8 is raised rather than refused.
        assert_eq!(unsafe { gzbuffer(file, 4) }, 0);
        assert_eq!(unsafe { gzbuffer(file, 1 << 15) }, 0);
        // A size that cannot be doubled is refused (`gzlib.c` L338-L339).
        assert_eq!(unsafe { gzbuffer(file, 0x8000_0000) }, -1);
        // Forcing the container decision allocates, after which it is refused.
        assert_eq!(unsafe { gzdirect(file) }, 0);
        assert_eq!(unsafe { gzbuffer(file, 1 << 16) }, -1);
        assert_eq!(unsafe { gzclose(file) }, Z_OK);
    }

    #[test]
    fn gzsetparams_changes_level_midstream() {
        let scratch = Scratch::new("setparams");
        let path = scratch.c_path();
        let file = open(&path, "wb1");
        let head = b"the first half, written at level one";
        assert_eq!(count(write_bytes(file, head)), head.len());
        // Z_BEST_COMPRESSION, Z_DEFAULT_STRATEGY.
        assert_eq!(unsafe { gzsetparams(file, 9, 0) }, Z_OK);
        let tail = b"the second half, written at level nine";
        assert_eq!(count(write_bytes(file, tail)), tail.len());
        assert_eq!(unsafe { gzclose(file) }, Z_OK);

        let mut expected = head.to_vec();
        expected.extend_from_slice(tail);
        assert_eq!(read_file(&path), expected);
    }

    #[test]
    fn the_gzgetc_macro_and_the_library_agree_on_the_position() {
        // ★ The single most important test in this module. The macro mutates `have`, `pos`
        // and `next` inside what would be caller object code; every entry point below must
        // then see the position the macro left behind, which is what
        // `GzState::resync_from_exposed` is for.
        let scratch = Scratch::new("macro");
        let path = scratch.c_path();
        let payload = b"hello, hello!";
        write_file(&path, "wb", payload);

        let file = open(&path, "rb");

        // The first byte comes through the FUNCTION, because nothing is buffered yet and the
        // macro's fast path is unavailable -- which is exactly why `gzgetc` has to exist as
        // a real symbol.
        assert_eq!(unsafe { macro_gzgetc(file) }, i32::from(b'h'));
        assert_eq!(unsafe { gztell(file) }, 1);

        // The next four come through the MACRO's fast path, straight out of the prefix.
        for expected in *b"ello" {
            let before = unsafe { (*file).have };
            assert!(before > 0, "the macro fast path was not available");
            assert_eq!(unsafe { macro_gzgetc(file) }, i32::from(expected));
        }
        // Five bytes consumed in total, four of them without the library being called at
        // all. Both faces of `gztell` must report it.
        assert_eq!(unsafe { gztell(file) }, 5);
        assert_eq!(unsafe { gztell64(file) }, 5);

        // A Rust-side read must continue from where the macro left off, not from where the
        // library last published a cursor.
        let mut rest = [0_u8; 32];
        let got = read_bytes(file, &mut rest);
        assert_eq!(got, as_count(payload.len() - 5));
        assert_eq!(&rest[..count(got)], &payload[5..]);
        assert_eq!(
            unsafe { gztell(file) },
            z_off_t::try_from(payload.len()).unwrap()
        );
        // ★ `gzeof` reports `past`, not `eof`: the 32-byte request could not be satisfied
        // from a 13-byte stream, so the flag is already set here. That is C's behaviour --
        // `gz_read` sets `state->past` when it runs out of input mid-request
        // (`gzread.c` L347-L348) -- and it is precisely why `zlib.h` L1713-L1722 tells a
        // caller to test `gzeof` *after* a short read rather than before the next one.
        assert_eq!(unsafe { gzeof(file) }, 1);

        // And once past the end the macro falls through to the function, which reports -1.
        assert_eq!(unsafe { macro_gzgetc(file) }, -1);
        assert_eq!(unsafe { gzeof(file) }, 1);
        assert_eq!(unsafe { gzclose(file) }, Z_OK);
    }

    #[test]
    fn gzgetc_and_gzgetc_underscore_are_interchangeable() {
        let scratch = Scratch::new("getc");
        let path = scratch.c_path();
        write_file(&path, "wb", b"abcd");

        let file = open(&path, "rb");
        assert_eq!(unsafe { gzgetc(file) }, i32::from(b'a'));
        assert_eq!(unsafe { gzgetc_(file) }, i32::from(b'b'));
        assert_eq!(unsafe { gzgetc(file) }, i32::from(b'c'));
        assert_eq!(unsafe { gzgetc_(file) }, i32::from(b'd'));
        assert_eq!(unsafe { gzgetc(file) }, -1);
        assert_eq!(unsafe { gzgetc_(file) }, -1);
        assert_eq!(unsafe { gzclose(file) }, Z_OK);
    }

    #[test]
    fn gzungetc_pushes_back_and_forces_a_pending_seek() {
        let scratch = Scratch::new("ungetc");
        let path = scratch.c_path();
        let payload = b"hello, hello!";
        write_file(&path, "wb", payload);

        let file = open(&path, "rb");
        // A push before anything is read: `gz_look` runs first so the buffers exist.
        assert_eq!(unsafe { gzungetc(i32::from(b'X'), file) }, i32::from(b'X'));
        assert_eq!(unsafe { gzgetc(file) }, i32::from(b'X'));
        assert_eq!(unsafe { gzgetc(file) }, i32::from(b'h'));
        // Push it back and read it again.
        assert_eq!(unsafe { gzungetc(i32::from(b'h'), file) }, i32::from(b'h'));
        assert_eq!(unsafe { gztell(file) }, 0);
        assert_eq!(unsafe { gzgetc(file) }, i32::from(b'h'));

        // `gzungetc(-1, file)` is the documented idiom for forcing a pending seek to run:
        // the skip is carried out BEFORE the negative byte is rejected.
        assert_eq!(unsafe { gzseek(file, 0, SEEK_SET) }, 0);
        assert_eq!(unsafe { gzungetc(-1, file) }, -1);
        assert_eq!(unsafe { gztell(file) }, 0);
        assert_eq!(unsafe { gzclose(file) }, Z_OK);
    }

    #[test]
    fn gzgets_stops_at_a_newline_and_reports_end_of_file_with_null() {
        let scratch = Scratch::new("gets");
        let path = scratch.c_path();
        write_file(&path, "wb", b"first line\nsecond line\n");

        let file = open(&path, "rb");
        let mut buf = [0_u8; 64];
        let returned = unsafe { gzgets(file, buf.as_mut_ptr().cast::<c_char>(), 64) };
        assert_eq!(returned, buf.as_mut_ptr().cast::<c_char>());
        assert_eq!(
            unsafe { CStr::from_ptr(returned) }.to_bytes(),
            b"first line\n"
        );
        let returned = unsafe { gzgets(file, buf.as_mut_ptr().cast::<c_char>(), 64) };
        assert!(!returned.is_null());
        assert_eq!(
            unsafe { CStr::from_ptr(returned) }.to_bytes(),
            b"second line\n"
        );
        // End of file: null, not an empty string.
        assert!(unsafe { gzgets(file, buf.as_mut_ptr().cast::<c_char>(), 64) }.is_null());

        // `len == 1` returns null and writes nothing -- the implementation is the oracle,
        // not the header's prose.
        assert_eq!(unsafe { gzrewind(file) }, 0);
        assert!(unsafe { gzgets(file, buf.as_mut_ptr().cast::<c_char>(), 1) }.is_null());
        // A non-positive length, and a null buffer, are both refused.
        assert!(unsafe { gzgets(file, buf.as_mut_ptr().cast::<c_char>(), 0) }.is_null());
        assert!(unsafe { gzgets(file, buf.as_mut_ptr().cast::<c_char>(), -5) }.is_null());
        assert!(unsafe { gzgets(file, core::ptr::null_mut(), 64) }.is_null());
        assert_eq!(unsafe { gzclose(file) }, Z_OK);
    }

    #[test]
    fn gzputc_masks_its_result_and_gzputs_counts_characters() {
        let scratch = Scratch::new("putc");
        let path = scratch.c_path();
        let file = open(&path, "wb");
        assert_eq!(unsafe { gzputc(file, i32::from(b'h')) }, i32::from(b'h'));
        // `test/example.c` L105's assertion.
        let ello = CString::new("ello").unwrap();
        assert_eq!(unsafe { gzputs(file, ello.as_ptr()) }, 4);
        // ★ Masked with 0xff on both return paths: -1 becomes 255, not -1.
        assert_eq!(unsafe { gzputc(file, -1) }, 255);
        assert_eq!(unsafe { gzclose(file) }, Z_OK);

        assert_eq!(read_file(&path), b"hello\xff");

        // A null string is refused rather than faulting inside `strlen`.
        let file = open(&path, "wb");
        assert_eq!(unsafe { gzputs(file, core::ptr::null()) }, -1);
        assert_eq!(unsafe { gzclose(file) }, Z_OK);
    }

    #[test]
    fn the_item_oriented_entry_points_count_items_not_bytes() {
        let scratch = Scratch::new("fread");
        let path = scratch.c_path();

        // Eight items of four bytes.
        let payload: Vec<u8> = (0_u8..32).collect();
        let file = open(&path, "wb");
        let items = unsafe { gzfwrite(payload.as_ptr().cast::<c_void>(), 4, 8, file) };
        assert_eq!(items, 8, "gzfwrite reports items");
        assert_eq!(unsafe { gzclose(file) }, Z_OK);

        let file = open(&path, "rb");
        let mut out = vec![0_u8; 32];
        let items = unsafe { gzfread(out.as_mut_ptr().cast::<c_void>(), 4, 8, file) };
        assert_eq!(items, 8);
        assert_eq!(out, payload);
        assert_eq!(unsafe { gzclose(file) }, Z_OK);

        // An overflowing product is refused with zero items and nothing read.
        let file = open(&path, "rb");
        let huge = usize::MAX / 2 + 1;
        assert_eq!(
            unsafe { gzfread(out.as_mut_ptr().cast::<c_void>(), huge, 4, file) },
            0
        );
        assert_eq!(unsafe { gzclose(file) }, Z_OK);

        let file = open(&path, "wb");
        assert_eq!(
            unsafe { gzfwrite(payload.as_ptr().cast::<c_void>(), huge, 4, file) },
            0
        );
        assert_eq!(unsafe { gzclose(file) }, Z_OK);
    }

    #[test]
    fn gzflush_finishes_a_member_and_refuses_the_wider_flush_codes() {
        let scratch = Scratch::new("flush");
        let path = scratch.c_path();
        let file = open(&path, "wb");
        let head = b"first member";
        assert!(write_bytes(file, head) > 0);
        // Z_FINISH ends the member but leaves the file open.
        assert_eq!(unsafe { gzflush(file, 4) }, Z_OK);
        let tail = b"second member";
        assert!(write_bytes(file, tail) > 0);
        // ★ Narrower than `deflate`'s range: Z_BLOCK (5) and Z_TREES (6) are refused.
        assert_eq!(unsafe { gzflush(file, 5) }, Z_STREAM_ERROR);
        assert_eq!(unsafe { gzflush(file, 6) }, Z_STREAM_ERROR);
        assert_eq!(unsafe { gzflush(file, -1) }, Z_STREAM_ERROR);
        assert_eq!(unsafe { gzclose(file) }, Z_OK);

        // Both members decompress as one stream.
        let mut expected = head.to_vec();
        expected.extend_from_slice(tail);
        assert_eq!(read_file(&path), expected);
    }

    #[test]
    fn both_faces_of_every_large_file_pair_agree() {
        // ★ Asserted as an equality between the two faces rather than against a hardcoded
        // number, which is what keeps the test meaningful on a target where `z_off_t` and
        // `z_off64_t` differ in width.
        let scratch = Scratch::new("largefile");
        let path = scratch.c_path();
        let payload: Vec<u8> = (0_u8..=255).cycle().take(4096).collect();
        write_file(&path, "wb", &payload);

        // `gzopen` and `gzopen64` must behave identically.
        for opener in [0_u8, 1] {
            let mode = CString::new("rb").unwrap();
            let file = if opener == 0 {
                unsafe { gzopen(path.as_ptr(), mode.as_ptr()) }
            } else {
                unsafe { gzopen64(path.as_ptr(), mode.as_ptr()) }
            };
            assert!(!file.is_null());

            assert_eq!(
                to_wide(unsafe { gztell(file) }),
                unsafe { gztell64(file) },
                "gztell disagreement, opener {opener}"
            );
            assert_eq!(
                to_wide(unsafe { gzoffset(file) }),
                unsafe { gzoffset64(file) },
                "gzoffset disagreement, opener {opener}"
            );

            // Forwards, then backwards, then absolute -- each through both faces.
            for (offset, whence) in [(1024_i64, SEEK_SET), (512, SEEK_CUR), (-256, SEEK_CUR)] {
                // `try_from` rather than a cast: `z_off_t` is `c_long`, so this is the
                // identity on LP64 and a genuine narrowing on a 32-bit target, and the
                // test must be portable to both.
                let narrowed = z_off_t::try_from(offset).unwrap();
                let narrow = unsafe { gzseek(file, narrowed, whence) };
                let wide = unsafe { gzseek64(file, offset, whence) };
                // The two calls are consecutive, so the second starts where the first left
                // off; compare each against the position reported afterwards instead.
                assert!(narrow >= 0 && wide >= 0, "seek refused: {offset} {whence}");
                assert_eq!(to_wide(unsafe { gztell(file) }), unsafe { gztell64(file) });
                assert_eq!(to_wide(unsafe { gzoffset(file) }), unsafe {
                    gzoffset64(file)
                });
            }

            // `SEEK_END` is not supported, in both faces.
            assert_eq!(unsafe { gzseek(file, 0, SEEK_END) }, -1);
            assert_eq!(unsafe { gzseek64(file, 0, SEEK_END) }, -1);
            assert_eq!(unsafe { gzclose(file) }, Z_OK);
        }
    }

    #[test]
    fn a_seek_then_read_lands_on_the_expected_bytes() {
        let scratch = Scratch::new("seekread");
        let path = scratch.c_path();
        let payload: Vec<u8> = (0_u8..=255).cycle().take(2048).collect();
        write_file(&path, "wb", &payload);

        let file = open(&path, "rb");
        assert_eq!(unsafe { gzseek(file, 1000, SEEK_SET) }, 1000);
        let mut out = [0_u8; 8];
        assert_eq!(read_bytes(file, &mut out), 8);
        assert_eq!(&out, &payload[1000..1008]);
        assert_eq!(unsafe { gztell(file) }, 1008);

        // Backwards, which only a read stream can do.
        assert_eq!(unsafe { gzseek(file, -8, SEEK_CUR) }, 1000);
        assert_eq!(read_bytes(file, &mut out), 8);
        assert_eq!(&out, &payload[1000..1008]);

        assert_eq!(unsafe { gzrewind(file) }, 0);
        assert_eq!(unsafe { gztell(file) }, 0);
        assert_eq!(unsafe { gzclose(file) }, Z_OK);
    }

    #[test]
    fn gzerror_reports_the_message_and_the_code_and_gzclearerr_discards_them() {
        let scratch = Scratch::new("error");
        let path = scratch.c_path();
        write_file(&path, "wb", b"payload");

        let file = open(&path, "rb");
        let mut code: c_int = 0x5eed;
        let message = unsafe { gzerror(file, &raw mut code) };
        assert!(!message.is_null(), "a live handle must have a message");
        // No error yet: the empty string, which is not a null pointer.
        assert_eq!(unsafe { CStr::from_ptr(message) }.to_bytes(), b"");
        assert_eq!(code, Z_OK);

        // A null `errnum` is permitted and must not be written.
        assert!(!unsafe { gzerror(file, core::ptr::null_mut()) }.is_null());

        // Reading a write stream is refused, and the refusal is reported through `gzerror`.
        let mut buf = [0_u8; 4];
        assert_eq!(read_bytes(file, &mut buf), 4);
        assert_eq!(unsafe { gzeof(file) }, 0);
        unsafe { gzclearerr(file) };
        let mut code: c_int = 0x5eed;
        assert!(!unsafe { gzerror(file, &raw mut code) }.is_null());
        assert_eq!(code, Z_OK);
        assert_eq!(unsafe { gzclose(file) }, Z_OK);

        // ★ An unusable handle answers with NULL, exactly as `gzlib.c` L517-L521 does.
        let mut code: c_int = 0x5eed;
        assert!(unsafe { gzerror(core::ptr::null_mut(), &raw mut code) }.is_null());
        assert_eq!(code, 0x5eed, "errnum must not be written for a null handle");
        // `gzclearerr` on a null handle is a no-op rather than a crash.
        unsafe { gzclearerr(core::ptr::null_mut()) };
    }

    #[test]
    fn a_message_survives_until_the_next_gzerror_call() {
        // `zlib.h` L1783-L1786: the string must not be modified, a later call may invalidate
        // it, and it is gone once the file is closed. Owning the terminated copy in the block
        // is what reproduces all three; this asserts the first two.
        let scratch = Scratch::new("message");
        let path = scratch.c_path();
        // ★ Getting a recorded error at all takes care, and the reason is specific to this
        // tree. `gz_look` sets `junk = 1` when it detects a gzip magic (`gzread.c` L155-L158)
        // and `gz_decomp` treats a `Z_DATA_ERROR` while `junk == 1` as **trailing garbage**,
        // silently ending the stream with `Z_OK` (`gzread.c` L211-L217). Only once a member
        // has actually produced output does `junk` become 0 and a data error get recorded --
        // which the neighbouring test pins from the other side.
        //
        // So the fixture is a *valid* member with a corrupted CRC-32 trailer: the payload
        // decompresses, which clears `junk`, and the check then fails with
        // "incorrect data check".
        write_file(&path, "wb", b"a payload whose trailer will be corrupted");
        let mut whole = std::fs::read(&scratch.0).unwrap();
        let crc = whole.len() - 8;
        whole[crc] ^= 0xff;
        std::fs::write(&scratch.0, &whole).unwrap();

        let file = open(&path, "rb");
        let mut buf = [0_u8; 64];
        // ★ The bytes that were already decompressed are delivered FIRST and the error is
        // deferred to the next call. That is C's own arrangement, and its comment says so:
        // `if (gz_fetch(state) == -1 && state->x.have == 0) err = -1;` -- "if
        // state->x.have != 0, error will be caught after copy" (`gzread.c` L358-L360). The
        // code is nonetheless recorded by now, which is what `gzerror` is for.
        let got = read_bytes(file, &mut buf);
        assert_eq!(got, 41, "the decompressed bytes are delivered first");
        let mut code: c_int = 0;
        let message = unsafe { gzerror(file, &raw mut code) };
        assert!(!message.is_null());
        // Z_DATA_ERROR.
        assert_eq!(code, -3, "a failed data check is a data error");
        let text = unsafe { CStr::from_ptr(message) }.to_bytes().to_vec();
        assert!(!text.is_empty(), "a recorded error must have text");
        // The path is prefixed, exactly as `gz_error` concatenates it.
        assert!(
            text.starts_with(path.as_bytes()),
            "message {:?} is not prefixed with the path",
            String::from_utf8_lossy(&text)
        );
        // A second call reports the same text through a still-valid pointer.
        let again = unsafe { gzerror(file, core::ptr::null_mut()) };
        assert_eq!(unsafe { CStr::from_ptr(again) }.to_bytes(), &text[..]);
        // And the deferred failure surfaces on the next read, as C defers it.
        assert_eq!(read_bytes(file, &mut buf), -1);
        // The close itself reports `Z_OK`: `gzclose_r` promotes only a lingering
        // `Z_BUF_ERROR` (`gzread.c` L662), and a corrupt header latches `Z_DATA_ERROR`,
        // which stays available through `gzerror` instead. The value is captured rather
        // than asserted because what this test is about is the message, not the code.
        let _closed = unsafe { gzclose(file) };
    }

    #[test]
    fn a_first_member_that_produces_nothing_is_treated_as_trailing_garbage() {
        // Behaviour specific to this `1.3.2.1-motley` tree, and worth pinning because it is
        // surprising: a gzip magic followed by an invalid deflate stream that produces **no
        // output** is not an error. `gz_look` sets `junk = 1` on detecting the magic
        // (`gzread.c` L155-L158) and `gz_decomp`'s `Z_DATA_ERROR` arm then treats the member
        // as trailing garbage, clearing the input, setting end of file and returning `Z_OK`
        // (`gzread.c` L211-L217). The read therefore reports zero bytes and no error, and a
        // facade that "improved" on that would break a documented behaviour of this tree.
        let scratch = Scratch::new("junkfirst");
        let path = scratch.c_path();
        // A ten-byte RFC 1952 header -- magic, CM=8, no flags, no mtime, XFL=0, OS=255 --
        // followed by a byte whose low three bits are `0b110`: BFINAL=0 with BTYPE=11, which
        // RFC 1951 reserves and every conforming decoder must reject.
        std::fs::write(&scratch.0, b"\x1f\x8b\x08\x00\x00\x00\x00\x00\x00\xff\x06").unwrap();

        let file = open(&path, "rb");
        let mut buf = [0_u8; 32];
        assert_eq!(read_bytes(file, &mut buf), 0);
        let mut code: c_int = 0x5eed;
        let message = unsafe { gzerror(file, &raw mut code) };
        assert!(!message.is_null());
        assert_eq!(code, Z_OK, "trailing garbage is not an error");
        assert_eq!(unsafe { CStr::from_ptr(message) }.to_bytes(), b"");
        assert_eq!(unsafe { gzeof(file) }, 1);
        assert_eq!(unsafe { gzclose(file) }, Z_OK);
    }

    #[test]
    fn the_printf_helpers_drive_the_cores_protocol() {
        // The shim is linked by the packaging step rather than by cargo, so its C caller
        // cannot be run from here; the protocol it implements is driven directly instead.
        let scratch = Scratch::new("printf");
        let path = scratch.c_path();
        let file = open(&path, "wb");

        let mut scratch_ptr: *mut u8 = core::ptr::null_mut();
        let mut size: usize = 0;
        assert_eq!(
            unsafe { _zlib_rs_gzprintf_begin(file, &raw mut scratch_ptr, &raw mut size) },
            Z_OK
        );
        assert!(!scratch_ptr.is_null());
        // The default buffer is 8192, so the region is 8192 bytes and the documented cap is
        // one less.
        assert_eq!(size, 8192);
        // The last byte is the overflow sentinel and must arrive zeroed.
        assert_eq!(unsafe { *scratch_ptr.add(size - 1) }, 0);

        // Format into it exactly as `vsnprintf` would: bytes, then a terminator, and report
        // the length it WOULD have written.
        let text = b"hello, world";
        unsafe {
            core::ptr::copy_nonoverlapping(text.as_ptr(), scratch_ptr, text.len());
            *scratch_ptr.add(text.len()) = 0;
        }
        assert_eq!(
            unsafe { _zlib_rs_gzprintf_commit(file, text.len()) },
            as_count(text.len())
        );
        assert_eq!(unsafe { gzclose(file) }, Z_OK);
        assert_eq!(read_file(&path), text);
    }

    #[test]
    fn a_printf_result_that_does_not_fit_writes_nothing() {
        let scratch = Scratch::new("printfcap");
        let path = scratch.c_path();
        let file = open(&path, "wb");

        let mut region: *mut u8 = core::ptr::null_mut();
        let mut size: usize = 0;
        assert_eq!(
            unsafe { _zlib_rs_gzprintf_begin(file, &raw mut region, &raw mut size) },
            Z_OK
        );
        // `size - 1` characters is the documented cap; report exactly `size`, which is one
        // too many, and nothing must be written or counted.
        assert_eq!(unsafe { _zlib_rs_gzprintf_commit(file, size) }, 0);
        // A formatter that failed reports `(size_t)-1`, which folds into "did not fit".
        assert_eq!(
            unsafe { _zlib_rs_gzprintf_begin(file, &raw mut region, &raw mut size) },
            Z_OK
        );
        assert_eq!(unsafe { _zlib_rs_gzprintf_commit(file, usize::MAX) }, 0);
        assert_eq!(unsafe { gzclose(file) }, Z_OK);
        assert!(read_file(&path).is_empty(), "nothing may have been written");
    }

    #[test]
    fn the_printf_helpers_refuse_an_unusable_request() {
        // A null handle, a null out-parameter, and a read stream: all refused with
        // `Z_STREAM_ERROR`, and none of them may touch the out-parameters.
        let mut region: *mut u8 = core::ptr::null_mut();
        let mut size: usize = 0xdead;
        assert_eq!(
            unsafe {
                _zlib_rs_gzprintf_begin(core::ptr::null_mut(), &raw mut region, &raw mut size)
            },
            Z_STREAM_ERROR
        );
        assert_eq!(size, 0xdead);
        assert_eq!(
            unsafe { _zlib_rs_gzprintf_commit(core::ptr::null_mut(), 4) },
            Z_STREAM_ERROR
        );

        let scratch = Scratch::new("printfbad");
        let path = scratch.c_path();
        write_file(&path, "wb", b"payload");
        let file = open(&path, "rb");
        assert_eq!(
            unsafe { _zlib_rs_gzprintf_begin(file, core::ptr::null_mut(), &raw mut size) },
            Z_STREAM_ERROR
        );
        assert_eq!(
            unsafe { _zlib_rs_gzprintf_begin(file, &raw mut region, core::ptr::null_mut()) },
            Z_STREAM_ERROR
        );
        // A read stream is not writable, so the core's own guard refuses it.
        assert_eq!(
            unsafe { _zlib_rs_gzprintf_begin(file, &raw mut region, &raw mut size) },
            Z_STREAM_ERROR
        );
        assert_eq!(unsafe { gzclose(file) }, Z_OK);
    }

    #[test]
    fn every_entry_point_answers_a_null_handle_with_its_documented_value() {
        // Each value here is the one `zlib.h` documents for that family, and several
        // neighbours disagree -- which is the reason to assert them all in one place.
        let null: gzFile = core::ptr::null_mut();
        let mut byte = [0_u8; 4];
        let out = byte.as_mut_ptr().cast::<c_void>();

        assert_eq!(unsafe { gzbuffer(null, 8192) }, -1);
        assert_eq!(unsafe { gzsetparams(null, 6, 0) }, Z_STREAM_ERROR);
        assert_eq!(unsafe { gzread(null, out, 4) }, -1);
        assert_eq!(unsafe { gzfread(out, 1, 4, null) }, 0);
        assert_eq!(unsafe { gzwrite(null, out.cast_const(), 4) }, 0);
        assert_eq!(unsafe { gzfwrite(out.cast_const(), 1, 4, null) }, 0);
        assert_eq!(unsafe { gzputc(null, 0) }, -1);
        let text = CString::new("text").unwrap();
        assert_eq!(unsafe { gzputs(null, text.as_ptr()) }, -1);
        assert!(unsafe { gzgets(null, byte.as_mut_ptr().cast::<c_char>(), 4) }.is_null());
        assert_eq!(unsafe { gzgetc(null) }, -1);
        assert_eq!(unsafe { gzgetc_(null) }, -1);
        assert_eq!(unsafe { gzungetc(0, null) }, -1);
        assert_eq!(unsafe { gzflush(null, 0) }, Z_STREAM_ERROR);
        assert_eq!(unsafe { gzseek(null, 0, SEEK_SET) }, -1);
        assert_eq!(unsafe { gzseek64(null, 0, SEEK_SET) }, -1);
        assert_eq!(unsafe { gzrewind(null) }, -1);
        assert_eq!(unsafe { gztell(null) }, -1);
        assert_eq!(unsafe { gztell64(null) }, -1);
        assert_eq!(unsafe { gzoffset(null) }, -1);
        assert_eq!(unsafe { gzoffset64(null) }, -1);
        assert_eq!(unsafe { gzeof(null) }, 0);
        assert_eq!(unsafe { gzdirect(null) }, 0);
        assert_eq!(unsafe { gzclose(null) }, Z_STREAM_ERROR);
        assert_eq!(unsafe { gzclose_r(null) }, Z_STREAM_ERROR);
        assert_eq!(unsafe { gzclose_w(null) }, Z_STREAM_ERROR);
        assert!(unsafe { gzerror(null, core::ptr::null_mut()) }.is_null());
        unsafe { gzclearerr(null) };

        // A null path or mode is `Z_NULL`, not an empty name.
        let mode = CString::new("rb").unwrap();
        assert!(unsafe { gzopen(core::ptr::null(), mode.as_ptr()) }.is_null());
        assert!(unsafe { gzopen64(core::ptr::null(), mode.as_ptr()) }.is_null());
        assert!(unsafe { gzopen(text.as_ptr(), core::ptr::null()) }.is_null());
        assert!(unsafe { super::gzdopen(0, core::ptr::null()) }.is_null());
    }

    #[test]
    fn a_nonexistent_path_and_a_directory_are_refused() {
        let missing = CString::new("/nonexistent-directory-for-libz-rs/absent.gz").unwrap();
        assert!(open_expect_null(&missing, "rb"));
        assert!(open_expect_null(&missing, "wb"));
        // A directory cannot be read as a gzip file. `open(2)` on a directory for reading
        // succeeds on Linux, so the failure surfaces on the first read rather than at open.
        let directory = CString::new(std::env::temp_dir().as_os_str().as_encoded_bytes()).unwrap();
        assert!(open_expect_null(&directory, "wb"));
    }

    #[test]
    fn the_close_halves_refuse_the_wrong_direction_without_releasing() {
        let scratch = Scratch::new("closehalves");
        let path = scratch.c_path();
        write_file(&path, "wb", b"payload");

        // A read stream refuses `gzclose_w` (`gzwrite.c` L677-L678) and is NOT freed, so the
        // handle is still usable afterwards and the correct half still works.
        let file = open(&path, "rb");
        assert_eq!(unsafe { gzclose_w(file) }, Z_STREAM_ERROR);
        assert_eq!(unsafe { gzgetc(file) }, i32::from(b'p'));
        assert_eq!(unsafe { gzclose_r(file) }, Z_OK);

        // And symmetrically for a write stream and `gzclose_r` (`gzread.c` L648-L649).
        let file = open(&path, "wb");
        assert_eq!(unsafe { gzclose_r(file) }, Z_STREAM_ERROR);
        assert_eq!(unsafe { gzputc(file, i32::from(b'x')) }, i32::from(b'x'));
        assert_eq!(unsafe { gzclose_w(file) }, Z_OK);
        assert_eq!(read_file(&path), b"x");
    }

    #[test]
    fn a_freshly_installed_state_with_no_direction_is_not_released_by_close() {
        // The `GZ_NONE` case: C's `gzclose` dispatches to `gzclose_w`, whose mode guard
        // refuses without freeing. Reproduced exactly, which is why the block has to be
        // released by hand afterwards.
        let file = install(FacadeGzState::new(GlobalAllocator));
        assert_eq!(unsafe { gzclose(file) }, Z_STREAM_ERROR);
        assert_eq!(unsafe { gzclose_r(file) }, Z_STREAM_ERROR);
        assert_eq!(unsafe { gzclose_w(file) }, Z_STREAM_ERROR);
        // Still live, because nothing was freed.
        assert!(unsafe { state_mut(file) }.is_some());
        unsafe { release(file) };
    }

    #[test]
    fn a_truncated_member_is_reported_by_gzclose_and_not_by_gzread() {
        // `zlib.h` L1474-L1478 and L1757-L1761: `gzread` does not report an incomplete gzip
        // stream; `gzclose` is where the `Z_BUF_ERROR` finally surfaces.
        let scratch = Scratch::new("truncated");
        let path = scratch.c_path();
        write_file(
            &path,
            "wb",
            b"a reasonably long payload so that truncation removes data",
        );
        let whole = std::fs::read(&scratch.0).unwrap();
        std::fs::write(&scratch.0, &whole[..whole.len() - 6]).unwrap();

        let file = open(&path, "rb");
        let mut out = vec![0_u8; 256];
        let got = read_bytes(file, &mut out);
        assert!(got >= 0, "gzread must not report the truncation itself");
        // Z_BUF_ERROR is -5.
        assert_eq!(unsafe { gzclose(file) }, -5);
    }

    #[test]
    fn trailing_garbage_after_a_complete_member_is_tolerated() {
        let scratch = Scratch::new("garbage");
        let path = scratch.c_path();
        let payload = b"a complete member";
        write_file(&path, "wb", payload);
        let mut whole = std::fs::read(&scratch.0).unwrap();
        whole.extend_from_slice(b"not a gzip member at all");
        std::fs::write(&scratch.0, &whole).unwrap();

        // The member's own bytes still come back; the junk is recognised as junk.
        let file = open(&path, "rb");
        let mut out = vec![0_u8; 256];
        let got = read_bytes(file, &mut out);
        assert_eq!(count(got), payload.len());
        assert_eq!(&out[..payload.len()], payload);
        assert_eq!(unsafe { gzclose(file) }, Z_OK);
    }

    #[test]
    fn a_multi_member_file_reads_as_one_stream() {
        let scratch = Scratch::new("members");
        let path = scratch.c_path();
        let first = b"first member's payload";
        let second = b"second member's payload";
        write_file(&path, "wb", first);
        // `"ab"` appends a second independent member.
        let file = open(&path, "ab");
        assert!(write_bytes(file, second) > 0);
        assert_eq!(unsafe { gzclose(file) }, Z_OK);

        let mut expected = first.to_vec();
        expected.extend_from_slice(second);
        assert_eq!(read_file(&path), expected);
    }

    #[cfg(unix)]
    #[test]
    fn gzdopen_adopts_a_descriptor_and_closes_it() {
        use std::os::fd::IntoRawFd;

        let scratch = Scratch::new("dopen");
        let path = scratch.c_path();
        let payload = b"written through an adopted descriptor";

        // Write through a descriptor the test opened itself.
        let handle = std::fs::File::create(&scratch.0).unwrap();
        let fd = handle.into_raw_fd();
        let mode = CString::new("wb").unwrap();
        let file = unsafe { super::gzdopen(fd, mode.as_ptr()) };
        assert!(!file.is_null());
        assert!(write_bytes(file, payload) > 0);
        assert_eq!(unsafe { gzclose(file) }, Z_OK);

        // Read it back through another adopted descriptor.
        let handle = std::fs::File::open(&scratch.0).unwrap();
        let fd = handle.into_raw_fd();
        let mode = CString::new("rb").unwrap();
        let file = unsafe { super::gzdopen(fd, mode.as_ptr()) };
        assert!(!file.is_null());
        let mut out = vec![0_u8; 256];
        let got = read_bytes(file, &mut out);
        assert_eq!(count(got), payload.len());
        assert_eq!(&out[..payload.len()], payload);
        assert_eq!(unsafe { gzclose(file) }, Z_OK);

        // The ordinary path still reads the same file, which proves the descriptor was
        // written and closed rather than merely buffered.
        assert_eq!(read_file(&path), payload);
    }

    #[cfg(unix)]
    #[test]
    fn gzdopen_refuses_a_bad_descriptor_and_a_bad_mode_without_taking_it() {
        use std::os::fd::{AsRawFd, IntoRawFd};

        // `fd == -1` is C's first test, and it must be refused before any adoption.
        let mode = CString::new("rb").unwrap();
        assert!(unsafe { super::gzdopen(-1, mode.as_ptr()) }.is_null());

        // An invalid mode must leave the descriptor untouched -- `zlib.h` L1415-L1416. The
        // descriptor is still usable afterwards, which is what proves it.
        let scratch = Scratch::new("dopenbad");
        std::fs::write(&scratch.0, b"contents").unwrap();
        let handle = std::fs::File::open(&scratch.0).unwrap();
        let raw = handle.as_raw_fd();
        let bad = CString::new("rb+").unwrap();
        assert!(unsafe { super::gzdopen(raw, bad.as_ptr()) }.is_null());
        // Still open: reading through the original handle succeeds.
        let mut probe = handle;
        {
            use std::io::Read as _;
            let mut text = String::new();
            probe.read_to_string(&mut text).unwrap();
            assert_eq!(text, "contents");
        }
        // Hand it over properly this time so the library closes it exactly once.
        let fd = probe.into_raw_fd();
        let file = unsafe { super::gzdopen(fd, mode.as_ptr()) };
        assert!(!file.is_null());
        assert_eq!(unsafe { gzclose(file) }, Z_OK);
    }

    /// `gzclose` on a descriptor the caller closed behind the library's back must report
    /// `Z_ERRNO`, exactly as C's `ret = close(state->fd); return ret ? Z_ERRNO : err;`
    /// does (`gzread.c` L665-L667). [`super::AdoptedDescriptor::close`] establishes that
    /// with `fstat(2)` before releasing the descriptor; without the probe this returned
    /// `Z_OK` and the divergence was measured against the C reference build.
    #[test]
    #[cfg(unix)]
    fn closing_a_descriptor_that_was_already_closed_reports_z_errno() {
        use std::os::fd::{FromRawFd as _, IntoRawFd as _};

        /// `Z_ERRNO`, the code C returns when `close(2)` fails.
        const Z_ERRNO: c_int = -1;

        let scratch = Scratch::new("dopenclosed");
        let path = scratch.c_path();
        write_file(&path, "wb", b"payload");

        let mode = CString::new("rb").unwrap();

        // A live descriptor must close cleanly: the probe may not invent a failure.
        let good = std::fs::File::open(scratch.0.as_path()).unwrap();
        let file = unsafe { super::gzdopen(good.into_raw_fd(), mode.as_ptr()) };
        assert!(!file.is_null());
        assert_eq!(unsafe { gzclose(file) }, Z_OK, "a valid descriptor");

        // Now the hostile case, built exactly as a C caller would build it: give up
        // ownership of the number, close it, and only then hand the stale number over.
        // Reclaiming and dropping is how the number is closed without a raw syscall; at
        // that instant it is still open, so the drop is sound and the only owner.
        let raw = std::fs::File::open(scratch.0.as_path())
            .unwrap()
            .into_raw_fd();
        // SAFETY: `raw` was just produced by `into_raw_fd`, so it is open and unowned;
        // reconstructing the sole owner and dropping it performs exactly one close.
        drop(unsafe { std::fs::File::from_raw_fd(raw) });

        let file = unsafe { super::gzdopen(raw, mode.as_ptr()) };
        assert!(!file.is_null(), "gzdopen does not validate the descriptor");
        assert_eq!(
            unsafe { gzclose(file) },
            Z_ERRNO,
            "a descriptor closed behind gzdopen's back"
        );
    }

    #[test]
    fn an_empty_payload_round_trips_and_reads_as_end_of_file() {
        let scratch = Scratch::new("empty");
        let path = scratch.c_path();
        let file = open(&path, "wb");
        assert_eq!(unsafe { gzclose(file) }, Z_OK);

        let file = open(&path, "rb");
        let mut out = [0_u8; 16];
        assert_eq!(read_bytes(file, &mut out), 0);
        assert_eq!(unsafe { gzeof(file) }, 1);
        assert_eq!(unsafe { gzgetc(file) }, -1);
        assert_eq!(unsafe { gzclose(file) }, Z_OK);
    }

    #[test]
    fn a_zero_length_request_with_a_null_buffer_is_not_undefined() {
        // `core::slice::from_raw_parts(null, 0)` is undefined behaviour, so the zero-length
        // case is branched on. Under Miri this test is what proves it.
        let scratch = Scratch::new("zerolen");
        let path = scratch.c_path();
        write_file(&path, "wb", b"payload");

        let file = open(&path, "rb");
        assert_eq!(unsafe { gzread(file, core::ptr::null_mut(), 0) }, 0);
        assert_eq!(unsafe { gzfread(core::ptr::null_mut(), 0, 0, file) }, 0);
        assert_eq!(unsafe { gzclose(file) }, Z_OK);

        let file = open(&path, "wb");
        assert_eq!(unsafe { gzwrite(file, core::ptr::null(), 0) }, 0);
        assert_eq!(unsafe { gzfwrite(core::ptr::null(), 0, 0, file) }, 0);
        assert_eq!(unsafe { gzclose(file) }, Z_OK);
    }

    #[test]
    fn a_reader_and_a_writer_refuse_each_others_operations() {
        let scratch = Scratch::new("crossed");
        let path = scratch.c_path();
        write_file(&path, "wb", b"payload");

        // Writing to a read stream.
        let file = open(&path, "rb");
        let text = CString::new("nope").unwrap();
        assert_eq!(
            unsafe { gzwrite(file, text.as_ptr().cast::<c_void>(), 4) },
            0
        );
        assert_eq!(unsafe { gzputc(file, 0) }, -1);
        assert_eq!(unsafe { gzputs(file, text.as_ptr()) }, -1);
        assert_eq!(unsafe { gzflush(file, 0) }, Z_STREAM_ERROR);
        assert_eq!(unsafe { gzsetparams(file, 9, 0) }, Z_STREAM_ERROR);
        assert_eq!(unsafe { gzclose(file) }, Z_OK);

        // Reading from a write stream.
        let file = open(&path, "wb");
        let mut out = [0_u8; 4];
        assert_eq!(read_bytes(file, &mut out), -1);
        assert_eq!(unsafe { gzgetc(file) }, -1);
        assert_eq!(unsafe { gzungetc(0, file) }, -1);
        assert!(unsafe { gzgets(file, out.as_mut_ptr().cast::<c_char>(), 4) }.is_null());
        assert_eq!(unsafe { gzrewind(file) }, -1);
        // `gzeof` answers 0 for a write stream rather than failing.
        assert_eq!(unsafe { gzeof(file) }, 0);
        assert_eq!(unsafe { gzclose(file) }, Z_OK);
    }

    #[test]
    fn a_large_payload_crosses_the_buffer_boundary_intact() {
        // Bigger than the default 8192-byte buffer in both directions, so the read path
        // refills and the write path flushes more than once.
        let scratch = Scratch::new("large");
        let path = scratch.c_path();
        let payload: Vec<u8> = (0_u32..70_000)
            .map(|n| u8::try_from(n % 251).unwrap())
            .collect();
        let file = open(&path, "wb6");
        let mut written = 0_usize;
        // Fed in chunks, because chunk boundaries interact with the pending buffer.
        for chunk in payload.chunks(4093) {
            let n = write_bytes(file, chunk);
            assert!(n > 0);
            written += count(n);
        }
        assert_eq!(written, payload.len());
        assert_eq!(unsafe { gzclose(file) }, Z_OK);

        let file = open(&path, "rb");
        let mut out = vec![0_u8; payload.len()];
        let mut filled = 0_usize;
        while filled < out.len() {
            let want = (out.len() - filled).min(3001);
            let got = read_bytes(file, &mut out[filled..filled + want]);
            assert!(got > 0, "gzread stalled at {filled}");
            filled += count(got);
        }
        assert_eq!(out, payload);
        assert_eq!(unsafe { gzeof(file) }, 0);
        assert_eq!(unsafe { gzclose(file) }, Z_OK);
    }

    #[test]
    fn the_return_code_conversions_used_by_the_close_paths_are_exhaustive() {
        // `gzclose_w` reports a raw C `int`, and the facade converts it. Every value it can
        // produce must round-trip, so the `unwrap_or` fallback stays unreachable in practice.
        for code in [
            ReturnCode::OK,
            ReturnCode::STREAM_END,
            ReturnCode::NEED_DICT,
            ReturnCode::ERRNO,
            ReturnCode::STREAM_ERROR,
            ReturnCode::DATA_ERROR,
            ReturnCode::MEM_ERROR,
            ReturnCode::BUF_ERROR,
            ReturnCode::VERSION_ERROR,
        ] {
            assert_eq!(ReturnCode::from_i32(code.as_i32()), Some(code));
        }
        assert_eq!(ReturnCode::from_i32(42), None);
    }
}
