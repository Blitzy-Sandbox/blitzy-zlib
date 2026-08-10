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
//! `vsnprintf` call. This crate's `build.rs` compiles it — with the platform
//! compiler, taking no `cc` dependency — and emits `-l static=` for the archive it
//! goes into, which rustc merges into the `libz.a` cargo produces. So the archive
//! defines both names, and the packaged shared object is relinked from that same
//! archive; a C compiler is consequently required to build this crate with
//! `libz-compat` and `gz`, and `build.rs` says so rather than quietly omitting them.
//! The artifact matrix in this crate's `lib.rs` states which artifact carries what.
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
//! `crates/libz-rs-sys/src/types.rs` pins the agreement between
//! [`gzFile_s`](crate::gzFile_s) and
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
//! **None.** `zlib_rs::gz::open`'s documentation carries the core's; this boundary now
//! has no entry of its own, and the four it used to carry are recorded below with what
//! closed them.
//!
//! One reference quirk is reproduced rather than corrected, and it is listed here because
//! it looks like a defect on inspection: C passes `O_CLOEXEC` -- an *open* flag -- to
//! `F_SETFD`, which defines only `FD_CLOEXEC`, so on Linux the `e` mode character has no
//! effect on an *adopted* descriptor. [`apply_cloexec`] passes the same argument C passes
//! and carries the measurement. Making `e` work there would be a behaviour change, and
//! this port does not make behaviour changes -- not even improvements.
//!
//! ## Four divergences this module used to carry, and why they are gone
//!
//! Recorded because they were real, were documented as forced, and were resolved by
//! one change: the descriptor layer stopped going through `std::fs::File` and now
//! makes the same five calls C makes -- `open`, `read`, `write`, `LSEEK`, `close` --
//! plus the two `fcntl` adjustments, through `libc`. See the note above
//! [`Descriptor`] for why every one of them is observable through the public API, and
//! `Cargo.toml`'s `gz` feature for why the dependency belongs to this layer alone.
//!
//! * **`errno` after a failed `gzopen`.** `zlib.h` L1394-L1401 promises it can be
//!   checked, and specifically that with `N` in the mode it "will be `EAGAIN` or
//!   `ENONBLOCK`". [`sys_open`] is now the failing call, and nothing between it and
//!   `gzopen`'s `return NULL` touches `errno`, so the value a caller reads is the one
//!   `open(2)` set. It used to be an observation about what `std` happened to do.
//! * **The `e` mode character on an adopted descriptor.** [`apply_cloexec`] now makes
//!   C's `fcntl(fd, F_SETFD, ...)` call, with C's argument; see the quirk noted above.
//!   The divergence was that no `fcntl` was made at all.
//! * **`O_NONBLOCK` on an adopted descriptor.** [`Descriptor::set_nonblocking`]
//!   performs C's `fcntl(fd, F_SETFL, fcntl(fd, F_GETFL) | O_NONBLOCK)`
//!   (`gzlib.c` L254-L257); the core drives it and discards the result exactly as C
//!   discards `fcntl`'s.
//! * **`gzdopen` off Unix.** It works everywhere now. A Windows `int fd` is a CRT
//!   file descriptor rather than a `HANDLE`, which is why `std::fs::File` could not
//!   adopt one -- but `libc` binds the CRT's `_open`, `_read`, `_write`, `_lseeki64`
//!   and `_close`, which are the very functions C's own gz layer calls there, so the
//!   same [`Descriptor`] serves both platforms. `x86_64-pc-windows-gnu` is compiled in
//!   CI to keep that arm honest; it is checked, not run, because the port's verified
//!   Tier-1 target is Linux.
//!
//! A fifth is resolved on this side of the boundary rather than removed from the
//! core's list. Divergence 1 of `zlib_rs::gz::open` -- `close(2)`'s result being
//! unobservable, so `gzclose` can only report the flush -- ends here: the core resolves
//! it by stating that "a stream whose `close(2)` status must be exact takes an injected
//! handle", and [`Descriptor`] is that handle for both openings. [`Descriptor::close`]
//! calls `close(2)` and returns its result, which `zlib_rs::gz::gzclose_r` turns into
//! `Z_ERRNO` exactly where C's `ret ? Z_ERRNO : err` does (`gzread.c` L665-L666). An
//! earlier revision had to establish the same condition indirectly with `fstat(2)`,
//! because `Drop for File` discards `close`'s status; that workaround is gone with the
//! `File`.
//!
//! Divergence 5 of the core's inventory -- boxing the handle can abort the process,
//! because `Box::new` has no fallible form -- does not apply to this facade either:
//! [`reserve_descriptor`] makes that allocation through `Vec::try_reserve_exact`,
//! before the file is opened, so exhaustion is `NULL` rather than an abort.
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
//! | 2, slice reconstruction | [`read_region`], [`write_slice`] |
//! | 3, opaque state round-trip | [`reserve_block`], [`commit_block`], [`block_mut`], [`release`] |
//! | 5, C strings | [`c_bytes`] |
//! | descriptor adoption | [`dopen_descriptor`], [`Descriptor::new`] |
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

use core::ffi::{c_char, c_int, c_uint, c_void, CStr};
use core::mem::MaybeUninit;

use core::ptr;

use std::alloc::{alloc, dealloc, Layout};
// `io` for `Error::last_os_error`, which is how `errno` is read without naming a single
// platform constant; see `last_os_error`.
use std::io;

use zlib_rs::allocate::GlobalAllocator;
use zlib_rs::error::ReturnCode;
use zlib_rs::gz::{
    self as rs_gz, narrow_offset, printf_begin, printf_commit, GzFileSlot, GzHandle, GzIoError,
    GzOpenSpec, GzSeekFrom, GzState, ZOff64, GZ_READ, GZ_WRITE,
};
use zlib_rs::read_buf::OutputRegion;

use crate::panic_guard::{fallback, guard, guard_code};
use crate::types::{gzFile, init_view, voidp, voidpc, widen, z_off64_t, z_off_t, z_size_t};

// ---------------------------------------------------------------------------
// The heap block behind a `gzFile` -- unsafe-site category 3
// ---------------------------------------------------------------------------

/// The concrete state type a `gzFile` points at.
///
/// `'static` because nothing the state borrows outlives the process: its buffers
/// come from the global allocator and its file handle is owned. `GlobalAllocator`
/// because a `gzFile` has no caller-supplied hooks -- see the module documentation
/// for the four C sites that make that the faithful choice rather than a shortcut.
/// cbindgen:ignore
type FacadeGzState = GzState<'static, GlobalAllocator>;

/// The tag a live block carries, checked before the block is treated as state.
///
/// The value is arbitrary but must not be a plausible accident: zero, `-1` and small
/// integers all occur in uninitialised or half-written memory far too often to be
/// useful. `gzguts.h` L159-L162 chooses `7247` and `31153` for its mode constants on
/// the same reasoning, which it describes as "a little integrity check on the passed
/// structure".
/// cbindgen:ignore
const GZ_TAG_LIVE: c_int = 0x7a6c_6701;

/// The tag [`release`] writes immediately before the block is freed.
///
/// Reading it back is undefined behaviour, because the memory has been released, so
/// this is a diagnostic for the double-close case `zlib.h` L1752-L1756 forbids and
/// nothing more. When the allocator has not yet reused the block -- which is the
/// common case for an immediate second call -- the second close reports
/// `Z_STREAM_ERROR` rather than running a second teardown.
/// cbindgen:ignore
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
///
/// ★ **The tag is the only thing this adds to the core state, and deliberately so.** An
/// earlier draft also kept a `Vec<u8>` here holding a NUL-terminated copy of the last
/// `gzerror` text, which cost every stream twenty-four bytes of fixed state and every
/// error a second allocation. The core now stores its message terminated, so
/// [`gzerror`] hands back a pointer into that one allocation and this block needs no
/// copy at all -- one allocation per error, exactly as C's `gz_error` makes.
#[repr(C)]
struct GzBlock {
    /// The core state. **Must remain the first field**; see the type documentation.
    state: FacadeGzState,
    /// [`GZ_TAG_LIVE`] while the handle is usable, [`GZ_TAG_CLOSED`] once released.
    tag: c_int,
}

// `alloc` requires a non-zero size, and `Layout::new::<GzBlock>()` is what is handed
// to it. The state carries the exposed prefix, a mode, a handle slot and two buffer
// slots, so this holds on every target; asserting it makes the precondition of the
// one `alloc` call in this module a build-time fact rather than a comment.
/// cbindgen:ignore
const _: () = assert!(size_of::<GzBlock>() > 0);

// The offset the `gzgetc` macro depends on, asserted where the block is declared.
// `crates/libz-rs-sys/src/types.rs` pins `gzFile_s` against the core's
// `GzFileExposed`; this pins the block against the state, which is the remaining
// link in the chain from a caller's field arithmetic to the bytes it lands on.
/// cbindgen:ignore
const _: () = assert!(core::mem::offset_of!(GzBlock, state) == 0);

/// `sizeof(gz_state)` for the LP64 model, measured against the in-tree `gzguts.h`
/// (its L171-L203) with a C probe on this target: the whole of what C's `gz_open`
/// allocates for a stream's fixed state at `gzlib.c` L104.
//
// ★ `#[allow(dead_code)]` for the reason `ENOUGH_LENS` in `types.rs` carries it: the only
// use is inside the `const _: () = assert!(…)` memory gate below, and rustc 1.80's
// dead-code pass does not traverse that body -- `make rust-msrv` reports
// `constant C_GZ_STATE_BYTES is never used` there while current stable counts it.
#[allow(dead_code)]
/// cbindgen:ignore
const C_GZ_STATE_BYTES: usize = 248;

// AAP §0.8.4's per-stream memory gate applied to the idle fixed state, as a
// build-time fact rather than a review note. `GzBlock` is 264 bytes against C's 248
// -- +6.5%, inside the 15% budget -- and this assertion is what stops a future field
// from quietly spending the remaining headroom: adding one machine word to either
// this block or the core's `GzState` fails the build instead of the gate.
//
// Scoped to the LP64 model on purpose, following the same discipline as the struct
// layout assertions in `types.rs` (AAP §0.6.3.3): C's `gz_state` embeds a whole
// `z_stream`, three of whose members are `uLong`, so `sizeof(gz_state)` is smaller
// under LLP64 while this state -- which counts bytes in `u64` regardless of target --
// is not. The measured figure above is therefore only the reference for the model it
// was measured on, and the relational assertion below is what holds everywhere.
/// cbindgen:ignore
const _: () = assert!(
    size_of::<core::ffi::c_ulong>() != 8 || size_of::<GzBlock>() * 100 <= C_GZ_STATE_BYTES * 115,
    "the idle gzFile state must stay within 15% of C's gz_state"
);

// True on every target: the facade's contribution to a stream's fixed state is the
// tag plus whatever padding keeps the state aligned, and never more than one machine
// word. This is the half of the figure this file controls, and it is asserted
// unconditionally because it is a property of this declaration rather than of the
// target's integer model.
/// cbindgen:ignore
const _: () = assert!(size_of::<GzBlock>() <= size_of::<FacadeGzState>() + size_of::<usize>());

/// Moves a freshly built state onto the heap and returns it as a `gzFile`.
///
/// ★ This is the first half of C's ordering, and the ordering is the whole point.
/// `gz_open` allocates the state at `gzlib.c` L100 and copies the path at L206, and only
/// then -- with both allocations behind it -- calls `open` at L246-L248. So in C an
/// out-of-memory condition is always reported *before* the file system is touched: no
/// file is created, no existing file is truncated, and on the `gzdopen` path the
/// caller's descriptor is neither adopted nor closed. `zlib.h` L1414-L1416 depends on
/// that last property outright.
///
/// A port that opened first and allocated afterwards would return the same `NULL` while
/// leaving a created or truncated file behind -- a destructive side effect on a call
/// documented to have failed -- which is why the reservation is separated from the
/// commit. Between the two, [`reserve_block`]'s result is an uninitialised allocation
/// and nothing else: it holds no state, so releasing it with
/// [`discard_reserved_block`] cannot run a destructor and cannot fail.
///
/// Exhaustion returns null, which is what `zlib.h` L1391-L1393 documents for
/// "insufficient memory to allocate the `gzFile` state". [`alloc`] is used rather than
/// `Box::new` because `Box::new` aborts the process instead; see the module
/// documentation.
fn reserve_block() -> *mut GzBlock {
    let layout = Layout::new::<GzBlock>();
    // SAFETY: unsafe-site category 3 -- the opaque state round-trip, on its
    // allocating side. `layout` has a non-zero size, which the `const` assertion
    // above proves at build time, and that is `alloc`'s only precondition. A null
    // return is exhaustion and is handled by every caller rather than dereferenced.
    let raw = unsafe { alloc(layout) };
    raw.cast()
}

/// Releases a reservation that was never committed.
///
/// The other half of [`reserve_block`]'s contract: the block is uninitialised, so this
/// is a bare `dealloc` with no drop glue, which is what makes abandoning a reservation
/// free of side effects. It is the path taken when the mode string is unusable, when the
/// path label cannot be copied, or when `open` fails -- all three of which C reaches
/// with `free(state)` and a `NULL` return (`gzlib.c` L129-L130, L207-L209, L265-L267).
///
/// # Safety
///
/// `block` must be a non-null pointer returned by [`reserve_block`] that has not been
/// committed by [`commit_block`] and has not already been passed here.
unsafe fn discard_reserved_block(block: *mut GzBlock) {
    // SAFETY: unsafe-site category 3. By this function's contract `block` came from
    // `reserve_block`, so it was allocated with exactly this layout and has not been
    // released; nothing was ever written to it, so there is nothing to drop first.
    unsafe { dealloc(block.cast(), Layout::new::<GzBlock>()) };
}

/// Moves a freshly built state into a reserved block and returns it as a `gzFile`.
///
/// The second half of C's `state = malloc(sizeof(gz_state))` (`gzlib.c` L100): the
/// memory was obtained by [`reserve_block`] before the file was opened, and this fills
/// it in. C fills its fields one at a time as it goes while this receives a state that
/// is already complete; the difference is forced by ownership -- the core's `gz_open`
/// cannot hand back a half-built state -- and is not observable, because both produce a
/// fully initialised structure or `NULL`, and both decide `NULL` before opening.
///
/// The exposed prefix is refreshed after the move. Nothing requires it at this point
/// -- a just-opened stream has no output buffer, so `next` is null and `have` is zero
/// -- but the pointer is derived state and re-deriving it at the block's final address
/// is what makes "the pointer a caller holds is always the most recently derived one"
/// unconditional.
///
/// # Safety
///
/// `block` must be a non-null pointer returned by [`reserve_block`] that has not been
/// committed or discarded. Ownership of `state` transfers into it.
unsafe fn commit_block(block: *mut GzBlock, state: FacadeGzState) -> gzFile {
    // SAFETY: unsafe-site category 3. `block` is the reservation described by this
    // function's contract: sized and aligned for `GzBlock` and uninitialised -- so
    // `write` is the correct primitive, since it initialises without dropping a
    // previous value, of which there is none.
    unsafe {
        block.write(GzBlock {
            state,
            tag: GZ_TAG_LIVE,
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
/// If `file` is non-null it must address a live [`GzBlock`] produced by
/// [`commit_block`] and not yet passed to [`release`], and no other reference to that
/// block may exist
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
    // SAFETY: unsafe-site categories 1 and 3 -- `block_mut` performs both the pointer
    // validation and the tag check, and the obligation it places on its caller is the one
    // this function places on its own: `file` is null, or it addresses a live `GzBlock`
    // from `install` that has not been released, with no other reference to it alive for
    // `'a`. Nothing is dereferenced here, and the borrow handed back is `block_mut`'s own
    // narrowed to a single field, so its exclusivity is unchanged.
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
/// `file` must address a live [`GzBlock`] produced by [`commit_block`] and not yet
/// released, and **no borrow of it may be outstanding**. After this returns, `file` is dangling
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

/// Rebuilds a caller's output buffer as **write-only** storage from a `(buf, len)` pair.
///
/// Serves `gzread`, `gzfread` and `gzgets`. Separate from
/// `crate::types::output_region` because the lengths differ in width: that helper
/// takes a `uInt`, while `gzfread`'s count is a [`z_size_t`] and is already a
/// [`usize`]. The zero-length rule and its justification are identical.
///
/// ★ An [`OutputRegion`] rather than a `&mut [u8]`, because `len` bytes of *room* is all
/// `zlib.h` L1456-L1463, L1492-L1497 and L1588-L1593 ask for. A caller may hand over a
/// buffer straight from `malloc` -- `test/minigzip.c` does exactly that -- and a byte
/// slice may not address a byte that holds no value. `crate::types::output_region`
/// carries the full argument, including why initialising the buffer here instead is not
/// an acceptable alternative.
///
/// ★ The zero-length case is not a formality. `core::slice::from_raw_parts_mut(null, 0)`
/// is **undefined behaviour** -- the pointer must be non-null and aligned even for an
/// empty slice -- so it is branched on rather than relied upon.
///
/// A null pointer with a non-zero length also yields the empty region. C would hand that
/// pointer to `memcpy` and crash (`gzread.c` L370), so there is no reference behaviour
/// to match; the core then writes nothing, and the caller sees the zero count that
/// `zlib.h` L1502-L1503 tells it to disambiguate with `gzerror`.
///
/// # Safety
///
/// If `len` is non-zero, `buf` must be non-null and writable for `len` bytes, and that
/// region must not be aliased by anything else -- including by another view this
/// module reconstructs -- for `'a`.
#[must_use]
unsafe fn read_region<'a>(buf: voidp, len: usize) -> OutputRegion<'a> {
    if len == 0 || buf.is_null() {
        return OutputRegion::empty();
    }
    // SAFETY: unsafe-site category 2 -- slice reconstruction. `buf` is non-null by the
    // test above and trivially aligned for `u8`, which `MaybeUninit<u8>` shares;
    // writability for `len` bytes and the absence of aliasing are this function's
    // documented obligations on its caller. No initialisation is required for the
    // element type, which is the point of using it.
    let slots = unsafe { core::slice::from_raw_parts_mut(buf.cast::<MaybeUninit<u8>>(), len) };
    OutputRegion::write_only(slots, init_view())
}

/// Rebuilds a caller's input buffer as a shared slice from a `(buf, len)` pair.
///
/// The [`read_region`] counterpart for `gzwrite` and `gzfwrite`, whose buffers are
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
// The platform descriptor -- `gzlib.c` L228-L285, `gzread.c` L30, `gzwrite.c` L115,
// `gzread.c` L665, `gzwrite.c` L696
// ---------------------------------------------------------------------------

// ★ WHY THIS LAYER EXISTS, AND WHY IT USES `libc` RATHER THAN `std::fs`
//
// C's gzip file layer is a thin skin over five raw calls -- `open`, `read`, `write`,
// `LSEEK` and `close` -- plus two `fcntl` adjustments, and every one of them is
// observable through the documented API:
//
//   * `zlib.h` L1391-L1401 promises that after a failed `gzopen` "errno can be checked
//     to determine if the reason gzopen failed was that the file could not be opened",
//     and that with `N` in the mode it "will be EAGAIN or ENONBLOCK" so the call can be
//     retried.  Only the failing `open(2)` itself can leave that value in place.
//   * `gzlib.c` L228-L244 composes an exact `oflag`, including `O_CLOEXEC` for `e` and
//     `O_NONBLOCK` for `N` and *nothing else*.  `std::fs::File` always adds `O_CLOEXEC`
//     and can express neither of those two mode characters, so a `std`-opened descriptor
//     carries flags C's would not.
//   * `gzlib.c` L253-L262 applies `F_SETFL` and `F_SETFD` to an adopted descriptor.
//     `fcntl` has no `std` equivalent at all.
//   * `gzread.c` L665 and `gzwrite.c` L696 make `close(state->fd)`'s result the
//     difference between `Z_OK` and `Z_ERRNO`.  `Drop for File` discards it.
//
// So this is precisely the case the AAP's dependency inventory (§0.5.1.3) admits `libc`
// for -- "only if a platform type or a raw file syscall is unavoidable in the gz layer"
// -- and it is reached for that reason and no other.  Everything above this layer stays
// on `std`/core.  The dependency is pulled in by the `gz` feature, because it is the gz
// layer and only the gz layer that needs it: a build with `--no-default-features
// --features libz-compat` exports no `gz*` symbols and takes no `libc` edge.
//
// One `Descriptor` type serves BOTH openings.  C has one too: `gz_open`'s `state->fd` is
// the same `int` whether it came from `open` (L246) or from the caller (L262), and every
// later operation is identical.  Keeping them one type is what makes the close semantics
// identical as well, which is the whole of the fix.

/// A file descriptor this library operates on: C's `state->fd`.
///
/// Owned in the sense that matters -- [`GzHandle::close`] is the one place a descriptor
/// is closed, exactly once -- and *not* owned in the sense [`Drop`] would imply.
///
/// # ★ Why `Drop` does not close
///
/// `zlib.h` L1414-L1416: "The duplicated descriptor should be saved to avoid a leak,
/// since `gzdopen` does not close `fd` if it fails." A descriptor still sitting in one of
/// these when it is dropped is therefore one the library never took ownership of in the
/// caller's eyes, because every path that *ends* a stream closes through
/// [`GzHandle::close`] first: `zlib_rs::gz::gzclose_r` (`gzread.c` L665),
/// `zlib_rs::gz::gzclose_w` (`gzwrite.c` L696) and `GzState::teardown` all call it. This
/// is the "supply your own handle type whose `Drop` does not close" option
/// `zlib_rs::gz::open` offers, and it is the reason a failed open can hand the descriptor
/// back untouched.
///
/// It follows that dropping one of these without closing it deliberately leaves the
/// descriptor open. That looks like a leak and is not: on the only path that reaches it,
/// closing would be the bug.
struct Descriptor {
    /// The descriptor, or `-1` once [`GzHandle::close`] has run.
    ///
    /// `-1` rather than an [`Option`] because that is the sentinel the platform already
    /// uses -- `open` reports failure with it (`gzlib.c` L264-L265) -- and because it
    /// makes closing idempotent without a second field, which the [`GzHandle`] contract
    /// requires: a close path closes explicitly and the state is then dropped.
    fd: c_int,
}

impl Descriptor {
    /// Wraps a descriptor this library is now responsible for operating on.
    ///
    /// `fd` must be open, and no other owner may close it behind this one's back; that is
    /// `zlib.h` L1414-L1416's contract for [`gzdopen`] and is trivially satisfied for a
    /// descriptor [`sys_open`] has just produced. A value of `-1` is accepted and means
    /// "already closed", which is the state [`Descriptor::relinquish`] leaves behind.
    const fn new(fd: c_int) -> Self {
        Self { fd }
    }

    /// Takes the descriptor out, leaving the handle closed.
    ///
    /// The single point at which the number stops being this handle's responsibility, so
    /// that neither closing nor relinquishing can happen twice.
    fn relinquish(&mut self) -> c_int {
        core::mem::replace(&mut self.fd, -1)
    }

    /// The descriptor, or [`CLOSED_DESCRIPTOR`] once it has been closed.
    ///
    /// # Errors
    ///
    /// [`CLOSED_DESCRIPTOR`] when [`GzHandle::close`] has already run.
    fn usable(&self) -> Result<c_int, GzIoError> {
        if self.fd == -1 {
            return Err(CLOSED_DESCRIPTOR);
        }
        Ok(self.fd)
    }
}

impl GzHandle for Descriptor {
    /// `read(state->fd, buf + *have, get)` -- `gz_load` (`gzread.c` L30).
    ///
    /// `Ok(0)` is end of file, as `read` returning 0 is there (L46-L47). A short read is
    /// normal rather than exceptional, because `gz_load` loops until the buffer is full
    /// (L26-L34), so nothing loops here. Interruptions are deliberately not retried:
    /// `read` returning `-1` with `EINTR` is an error to `gz_load` (L35-L44) and is
    /// reported as one.
    ///
    /// The length is capped so that it can be expressed to the platform. C caps at
    /// `((unsigned)-1 >> 2) + 1` for the same reason (`gzread.c` L21) and the core
    /// applies that cap before calling; this is the narrowing that remains.
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, GzIoError> {
        let fd = self.usable()?;
        let count = sys_count(buf.len());
        // SAFETY: unsafe-site category 2 in the platform direction, plus the descriptor
        // contract from `Descriptor::new`. `buf` is a live, writable slice and `count` is
        // at most its length, so `read` writes only within it; `fd` is open, because
        // `usable` has rejected the closed sentinel and nothing else can have closed it.
        let got = unsafe { libc::read(fd, buf.as_mut_ptr().cast::<c_void>(), count) };
        if got < 0 {
            return Err(last_os_error());
        }
        // Non-negative and at most `count`, so the conversion cannot fail; a platform
        // reporting more than was asked for would be reported as an error rather than
        // trusted.
        usize::try_from(got).map_err(|_| last_os_error())
    }

    /// `write(state->fd, state->x.next, put)` -- `gz_comp` (`gzwrite.c` L115).
    ///
    /// A short write is normal: `gz_comp` loops until the buffer is drained (L110-L123),
    /// so no loop is added here.
    fn write(&mut self, buf: &[u8]) -> Result<usize, GzIoError> {
        let fd = self.usable()?;
        let count = sys_count(buf.len());
        // SAFETY: as `read` above, in the reading direction: `buf` is a live slice and
        // `count` is at most its length, so `write` reads only within it.
        let put = unsafe { libc::write(fd, buf.as_ptr().cast::<c_void>(), count) };
        if put < 0 {
            return Err(last_os_error());
        }
        usize::try_from(put).map_err(|_| last_os_error())
    }

    /// The `LSEEK` macro (`gzlib.c` L8-L16), which resolves to `lseek64` where the
    /// platform has one and to `lseek` where its `off_t` is already sixty-four bits.
    ///
    /// See [`sys_lseek`] for which spelling each target gets and why.
    fn seek(&mut self, offset: ZOff64, whence: GzSeekFrom) -> Result<ZOff64, GzIoError> {
        let fd = self.usable()?;
        let origin = match whence {
            GzSeekFrom::Start => SEEK_SET,
            GzSeekFrom::Current => SEEK_CUR,
            GzSeekFrom::End => SEEK_END,
        };
        // SAFETY: the descriptor contract from `Descriptor::new`; `usable` has established
        // that `fd` is not the closed sentinel. `lseek` dereferences nothing.
        let position = unsafe { sys_lseek(fd, offset, origin) };
        if position == -1 {
            return Err(last_os_error());
        }
        Ok(position)
    }

    /// `fcntl(fd, F_SETFL, fcntl(fd, F_GETFL) | O_NONBLOCK)` -- `gzlib.c` L254-L257.
    ///
    /// Reached only for an adopted descriptor, because a descriptor this library opened
    /// received `O_NONBLOCK` from `open` itself. `zlib_rs::gz::open::gz_open_handle`
    /// discards the result exactly as C discards `fcntl`'s, so a failure changes nothing
    /// observable -- but it is reported rather than swallowed, because the two are not the
    /// same thing.
    ///
    /// Clearing the flag is not something C ever asks for, and is implemented for
    /// completeness rather than for a caller: `nonblocking == false` masks the bit out.
    ///
    /// # Errors
    ///
    /// Whatever `fcntl` reports, or [`CLOSED_DESCRIPTOR`]. On a platform without
    /// `O_NONBLOCK` -- Windows, where C's `#ifdef` excludes the whole branch -- the
    /// request succeeds without doing anything, which is what C does there.
    #[cfg(unix)]
    fn set_nonblocking(&mut self, nonblocking: bool) -> Result<(), GzIoError> {
        let fd = self.usable()?;
        // SAFETY: the descriptor contract from `Descriptor::new`. `F_GETFL` takes no
        // third argument and `F_SETFL` takes an `int`, which is what is passed; `fcntl`
        // dereferences nothing in either case.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags == -1 {
            return Err(last_os_error());
        }
        let updated = if nonblocking {
            flags | libc::O_NONBLOCK
        } else {
            flags & !libc::O_NONBLOCK
        };
        // SAFETY: as above.
        if unsafe { libc::fcntl(fd, libc::F_SETFL, updated) } == -1 {
            return Err(last_os_error());
        }
        Ok(())
    }

    /// Nothing to do: `gzlib.c` L254-L257 is inside `#ifdef O_NONBLOCK`, which this
    /// platform does not define, so C ignores `N` here as well.
    #[cfg(not(unix))]
    fn set_nonblocking(&mut self, _nonblocking: bool) -> Result<(), GzIoError> {
        self.usable().map(|_fd| ())
    }

    /// `close(state->fd)` -- `gzread.c` L665 and `gzwrite.c` L696. Idempotent.
    ///
    /// ★ The result is the real one, and that is the point. `gzclose_r` returns
    /// `ret ? Z_ERRNO : err` and `gzclose_w` sets `ret = Z_ERRNO` when `close` reports
    /// `-1`, so a caller can tell a failed close from a successful one -- which a handle
    /// built on `std::fs::File` cannot offer, because `Drop for File` discards `close`'s
    /// status. `zlib_rs::gz::gzclose_r` routes this `Result` straight into that choice.
    ///
    /// The descriptor leaves the handle *before* the call, so a failed close does not
    /// leave a number behind that a second call would try again -- C cannot retry either,
    /// since it has already freed the structure.
    fn close(&mut self) -> Result<(), GzIoError> {
        let fd = self.relinquish();
        if fd == -1 {
            // Already closed; the trait requires this to succeed.
            return Ok(());
        }
        // SAFETY: the descriptor contract from `Descriptor::new` -- `fd` was open and this
        // handle was its owner for closing purposes. `relinquish` guarantees this runs at
        // most once per descriptor, which is what makes a double close impossible.
        if unsafe { libc::close(fd) } == -1 {
            return Err(last_os_error());
        }
        Ok(())
    }
}

impl Drop for Descriptor {
    /// Relinquishes the descriptor **without closing it**; see the type documentation.
    fn drop(&mut self) {
        // The number is intentionally discarded: on the only path that reaches this --
        // a failed open -- ownership returns to the caller, which is `zlib.h` L1415-L1416.
        let _relinquished = self.relinquish();
    }
}

/// The failure reported when an operation is attempted on an already-closed handle.
///
/// No syscall is reached, so there is no `errno` to report and zero is used -- the value
/// `GzIoError`'s own documentation gives for "the implementation could not determine
/// one". C cannot reach the condition at all: its descriptor is an `int` that stays in
/// the structure until `free`, and `zlib.h` L1752-L1753 forbids using a handle after
/// `gzclose`. It is reachable here only through `GzState`'s `Drop` running after an
/// explicit close, which `GzHandle::close`'s idempotence requirement anticipates.
/// cbindgen:ignore
const CLOSED_DESCRIPTOR: GzIoError = GzIoError::new(0, false);

/// The two facts the `gzFile` layer acts on, taken from the platform's `errno`.
///
/// C reports a failed call as `gz_error(state, Z_ERRNO, zstrerror())`, where `zstrerror()`
/// expands to `strerror(errno)` (`gzguts.h` L131-L133), and separately records
/// `EAGAIN`/`EWOULDBLOCK` in `state->again` (`gzread.c` L36, `gzwrite.c` L117-L118). Both
/// travel in [`GzIoError`]: the number, so the message can be rendered as `strerror`
/// would render it, and the stall flag, because a stalled non-blocking stream is a retry
/// signal rather than an error.
///
/// `io::Error::last_os_error()` is `errno` -- it is documented as such and performs no
/// call of its own, so it cannot disturb the value it reads. The `WouldBlock` kind test
/// rather than a numeric comparison is what keeps this crate free of hardcoded `errno`
/// numbers; `std` maps both `EAGAIN` and `EWOULDBLOCK` to it, which is exactly C's pair.
fn last_os_error() -> GzIoError {
    let error = io::Error::last_os_error();
    GzIoError::new(
        error.raw_os_error().unwrap_or(0),
        error.kind() == io::ErrorKind::WouldBlock,
    )
}

/// A byte count narrowed to the type the platform's `read`/`write` takes.
///
/// POSIX takes a `size_t`, so nothing is lost there; the Windows CRT takes an `unsigned`,
/// so a longer request is truncated to one call's worth and the core's loop supplies the
/// rest. C performs the same narrowing for the same reason, capping at
/// `((unsigned)-1 >> 2) + 1` before it calls (`gzread.c` L21, `gzwrite.c` L96).
#[cfg(unix)]
fn sys_count(len: usize) -> libc::size_t {
    len
}

/// As the Unix form, narrowing to the CRT's `unsigned` instead.
#[cfg(not(unix))]
fn sys_count(len: usize) -> c_uint {
    c_uint::try_from(len).unwrap_or(c_uint::MAX)
}

/// `SEEK_SET`, `SEEK_CUR` and `SEEK_END`, from the platform rather than from a literal.
/// cbindgen:ignore
const SEEK_SET: c_int = libc::SEEK_SET;
/// See [`SEEK_SET`].
/// cbindgen:ignore
const SEEK_CUR: c_int = libc::SEEK_CUR;
/// See [`SEEK_SET`].
/// cbindgen:ignore
const SEEK_END: c_int = libc::SEEK_END;

/// The `LSEEK` of `gzlib.c` L8-L16, in the spelling this target needs.
///
/// C picks between four names by platform, and the choice is about the *offset width*
/// rather than the function: `_lseeki64` on Windows, `lseek64` where
/// `_LARGEFILE64_SOURCE` is in force, `llseek` on one old platform, and plain `lseek`
/// otherwise. This picks the same way:
///
/// * Windows and Linux/Android get `lseek64`, which `libc` binds to `_lseeki64` and
///   `lseek64` respectively. On 32-bit Linux this is the *only* spelling that can express
///   an offset past 2 GiB, which is why it is not simplified away.
/// * Everything else -- macOS and the BSDs -- gets `lseek`, whose `off_t` is already
///   sixty-four bits wide there, so `lseek64` does not exist and would not add anything.
///
/// # Safety
///
/// `fd` must be an open descriptor; `lseek` dereferences nothing.
#[cfg(any(windows, target_os = "linux", target_os = "android"))]
unsafe fn sys_lseek(fd: c_int, offset: ZOff64, origin: c_int) -> ZOff64 {
    // SAFETY: this function's own contract, forwarded unchanged.
    unsafe { libc::lseek64(fd, offset, origin) }
}

/// See the Windows/Linux form of [`sys_lseek`].
///
/// # Safety
///
/// As the other form.
#[cfg(not(any(windows, target_os = "linux", target_os = "android")))]
unsafe fn sys_lseek(fd: c_int, offset: ZOff64, origin: c_int) -> ZOff64 {
    // SAFETY: this function's own contract, forwarded unchanged.
    let position = unsafe { libc::lseek(fd, offset, origin) };
    // `off_t` is sixty-four bits wide on every target that reaches this arm, so this is a
    // width assertion rather than a conversion; `-1` survives it and is what the caller
    // tests for.
    ZOff64::try_from(position).unwrap_or(-1)
}

/// `O_LARGEFILE`, or zero where the platform does not have it.
///
/// C ORs it in under `#ifdef` (`gzlib.c` L230-L232). Measured on
/// x86_64-unknown-linux-gnu with glibc: the macro **is** defined and its value **is
/// zero**, because a 64-bit `off_t` is already the default there -- which is why `libc`
/// does not declare it for 64-bit Linux at all. It is undefined on macOS and Windows.
/// The one place it carries a bit is 32-bit Linux, where it is `0o100000` and is what
/// lets `open` succeed on a file larger than 2 GiB; `libc` declares it for every 32-bit
/// Linux architecture, gnu and musl alike.
#[cfg(all(target_os = "linux", target_pointer_width = "32"))]
/// cbindgen:ignore
const LARGEFILE: c_int = libc::O_LARGEFILE;

/// See the 32-bit Linux form of [`LARGEFILE`].
#[cfg(not(all(target_os = "linux", target_pointer_width = "32")))]
/// cbindgen:ignore
const LARGEFILE: c_int = 0;

/// `O_BINARY`, or zero where the platform does not have it.
///
/// C ORs it in under `#ifdef` (`gzlib.c` L233-L235). Only the Windows CRT defines it, and
/// there it is essential: without it the CRT translates `\r\n` in a stream of compressed
/// bytes. POSIX has no text mode and no such macro.
#[cfg(windows)]
/// cbindgen:ignore
const BINARY: c_int = libc::O_BINARY;

/// See the Windows form of [`BINARY`].
#[cfg(not(windows))]
/// cbindgen:ignore
const BINARY: c_int = 0;

/// `O_CLOEXEC` for the `e` mode character, or zero where the platform has no such flag.
///
/// C's `case 'e': oflag |= O_CLOEXEC;` sits inside `#ifdef O_CLOEXEC` (`gzlib.c`
/// L133-L137), so on a platform without it the character is ignored -- it falls through
/// to the `default:` arm, which the source annotates "could consider as an error, but just
/// ignore". Windows has `O_NOINHERIT`, which is *not* what C uses, so it is deliberately
/// not substituted: doing so would apply a flag the reference never applies.
#[cfg(unix)]
/// cbindgen:ignore
const CLOEXEC: c_int = libc::O_CLOEXEC;

/// See the Unix form of [`CLOEXEC`].
#[cfg(not(unix))]
/// cbindgen:ignore
const CLOEXEC: c_int = 0;

/// `O_NONBLOCK` for the `N` mode character, or zero where the platform has no such flag.
///
/// As [`CLOEXEC`], for `gzlib.c` L157-L161.
#[cfg(unix)]
/// cbindgen:ignore
const NONBLOCK: c_int = libc::O_NONBLOCK;

/// See the Unix form of [`NONBLOCK`].
#[cfg(not(unix))]
/// cbindgen:ignore
const NONBLOCK: c_int = 0;

/// The `oflag` C composes for `open`: `gzlib.c` L228-L244, term for term.
///
/// ```text
/// oflag |= O_LARGEFILE | O_BINARY |
///          (mode == GZ_READ ? O_RDONLY
///                           : (O_WRONLY | O_CREAT | (exclusive ? O_EXCL : 0) |
///                              (mode == GZ_WRITE ? O_TRUNC : O_APPEND)));
/// ```
///
/// with `O_CLOEXEC` and `O_NONBLOCK` already in `oflag` from the mode string
/// (L133-L137, L157-L161). Each `#ifdef` C guards a term with becomes a `const` that is
/// zero where the platform lacks the flag, so the composition below reads as one
/// expression on every target -- see [`LARGEFILE`], [`BINARY`], [`CLOEXEC`] and
/// [`NONBLOCK`].
///
/// Two properties are worth stating because getting either wrong is silent:
///
/// * **Nothing is added.** In particular `O_CLOEXEC` is *not* set unless the mode string
///   asked for it, which is where a `std::fs::File`-based implementation necessarily
///   differs: `std` always sets it. A caller that expects the descriptor to survive
///   `exec` -- the default in C -- sees C's behaviour here.
/// * `GZ_APPEND` is the mode the state carries *before* `finish_open` rewrites it to
///   `GZ_WRITE` (`gzlib.c` L270-L272), so `O_APPEND` versus `O_TRUNC` is decided from
///   the parsed spec rather than from the state, exactly as C decides it from the
///   still-unrewritten `state->mode`.
fn open_flags(spec: &GzOpenSpec) -> c_int {
    let cloexec = if spec.cloexec() { CLOEXEC } else { 0 };
    let nonblock = if spec.nonblocking() { NONBLOCK } else { 0 };
    let access = if spec.mode() == GZ_READ {
        libc::O_RDONLY
    } else {
        let exclusive = if spec.exclusive() { libc::O_EXCL } else { 0 };
        let truncate = if spec.mode() == GZ_WRITE {
            libc::O_TRUNC
        } else {
            libc::O_APPEND
        };
        libc::O_WRONLY | libc::O_CREAT | exclusive | truncate
    };
    cloexec | nonblock | LARGEFILE | BINARY | access
}

/// The permission bits C passes to `open` for a file it creates: `gzlib.c` L247.
///
/// `0666`, left for the caller's umask to narrow, which is what every well-behaved
/// creator does. Only reached when the flags include `O_CREAT`; `open` ignores it
/// otherwise.
#[cfg(unix)]
/// cbindgen:ignore
const CREATE_PERMISSIONS: libc::mode_t = 0o666;

/// `open(path, oflag, 0666)` -- `gzlib.c` L246-L247.
///
/// Returns the descriptor, or `-1` with the platform's `errno` set, which is the state
/// [`open_descriptor`] relies on: nothing between here and `gzopen`'s `return NULL`
/// touches `errno`, so a caller reading it afterwards sees what `open` left, and
/// `zlib.h` L1394-L1401 promises exactly that.
///
/// # Safety
///
/// `path` must be a NUL-terminated string that stays readable for the duration of the
/// call. It is passed through unchanged; nothing else is dereferenced.
#[cfg(unix)]
unsafe fn sys_open(path: &CStr, oflag: c_int) -> c_int {
    // SAFETY: this function's own contract. `CStr::as_ptr` yields a NUL-terminated
    // pointer valid for the borrow, `oflag` is a flag word, and the third argument is the
    // `mode_t` the variadic signature expects when `O_CREAT` is present -- supplying it
    // unconditionally is harmless and is what C's own call does.
    unsafe { libc::open(path.as_ptr(), oflag, CREATE_PERMISSIONS) }
}

/// The Windows CRT's `_open(path, oflag, _S_IREAD | _S_IWRITE)`.
///
/// C reaches the same function through the same name on this platform -- `gzlib.c` L247's
/// `open` is the CRT's `_open` -- so the narrow-path encoding a caller gets is the CRT's,
/// which is what a Windows program passing an ANSI path expects. The permission argument
/// is the pair C uses for `_wopen` at L251, which is the only one the CRT honours.
///
/// # Safety
///
/// As the Unix form.
#[cfg(not(unix))]
unsafe fn sys_open(path: &CStr, oflag: c_int) -> c_int {
    // SAFETY: this function's own contract; see the Unix form.
    unsafe { libc::open(path.as_ptr(), oflag, libc::S_IREAD | libc::S_IWRITE) }
}

/// The Windows CRT's `_wopen(path, oflag, _S_IREAD | _S_IWRITE)` -- `gzlib.c` L250-L251.
///
/// The wide-path counterpart of [`sys_open`], reached only from [`gzopen_w`], whose `fd`
/// sentinel in C is `-2` precisely so that `gz_open` can pick this call.
///
/// # Safety
///
/// `path` must be a NUL-terminated wide string that stays readable for the duration of
/// the call.
#[cfg(windows)]
unsafe fn sys_wopen(path: *const wchar_t, oflag: c_int) -> c_int {
    // SAFETY: this function's own contract. The pointer is passed through unchanged and
    // the CRT reads it up to its terminator.
    unsafe { libc::wopen(path, oflag, libc::S_IREAD | libc::S_IWRITE) }
}

/// `fcntl(fd, F_SETFD, fcntl(fd, F_GETFD) | O_CLOEXEC)` -- `gzlib.c` L258-L261, term for
/// term.
///
/// The descriptor-flag half of what C does to an *adopted* descriptor; the
/// file-status half is [`GzHandle::set_nonblocking`], which the core drives. Both
/// results are discarded, because C discards `fcntl`'s -- the call is best effort, and a
/// descriptor that cannot take the flag is still a usable stream.
///
/// The `e` character is discharged here, in the facade, rather than in the core because
/// [`GzHandle`] has no descriptor-flag method to carry it -- which
/// `zlib_rs::gz::open::gz_open_handle`'s documentation states as a facade obligation.
/// [`dopen_descriptor`] makes the call once the stream exists, mirroring C's own
/// ordering; the `N` character takes the other route, arriving later through
/// [`GzHandle::set_nonblocking`].
///
/// ★ **`O_CLOEXEC` is passed to `F_SETFD` deliberately, and it is not the same constant as
/// `FD_CLOEXEC`.** `F_SETFD` defines exactly one bit, `FD_CLOEXEC` (`0x1`); `O_CLOEXEC` is
/// an *open* flag and is `0x80000` on Linux. So C's call sets a bit `fcntl` does not
/// define, and measured on this target it is silently ignored -- `fcntl` returns 0 and
/// `F_GETFD` still reads `0`:
///
/// ```text
/// raw  F_SETFD|O_CLOEXEC ret=0 errno=0 getfd=0x0 fd_cloexec=0
/// C    gzdopen(fd,"rbe")            getfd=0x0 fd_cloexec=0
/// C    gzdopen(fd,"rb")             getfd=0x0 fd_cloexec=0
/// ```
///
/// The consequence is that in the reference implementation the `e` character has **no
/// effect on an adopted descriptor** on Linux, however clearly `zlib.h` L1396-L1397
/// describes it as "close the file on `exec`". Substituting `FD_CLOEXEC` here would make
/// `e` work -- and would be a behaviour change, which this port does not make: a caller
/// that passes `e` to `gzdopen` and then relies on the descriptor being inherited across
/// `exec` behaves the same under both libraries only if the quirk is reproduced. It was
/// measured before it was decided, and both outcomes were compared against the C build;
/// see `an_adopted_descriptor_receives_the_same_fcntl_c_makes`, which pins the measured
/// result. The path-based case is unaffected and does work, because there the flag rides
/// in on `open`'s `oflag`, where `O_CLOEXEC` is the correct constant.
#[cfg(unix)]
fn apply_cloexec(fd: c_int) {
    // SAFETY: the descriptor contract from `Descriptor::new`; the caller has just
    // established that `fd` is open. `F_GETFD` takes no third argument and `F_SETFD`
    // takes an `int`; `fcntl` dereferences nothing.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags == -1 {
        // C does not test `fcntl`'s result either (`gzlib.c` L259-L260).
        return;
    }
    // SAFETY: as above. `O_CLOEXEC` rather than `FD_CLOEXEC` is C's own argument; see the
    // measurement above this function.
    let _ignored = unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::O_CLOEXEC) };
}

/// Nothing to do: `gzlib.c` L258-L261 is inside `#ifdef O_CLOEXEC`, which this platform
/// does not define, so C ignores `e` for an adopted descriptor as well.
#[cfg(not(unix))]
fn apply_cloexec(_fd: c_int) {}

/// Opens `target` with C's flags and boxes the result as a [`GzHandle`], reusing a box
/// that was allocated **before** anything was opened.
///
/// ★ The two-phase shape is what keeps the allocation ordering C's. `gz_open` finishes
/// every allocation it needs -- the state at `gzlib.c` L100, the path at L206 -- before it
/// calls `open` at L246, so an out-of-memory condition can never leave a created or
/// truncated file behind. In this port there is a third allocation C does not have: the
/// `Box<dyn GzHandle>` the core's handle slot holds. Allocating it *after* the open would
/// reintroduce exactly the destructive failure C avoids, so it is allocated first, with
/// the closed sentinel `-1` inside, and the real descriptor is written into it once
/// `open` has succeeded. Nothing fallible remains after that.
///
/// The box arrives already allocated for the same reason, from
/// [`reserve_descriptor`]: it is the caller that knows whether the mode string and the
/// path label were acceptable, and those must be settled first too.
///
/// # Errors
///
/// The platform's failure from `open`, with `errno` left exactly as `open` left it.
///
/// # Safety
///
/// `target` must be a NUL-terminated narrow or wide string, matching its variant, that
/// stays readable for the duration of the call.
unsafe fn open_descriptor<'a>(
    target: OpenTarget<'_>,
    spec: &GzOpenSpec,
    mut reserved: ReservedDescriptor,
) -> Result<GzFileSlot<'a>, GzIoError> {
    let oflag = open_flags(spec);
    let fd = match target {
        // SAFETY: this function's own contract supplies `sys_open`'s.
        OpenTarget::Narrow(path) => unsafe { sys_open(path, oflag) },
        #[cfg(windows)]
        // SAFETY: this function's own contract supplies `sys_wopen`'s.
        OpenTarget::Wide(path) => unsafe { sys_wopen(path, oflag) },
    };
    if fd == -1 {
        // `gzlib.c` L264-L267: the state and the path are freed and `NULL` is returned.
        // The reservation is dropped here, and its `Drop` has nothing to relinquish because
        // the sentinel is still `-1`. `errno` is untouched on the way out.
        return Err(last_os_error());
    }
    // The last fallible step is already behind us; this is a store into memory that is
    // both allocated and initialised.
    let [slot] = &mut *reserved;
    *slot = Descriptor::new(fd);
    Ok(GzFileSlot::Boxed(reserved))
}

/// The heap-allocated [`Descriptor`] a later `open` will fill.
///
/// `Box<[Descriptor; 1]>` rather than `Box<Descriptor>` because that is the shape the core
/// can turn into a `Box<dyn GzHandle>` without `unsafe` and without `Box::new`:
/// `zlib_rs::gz::try_box_handle` documents the trick and the core implements `GzHandle` for
/// `[H; 1]` precisely so that the coercion is licensed. `[Descriptor; 1]` has exactly the
/// size and alignment of `Descriptor`, so the array costs nothing at run time.
///
/// The type is named because it is the value that travels *between* the two phases
/// [`open_descriptor`] describes -- allocated first, filled in after a successful open.
type ReservedDescriptor = Box<[Descriptor; 1]>;

/// Allocates the handle box a later `open` will fill, or reports exhaustion.
///
/// The other half of [`open_descriptor`]'s ordering contract, and the reason
/// `zlib_rs::gz::open`'s divergence 5 -- "boxing the handle can abort the process" -- does
/// not apply to this facade: the allocation is made fallibly here, before anything can be
/// damaged by the report, rather than by `Box::new` after the file has been opened.
///
/// The steps are `try_box_handle`'s, minus its final coercion, which is what keeps the
/// value typed: reserve room for one `Descriptor` fallibly, move the closed sentinel in,
/// convert to a boxed slice -- a no-op, because `len == capacity` -- and then to a
/// one-element array, which is a pointer-metadata change with no allocation.
///
/// [`None`] is exhaustion, which every caller turns into C's `NULL` (`zlib.h` L1391-L1393
/// and L1417-L1418). C's equivalent is a `malloc` returning null (`gzlib.c` L101-L102),
/// after which it returns `NULL` without consulting `errno`.
fn reserve_descriptor() -> Option<ReservedDescriptor> {
    let mut slot: Vec<Descriptor> = Vec::new();
    // The fallible half. `try_reserve_exact` reports exhaustion where `reserve` aborts.
    slot.try_reserve_exact(1).ok()?;
    // Capacity is already sufficient, so this cannot reallocate.
    slot.push(Descriptor::new(-1));
    // `len == capacity`, so the implied shrink is a no-op and nothing is reallocated.
    let boxed: Box<[Descriptor]> = slot.into_boxed_slice();
    // Cannot fail -- the slice holds exactly one element -- and is written as a `try_into`
    // rather than an `expect` because library code here does not panic.
    boxed.try_into().ok()
}

/// What [`open_descriptor`] opens: a narrow path, or a wide one on Windows.
///
/// C encodes the same choice in `gz_open`'s `fd` parameter -- `-1` means "open the narrow
/// `path`", `-2` means "open it with `_wopen`" (`gzlib.c` L246-L252) -- and the wide arm
/// exists only where `WIDECHAR` does, which `gzguts.h` L54-L56 ties to `_WIN32`.
///
/// The pointers are the caller's own, unchanged and unterminated-by-us: C hands `open`
/// the very `const char *` it was given, and so does this. Copying it would be the one
/// way to introduce an allocation between the parse and the open, which is what
/// [`open_descriptor`] exists to avoid.
// Copied rather than moved: every variant is a borrowed pointer, so a copy is the pointer
// itself and nothing is duplicated.  It also has to be `Copy` for `open_descriptor` to take
// it by value from inside a closure that borrows it.
#[derive(Clone, Copy)]
enum OpenTarget<'p> {
    /// A NUL-terminated narrow path, as [`gzopen`] and [`gzopen64`] receive it.
    Narrow(&'p CStr),
    /// A NUL-terminated wide path, as [`gzopen_w`] receives it.
    #[cfg(windows)]
    Wide(*const wchar_t),
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
    // cannot be a `&CStr`, so the test belongs here rather than in the core.
    if path.is_null() || mode.is_null() {
        return fallback::null_handle();
    }
    // SAFETY: unsafe-site category 5, discharged by this function's own contract together
    // with the null test above: each pointer is a NUL-terminated string that stays
    // readable for the duration of the call. The narrow path is kept as a `CStr` rather
    // than as bytes because it is handed to `open` unchanged, exactly as C hands `open`
    // the very pointer it was given (`gzlib.c` L247).
    let (path, mode) = unsafe { (CStr::from_ptr(path), CStr::from_ptr(mode)) };

    // SAFETY: `path` is the caller's NUL-terminated string, live for this call.
    unsafe { open_target(OpenTarget::Narrow(path), path.to_bytes(), mode.to_bytes()) }
}

/// The body shared by every opening entry point, once its path is a platform-ready target
/// and its label and mode are bytes.
///
/// ★ **The ordering here is C's, and it is what makes an out-of-memory failure
/// non-destructive.** `gz_open` performs both of its allocations before it opens anything
/// (`gzlib.c` L100 and L206, then `open` at L246), so `NULL` from exhaustion never leaves
/// a created or truncated file behind. This reproduces that with three reservations, all
/// taken before the file system is touched:
///
/// 1. the `gzFile` block itself, [`reserve_block`] -- C's L100;
/// 2. the handle box, [`reserve_descriptor`] -- an allocation C does not have, which is
///    why it must be hoisted rather than left where it naturally falls;
/// 3. the path label, inside the core's `gz_open_with` -- C's L206.
///
/// Only then does the opener run, and after it nothing fallible remains: the descriptor is
/// stored into memory that already exists, and [`commit_block`] moves the finished state
/// into the block that was reserved in step 1.
///
/// The mode string is parsed by the core, which is also what rejects it before any of this
/// matters; `zlib_rs::gz::open`'s facade obligations require exactly that order.
///
/// # Safety
///
/// `target` must satisfy [`open_descriptor`]'s contract, and `label` and `mode` must be
/// readable for the duration of the call.
unsafe fn open_target(target: OpenTarget<'_>, label: &[u8], mode: &[u8]) -> gzFile {
    // Step 1: C's L100, before anything else can fail destructively.
    let block = reserve_block();
    if block.is_null() {
        return fallback::null_handle();
    }
    // Step 2: the allocation C does not have, hoisted ahead of the open for the reason
    // above. Failure releases the reservation from step 1 and touches nothing else.
    let Some(reserved) = reserve_descriptor() else {
        // SAFETY: `block` is the uncommitted reservation from step 1.
        unsafe { discard_reserved_block(block) };
        return fallback::null_handle();
    };

    // Every `Err` is C's `NULL`, which the core's `GzOpenError` documentation states
    // explicitly: an unusable mode string, an exhausted allocator and a refusal from the
    // file system are all answered the same way. A refusal from `open` leaves the
    // platform's `errno` exactly as `open` set it, and nothing on the way out disturbs
    // it -- which is what `zlib.h` L1394-L1401 promises a caller.
    let opened = rs_gz::gz_open_with(
        rs_gz::GzPathTarget::Narrow(label),
        label,
        mode,
        GlobalAllocator,
        // SAFETY: `target` satisfies `open_descriptor`'s contract by this function's own,
        // and the closure runs exactly once, inside this call, while it is still live.
        |_target, spec| unsafe { open_descriptor(target, spec, reserved) },
    );

    let Ok(state) = opened else {
        // Nothing was built, so nothing was committed and the reservation goes back.
        // SAFETY: `block` is the uncommitted reservation from step 1.
        unsafe { discard_reserved_block(block) };
        return fallback::null_handle();
    };
    // The last step of the ordering: the state is complete, so this is a move into memory
    // that was reserved before the file was opened.
    // SAFETY: `block` is the uncommitted reservation from step 1.
    unsafe { commit_block(block, state) }
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
/// consulted for the first of those and describes the `open(2)` that failed: nothing
/// between it and this return touches the value, and every allocation this call needs is
/// made *before* the open, so exhaustion cannot overwrite it -- nor create or truncate the
/// file. See [`open_target`] for the ordering and [`sys_open`] for the call.
///
/// # Safety
///
/// `path` and `mode` must each be either null or a NUL-terminated byte string that stays
/// readable, and is not written by anything else, for the duration of the call.
#[no_mangle]
pub unsafe extern "C" fn gzopen(path: *const c_char, mode: *const c_char) -> gzFile {
    guard(|| {
        // SAFETY: unsafe-site category 5 -- `open_path` is the only reader of the two C
        // strings, and the obligation it places on its caller is the one this function places
        // on its own: each pointer is null, which `c_bytes` answers rather than dereferences,
        // or it is the first byte of a NUL-terminated string that stays readable and unchanged
        // for the duration of the call. Nothing is dereferenced here, and neither pointer is
        // retained after it returns.
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
        // SAFETY: unsafe-site category 5 -- `open_path` is the only reader of the two C
        // strings, and the obligation it places on its caller is the one this function places
        // on its own: each pointer is null, which `c_bytes` answers rather than dereferences,
        // or it is the first byte of a NUL-terminated string that stays readable and unchanged
        // for the duration of the call. Nothing is dereferenced here, and neither pointer is
        // retained after it returns.
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
/// * **A failed `gzdopen` does not close `fd`.** Three things together make that hold on
///   every failure path: the mode string is parsed *before* the descriptor is adopted,
///   **every allocation is made before it too** (see [`dopen_descriptor`]'s five steps),
///   and [`Descriptor`]'s [`Drop`] relinquishes rather than closes.
///
/// Returns `Z_NULL` if there was insufficient memory for the state, if the mode was
/// invalid, or if `fd` is `-1` (`zlib.h` L1417-L1418 and L1424).
///
/// Available on every platform. A Windows `int fd` is a CRT file descriptor rather than a
/// `HANDLE`, which is why it cannot be adopted by `std::fs::File`; nothing here uses one --
/// [`Descriptor`] calls the CRT's `_read`, `_write`, `_lseeki64` and `_close`, which are
/// the functions C's own gz layer calls there.
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
        // `gzlib.c` L302-L303: `if (fd == -1 || (path = malloc(...)) == NULL) return NULL;`
        // -- `fd == -1` is C's FIRST test and its `||` short-circuits, so a `-1` descriptor
        // is answered before `mode` is looked at at all. Tested here rather than only in
        // `dopen_descriptor` because `c_bytes` scans the string: deciding after the scan
        // would read `mode`'s bytes on a call C answers without touching them, so
        // `gzdopen(-1, garbage)` would fault here and return `NULL` there.
        if fd == -1 {
            return fallback::null_handle();
        }
        // SAFETY: unsafe-site category 5, discharged by this function's own contract:
        // `mode` is null -- handled by `c_bytes` -- or a NUL-terminated readable string.
        let mode = unsafe { c_bytes(mode) };
        let Some(mode) = mode else {
            return fallback::null_handle();
        };
        // SAFETY: unsafe-site category 5 -- `dopen_descriptor` reads `mode`, and the obligation
        // it places on its caller is the one this function places on its own: `mode` is null,
        // which `c_bytes` answers rather than dereferences, or it is the first byte of a
        // NUL-terminated string that stays readable and unchanged for the duration of the call.
        // `fd` is a plain integer and carries no obligation. Nothing is dereferenced here, and
        // `mode` is not retained after the call returns.
        unsafe { dopen_descriptor(fd, mode) }
    })
}

/// The Unix body of [`gzdopen`], once the mode string is a slice.
///
/// Split out so that the descriptor-specific reasoning -- and the `#[cfg]` -- sit away
/// from the exported signature, and so that the non-Unix stub has a real function to
/// stand in for rather than a conditionally absent block.
///
/// The order of the steps is required rather than tidy, and it is C's:
///
/// 1. **Reject `fd == -1`.** C's first test (`gzlib.c` L302-L303), before anything is
///    allocated and before the descriptor is touched.
/// 2. **Parse the mode string.** `zlib_rs::gz::open`'s facade obligations require this to
///    come before adoption, so that an unusable mode never reaches the descriptor. The
///    parse is needed here anyway, for the `e` flag in step 5.
/// 3. **Reserve the `gzFile` block and the handle box.** Both allocations happen before
///    the descriptor is adopted, which is what keeps an out-of-memory failure from closing
///    or otherwise disturbing a descriptor the library never took. `zlib.h` L1414-L1416
///    depends on that: "gzdopen does not close fd if it fails."
/// 4. **Adopt.** The descriptor goes into the reserved box. Nothing that could damage it
///    remains: the core still copies the label, which can fail for memory (C's L206), and
///    that path hands the box back so its [`Drop`] relinquishes the descriptor unclosed.
/// 5. **Apply the descriptor flags,** once the stream exists. `fcntl(fd, F_SETFD, ...)`
///    for `e` (`gzlib.c` L258-L261) happens here, after the label copy, exactly as C
///    reaches L258 after L206; `O_NONBLOCK` for `N` is applied by the core through
///    [`GzHandle::set_nonblocking`] (L254-L257), which is the seam it provides for it.
///
/// ★ This body is no longer Unix-only, and that is the resolution of what used to be
/// divergence 4. A Windows `int fd` is a **CRT file descriptor**, not a `HANDLE`, so
/// `std::fs::File` cannot adopt one -- but nothing here goes through `File`. `libc` binds
/// the CRT's `_read`, `_write`, `_lseeki64` and `_close`, which are precisely the calls C's
/// own gz layer makes on that platform, so the same [`Descriptor`] serves both. See the
/// note above [`Descriptor`].
///
/// # Safety
///
/// As [`gzdopen`].
unsafe fn dopen_descriptor(fd: c_int, mode: &[u8]) -> gzFile {
    // Step 1: `gzlib.c` L302-L303.
    if fd == -1 {
        return fallback::null_handle();
    }
    // Step 2. The core re-parses the same string when it builds the state; parsing twice
    // is what C does too, in the sense that it reads `oflag` back out of its own local.
    let Ok(spec) = GzOpenSpec::parse(mode) else {
        return fallback::null_handle();
    };

    // Step 3, both reservations, before the descriptor is adopted.
    let block = reserve_block();
    if block.is_null() {
        return fallback::null_handle();
    }
    let Some(mut reserved) = reserve_descriptor() else {
        // SAFETY: `block` is the uncommitted reservation from step 3.
        unsafe { discard_reserved_block(block) };
        return fallback::null_handle();
    };

    // Step 4: `gzlib.c` L262, `state->fd = fd;`. From here the descriptor is this handle's
    // to close -- and only through `GzHandle::close`, never through `Drop`, so a failure
    // below hands it back to the caller untouched.
    let [slot] = &mut *reserved;
    *slot = Descriptor::new(fd);

    // The core adds C's `"<fd:%d>"` label (`gzlib.c` L304-L306), repeats the `fd == -1`
    // test, copies the label -- C's L206 -- and applies `O_NONBLOCK` for `N` through
    // `GzHandle::set_nonblocking` (C's L254-L257), then goes down the same path as an
    // opening by name.
    match rs_gz::gzdopen(reserved, fd, mode, GlobalAllocator) {
        Ok(state) => {
            // Step 5: the descriptor-flag half of C's adoption, `fcntl(fd, F_SETFD, ...)`
            // at `gzlib.c` L258-L261. Best effort, as C's is, and performed only once the
            // stream exists -- C reaches those lines after its own label allocation
            // (L206), so a stream that failed for memory has had no flag applied to its
            // descriptor in either implementation.
            if spec.cloexec() {
                apply_cloexec(fd);
            }
            // SAFETY: `block` is the uncommitted reservation from step 3.
            unsafe { commit_block(block, state) }
        }
        // The handle comes back unclosed and is dropped here; its `Drop` relinquishes the
        // descriptor without closing it, which is `zlib.h` L1415-L1416's promise. Naming
        // the field rather than dropping the whole error is what makes that deliberate.
        Err(failure) => {
            drop(failure.handle);
            // SAFETY: as above; nothing was committed.
            unsafe { discard_reserved_block(block) };
            fallback::null_handle()
        }
    }
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

// The C runtime's `wcstombs`, used to narrow a wide path exactly as `gz_open` does.
//
// `gzlib.c` L200-L219 narrows the error label with `wcstombs`, which converts to the
// **active code page** rather than to UTF-8 and reports `(size_t)-1` for text that page
// cannot represent. Reproducing the label C would record means calling the very same
// function, so it is declared here rather than approximated: no crate in the dependency
// inventory (AAP §0.5.1.3) provides it, and `std` already links the C runtime that defines
// it, so an `extern "C"` declaration costs nothing and adds no dependency.
#[cfg(windows)]
extern "C" {
    /// `size_t wcstombs(char *dest, const wchar_t *src, size_t n);`
    ///
    /// Called in the two-step form C uses. With `dest` null it returns the number of bytes
    /// the conversion needs, excluding the terminator; called again with a destination it
    /// fills up to `n` bytes. Either call returns `(size_t)-1` if some character has no
    /// multibyte representation in the active code page.
    fn wcstombs(dest: *mut c_char, src: *const wchar_t, n: usize) -> usize;
}

/// Narrows a wide path to the bytes C would store in `state->path`.
///
/// The Rust counterpart of `gzlib.c` L198-L219 for the `fd == -2` case, kept faithful in
/// three respects that a UTF-8 conversion gets wrong:
///
/// 1. it uses [`wcstombs`], so the label is in the active code page, which is what a C
///    caller reading `gzerror`'s message sees;
/// 2. an unconvertible path is **not** an error. C computes `len = wcstombs(NULL, path, 0)`,
///    gets `(size_t)-1`, allocates `len + 1` -- which wraps to zero -- and stores an empty
///    string. This returns an empty label for that case, which is the same observable
///    outcome without relying on the wrap;
/// 3. the allocation is **fallible**. C answers a failed `malloc` with `NULL`
///    (`gzlib.c` L206-L209), so exhaustion must be reportable rather than fatal. An
///    infallible `String` conversion aborts the process instead, and because the size comes
///    from the caller's path that is a caller-controlled abort.
///
/// Returns [`None`] only for allocation failure, which the caller answers with `Z_NULL`.
///
/// # Safety
///
/// `path` must be non-null and a zero-terminated array of `wchar_t` that stays readable, and
/// is not written by anything else, for the duration of the call.
#[cfg(windows)]
unsafe fn narrow_wide_label(path: *const wchar_t) -> Option<Vec<u8>> {
    // `wcstombs(NULL, path, 0)` -- C's L200-L202 sizing call.
    //
    // SAFETY: unsafe-site category 5, discharged by this function's own contract: `path` is
    // a non-null NUL-terminated wide string readable for the duration of the call, and a
    // null destination with a zero count asks `wcstombs` only to measure it.
    let needed = unsafe { wcstombs(ptr::null_mut(), path, 0) };

    // C's `(size_t)-1` sentinel: the path cannot be represented in the active code page, so
    // the label is empty. An empty `Vec` allocates nothing and cannot fail.
    if needed == usize::MAX {
        return Some(Vec::new());
    }

    // `malloc(len + 1)`, made fallible. `try_reserve_exact` is the only allocation here and
    // the only thing that can return `None`.
    let mut buffer = Vec::<u8>::new();
    let room = needed.checked_add(1)?;
    buffer.try_reserve_exact(room).ok()?;
    buffer.resize(room, 0);

    if needed != 0 {
        // C's `wcstombs(state->path, path, len + 1)` at L212.
        //
        // SAFETY: unsafe-site category 5. `buffer` holds `needed + 1` writable bytes, which
        // is the count passed, and `path` is the caller's NUL-terminated wide string. The
        // destination and the source cannot overlap: one is this function's own allocation.
        let written = unsafe { wcstombs(buffer.as_mut_ptr().cast::<c_char>(), path, room) };
        // A second call that now reports failure, or more bytes than it just asked for, is a
        // runtime disagreeing with itself. Truncating to what was requested keeps the label
        // inside its own allocation; the label is diagnostic only, so a short one is
        // harmless where a wrong length would not be.
        let produced = if written == usize::MAX {
            0
        } else {
            written.min(needed)
        };
        buffer.truncate(produced);
    } else {
        // C's `*(state->path) = 0` at L214: a zero-length path narrows to an empty string.
        buffer.clear();
    }

    Some(buffer)
}

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
/// The conversion is the boundary's job, and it goes through the C runtime's own
/// `wcstombs` (`gzlib.c` L200-L219) rather than a UTF-8 approximation, so the label a
/// caller reads back from `gzerror` is in the active code page exactly as C's is. See
/// [`narrow_wide_label`] for the three properties that depends on -- the code page, the
/// treatment of an unconvertible path, and the fallible allocation.
///
/// Note what travels where: the caller's `wchar_t` units are passed through
/// **unconverted** as the thing to open, while the narrowed text is supplied separately as
/// the error label -- exactly the split C draws between `_wopen`'s argument and
/// `state->path`. Narrowing the path *and* opening the narrowing could name a different
/// file, or none.
///
/// The order of operations is C's and is load-bearing: the mode string is rejected before
/// any allocation whose size the caller controls. The body says why.
///
/// # Safety
///
/// `path` must be null or a zero-terminated array of `wchar_t` that stays readable for the
/// duration of the call, and `mode` must satisfy [`gzopen`]'s contract.
#[cfg(windows)]
#[no_mangle]
pub unsafe extern "C" fn gzopen_w(path: *const wchar_t, mode: *const c_char) -> gzFile {
    guard(|| {
        if path.is_null() {
            // `gzlib.c` L96-L97 again: a null name is `NULL`, not an empty one -- and it is
            // C's first operand, so it is answered before `mode` is scanned. See
            // `open_path` for why the order and not just the outcome matters.
            return fallback::null_handle();
        }
        // SAFETY: unsafe-site category 5, discharged by this function's own contract:
        // `mode` is null -- handled by `c_bytes` -- or a NUL-terminated readable string.
        let mode = unsafe { c_bytes(mode) };
        let Some(mode) = mode else {
            return fallback::null_handle();
        };
        // ★ **The mode is judged before the path is narrowed, and that order is C's.**
        // `gz_open` parses and rejects the mode string at `gzlib.c` L107-L197 -- freeing
        // only the fixed-size state on each of its four refusals -- and reaches the path
        // narrowing and its `malloc` at L198-L219 solely for a mode it has accepted. Doing
        // the caller-sized work first would let an invalid mode still provoke an
        // allocation the size of the caller's path, which is both a divergence and, since
        // the size is the caller's to choose, a denial-of-service lever.
        //
        // The grammar is not re-implemented here: [`GzOpenSpec::parse`] is the core's
        // single source of truth, and its documentation records that being callable ahead
        // of any commitment is exactly what it is for. The accepted value is deliberately
        // discarded -- `open_target` re-parses the same bytes through the core, so this is
        // a rejection gate rather than a second parse whose result could drift.
        if GzOpenSpec::parse(mode).is_err() {
            return fallback::null_handle();
        }
        // The label C would record, in the active code page and through a fallible
        // allocation. `None` is a failed `malloc`, which C answers with `NULL`
        // (`gzlib.c` L206-L209).
        //
        // SAFETY: `path` is non-null by the test above and, by this function's contract, a
        // zero-terminated `wchar_t` array readable for the duration of the call -- which is
        // `narrow_wide_label`'s requirement.
        let narrowed = unsafe { narrow_wide_label(path) };
        let Some(label) = narrowed else {
            return fallback::null_handle();
        };
        // The units are opened **unconverted**, through `_wopen`, which is what C does
        // (`gzlib.c` L250-L251); only the label is narrowed. `open_target` performs the
        // same three reservations before opening as it does for a narrow path.
        //
        // SAFETY: `path` is the caller's zero-terminated wide string, live for this call,
        // which is `open_descriptor`'s contract for the `Wide` variant.
        unsafe { open_target(OpenTarget::Wide(path), &label, mode) }
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
/// `file` must be null or satisfy `block_mut`'s contract.
#[no_mangle]
pub unsafe extern "C" fn gzbuffer(file: gzFile, size: c_uint) -> c_int {
    guard(|| {
        // SAFETY: unsafe-site categories 1 and 3 -- handle validation, then the state
        // borrow. Nothing is read through `file` before it is checked: `state_mut` tests it
        // for null and for `GzBlock` alignment, then reads the tag, and answers `None`
        // unless the block is one `install` produced and `release` has not taken. What this
        // call site adds is what no check can establish: a non-null `file` addresses a live
        // block, passing an already-closed handle being undefined here exactly as it is in
        // C (`zlib.h` L1752-L1756). The borrow is the only one taken for this call and ends
        // with it. The core answers a `None` with -1 itself -- C's `if (file == NULL)
        // return -1;` at `gzlib.c` L326-L327 -- so the guard's result travels inward rather
        // than being branched on here.
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
/// `file` must be null or satisfy `block_mut`'s contract.
#[no_mangle]
pub unsafe extern "C" fn gzsetparams(file: gzFile, level: c_int, strategy: c_int) -> c_int {
    guard_code(|| {
        // SAFETY: unsafe-site categories 1 and 3 -- handle validation, then the state
        // borrow. Nothing is read through `file` before it is checked: `state_mut` tests it
        // for null and for `GzBlock` alignment, then reads the tag, and answers `None`
        // unless the block is one `install` produced and `release` has not taken. What this
        // call site adds is what no check can establish: a non-null `file` addresses a live
        // block, passing an already-closed handle being undefined here exactly as it is in
        // C (`zlib.h` L1752-L1756). The borrow is the only one taken for this call and ends
        // with it. As in `gzbuffer`, the core owns the `None` answer -- C's `if (file ==
        // NULL) return Z_STREAM_ERROR;` at `gzwrite.c` L637-L638.
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
/// `file` must be null or satisfy `block_mut`'s contract. If `len` is non-zero, `buf`
/// must be non-null and writable for `len` bytes, and that region must not overlap the
/// state behind `file` or any other object the library holds.
#[no_mangle]
pub unsafe extern "C" fn gzread(file: gzFile, buf: voidp, len: c_uint) -> c_int {
    guard(|| {
        // SAFETY: unsafe-site categories 1 and 3 -- handle validation, then the state
        // borrow. Nothing is read through `file` before it is checked: `state_mut` tests it
        // for null and for `GzBlock` alignment, then reads the tag, and answers `None`
        // unless the block is one `install` produced and `release` has not taken. What this
        // call site adds is what no check can establish: a non-null `file` addresses a live
        // block, passing an already-closed handle being undefined here exactly as it is in
        // C (`zlib.h` L1752-L1756). The borrow is the only one taken for this call and ends
        // with it. The block is a separate allocation from the caller's buffer, so the
        // slice built below cannot alias it.
        let state = unsafe { state_mut(file) };
        let Some(state) = state else {
            return fallback::GZ_ERROR;
        };
        // SAFETY: unsafe-site category 2, discharged by this function's own contract:
        // `len` bytes at `buf` are writable and unaliased for the duration of the call.
        // `widen` is the crate's `uInt` -> `usize` conversion and cannot lose a bit.
        let mut window = unsafe { read_region(buf, widen(len)) };
        rs_gz::gzread_into(state, &mut window)
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
/// `file` must be null or satisfy `block_mut`'s contract. If `size * nitems` is
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
        // SAFETY: unsafe-site categories 1 and 3 -- handle validation, then the state
        // borrow. Nothing is read through `file` before it is checked: `state_mut` tests it
        // for null and for `GzBlock` alignment, then reads the tag, and answers `None`
        // unless the block is one `install` produced and `release` has not taken. What this
        // call site adds is what no check can establish: a non-null `file` addresses a live
        // block, passing an already-closed handle being undefined here exactly as it is in
        // C (`zlib.h` L1752-L1756). The borrow is the only one taken for this call and ends
        // with it. The block is a separate allocation from the caller's buffer, so the
        // slice built below cannot alias it.
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
        let mut window = unsafe { read_region(buf, len) };
        rs_gz::gzfread_into(state, &mut window, size, nitems)
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
/// `file` must be null or satisfy `block_mut`'s contract. If `len` is at least 1, `buf`
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
        // SAFETY: unsafe-site categories 1 and 3 -- handle validation, then the state
        // borrow. Nothing is read through `file` before it is checked: `state_mut` tests it
        // for null and for `GzBlock` alignment, then reads the tag, and answers `None`
        // unless the block is one `install` produced and `release` has not taken. What this
        // call site adds is what no check can establish: a non-null `file` addresses a live
        // block, passing an already-closed handle being undefined here exactly as it is in
        // C (`zlib.h` L1752-L1756). The borrow is the only one taken for this call and ends
        // with it. The block is a separate allocation from the caller's buffer, so the
        // slice built below cannot alias it.
        let state = unsafe { state_mut(file) };
        let Some(state) = state else {
            return fallback::null_string();
        };
        // SAFETY: unsafe-site category 2, discharged by this function's own contract:
        // `capacity` is `len` and those bytes at `buf` are writable and unaliased for the
        // duration of the call. `c_char` and `u8` have the same size and alignment, and
        // the cast is to a byte pointer rather than a differently-sized one.
        let mut window = unsafe { read_region(buf.cast(), capacity) };
        match rs_gz::gzgets_into(state, &mut window) {
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
    // SAFETY: unsafe-site categories 1 and 3 -- handle validation, then the state borrow.
    // Nothing is read through `file` before it is checked: `state_mut` tests it for null
    // and for `GzBlock` alignment, then reads the tag, and answers `None` unless the block
    // is one `install` produced and `release` has not taken. What this call site adds is
    // what no check can establish: a non-null `file` addresses a live block, passing an
    // already-closed handle being undefined here exactly as it is in C (`zlib.h`
    // L1752-L1756). The borrow is the only one taken for this call and ends with it.
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
/// `file` must be null or satisfy `block_mut`'s contract.
#[no_mangle]
pub unsafe extern "C" fn gzgetc(file: gzFile) -> c_int {
    guard(|| {
        // SAFETY: unsafe-site categories 1 and 3 -- `getc_body` performs the validation and
        // takes the borrow, and the obligation it places on its caller is the one this
        // function places on its own, so it is discharged rather than weakened: `file` is
        // null, and is answered with the documented failure value rather than dereferenced,
        // or it addresses a live `GzBlock` from `install` that has not been closed and that
        // nothing else has borrowed. Nothing is dereferenced here and no borrow outlives
        // the call.
        unsafe { getc_body(file) }
    })
}

/// Read and decompress one byte from `file`. The function form, kept for compatibility.
///
/// `zlib.h` L1961 declares it as `int gzgetc_(gzFile file);` and annotates it, in a trailing
/// comment, as being there for backward compatibility. Ported from `gzread.c` L500-L502.
///
/// ★ THE ANNOTATION IS PARAPHRASED ON PURPOSE, AND MUST STAY THAT WAY. Quoting the header's
/// trailing comment verbatim would put a C block-comment TERMINATOR inside this doc comment, and
/// cbindgen wraps each doc comment in a single C block comment (`documentation_style = "c"` in
/// `cbindgen.toml`). The terminator closes that block early, leaving the rest of the prose to be
/// lexed as code: the generated header then fails to parse, which was measured in C89, C99, C17
/// and C++17 alike. No doc comment anywhere in this crate may contain that two-character
/// sequence -- and `make rust-header` compiles the generated artifact as C and as C++ so that a
/// reintroduction is a build failure rather than a surprise for whoever includes it next.
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
        // SAFETY: unsafe-site categories 1 and 3 -- `getc_body` performs the validation and
        // takes the borrow, and the obligation it places on its caller is the one this
        // function places on its own, so it is discharged rather than weakened: `file` is
        // null, and is answered with the documented failure value rather than dereferenced,
        // or it addresses a live `GzBlock` from `install` that has not been closed and that
        // nothing else has borrowed. Nothing is dereferenced here and no borrow outlives
        // the call.
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
/// `file` must be null or satisfy `block_mut`'s contract.
#[no_mangle]
pub unsafe extern "C" fn gzungetc(c: c_int, file: gzFile) -> c_int {
    guard(|| {
        // SAFETY: unsafe-site categories 1 and 3 -- handle validation, then the state
        // borrow. Nothing is read through `file` before it is checked: `state_mut` tests it
        // for null and for `GzBlock` alignment, then reads the tag, and answers `None`
        // unless the block is one `install` produced and `release` has not taken. What this
        // call site adds is what no check can establish: a non-null `file` addresses a live
        // block, passing an already-closed handle being undefined here exactly as it is in
        // C (`zlib.h` L1752-L1756). The borrow is the only one taken for this call and ends
        // with it.
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
/// `file` must be null or satisfy `block_mut`'s contract. If `len` is non-zero, `buf`
/// must be non-null and readable for `len` bytes, and must stay valid and unwritten for
/// the duration of the call.
#[no_mangle]
pub unsafe extern "C" fn gzwrite(file: gzFile, buf: voidpc, len: c_uint) -> c_int {
    guard(|| {
        // SAFETY: unsafe-site categories 1 and 3 -- handle validation, then the state
        // borrow. Nothing is read through `file` before it is checked: `state_mut` tests it
        // for null and for `GzBlock` alignment, then reads the tag, and answers `None`
        // unless the block is one `install` produced and `release` has not taken. What this
        // call site adds is what no check can establish: a non-null `file` addresses a live
        // block, passing an already-closed handle being undefined here exactly as it is in
        // C (`zlib.h` L1752-L1756). The borrow is the only one taken for this call and ends
        // with it. The block is a separate allocation from the caller's buffer, so the
        // slice built below cannot alias it.
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
/// `file` must be null or satisfy `block_mut`'s contract. If `size * nitems` is
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
        // SAFETY: unsafe-site categories 1 and 3 -- handle validation, then the state
        // borrow. Nothing is read through `file` before it is checked: `state_mut` tests it
        // for null and for `GzBlock` alignment, then reads the tag, and answers `None`
        // unless the block is one `install` produced and `release` has not taken. What this
        // call site adds is what no check can establish: a non-null `file` addresses a live
        // block, passing an already-closed handle being undefined here exactly as it is in
        // C (`zlib.h` L1752-L1756). The borrow is the only one taken for this call and ends
        // with it. The block is a separate allocation from the caller's buffer, so the
        // slice built below cannot alias it.
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
/// `file` must be null or satisfy `block_mut`'s contract.
#[no_mangle]
pub unsafe extern "C" fn gzputc(file: gzFile, c: c_int) -> c_int {
    guard(|| {
        // SAFETY: unsafe-site categories 1 and 3 -- handle validation, then the state
        // borrow. Nothing is read through `file` before it is checked: `state_mut` tests it
        // for null and for `GzBlock` alignment, then reads the tag, and answers `None`
        // unless the block is one `install` produced and `release` has not taken. What this
        // call site adds is what no check can establish: a non-null `file` addresses a live
        // block, passing an already-closed handle being undefined here exactly as it is in
        // C (`zlib.h` L1752-L1756). The borrow is the only one taken for this call and ends
        // with it.
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
/// `file` must be null or satisfy `block_mut`'s contract. `s` must be null or a
/// NUL-terminated byte string that stays readable, and is not written by anything else,
/// for the duration of the call.
#[no_mangle]
pub unsafe extern "C" fn gzputs(file: gzFile, s: *const c_char) -> c_int {
    guard(|| {
        // ★ The string is scanned LAST. C's order is `file == NULL`, then
        // `state->mode != GZ_WRITE || state->err != Z_OK`, and only then `strlen(s)`
        // (`gzwrite.c` L358-L364) -- so a stream that is null or not writable is refused
        // without `s` being read at all. Scanning first, as a `c_bytes`-then-`state_mut`
        // sequence would, dereferences a pointer C never touches: `gzputs(NULL, garbage)`
        // and `gzputs(read_stream, garbage)` both return -1 in C and would both fault here.
        //
        // SAFETY: unsafe-site categories 1 and 3 -- handle validation, then the state
        // borrow. Nothing is read through `file` before it is checked: `state_mut` tests it
        // for null and for `GzBlock` alignment, then reads the tag, and answers `None`
        // unless the block is one `reserve_block`/`commit_block` produced and `release` has
        // not taken. What this call site adds is what no check can establish: a non-null
        // `file` addresses a live block, passing an already-closed handle being undefined
        // here exactly as it is in C (`zlib.h` L1752-L1756). The borrow is the only one taken
        // for this call and ends with it, before `s` is scanned at all.

        let state = unsafe { state_mut(file) };
        let Some(state) = state else {
            return fallback::GZ_ERROR;
        };
        // C's second test, asked without mutating anything so that it can be asked here
        // rather than inside the core's `gzputs`. The core repeats it -- this is an
        // ordering guard, not a replacement -- and neither records an error, because C
        // returns -1 from this path leaving `state->err` exactly as it was.
        if !state.accepts_writes() {
            return fallback::GZ_ERROR;
        }
        // SAFETY: unsafe-site category 5, discharged by this function's own contract: `s`
        // is null -- handled by `c_bytes` -- or a NUL-terminated readable string.
        let text = unsafe { c_bytes(s) };
        let Some(text) = text else {
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
/// `file` must be null or satisfy `block_mut`'s contract.
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
        // SAFETY: unsafe-site categories 1 and 3 -- handle validation, then the state
        // borrow. Nothing is read through `file` before it is checked: `state_mut` tests it
        // for null and for `GzBlock` alignment, then reads the tag, and answers `None`
        // unless the block is one `install` produced and `release` has not taken. What this
        // call site adds is what no check can establish: a non-null `file` addresses a live
        // block, passing an already-closed handle being undefined here exactly as it is in
        // C (`zlib.h` L1752-L1756). The borrow is the only one taken for this call and ends
        // with it. `scratch` and `size` were tested for null above and address storage
        // owned by the shim, which is a separate allocation again, so the writes through
        // them below cannot alias either the block or each other.
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
        // SAFETY: unsafe-site categories 1 and 3 -- handle validation, then the state
        // borrow. Nothing is read through `file` before it is checked: `state_mut` tests it
        // for null and for `GzBlock` alignment, then reads the tag, and answers `None`
        // unless the block is one `install` produced and `release` has not taken. What this
        // call site adds is what no check can establish: a non-null `file` addresses a live
        // block, passing an already-closed handle being undefined here exactly as it is in
        // C (`zlib.h` L1752-L1756). The borrow is the only one taken for this call and ends
        // with it. The region lent by `_zlib_rs_gzprintf_begin` is no longer borrowed: its
        // loan was consumed there, and the shim performed nothing but `vsnprintf` in
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
    // SAFETY: unsafe-site categories 1 and 3 -- handle validation, then the state borrow.
    // Nothing is read through `file` before it is checked: `state_mut` tests it for null
    // and for `GzBlock` alignment, then reads the tag, and answers `None` unless the block
    // is one `install` produced and `release` has not taken. What this call site adds is
    // what no check can establish: a non-null `file` addresses a live block, passing an
    // already-closed handle being undefined here exactly as it is in C (`zlib.h`
    // L1752-L1756). The borrow is the only one taken for this call and ends with it.
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
/// `file` must be null or satisfy `block_mut`'s contract.
#[no_mangle]
pub unsafe extern "C" fn gzseek(file: gzFile, offset: z_off_t, whence: c_int) -> z_off_t {
    guard(|| {
        // SAFETY: unsafe-site categories 1 and 3 -- `seek_body` performs the validation and
        // takes the borrow, and the obligation it places on its caller is the one this
        // function places on its own, so it is discharged rather than weakened: `file` is
        // null, and is answered with the documented failure value rather than dereferenced,
        // or it addresses a live `GzBlock` from `install` that has not been closed and that
        // nothing else has borrowed. Nothing is dereferenced here and no borrow outlives
        // the call.
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
        // SAFETY: unsafe-site categories 1 and 3 -- `seek_body` performs the validation and
        // takes the borrow, and the obligation it places on its caller is the one this
        // function places on its own, so it is discharged rather than weakened: `file` is
        // null, and is answered with the documented failure value rather than dereferenced,
        // or it addresses a live `GzBlock` from `install` that has not been closed and that
        // nothing else has borrowed. Nothing is dereferenced here and no borrow outlives
        // the call.
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
/// `file` must be null or satisfy `block_mut`'s contract.
#[no_mangle]
pub unsafe extern "C" fn gzrewind(file: gzFile) -> c_int {
    guard(|| {
        // SAFETY: unsafe-site categories 1 and 3 -- handle validation, then the state
        // borrow. Nothing is read through `file` before it is checked: `state_mut` tests it
        // for null and for `GzBlock` alignment, then reads the tag, and answers `None`
        // unless the block is one `install` produced and `release` has not taken. What this
        // call site adds is what no check can establish: a non-null `file` addresses a live
        // block, passing an already-closed handle being undefined here exactly as it is in
        // C (`zlib.h` L1752-L1756). The borrow is the only one taken for this call and ends
        // with it.
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
    // SAFETY: unsafe-site categories 1 and 3 -- handle validation, then the state borrow.
    // Nothing is read through `file` before it is checked: `state_ref` tests it for null
    // and for `GzBlock` alignment, then reads the tag, and answers `None` unless the block
    // is one `install` produced and `release` has not taken. What this call site adds is
    // what no check can establish: a non-null `file` addresses a live block, passing an
    // already-closed handle being undefined here exactly as it is in C (`zlib.h`
    // L1752-L1756). The borrow is shared, no mutable borrow of the same block is taken
    // anywhere in this call, and it ends with it.
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
/// `file` must be null or satisfy `state_ref`'s contract.
#[no_mangle]
pub unsafe extern "C" fn gztell(file: gzFile) -> z_off_t {
    guard(|| {
        // SAFETY: unsafe-site categories 1 and 3 -- `tell_body` performs the validation and
        // takes the borrow, and the obligation it places on its caller is the one this
        // function places on its own, so it is discharged rather than weakened: `file` is
        // null, and is answered with the documented failure value rather than dereferenced,
        // or it addresses a live `GzBlock` from `install` that has not been closed and that
        // nothing else has borrowed. Nothing is dereferenced here and no borrow outlives
        // the call.
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
        // SAFETY: unsafe-site categories 1 and 3 -- `tell_body` performs the validation and
        // takes the borrow, and the obligation it places on its caller is the one this
        // function places on its own, so it is discharged rather than weakened: `file` is
        // null, and is answered with the documented failure value rather than dereferenced,
        // or it addresses a live `GzBlock` from `install` that has not been closed and that
        // nothing else has borrowed. Nothing is dereferenced here and no borrow outlives
        // the call.
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
    // SAFETY: unsafe-site categories 1 and 3 -- handle validation, then the state borrow.
    // Nothing is read through `file` before it is checked: `state_mut` tests it for null
    // and for `GzBlock` alignment, then reads the tag, and answers `None` unless the block
    // is one `install` produced and `release` has not taken. What this call site adds is
    // what no check can establish: a non-null `file` addresses a live block, passing an
    // already-closed handle being undefined here exactly as it is in C (`zlib.h`
    // L1752-L1756). The borrow is the only one taken for this call and ends with it.
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
/// `file` must be null or satisfy `block_mut`'s contract.
#[no_mangle]
pub unsafe extern "C" fn gzoffset(file: gzFile) -> z_off_t {
    guard(|| {
        // SAFETY: unsafe-site categories 1 and 3 -- `offset_body` performs the validation
        // and takes the borrow, and the obligation it places on its caller is the one this
        // function places on its own, so it is discharged rather than weakened: `file` is
        // null, and is answered with the documented failure value rather than dereferenced,
        // or it addresses a live `GzBlock` from `install` that has not been closed and that
        // nothing else has borrowed. Nothing is dereferenced here and no borrow outlives
        // the call.
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
        // SAFETY: unsafe-site categories 1 and 3 -- `offset_body` performs the validation
        // and takes the borrow, and the obligation it places on its caller is the one this
        // function places on its own, so it is discharged rather than weakened: `file` is
        // null, and is answered with the documented failure value rather than dereferenced,
        // or it addresses a live `GzBlock` from `install` that has not been closed and that
        // nothing else has borrowed. Nothing is dereferenced here and no borrow outlives
        // the call.
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
/// `file` must be null or satisfy `block_mut`'s contract.
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
/// `file` must be null or satisfy `block_mut`'s contract.
#[no_mangle]
pub unsafe extern "C" fn gzdirect(file: gzFile) -> c_int {
    guard(|| {
        // SAFETY: unsafe-site categories 1 and 3 -- handle validation, then the state
        // borrow. Nothing is read through `file` before it is checked: `state_mut` tests it
        // for null and for `GzBlock` alignment, then reads the tag, and answers `None`
        // unless the block is one `install` produced and `release` has not taken. What this
        // call site adds is what no check can establish: a non-null `file` addresses a live
        // block, passing an already-closed handle being undefined here exactly as it is in
        // C (`zlib.h` L1752-L1756). The borrow is the only one taken for this call and ends
        // with it. The core answers a `None` with 0 itself.
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
/// # Lifetime of the returned string, and why nothing is allocated here
///
/// `zlib.h` L1783-L1786: the application must not modify it, a later call may invalidate
/// it, and it is unavailable once the file is closed. All three hold because the pointer
/// addresses the stream's own stored message -- `zlib_rs::gz::GzState::msg_with_nul` --
/// which `gz_error` built, terminator included, when the error was *recorded*. That is
/// exactly C's arrangement: the `malloc` is at `gzlib.c` L576-L584 and `gzerror` is a
/// pure read at L527.
///
/// ★ **This function performs no allocation, and that is a correctness property rather
/// than an optimisation.** An earlier revision stored the message unterminated and built
/// a terminated copy here, which introduced an allocation failure path C does not have --
/// on the one call a caller makes *because* something has already gone wrong. Under
/// memory pressure the text explaining the failure would then be the thing that could not
/// be produced, and the caller would see `""` instead of the reason. There is now no
/// query-time allocation to fail.
///
/// The two answers that are not stored messages are `'static`: [`fallback::GZ_OUT_OF_MEMORY`]
/// for `Z_MEM_ERROR`, for which C returns a literal precisely because `gz_error` declines
/// to allocate one (`gzlib.c` L522-L523 and L571-L572), and [`fallback::GZ_NO_MESSAGE`]
/// -- the empty string -- when there is no message at all (L527).
///
/// # Safety
///
/// `file` must be null or satisfy `block_mut`'s contract. `errnum` must be null or a
/// writable, aligned `int`.
#[no_mangle]
pub unsafe extern "C" fn gzerror(file: gzFile, errnum: *mut c_int) -> *const c_char {
    guard(|| {
        // SAFETY: unsafe-site categories 1 and 3, discharged by this function's own
        // contract.
        let state = unsafe { state_mut(file) };
        let Some(state) = state else {
            // `gzlib.c` L517-L518: `if (file == NULL) return NULL;`. See above.
            return ptr::null();
        };

        let mut code = ReturnCode::OK.as_i32();
        // The core's answer is consumed as two facts -- whether there is one at all, and
        // whether it is empty -- so that its borrow of the state ends here and the
        // pointer below can be derived from a fresh one. The bytes themselves are not
        // used: `msg_with_nul` returns the same message with C's terminator attached.
        let Some(empty) = rs_gz::gzerror(Some(state), Some(&mut code)).map(<[u8]>::is_empty) else {
            // The core returns `None` on exactly the paths where C returns `NULL` without
            // writing `*errnum` -- an unrecognised mode, or an exposed prefix that is not
            // consistent with the output buffer -- so neither is written here either.
            return ptr::null();
        };

        // C's `if (errnum != NULL) *errnum = state->err;` (`gzlib.c` L524-L525),
        // performed only on the path where C performs it.
        if !errnum.is_null() {
            // SAFETY: unsafe-site category 2, in the outward direction. `errnum` is
            // non-null on this arm and is a writable, aligned `int` by this function's
            // contract, so exactly one `int` is written and nothing else is touched.
            unsafe { *errnum = code };
        }

        if empty {
            // C's `""` (`gzlib.c` L527), a static empty string rather than an allocation.
            return fallback::GZ_NO_MESSAGE.as_ptr();
        }
        if code == ReturnCode::MEM_ERROR.as_i32() {
            // `gzlib.c` L522-L523: `if (state->err == Z_MEM_ERROR) return "out of memory";`.
            // A literal, because `gz_error` deliberately stored nothing (L571-L572).
            return fallback::GZ_OUT_OF_MEMORY.as_ptr();
        }

        // The stored message, terminator and all. No allocation, no copy, and no failure
        // path -- see the note above.
        match state.msg_with_nul() {
            Some(stored) => stored.as_ptr().cast::<c_char>(),
            None => fallback::GZ_NO_MESSAGE.as_ptr(),
        }
    })
}

/// Clear the error and end-of-file flags for `file`.
///
/// `zlib.h` L1792: `void gzclearerr(gzFile file);`. Ported from `gzlib.c` L531-L547.
/// Analogous to `stdio`'s `clearerr`, and useful for continuing to read a gzip file that
/// is being written concurrently (`zlib.h` L1794-L1795).
///
/// ★ **The only `void`-returning export in this crate**, which is why it routes through
/// `panic_guard::guard` with `T = ()` rather than through `guard_code`, and why
/// that guard is deliberately not `#[must_use]`.
///
/// The end-of-file flags are cleared only on a read stream, because neither means anything
/// on a write stream; the error itself is cleared for both. Note that `Z_OK` is not a fatal
/// code, so the exposed `have` count survives -- clearing an error must not discard output
/// the caller has not yet been given.
///
/// # Safety
///
/// `file` must be null or satisfy `block_mut`'s contract.
#[no_mangle]
pub unsafe extern "C" fn gzclearerr(file: gzFile) {
    guard(|| {
        // SAFETY: unsafe-site categories 1 and 3 -- handle validation, then the state
        // borrow. Nothing is read through `file` before it is checked: `state_mut` tests it
        // for null and for `GzBlock` alignment, then reads the tag, and answers `None`
        // unless the block is one `install` produced and `release` has not taken. What this
        // call site adds is what no check can establish: a non-null `file` addresses a live
        // block, passing an already-closed handle being undefined here exactly as it is in
        // C (`zlib.h` L1752-L1756). The borrow is the only one taken for this call and ends
        // with it. The core makes a `None` a no-op itself -- C's two early returns at
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
    // SAFETY: unsafe-site categories 1 and 3 -- handle validation, then the state borrow.
    // Nothing is read through `file` before it is checked: `state_ref` tests it for null
    // and for `GzBlock` alignment, then reads the tag, and answers `None` unless the block
    // is one `install` produced and `release` has not taken. What this call site adds is
    // what no check can establish: a non-null `file` addresses a live block, passing an
    // already-closed handle being undefined here exactly as it is in C (`zlib.h`
    // L1752-L1756). The borrow is shared, no mutable borrow of the same block is taken
    // anywhere in this call, and it ends with it. A shared borrow suffices: `mode` is a
    // plain field and this decides nothing else.
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
/// undefined behaviour. What this file adds is that `release` poisons the tag first, so a
/// second close on memory the allocator has not yet reused reports `Z_STREAM_ERROR` instead
/// of running a second teardown. That is a diagnostic and not a licence.
///
/// # Safety
///
/// `file` must be null or satisfy `block_mut`'s contract, and must not have been closed
/// before. After this returns, `file` is dangling and must never be used again.
#[no_mangle]
pub unsafe extern "C" fn gzclose(file: gzFile) -> c_int {
    guard_code(|| {
        // SAFETY: unsafe-site categories 1 and 3 -- `close_direction` performs the
        // validation and takes the borrow, and the obligation it places on its caller is
        // the one this function places on its own, so it is discharged rather than
        // weakened: `file` is null, and is answered with the documented failure value
        // rather than dereferenced, or it addresses a live `GzBlock` from `install` that
        // has not been closed and that nothing else has borrowed. Nothing is dereferenced
        // here and no borrow outlives the call. Reading the direction first is what keeps
        // the release decision faithful to C's, and the shared borrow it takes has ended
        // before the mutable one below begins.
        let direction = unsafe { close_direction(file) };
        if direction.is_none() {
            // `gzclose.c` L15-L16 for a null handle, and `gzwrite.c` L677-L678 for a mode
            // that is not a direction: `Z_STREAM_ERROR`, and nothing is freed.
            return ReturnCode::STREAM_ERROR;
        }
        // SAFETY: unsafe-site categories 1 and 3 -- handle validation, then the state
        // borrow. Nothing is read through `file` before it is checked: `state_mut` tests it
        // for null and for `GzBlock` alignment, then reads the tag, and answers `None`
        // unless the block is one `install` produced and `release` has not taken. What this
        // call site adds is what no check can establish: a non-null `file` addresses a live
        // block, passing an already-closed handle being undefined here exactly as it is in
        // C (`zlib.h` L1752-L1756). The borrow is the only one taken for this call and ends
        // with it. The shared borrow `close_direction` took above has already ended, so
        // this is again the only borrow of the block alive.
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
        // SAFETY: unsafe-site categories 1 and 3 -- `close_direction` performs the
        // validation and takes the borrow, and the obligation it places on its caller is
        // the one this function places on its own, so it is discharged rather than
        // weakened: `file` is null, and is answered with the documented failure value
        // rather than dereferenced, or it addresses a live `GzBlock` from `install` that
        // has not been closed and that nothing else has borrowed. Nothing is dereferenced
        // here and no borrow outlives the call.
        if unsafe { close_direction(file) } != Some(true) {
            // `gzread.c` L648-L649: not a read stream, so nothing is torn down and nothing
            // is freed.
            return ReturnCode::STREAM_ERROR;
        }
        // SAFETY: unsafe-site categories 1 and 3 -- handle validation, then the state
        // borrow. Nothing is read through `file` before it is checked: `state_mut` tests it
        // for null and for `GzBlock` alignment, then reads the tag, and answers `None`
        // unless the block is one `install` produced and `release` has not taken. What this
        // call site adds is what no check can establish: a non-null `file` addresses a live
        // block, passing an already-closed handle being undefined here exactly as it is in
        // C (`zlib.h` L1752-L1756). The borrow is the only one taken for this call and ends
        // with it. The shared borrow `close_direction` took above has already ended.
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
        // SAFETY: unsafe-site categories 1 and 3 -- `close_direction` performs the
        // validation and takes the borrow, and the obligation it places on its caller is
        // the one this function places on its own, so it is discharged rather than
        // weakened: `file` is null, and is answered with the documented failure value
        // rather than dereferenced, or it addresses a live `GzBlock` from `install` that
        // has not been closed and that nothing else has borrowed. Nothing is dereferenced
        // here and no borrow outlives the call.
        if unsafe { close_direction(file) } != Some(false) {
            // `gzwrite.c` L677-L678: not a write stream, so nothing is torn down and
            // nothing is freed.
            return ReturnCode::STREAM_ERROR;
        }
        // SAFETY: unsafe-site categories 1 and 3 -- handle validation, then the state
        // borrow. Nothing is read through `file` before it is checked: `state_mut` tests it
        // for null and for `GzBlock` alignment, then reads the tag, and answers `None`
        // unless the block is one `install` produced and `release` has not taken. What this
        // call site adds is what no check can establish: a non-null `file` addresses a live
        // block, passing an already-closed handle being undefined here exactly as it is in
        // C (`zlib.h` L1752-L1756). The borrow is the only one taken for this call and ends
        // with it. The shared borrow `close_direction` took above has already ended.
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
//   * The two `gzprintf` helpers, driven directly rather than through the shim. The shim
//     IS linked into this test binary -- `build.rs` compiles it for every artifact -- but
//     its C caller needs a `va_list`, which a Rust test cannot construct, so the protocol
//     the two helpers implement is exercised here and the shim itself is exercised by the
//     unmodified `test/example.c`, whose `gzprintf(file, ", %s!", "hello") == 8` assertion
//     runs against the packaged library.
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
// immutable. Their real caller is `csrc/gzprintf_shim.c`, which `build.rs` compiles and links
// into every artifact; because reaching them through it needs a `va_list` that no Rust caller
// can build, these tests are the only Rust callers they will ever have.
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::used_underscore_items
)]
mod tests {
    //! Unit tests for the `gzFile` facade, driven through the real C entry points.
    //!
    //! # Where the `unsafe` lives
    //!
    //! Not in the test bodies. Every raw call into this module's `extern "C"` exports and
    //! private helpers goes through a named **call gate** -- `close`, `tell`, `get_line`,
    //! `printf_begin` and so on -- and the gates are collected in one section below, under
    //! a banner that states the invariant they all discharge. A test body therefore reads
    //! as named operations, and an auditor reading the raw calls has one section to read
    //! rather than two hundred call sites.
    //!
    //! A handful of blocks are still written inline, and each carries its own
    //! `// SAFETY:`: the ones that reproduce the *caller-compiled* `gzgetc` macro, the ones
    //! that write into the region `printf_begin` hands over, the one that corrupts a block's
    //! tag on purpose, and the one that closes a descriptor behind the library's back. Those
    //! are the sites where the invariant is the interesting part of the test rather than
    //! boilerplate, so stating it locally is the point.
    //!
    //! `clippy::undocumented_unsafe_blocks` is denied workspace-wide, so this arrangement is
    //! machine-checked: there is no configuration in which an undocumented block can be
    //! added to this module without failing the build.
    //!
    //! Tests may panic -- that is how they report failure -- so `clippy.toml` allows
    //! panicking here and only here, and the `#[allow]` above re-enables `unwrap`,
    //! indexing and `panic!` for the same reason.

    use super::{
        _zlib_rs_gzprintf_begin, _zlib_rs_gzprintf_commit, block_mut, commit_block, gzbuffer,
        gzclearerr, gzclose, gzclose_r, gzclose_w, gzdirect, gzeof, gzerror, gzflush, gzfread,
        gzfwrite, gzgetc, gzgetc_, gzgets, gzoffset, gzoffset64, gzopen, gzopen64, gzputc, gzputs,
        gzread, gzrewind, gzseek, gzseek64, gzsetparams, gztell, gztell64, gzungetc, gzwrite,
        release, reserve_block, state_mut, FacadeGzState, GzBlock, GZ_TAG_CLOSED, GZ_TAG_LIVE,
    };

    use core::ffi::{c_char, c_int, c_uint, c_void};
    use core::sync::atomic::{AtomicU32, Ordering};

    use std::ffi::{CStr, CString};
    #[cfg(windows)]
    use std::os::windows::ffi::OsStrExt;

    #[cfg(windows)]
    use super::{gzopen_w, narrow_wide_label};
    use std::path::PathBuf;

    use zlib_rs::allocate::GlobalAllocator;
    use zlib_rs::error::ReturnCode;

    use crate::types::{gzFile, gzFile_s, voidp, voidpc, z_off64_t, z_off_t, z_size_t};

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
        // SAFETY: the C-string clause of the call-gate banner below. Both pointers come from
        // `CString`s -- `path` the caller's, `mode` this local -- so both are non-null,
        // NUL-terminated and readable for the whole call.
        let file = unsafe { gzopen(path.as_ptr(), mode.as_ptr()) };
        assert!(!file.is_null(), "gzopen({mode:?}) returned Z_NULL");
        file
    }

    /// `gzopen` that is expected to fail.
    fn open_expect_null(path: &CString, mode: &str) -> bool {
        let mode = CString::new(mode).unwrap();
        // SAFETY: as [`open`] -- two live `CString` pointers.
        let file = unsafe { gzopen(path.as_ptr(), mode.as_ptr()) };
        if file.is_null() {
            return true;
        }
        // SAFETY: `file` is non-null here, so it is the live handle the call above returned
        // and has not been closed; closing it is what keeps a wrongly-successful open from
        // leaking.
        assert_eq!(close(file), Z_OK);
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
        // SAFETY: the handle and buffer clauses of the call-gate banner below. `len` is
        // exactly `payload.len()` -- `try_from` rather than a cast, so it cannot be a
        // truncation -- and `payload` is a live slice the caller keeps for the call.
        unsafe { gzwrite(file, payload.as_ptr().cast::<c_void>(), len) }
    }

    /// `gzread` with Rust-side types, returning the raw count -- negative on error.
    fn read_bytes(file: gzFile, out: &mut [u8]) -> c_int {
        let len = c_uint::try_from(out.len()).unwrap();
        // SAFETY: the handle and buffer clauses of the call-gate banner below. `len` is
        // exactly `out.len()`, and `out` is a live exclusive slice, so the writable extent
        // is exact and unaliased for the call.
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

    // =======================================================================
    // Call gates -- every raw call into this module's exports lives below
    // =======================================================================
    //
    // ★ THE INVARIANT EVERY GATE IN THIS SECTION DISCHARGES.
    //
    // Stating it once, here, and routing every call through a named gate is what makes
    // this module locally auditable: a reader of any test body sees named operations
    // rather than bare `unsafe`, and a reader auditing the raw calls has one section to
    // read instead of two hundred call sites. The gates are deliberately thin -- one
    // forward each, no logic -- so that nothing can hide inside one.
    //
    // * **Handles.** Every `gzFile` argument is either a null pointer passed on purpose,
    //   to exercise an entry guard, or the exact non-null value a preceding [`open`],
    //   [`open_raw`], [`open64_raw`], [`dopen`] or [`install`] returned inside the *same*
    //   test and has not yet closed or released -- so it always addresses a live
    //   [`GzBlock`] carrying the [`GZ_TAG_LIVE`] tag. The deliberately-hostile cases (a
    //   closed handle, a double close, a foreign pointer with the wrong tag) build a
    //   valid, live allocation first and corrupt only the tag, which is exactly the state
    //   each guard is specified to reject.
    // * **Buffers.** Every buffer pointer is `as_ptr`/`as_mut_ptr` of a local array,
    //   [`Vec`] or [`CString`] that outlives the call, and the length handed alongside it
    //   is that same object's length, so the readable or writable extent is exact. A
    //   deliberately wild pointer appears only where the entry point is specified to
    //   refuse before reading it, and [`wild`] documents why that is evidence.
    // * **C strings.** Every `*const c_char` is either null on purpose or the pointer of
    //   a [`CString`] bound to a local that outlives the call, so it stays
    //   NUL-terminated and readable throughout.
    // * **Exclusivity.** These tests are single-threaded within a test function, and no
    //   handle is shared between threads, so no call below aliases another's state.
    //
    // Each gate's own `// SAFETY:` line names the part of this banner it relies on, plus
    // anything specific to that entry point -- a released block, an out-parameter, a
    // buffer whose length must match a count.

    /// `gzopen(path, mode)` with both pointers raw -- `zlib.h` L1357.
    ///
    /// [`open`] is the ordinary route; this one exists for the null-argument and
    /// guard-ordering cases, which cannot be expressed with `&CString`.
    fn open_raw(path: *const c_char, mode: *const c_char) -> gzFile {
        // SAFETY: the C-string clause of this section's banner, for both pointers.
        unsafe { gzopen(path, mode) }
    }

    /// `gzopen64(path, mode)` -- `zlib.h` L1978. The large-file face of [`open_raw`].
    fn open64_raw(path: *const c_char, mode: *const c_char) -> gzFile {
        // SAFETY: the C-string clause of this section's banner, for both pointers.
        unsafe { gzopen64(path, mode) }
    }

    /// `gzdopen(fd, mode)` -- `zlib.h` L1404.
    ///
    /// `fd` is `-1`, an invalid number passed on purpose, or an **open** descriptor whose
    /// ownership transfers to the call exactly once -- always obtained by `into_raw_fd` on
    /// a [`File`] the test no longer uses, or by `as_raw_fd` on one whose ownership the
    /// test transfers in the same statement.
    fn dopen(fd: c_int, mode: *const c_char) -> gzFile {
        // SAFETY: the C-string clause of this section's banner for `mode`, plus the
        // descriptor-ownership statement above, which is `zlib.h` L1414-L1416's own
        // requirement on a C caller.
        unsafe { super::gzdopen(fd, mode) }
    }

    /// `gzclose(file)` -- `zlib.h` L1750. Releases the block on the paths where it tears
    /// the stream down, so no test uses `file` again except to prove a second close is
    /// refused.
    fn close(file: gzFile) -> c_int {
        // SAFETY: the handle clause of this section's banner.
        unsafe { gzclose(file) }
    }

    /// `gzclose_r(file)` -- `zlib.h` L1763. Releases the block only for a read stream.
    fn close_read(file: gzFile) -> c_int {
        // SAFETY: the handle clause of this section's banner.
        unsafe { gzclose_r(file) }
    }

    /// `gzclose_w(file)` -- `zlib.h` L1764. Releases the block only for a write stream.
    fn close_write(file: gzFile) -> c_int {
        // SAFETY: the handle clause of this section's banner.
        unsafe { gzclose_w(file) }
    }

    /// `gzbuffer(file, size)` -- `zlib.h` L1429.
    fn set_buffer(file: gzFile, size: c_uint) -> c_int {
        // SAFETY: the handle clause of this section's banner.
        unsafe { gzbuffer(file, size) }
    }

    /// `gzsetparams(file, level, strategy)` -- `zlib.h` L1445.
    fn set_params(file: gzFile, level: c_int, strategy: c_int) -> c_int {
        // SAFETY: the handle clause of this section's banner.
        unsafe { gzsetparams(file, level, strategy) }
    }

    /// `gzflush(file, flush)` -- `zlib.h` L1647.
    fn flush(file: gzFile, mode: c_int) -> c_int {
        // SAFETY: the handle clause of this section's banner.
        unsafe { gzflush(file, mode) }
    }

    /// `gzputc(file, c)` -- `zlib.h` L1607.
    fn put_byte(file: gzFile, c: c_int) -> c_int {
        // SAFETY: the handle clause of this section's banner.
        unsafe { gzputc(file, c) }
    }

    /// `gzputs(file, s)` -- `zlib.h` L1575.
    fn put_string(file: gzFile, s: *const c_char) -> c_int {
        // SAFETY: the handle clause of this section's banner, plus its C-string clause for
        // `s` -- which is null or wild in the guard-ordering tests, where the entry point
        // is specified to refuse before measuring it.
        unsafe { gzputs(file, s) }
    }

    /// `gzgetc(file)` -- `zlib.h` L1620, the function behind the macro.
    fn get_byte(file: gzFile) -> c_int {
        // SAFETY: the handle clause of this section's banner.
        unsafe { gzgetc(file) }
    }

    /// `gzgetc_(file)` -- `zlib.h` L2020, the macro's fallback symbol.
    fn get_byte_fallback(file: gzFile) -> c_int {
        // SAFETY: the handle clause of this section's banner.
        unsafe { gzgetc_(file) }
    }

    /// `gzungetc(c, file)` -- `zlib.h` L1633.
    fn unget_byte(c: c_int, file: gzFile) -> c_int {
        // SAFETY: the handle clause of this section's banner.
        unsafe { gzungetc(c, file) }
    }

    /// `gzgets(file, buf, len)` -- `zlib.h` L1588.
    fn get_line(file: gzFile, buf: *mut c_char, len: c_int) -> *mut c_char {
        // SAFETY: the handle clause of this section's banner, plus its buffer clause: when
        // `len >= 1`, `buf` addresses at least `len` writable bytes of a live local.
        unsafe { gzgets(file, buf, len) }
    }

    /// `gzread(file, buf, len)` with both halves raw -- `zlib.h` L1465.
    ///
    /// [`read_bytes`] is the ordinary route; this one takes the pair apart for the
    /// zero-length and null-buffer cases.
    fn read_raw(file: gzFile, buf: voidp, len: c_uint) -> c_int {
        // SAFETY: the handle and buffer clauses of this section's banner. A zero `len` is
        // paired with a null `buf` on purpose, which the entry point must answer without
        // forming a slice.
        unsafe { gzread(file, buf, len) }
    }

    /// `gzwrite(file, buf, len)` with both halves raw -- `zlib.h` L1519.
    fn write_raw(file: gzFile, buf: voidpc, len: c_uint) -> c_int {
        // SAFETY: the handle and buffer clauses of this section's banner, with `buf`
        // readable for `len` bytes -- or null with `len == 0`.
        unsafe { gzwrite(file, buf, len) }
    }

    /// `gzfread(buf, size, nitems, file)` -- `zlib.h` L1492. Note C's argument order.
    fn fread_items(buf: voidp, size: z_size_t, nitems: z_size_t, file: gzFile) -> z_size_t {
        // SAFETY: the handle and buffer clauses of this section's banner, with `buf`
        // writable for `size * nitems` bytes whenever that product is non-zero.
        unsafe { gzfread(buf, size, nitems, file) }
    }

    /// `gzfwrite(buf, size, nitems, file)` -- `zlib.h` L1529. Note C's argument order.
    fn fwrite_items(buf: voidpc, size: z_size_t, nitems: z_size_t, file: gzFile) -> z_size_t {
        // SAFETY: the handle and buffer clauses of this section's banner, with `buf`
        // readable for `size * nitems` bytes whenever that product is non-zero.
        unsafe { gzfwrite(buf, size, nitems, file) }
    }

    /// `gzseek(file, offset, whence)` -- `zlib.h` L1670.
    fn seek(file: gzFile, offset: z_off_t, whence: c_int) -> z_off_t {
        // SAFETY: the handle clause of this section's banner.
        unsafe { gzseek(file, offset, whence) }
    }

    /// `gzseek64(file, offset, whence)` -- `zlib.h` L1982.
    fn seek64(file: gzFile, offset: z_off64_t, whence: c_int) -> z_off64_t {
        // SAFETY: the handle clause of this section's banner.
        unsafe { gzseek64(file, offset, whence) }
    }

    /// `gzrewind(file)` -- `zlib.h` L1687.
    fn rewind(file: gzFile) -> c_int {
        // SAFETY: the handle clause of this section's banner.
        unsafe { gzrewind(file) }
    }

    /// `gztell(file)` -- `zlib.h` L1694.
    fn tell(file: gzFile) -> z_off_t {
        // SAFETY: the handle clause of this section's banner.
        unsafe { gztell(file) }
    }

    /// `gztell64(file)` -- `zlib.h` L1983.
    fn tell64(file: gzFile) -> z_off64_t {
        // SAFETY: the handle clause of this section's banner.
        unsafe { gztell64(file) }
    }

    /// `gzoffset(file)` -- `zlib.h` L1704.
    fn offset(file: gzFile) -> z_off_t {
        // SAFETY: the handle clause of this section's banner.
        unsafe { gzoffset(file) }
    }

    /// `gzoffset64(file)` -- `zlib.h` L1984.
    fn offset64(file: gzFile) -> z_off64_t {
        // SAFETY: the handle clause of this section's banner.
        unsafe { gzoffset64(file) }
    }

    /// `gzeof(file)` -- `zlib.h` L1711.
    fn eof(file: gzFile) -> c_int {
        // SAFETY: the handle clause of this section's banner.
        unsafe { gzeof(file) }
    }

    /// `gzdirect(file)` -- `zlib.h` L1726.
    fn direct(file: gzFile) -> c_int {
        // SAFETY: the handle clause of this section's banner.
        unsafe { gzdirect(file) }
    }

    /// `gzerror(file, errnum)` -- `zlib.h` L1775.
    fn last_error(file: gzFile, errnum: *mut c_int) -> *const c_char {
        // SAFETY: the handle clause of this section's banner, plus: `errnum` is null on
        // purpose, or `&mut` of a live `c_int` local, which is the one word the entry point
        // writes through it.
        unsafe { gzerror(file, errnum) }
    }

    /// `gzclearerr(file)` -- `zlib.h` L1792.
    fn clear_error(file: gzFile) {
        // SAFETY: the handle clause of this section's banner.
        unsafe { gzclearerr(file) }
    }

    /// `_zlib_rs_gzprintf_begin(file, scratch, size)` -- the first half of the protocol
    /// `csrc/gzprintf_shim.c` drives.
    fn printf_begin(file: gzFile, scratch: *mut *mut u8, size: *mut z_size_t) -> c_int {
        // SAFETY: the handle clause of this section's banner, plus: `scratch` and `size`
        // are `&mut` of live locals -- one pointer slot and one `z_size_t` -- which are the
        // two out-parameters this helper writes.
        unsafe { _zlib_rs_gzprintf_begin(file, scratch, size) }
    }

    /// `_zlib_rs_gzprintf_commit(file, reported)` -- the second half of that protocol.
    fn printf_commit(file: gzFile, reported: z_size_t) -> c_int {
        // SAFETY: the handle clause of this section's banner. `reported` is a plain count;
        // an over-large one is a refusal case the helper is specified to answer.
        unsafe { _zlib_rs_gzprintf_commit(file, reported) }
    }

    /// [`super::block_mut`] on a handle -- reaches the private [`GzBlock`] behind it.
    fn block_of(file: gzFile) -> Option<&'static mut GzBlock> {
        // SAFETY: the handle clause of this section's banner, which is `block_mut`'s own
        // contract. The `'static` the signature names is a test-local convenience: every
        // borrow taken through it is dropped before the block is closed or released.
        unsafe { block_mut(file) }
    }

    /// [`super::state_mut`] on a handle -- reaches the core state behind it.
    fn state_of(file: gzFile) -> Option<&'static mut FacadeGzState> {
        // SAFETY: as [`block_of`].
        unsafe { state_mut(file) }
    }

    /// [`super::release`] on a handle -- frees the block without a close.
    fn release_block(file: gzFile) {
        // SAFETY: the handle clause of this section's banner. `file` addresses a live block
        // this test allocated through [`install`] and has not released, and nothing borrows
        // it at this point; it is never used again afterwards.
        unsafe { release(file) }
    }

    /// A C string's bytes, for reading a message an entry point returned.
    fn c_str(text: *const c_char) -> &'static CStr {
        assert!(!text.is_null(), "the message pointer must not be null");
        // SAFETY: `text` is non-null by the assertion above and is a pointer this library
        // returned -- either a `'static` literal or the message buffer owned by a stream
        // that is still open -- so it is NUL-terminated and readable, and `CStr::from_ptr`
        // cannot scan past the terminator.
        unsafe { CStr::from_ptr(text) }
    }

    /// Writes `payload` to a fresh gzip file at `path` with the given mode.
    fn write_file(path: &CString, mode: &str, payload: &[u8]) {
        let file = open(path, mode);
        assert_eq!(count(write_bytes(file, payload)), payload.len());
        assert_eq!(close(file), Z_OK);
    }

    /// Reads the whole of the gzip file at `path` back through `gzread`.
    fn read_file(path: &CString) -> Vec<u8> {
        let file = open(path, "rb");
        let mut out = vec![0_u8; 4096];
        let got = read_bytes(file, &mut out);
        assert!(got >= 0, "gzread reported {got}");
        assert_eq!(close(file), Z_OK);
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
        let tag = block_of(file).map(|block| block.tag);
        assert_eq!(tag, Some(GZ_TAG_LIVE));
        assert!(state_of(file).is_some());

        // The exposed prefix of a state with no output buffer: nothing available, pointing
        // nowhere, which is the pair the `gzgetc` macro answers by taking its function arm.
        // SAFETY: `file` is the live handle `install` returned, and `gzFile_s` is the
        // `#[repr(C)]` prefix this library places at offset 0 of that block -- which is the
        // whole point of the layout and what the `gzgetc` macro relies on. The borrow is read
        // only and is dropped before anything else touches the block.
        let exposed = unsafe { &*file };
        assert_eq!(exposed.have, 0);
        assert!(exposed.next.is_null());
        assert_eq!(exposed.pos, 0);

        release_block(file);
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
        assert!(block_of(core::ptr::null_mut()).is_none());
        assert!(state_of(core::ptr::null_mut()).is_none());

        let file = install(FacadeGzState::new(GlobalAllocator));
        // Poison the tag by hand: this is what `release` does, and it is what a second
        // close would see while the allocator has not yet reused the block.
        block_of(file).unwrap().tag = GZ_TAG_CLOSED;
        assert!(block_of(file).is_none());
        assert!(state_of(file).is_none());
        // Restore it so the block can be released through the normal path.
        //
        // SAFETY: `file` addresses the live `GzBlock` this test allocated through `install`;
        // only its `tag` field was corrupted above, so the allocation itself is intact and
        // writing the field back is a single aligned store. Nothing borrows the block here --
        // the two `is_none()` assertions above dropped their results.
        unsafe { (*file.cast::<GzBlock>()).tag = GZ_TAG_LIVE };
        release_block(file);
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

    /// `gzopen_w` applies the same four mode rejections `gzopen` does.
    ///
    /// The point is the *ordering* as much as the outcome. `gz_open` parses and rejects the
    /// mode at `gzlib.c` L107-L197 and only reaches the path narrowing and its `malloc` at
    /// L198-L219 for a mode it has accepted, so a refused mode must cost no allocation whose
    /// size the caller chose. An earlier revision of this entry point converted the whole
    /// wide path -- infallibly, through `String::from_utf16_lossy` -- before looking at the
    /// mode at all, which both diverged from that order and let a caller provoke an
    /// out-of-memory abort with a long path and a mode that was never going to be accepted.
    ///
    /// The four are C's: `+` anywhere (L129-L131), no `r`/`w`/`a` (L174-L177), `T` while
    /// reading (L181-L185) and `G` while writing (L191-L195). They are checked through the
    /// wide entry point specifically, because the gate that applies them lives in it.
    #[cfg(windows)]
    #[test]
    fn gzopen_w_refuses_the_rejected_mode_strings() {
        let scratch = Scratch::new("wmodes");
        let wide: Vec<u16> = scratch
            .0
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();

        for mode in ["rb+", "wb+", "b", "9", "rbT", "wbG"] {
            let c_mode = CString::new(mode).unwrap();
            // SAFETY: `wide` is NUL-terminated and live for the call; `c_mode` likewise.
            let file = unsafe { gzopen_w(wide.as_ptr(), c_mode.as_ptr()) };
            assert!(file.is_null(), "wide open accepted mode {mode:?}");
        }
        // Nothing was created: every one of those was refused before the file was opened.
        assert!(!scratch.0.exists());
    }

    /// A wide path round-trips through `gzopen_w`.
    ///
    /// Covers the half of the change the rejection test cannot: that an *accepted* mode still
    /// reaches the open, that the label narrowing does not disturb the path -- the `wchar_t`
    /// units are handed to `_wopen` unconverted, which is C's split at `gzlib.c` L250-L251 --
    /// and that the bytes come back.
    #[cfg(windows)]
    #[test]
    fn gzopen_w_round_trips_a_wide_path() {
        let scratch = Scratch::new("wroundtrip");
        let wide: Vec<u16> = scratch
            .0
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let payload = b"a wide path names the same file a narrow one does";

        let mode = CString::new("wb").unwrap();
        // SAFETY: both strings are NUL-terminated and live for the call.
        let writer = unsafe { gzopen_w(wide.as_ptr(), mode.as_ptr()) };
        assert!(!writer.is_null());
        assert_eq!(
            write_bytes(writer, payload),
            c_int::try_from(payload.len()).unwrap()
        );
        assert_eq!(close(writer), Z_OK);

        // Read it back through the *narrow* entry point: the same file, either spelling.
        assert_eq!(read_file(&scratch.c_path()), payload);
    }

    /// The error label is narrowed with the C runtime's `wcstombs`, and cannot abort.
    ///
    /// `gzlib.c` L200-L219 converts the label with `wcstombs`, which targets the **active code
    /// page**; this library used `String::from_utf16_lossy`, which targets UTF-8, so the two
    /// disagreed for any path outside ASCII. The three properties asserted here are the ones
    /// `narrow_wide_label` exists to hold: an ASCII path narrows byte-for-byte, an empty path
    /// narrows to empty (C's `*(state->path) = 0` at L214), and neither allocates fatally.
    #[cfg(windows)]
    #[test]
    fn a_wide_label_is_narrowed_through_the_c_runtime() {
        let ascii: Vec<u16> = "plain-ascii-name.gz"
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        // SAFETY: NUL-terminated and live for the call.
        let narrowed = unsafe { narrow_wide_label(ascii.as_ptr()) };
        assert_eq!(
            narrowed.as_deref(),
            Some(&b"plain-ascii-name.gz"[..]),
            "ASCII must narrow byte-for-byte in any code page"
        );

        let empty: Vec<u16> = vec![0];
        // SAFETY: NUL-terminated and live for the call.
        let narrowed = unsafe { narrow_wide_label(empty.as_ptr()) };
        assert_eq!(
            narrowed.as_deref(),
            Some(&b""[..]),
            "an empty wide path narrows to an empty label"
        );
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
        assert_eq!(direct(file), 1);
        assert_eq!(read_file(&path), payload);
        assert_eq!(close(file), Z_OK);
    }

    /// A transparent read that comes up short leaves the rest of the caller's buffer alone.
    ///
    /// `gzread.c` L367-L369 hands the caller's buffer straight to `gz_load` for a large request
    /// on a transparent stream, and `gz_load`'s loop (L26-L34) advances `*have` by each `read`
    /// return and gives up only when one returns `<= 0` -- so it under-fills the window just at
    /// end of file, and there C has touched the returned prefix and nothing else. A caller may
    /// rely on that, because the count `gzread` returns is the only thing that describes what was
    /// written.
    ///
    /// The buffer is pre-filled with a **position-dependent** sentinel: a constant would pass by
    /// coincidence against any implementation that happened to fill with the same byte, and an
    /// earlier revision of this library filled the whole offered window with zeros.
    ///
    /// The sizes are chosen to reach the path under test rather than to be round: the file is
    /// larger than one input buffer (the default `size` is 8192, so `gz_look` delivers 8192 bytes
    /// through the layer's own buffer first) and the request is far larger than `size << 1`,
    /// which is what steers `gz_read` past its double-buffering branch and into the direct one.
    /// A large transparent read consumes from the descriptor exactly what it delivers.
    ///
    /// `gzread.c` L367-L369 gives `gz_load` the caller's buffer and the *whole* remaining request,
    /// so the transparent path never reads past the request boundary. That is observable, because
    /// `gzoffset` reports `lseek(fd, 0, SEEK_CUR) - strm->avail_in` (`gzlib.c` L478-L481) and a
    /// transparent stream buffers nothing in `strm` for that subtraction to discount -- so any byte
    /// read ahead shows up in the result.
    ///
    /// This pins the property because the staging this port performs could silently break it. An
    /// intermediate revision staged a *single* `size << 1` read per pass of the outer loop and let
    /// the remainder fall back through it; a request whose tail is shorter than `size << 1` then
    /// dropped into the double-buffering arm and pulled a whole further buffer in. The sizes below
    /// are chosen to hit exactly that case: with `size` 8 the direct arm moves 16 bytes at a time,
    /// `gz_look` delivers the first 8 through the layer's own buffer, and the 28-byte request
    /// therefore leaves a 4-byte tail. Measured against that revision this read 40 bytes from the
    /// descriptor while returning 28.
    #[test]
    fn a_large_transparent_read_does_not_read_past_the_request() {
        const FILE_LEN: usize = 100;
        const REQUEST: usize = 28;

        let scratch = Scratch::new("noreadahead");
        let path = scratch.c_path();
        let payload: Vec<u8> = (0..FILE_LEN)
            .map(|at| u8::try_from(at % 251).unwrap_or(0))
            .collect();
        write_file(&path, "wbT", &payload);

        let file = open(&path, "rb");
        assert!(!file.is_null());
        // `size` 8 makes the direct arm's staging buffer 16 bytes, so a 28-byte request has a tail
        // shorter than one bufferful.
        assert_eq!(set_buffer(file, 8), 0);

        let mut buffer = vec![0_u8; REQUEST];
        let got = read_bytes(file, &mut buffer);
        assert_eq!(count(got), REQUEST, "the request is satisfied in full");
        assert_eq!(
            buffer,
            payload[..REQUEST],
            "and the delivered bytes are the file's"
        );
        assert_eq!(
            offset(file),
            z_off_t::try_from(REQUEST).unwrap(),
            "the descriptor must sit exactly at the request boundary, with nothing read ahead"
        );
        assert_eq!(close(file), Z_OK);
    }

    #[test]
    fn a_short_transparent_read_leaves_the_tail_of_the_callers_buffer_intact() {
        const FILE_LEN: usize = 20_000;
        const REQUEST: usize = 40_000;

        let scratch = Scratch::new("shortread");
        let path = scratch.c_path();

        // A plain file, not a gzip member, so the reader takes the transparent path. Written
        // with `T` through this library's own writer, which
        // `a_transparent_write_is_reported_by_gzdirect` establishes stores bytes verbatim.
        let payload: Vec<u8> = (0..FILE_LEN)
            .map(|at| u8::try_from(at % 251).unwrap_or(0))
            .collect();
        write_file(&path, "wbT", &payload);
        assert_eq!(std::fs::read(&scratch.0).unwrap(), payload);

        let sentinel: Vec<u8> = (0..REQUEST)
            .map(|at| u8::try_from(at % 241).unwrap_or(0) ^ 0x80)
            .collect();
        let mut buffer = sentinel.clone();

        let file = open(&path, "rb");
        assert_eq!(direct(file), 1, "the file must be read transparently");
        let got = read_bytes(file, &mut buffer);
        assert_eq!(
            count(got),
            FILE_LEN,
            "the whole file is delivered in one call"
        );
        assert_eq!(close(file), Z_OK);

        assert_eq!(
            buffer.get(..FILE_LEN),
            payload.get(..),
            "the returned prefix is the file's bytes"
        );
        assert_eq!(
            buffer.get(FILE_LEN..),
            sentinel.get(FILE_LEN..),
            "and every byte past the returned count is exactly as the caller left it"
        );

        // The same file again with the smallest buffer `gzbuffer` will accept, so that the
        // staging loop has to run many times rather than twice. This is what makes the branch
        // under test unambiguous: with `size` at its floor of 8, `size << 1` is 16, so every
        // iteration after the first `gz_look` takes the direct arm.
        let mut buffer = sentinel.clone();
        let file = open(&path, "rb");
        assert_eq!(set_buffer(file, 8), 0);
        assert_eq!(direct(file), 1);
        assert_eq!(count(read_bytes(file, &mut buffer)), FILE_LEN);
        assert_eq!(close(file), Z_OK);
        assert_eq!(
            buffer.get(..FILE_LEN),
            payload.get(..),
            "a staged read that loops delivers the same bytes"
        );
        assert_eq!(
            buffer.get(FILE_LEN..),
            sentinel.get(FILE_LEN..),
            "and still leaves the tail alone"
        );
    }

    /// The same guarantee for `gzfread`, over a compressed stream.
    ///
    /// `gzfread` is `gz_read` behind an item/size product (`gzread.c` L437-L459), and the
    /// decompressing large-request branch (L371-L378) writes through `inflate` rather than
    /// through the handle. Both branches must leave the tail alone for the same reason, and the
    /// decompressing one always has -- it is asserted here so a future change to either cannot
    /// quietly diverge from the other.
    #[test]
    fn a_short_gzfread_leaves_the_tail_of_the_callers_buffer_intact() {
        const FILE_LEN: usize = 20_000;
        const REQUEST: usize = 40_000;

        let scratch = Scratch::new("shortfread");
        let path = scratch.c_path();

        // Deliberately compressible, so the member is far smaller than the payload and the
        // decompressing branch has to loop.
        let payload: Vec<u8> = (0..FILE_LEN)
            .map(|at| u8::try_from(at % 7).unwrap_or(0))
            .collect();
        write_file(&path, "wb", &payload);

        let sentinel: Vec<u8> = (0..REQUEST)
            .map(|at| u8::try_from(at % 241).unwrap_or(0) ^ 0x80)
            .collect();
        let mut buffer = sentinel.clone();

        let file = open(&path, "rb");
        assert_eq!(direct(file), 0, "the file must be a gzip member");
        let len = z_size_t::try_from(buffer.len()).unwrap();
        let got = fread_items(buffer.as_mut_ptr().cast::<c_void>(), len, 1, file);
        assert_eq!(got, 0, "one item of 40000 bytes cannot be completed");
        assert_eq!(close(file), Z_OK);

        assert_eq!(
            buffer.get(..FILE_LEN),
            payload.get(..),
            "the bytes that were delivered are the payload's"
        );
        assert_eq!(
            buffer.get(FILE_LEN..),
            sentinel.get(FILE_LEN..),
            "and the tail is exactly as the caller left it"
        );
    }

    /// A `gzungetc` still succeeds after a large transparent read.
    ///
    /// `gzread.c` L577-L584 handles an empty output buffer by parking the pushed byte at
    /// `out + (size << 1) - 1`, which is what "allows more pushing" means. The transparent
    /// large-request path stages its read through that same buffer, so this asserts the staging
    /// leaves the push room intact: nothing is published, so `x.have` is still zero and the
    /// empty-buffer arm is the one taken.
    #[test]
    fn a_push_back_still_works_after_a_large_transparent_read() {
        const FILE_LEN: usize = 20_000;

        let scratch = Scratch::new("ungetafter");
        let path = scratch.c_path();
        let payload: Vec<u8> = (0..FILE_LEN)
            .map(|at| u8::try_from(at % 251).unwrap_or(0))
            .collect();
        write_file(&path, "wbT", &payload);

        let file = open(&path, "rb");
        let mut buffer = vec![0_u8; 40_000];
        assert_eq!(count(read_bytes(file, &mut buffer)), FILE_LEN);

        assert_eq!(unget_byte(0x5a, file), 0x5a, "the push must be accepted");
        let mut one = [0_u8; 1];
        assert_eq!(count(read_bytes(file, &mut one)), 1);
        assert_eq!(one[0], 0x5a, "and the pushed byte comes back first");
        assert_eq!(close(file), Z_OK);
    }

    #[test]
    fn gzbuffer_is_refused_once_the_buffers_exist() {
        let scratch = Scratch::new("buffer");
        let path = scratch.c_path();
        write_file(&path, "wb", b"payload");

        let file = open(&path, "rb");
        // Before any read: accepted, and a size below 8 is raised rather than refused.
        assert_eq!(set_buffer(file, 4), 0);
        assert_eq!(set_buffer(file, 1 << 15), 0);
        // A size that cannot be doubled is refused (`gzlib.c` L338-L339).
        assert_eq!(set_buffer(file, 0x8000_0000), -1);
        // Forcing the container decision allocates, after which it is refused.
        assert_eq!(direct(file), 0);
        assert_eq!(set_buffer(file, 1 << 16), -1);
        assert_eq!(close(file), Z_OK);
    }

    #[test]
    fn gzsetparams_changes_level_midstream() {
        let scratch = Scratch::new("setparams");
        let path = scratch.c_path();
        let file = open(&path, "wb1");
        let head = b"the first half, written at level one";
        assert_eq!(count(write_bytes(file, head)), head.len());
        // Z_BEST_COMPRESSION, Z_DEFAULT_STRATEGY.
        assert_eq!(set_params(file, 9, 0), Z_OK);
        let tail = b"the second half, written at level nine";
        assert_eq!(count(write_bytes(file, tail)), tail.len());
        assert_eq!(close(file), Z_OK);

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
        // SAFETY: `macro_gzgetc`'s contract is a live handle, which the call-gate banner's
        // handle clause supplies -- `file` is the value `open` just returned and is still
        // open. It is spelled out here rather than routed through a gate because reproducing
        // the *caller-compiled* macro is the point of this test: the whole reason
        // `gzFile_s`'s three fields sit at offset 0 is that this arithmetic happens in code
        // this library never sees.
        assert_eq!(unsafe { macro_gzgetc(file) }, i32::from(b'h'));
        assert_eq!(tell(file), 1);

        // The next four come through the MACRO's fast path, straight out of the prefix.
        for expected in *b"ello" {
            // SAFETY: `file` is the same live handle, and `have` is the first field of the
            // `#[repr(C)]` `gzFile_s` prefix at offset 0 of its block. One aligned `c_uint`
            // is read, exactly as the macro's own test of `(g)->have` reads it.
            let before = unsafe { (*file).have };
            assert!(before > 0, "the macro fast path was not available");
            // SAFETY: as the first `macro_gzgetc` call above -- a live handle, and the point
            // is that the caller's macro arithmetic reaches the same answer.
            assert_eq!(unsafe { macro_gzgetc(file) }, i32::from(expected));
        }
        // Five bytes consumed in total, four of them without the library being called at
        // all. Both faces of `gztell` must report it.
        assert_eq!(tell(file), 5);
        assert_eq!(tell64(file), 5);

        // A Rust-side read must continue from where the macro left off, not from where the
        // library last published a cursor.
        let mut rest = [0_u8; 32];
        let got = read_bytes(file, &mut rest);
        assert_eq!(got, as_count(payload.len() - 5));
        assert_eq!(&rest[..count(got)], &payload[5..]);
        assert_eq!(tell(file), z_off_t::try_from(payload.len()).unwrap());
        // ★ `gzeof` reports `past`, not `eof`: the 32-byte request could not be satisfied
        // from a 13-byte stream, so the flag is already set here. That is C's behaviour --
        // `gz_read` sets `state->past` when it runs out of input mid-request
        // (`gzread.c` L347-L348) -- and it is precisely why `zlib.h` L1713-L1722 tells a
        // caller to test `gzeof` *after* a short read rather than before the next one.
        assert_eq!(eof(file), 1);

        // And once past the end the macro falls through to the function, which reports -1.
        //
        // SAFETY: as above -- `file` is still the live handle; end of file is a state, not an
        // invalidation.
        assert_eq!(unsafe { macro_gzgetc(file) }, -1);
        assert_eq!(eof(file), 1);
        assert_eq!(close(file), Z_OK);
    }

    #[test]
    fn gzgetc_and_gzgetc_underscore_are_interchangeable() {
        let scratch = Scratch::new("getc");
        let path = scratch.c_path();
        write_file(&path, "wb", b"abcd");

        let file = open(&path, "rb");
        assert_eq!(get_byte(file), i32::from(b'a'));
        assert_eq!(get_byte_fallback(file), i32::from(b'b'));
        assert_eq!(get_byte(file), i32::from(b'c'));
        assert_eq!(get_byte_fallback(file), i32::from(b'd'));
        assert_eq!(get_byte(file), -1);
        assert_eq!(get_byte_fallback(file), -1);
        assert_eq!(close(file), Z_OK);
    }

    #[test]
    fn gzungetc_pushes_back_and_forces_a_pending_seek() {
        let scratch = Scratch::new("ungetc");
        let path = scratch.c_path();
        let payload = b"hello, hello!";
        write_file(&path, "wb", payload);

        let file = open(&path, "rb");
        // A push before anything is read: `gz_look` runs first so the buffers exist.
        assert_eq!(unget_byte(i32::from(b'X'), file), i32::from(b'X'));
        assert_eq!(get_byte(file), i32::from(b'X'));
        assert_eq!(get_byte(file), i32::from(b'h'));
        // Push it back and read it again.
        assert_eq!(unget_byte(i32::from(b'h'), file), i32::from(b'h'));
        assert_eq!(tell(file), 0);
        assert_eq!(get_byte(file), i32::from(b'h'));

        // `gzungetc(-1, file)` is the documented idiom for forcing a pending seek to run:
        // the skip is carried out BEFORE the negative byte is rejected.
        assert_eq!(seek(file, 0, SEEK_SET), 0);
        assert_eq!(unget_byte(-1, file), -1);
        assert_eq!(tell(file), 0);
        assert_eq!(close(file), Z_OK);
    }

    #[test]
    fn gzgets_stops_at_a_newline_and_reports_end_of_file_with_null() {
        let scratch = Scratch::new("gets");
        let path = scratch.c_path();
        write_file(&path, "wb", b"first line\nsecond line\n");

        let file = open(&path, "rb");
        let mut buf = [0_u8; 64];
        let returned = get_line(file, buf.as_mut_ptr().cast::<c_char>(), 64);
        assert_eq!(returned, buf.as_mut_ptr().cast::<c_char>());
        assert_eq!(c_str(returned).to_bytes(), b"first line\n");
        let returned = get_line(file, buf.as_mut_ptr().cast::<c_char>(), 64);
        assert!(!returned.is_null());
        assert_eq!(c_str(returned).to_bytes(), b"second line\n");
        // End of file: null, not an empty string.
        assert!(get_line(file, buf.as_mut_ptr().cast::<c_char>(), 64).is_null());

        // `len == 1` returns null and writes nothing -- the implementation is the oracle,
        // not the header's prose.
        assert_eq!(rewind(file), 0);
        assert!(get_line(file, buf.as_mut_ptr().cast::<c_char>(), 1).is_null());
        // A non-positive length, and a null buffer, are both refused.
        assert!(get_line(file, buf.as_mut_ptr().cast::<c_char>(), 0).is_null());
        assert!(get_line(file, buf.as_mut_ptr().cast::<c_char>(), -5).is_null());
        assert!(get_line(file, core::ptr::null_mut(), 64).is_null());
        assert_eq!(close(file), Z_OK);
    }

    #[test]
    fn gzputc_masks_its_result_and_gzputs_counts_characters() {
        let scratch = Scratch::new("putc");
        let path = scratch.c_path();
        let file = open(&path, "wb");
        assert_eq!(put_byte(file, i32::from(b'h')), i32::from(b'h'));
        // `test/example.c` L105's assertion.
        let ello = CString::new("ello").unwrap();
        assert_eq!(put_string(file, ello.as_ptr()), 4);
        // ★ Masked with 0xff on both return paths: -1 becomes 255, not -1.
        assert_eq!(put_byte(file, -1), 255);
        assert_eq!(close(file), Z_OK);

        assert_eq!(read_file(&path), b"hello\xff");

        // A null string is refused rather than faulting inside `strlen`.
        let file = open(&path, "wb");
        assert_eq!(put_string(file, core::ptr::null()), -1);
        assert_eq!(close(file), Z_OK);
    }

    #[test]
    fn the_item_oriented_entry_points_count_items_not_bytes() {
        let scratch = Scratch::new("fread");
        let path = scratch.c_path();

        // Eight items of four bytes.
        let payload: Vec<u8> = (0_u8..32).collect();
        let file = open(&path, "wb");
        let items = fwrite_items(payload.as_ptr().cast::<c_void>(), 4, 8, file);
        assert_eq!(items, 8, "gzfwrite reports items");
        assert_eq!(close(file), Z_OK);

        let file = open(&path, "rb");
        let mut out = vec![0_u8; 32];
        let items = fread_items(out.as_mut_ptr().cast::<c_void>(), 4, 8, file);
        assert_eq!(items, 8);
        assert_eq!(out, payload);
        assert_eq!(close(file), Z_OK);

        // An overflowing product is refused with zero items and nothing read.
        let file = open(&path, "rb");
        let huge = usize::MAX / 2 + 1;
        assert_eq!(
            fread_items(out.as_mut_ptr().cast::<c_void>(), huge, 4, file),
            0
        );
        assert_eq!(close(file), Z_OK);

        let file = open(&path, "wb");
        assert_eq!(
            fwrite_items(payload.as_ptr().cast::<c_void>(), huge, 4, file),
            0
        );
        assert_eq!(close(file), Z_OK);
    }

    #[test]
    fn gzflush_finishes_a_member_and_refuses_the_wider_flush_codes() {
        let scratch = Scratch::new("flush");
        let path = scratch.c_path();
        let file = open(&path, "wb");
        let head = b"first member";
        assert!(write_bytes(file, head) > 0);
        // Z_FINISH ends the member but leaves the file open.
        assert_eq!(flush(file, 4), Z_OK);
        let tail = b"second member";
        assert!(write_bytes(file, tail) > 0);
        // ★ Narrower than `deflate`'s range: Z_BLOCK (5) and Z_TREES (6) are refused.
        assert_eq!(flush(file, 5), Z_STREAM_ERROR);
        assert_eq!(flush(file, 6), Z_STREAM_ERROR);
        assert_eq!(flush(file, -1), Z_STREAM_ERROR);
        assert_eq!(close(file), Z_OK);

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
                open_raw(path.as_ptr(), mode.as_ptr())
            } else {
                open64_raw(path.as_ptr(), mode.as_ptr())
            };
            assert!(!file.is_null());

            assert_eq!(
                to_wide(tell(file)),
                tell64(file),
                "gztell disagreement, opener {opener}"
            );
            assert_eq!(
                to_wide(offset(file)),
                offset64(file),
                "gzoffset disagreement, opener {opener}"
            );

            // Forwards, then backwards, then absolute -- each through both faces.
            // Named `distance` rather than `offset` so that it cannot shadow the `offset`
            // call gate, which this body also uses.
            for (distance, whence) in [(1024_i64, SEEK_SET), (512, SEEK_CUR), (-256, SEEK_CUR)] {
                // `try_from` rather than a cast: `z_off_t` is `c_long`, so this is the
                // identity on LP64 and a genuine narrowing on a 32-bit target, and the
                // test must be portable to both.
                let narrowed = z_off_t::try_from(distance).unwrap();
                let narrow = seek(file, narrowed, whence);
                let wide = seek64(file, distance, whence);
                // The two calls are consecutive, so the second starts where the first left
                // off; compare each against the position reported afterwards instead.
                assert!(
                    narrow >= 0 && wide >= 0,
                    "seek refused: {distance} {whence}"
                );
                assert_eq!(to_wide(tell(file)), tell64(file));
                assert_eq!(to_wide(offset(file)), offset64(file));
            }

            // `SEEK_END` is not supported, in both faces.
            assert_eq!(seek(file, 0, SEEK_END), -1);
            assert_eq!(seek64(file, 0, SEEK_END), -1);
            assert_eq!(close(file), Z_OK);
        }
    }

    #[test]
    fn a_seek_then_read_lands_on_the_expected_bytes() {
        let scratch = Scratch::new("seekread");
        let path = scratch.c_path();
        let payload: Vec<u8> = (0_u8..=255).cycle().take(2048).collect();
        write_file(&path, "wb", &payload);

        let file = open(&path, "rb");
        assert_eq!(seek(file, 1000, SEEK_SET), 1000);
        let mut out = [0_u8; 8];
        assert_eq!(read_bytes(file, &mut out), 8);
        assert_eq!(&out, &payload[1000..1008]);
        assert_eq!(tell(file), 1008);

        // Backwards, which only a read stream can do.
        assert_eq!(seek(file, -8, SEEK_CUR), 1000);
        assert_eq!(read_bytes(file, &mut out), 8);
        assert_eq!(&out, &payload[1000..1008]);

        assert_eq!(rewind(file), 0);
        assert_eq!(tell(file), 0);
        assert_eq!(close(file), Z_OK);
    }

    #[test]
    fn gzerror_reports_the_message_and_the_code_and_gzclearerr_discards_them() {
        let scratch = Scratch::new("error");
        let path = scratch.c_path();
        write_file(&path, "wb", b"payload");

        let file = open(&path, "rb");
        let mut code: c_int = 0x5eed;
        let message = last_error(file, core::ptr::addr_of_mut!(code));
        assert!(!message.is_null(), "a live handle must have a message");
        // No error yet: the empty string, which is not a null pointer.
        assert_eq!(c_str(message).to_bytes(), b"");
        assert_eq!(code, Z_OK);

        // A null `errnum` is permitted and must not be written.
        assert!(!last_error(file, core::ptr::null_mut()).is_null());

        // Reading a write stream is refused, and the refusal is reported through `gzerror`.
        let mut buf = [0_u8; 4];
        assert_eq!(read_bytes(file, &mut buf), 4);
        assert_eq!(eof(file), 0);
        clear_error(file);
        let mut code: c_int = 0x5eed;
        assert!(!last_error(file, core::ptr::addr_of_mut!(code)).is_null());
        assert_eq!(code, Z_OK);
        assert_eq!(close(file), Z_OK);

        // ★ An unusable handle answers with NULL, exactly as `gzlib.c` L517-L521 does.
        let mut code: c_int = 0x5eed;
        assert!(last_error(core::ptr::null_mut(), core::ptr::addr_of_mut!(code)).is_null());
        assert_eq!(code, 0x5eed, "errnum must not be written for a null handle");
        // `gzclearerr` on a null handle is a no-op rather than a crash.
        clear_error(core::ptr::null_mut());
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
        let message = last_error(file, core::ptr::addr_of_mut!(code));
        assert!(!message.is_null());
        // Z_DATA_ERROR.
        assert_eq!(code, -3, "a failed data check is a data error");
        let text = c_str(message).to_bytes().to_vec();
        assert!(!text.is_empty(), "a recorded error must have text");
        // The path is prefixed, exactly as `gz_error` concatenates it.
        assert!(
            text.starts_with(path.as_bytes()),
            "message {:?} is not prefixed with the path",
            String::from_utf8_lossy(&text)
        );
        // A second call reports the same text through a still-valid pointer.
        let again = last_error(file, core::ptr::null_mut());
        assert_eq!(c_str(again).to_bytes(), &text[..]);
        // And the deferred failure surfaces on the next read, as C defers it.
        assert_eq!(read_bytes(file, &mut buf), -1);
        // The close itself reports `Z_OK`: `gzclose_r` promotes only a lingering
        // `Z_BUF_ERROR` (`gzread.c` L662), and a corrupt header latches `Z_DATA_ERROR`,
        // which stays available through `gzerror` instead. The value is captured rather
        // than asserted because what this test is about is the message, not the code.
        let _closed = close(file);
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
        let message = last_error(file, core::ptr::addr_of_mut!(code));
        assert!(!message.is_null());
        assert_eq!(code, Z_OK, "trailing garbage is not an error");
        assert_eq!(c_str(message).to_bytes(), b"");
        assert_eq!(eof(file), 1);
        assert_eq!(close(file), Z_OK);
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
            printf_begin(
                file,
                core::ptr::addr_of_mut!(scratch_ptr),
                core::ptr::addr_of_mut!(size),
            ),
            Z_OK
        );
        assert!(!scratch_ptr.is_null());
        // The default buffer is 8192, so the region is 8192 bytes and the documented cap is
        // one less.
        assert_eq!(size, 8192);
        // The last byte is the overflow sentinel and must arrive zeroed.
        //
        // SAFETY: `printf_begin` returned `Z_OK` and a non-null `scratch_ptr` -- both
        // asserted above -- which is its promise that `size` bytes at that address are the
        // stream's own scratch region and are readable and writable until `printf_commit`.
        // `size` is at least 1, so `size - 1` is inside it.
        assert_eq!(unsafe { *scratch_ptr.add(size - 1) }, 0);

        // Format into it exactly as `vsnprintf` would: bytes, then a terminator, and report
        // the length it WOULD have written.
        let text = b"hello, world";
        // SAFETY: two writes into the region `printf_begin` handed over, which is what the
        // shim's `vsnprintf` does with it. `text.len() + 1` is 13 and `size` is 8192 --
        // asserted above -- so both the copy and the terminator stay inside the region, and
        // `text` is a `'static` literal that cannot overlap the stream's scratch buffer.
        unsafe {
            core::ptr::copy_nonoverlapping(text.as_ptr(), scratch_ptr, text.len());
            *scratch_ptr.add(text.len()) = 0;
        }
        assert_eq!(printf_commit(file, text.len()), as_count(text.len()));
        assert_eq!(close(file), Z_OK);
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
            printf_begin(
                file,
                core::ptr::addr_of_mut!(region),
                core::ptr::addr_of_mut!(size),
            ),
            Z_OK
        );
        // `size - 1` characters is the documented cap; report exactly `size`, which is one
        // too many, and nothing must be written or counted.
        assert_eq!(printf_commit(file, size), 0);
        // A formatter that failed reports `(size_t)-1`, which folds into "did not fit".
        assert_eq!(
            printf_begin(
                file,
                core::ptr::addr_of_mut!(region),
                core::ptr::addr_of_mut!(size),
            ),
            Z_OK
        );
        assert_eq!(printf_commit(file, usize::MAX), 0);
        assert_eq!(close(file), Z_OK);
        assert!(read_file(&path).is_empty(), "nothing may have been written");
    }

    #[test]
    fn the_printf_helpers_refuse_an_unusable_request() {
        // A null handle, a null out-parameter, and a read stream: all refused with
        // `Z_STREAM_ERROR`, and none of them may touch the out-parameters.
        let mut region: *mut u8 = core::ptr::null_mut();
        let mut size: usize = 0xdead;
        assert_eq!(
            printf_begin(
                core::ptr::null_mut(),
                core::ptr::addr_of_mut!(region),
                core::ptr::addr_of_mut!(size),
            ),
            Z_STREAM_ERROR
        );
        assert_eq!(size, 0xdead);
        assert_eq!(printf_commit(core::ptr::null_mut(), 4), Z_STREAM_ERROR);

        let scratch = Scratch::new("printfbad");
        let path = scratch.c_path();
        write_file(&path, "wb", b"payload");
        let file = open(&path, "rb");
        assert_eq!(
            printf_begin(file, core::ptr::null_mut(), core::ptr::addr_of_mut!(size)),
            Z_STREAM_ERROR
        );
        assert_eq!(
            printf_begin(file, core::ptr::addr_of_mut!(region), core::ptr::null_mut(),),
            Z_STREAM_ERROR
        );
        // A read stream is not writable, so the core's own guard refuses it.
        assert_eq!(
            printf_begin(
                file,
                core::ptr::addr_of_mut!(region),
                core::ptr::addr_of_mut!(size),
            ),
            Z_STREAM_ERROR
        );
        assert_eq!(close(file), Z_OK);
    }

    #[test]
    fn every_entry_point_answers_a_null_handle_with_its_documented_value() {
        // Each value here is the one `zlib.h` documents for that family, and several
        // neighbours disagree -- which is the reason to assert them all in one place.
        let null: gzFile = core::ptr::null_mut();
        let mut byte = [0_u8; 4];
        let out = byte.as_mut_ptr().cast::<c_void>();

        assert_eq!(set_buffer(null, 8192), -1);
        assert_eq!(set_params(null, 6, 0), Z_STREAM_ERROR);
        assert_eq!(read_raw(null, out, 4), -1);
        assert_eq!(fread_items(out, 1, 4, null), 0);
        assert_eq!(write_raw(null, out.cast_const(), 4), 0);
        assert_eq!(fwrite_items(out.cast_const(), 1, 4, null), 0);
        assert_eq!(put_byte(null, 0), -1);
        let text = CString::new("text").unwrap();
        assert_eq!(put_string(null, text.as_ptr()), -1);
        assert!(get_line(null, byte.as_mut_ptr().cast::<c_char>(), 4).is_null());
        assert_eq!(get_byte(null), -1);
        assert_eq!(get_byte_fallback(null), -1);
        assert_eq!(unget_byte(0, null), -1);
        assert_eq!(flush(null, 0), Z_STREAM_ERROR);
        assert_eq!(seek(null, 0, SEEK_SET), -1);
        assert_eq!(seek64(null, 0, SEEK_SET), -1);
        assert_eq!(rewind(null), -1);
        assert_eq!(tell(null), -1);
        assert_eq!(tell64(null), -1);
        assert_eq!(offset(null), -1);
        assert_eq!(offset64(null), -1);
        assert_eq!(eof(null), 0);
        assert_eq!(direct(null), 0);
        assert_eq!(close(null), Z_STREAM_ERROR);
        assert_eq!(close_read(null), Z_STREAM_ERROR);
        assert_eq!(close_write(null), Z_STREAM_ERROR);
        assert!(last_error(null, core::ptr::null_mut()).is_null());
        clear_error(null);

        // A null path or mode is `Z_NULL`, not an empty name.
        let mode = CString::new("rb").unwrap();
        assert!(open_raw(core::ptr::null(), mode.as_ptr()).is_null());
        assert!(open64_raw(core::ptr::null(), mode.as_ptr()).is_null());
        assert!(open_raw(text.as_ptr(), core::ptr::null()).is_null());
        assert!(dopen(0, core::ptr::null()).is_null());
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
        assert_eq!(close_write(file), Z_STREAM_ERROR);
        assert_eq!(get_byte(file), i32::from(b'p'));
        assert_eq!(close_read(file), Z_OK);

        // And symmetrically for a write stream and `gzclose_r` (`gzread.c` L648-L649).
        let file = open(&path, "wb");
        assert_eq!(close_read(file), Z_STREAM_ERROR);
        assert_eq!(put_byte(file, i32::from(b'x')), i32::from(b'x'));
        assert_eq!(close_write(file), Z_OK);
        assert_eq!(read_file(&path), b"x");
    }

    #[test]
    fn a_freshly_installed_state_with_no_direction_is_not_released_by_close() {
        // The `GZ_NONE` case: C's `gzclose` dispatches to `gzclose_w`, whose mode guard
        // refuses without freeing. Reproduced exactly, which is why the block has to be
        // released by hand afterwards.
        let file = install(FacadeGzState::new(GlobalAllocator));
        assert_eq!(close(file), Z_STREAM_ERROR);
        assert_eq!(close_read(file), Z_STREAM_ERROR);
        assert_eq!(close_write(file), Z_STREAM_ERROR);
        // Still live, because nothing was freed.
        assert!(state_of(file).is_some());
        release_block(file);
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
        assert_eq!(close(file), -5);
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
        assert_eq!(close(file), Z_OK);
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
        assert_eq!(close(file), Z_OK);

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
        let file = dopen(fd, mode.as_ptr());
        assert!(!file.is_null());
        assert!(write_bytes(file, payload) > 0);
        assert_eq!(close(file), Z_OK);

        // Read it back through another adopted descriptor.
        let handle = std::fs::File::open(&scratch.0).unwrap();
        let fd = handle.into_raw_fd();
        let mode = CString::new("rb").unwrap();
        let file = dopen(fd, mode.as_ptr());
        assert!(!file.is_null());
        let mut out = vec![0_u8; 256];
        let got = read_bytes(file, &mut out);
        assert_eq!(count(got), payload.len());
        assert_eq!(&out[..payload.len()], payload);
        assert_eq!(close(file), Z_OK);

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
        assert!(dopen(-1, mode.as_ptr()).is_null());

        // An invalid mode must leave the descriptor untouched -- `zlib.h` L1415-L1416. The
        // descriptor is still usable afterwards, which is what proves it.
        let scratch = Scratch::new("dopenbad");
        std::fs::write(&scratch.0, b"contents").unwrap();
        let handle = std::fs::File::open(&scratch.0).unwrap();
        let raw = handle.as_raw_fd();
        let bad = CString::new("rb+").unwrap();
        assert!(dopen(raw, bad.as_ptr()).is_null());
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
        let file = dopen(fd, mode.as_ptr());
        assert!(!file.is_null());
        assert_eq!(close(file), Z_OK);
    }

    /// Reads one of a descriptor's two flag words, failing the test if `fcntl` refuses.
    ///
    /// `get` is [`libc::F_GETFD`] for the descriptor flags or [`libc::F_GETFL`] for the file
    /// status flags -- the same pair [`update_flag_word`] writes through.
    /// Places a state block behind a `gzFile`, the way an entry point does.
    ///
    /// The production paths keep the two halves apart on purpose -- every allocation happens
    /// before the file system is touched -- so the convenience of doing both at once lives
    /// here rather than beside them.
    fn install(state: FacadeGzState) -> gzFile {
        let block = reserve_block();
        assert!(!block.is_null(), "reserve_block reported exhaustion");
        // SAFETY: `block` is the reservation from the line above, uncommitted.
        unsafe { commit_block(block, state) }
    }

    /// Sets or clears one bit of a descriptor's flag word, reporting `fcntl`'s refusal.
    ///
    /// The descriptor layer composes the exact `fcntl` calls `gzlib.c` L228-L262 makes and
    /// nothing more, so the general read-modify-write a test needs to *arrange* a starting
    /// state lives here instead of in the library.
    #[cfg(unix)]
    fn update_flag_word(
        fd: c_int,
        get: c_int,
        set: c_int,
        mask: c_int,
        on: bool,
    ) -> Result<(), ()> {
        // SAFETY: raw C calls with no pointer argument. `F_GETFD`/`F_GETFL` take no third
        // argument and `F_SETFD`/`F_SETFL` take an `int`, so both variadic calls are well
        // formed; the only failure mode is the `-1` return, which is reported rather than
        // ignored.
        unsafe {
            let current = libc::fcntl(fd, get);
            if current == -1 {
                return Err(());
            }
            let wanted = if on { current | mask } else { current & !mask };
            if libc::fcntl(fd, set, wanted) == -1 {
                return Err(());
            }
        }
        Ok(())
    }

    #[cfg(unix)]
    fn descriptor_flags(fd: c_int, get: c_int) -> c_int {
        // SAFETY: a raw C call with no pointer argument. `fd` is a descriptor the caller
        // has just proved open, and `F_GETFD`/`F_GETFL` take no third argument, so the
        // variadic call is well formed. The only failure mode is the `-1` return.
        let flags = unsafe { libc::fcntl(fd, get) };
        assert_ne!(flags, -1, "fcntl(fd, {get}) failed");
        flags
    }

    /// `gzdopen(fd, "…e…")` reproduces the reference's answer for an adopted descriptor.
    ///
    /// ★ **Measured, not assumed: on Linux the `e` character has no effect here.** C applies
    /// the flag with `fcntl(fd, F_SETFD, O_CLOEXEC)` (`gzlib.c` L253-L262), and `O_CLOEXEC`
    /// is an *open* flag while `F_SETFD` defines only `FD_CLOEXEC`; the kernel ignores the
    /// undefined bits, so the reference leaves the descriptor flag word untouched. The port
    /// reproduces that verbatim, because behavioural parity with the oracle outranks keeping
    /// the promise `zlib.h` L1377-L1378 makes -- an "improvement" here would make the two
    /// libraries observably differ. The path-based open is unaffected: there the flag is
    /// folded into the `oflag` as `O_CLOEXEC`, where it is defined and does apply.
    ///
    /// `zlib.h` L1377-L1378: "the addition of `e` when reading or writing will set the flag
    /// to close the file on an `execve()` call." This is the descriptor-side half of that
    /// promise, which only `gzdopen` can keep -- a path-based open folds the flag into
    /// `open(2)` instead.
    ///
    /// ★ The flag is cleared first, and that step is load-bearing rather than tidy: `std`
    /// opens every [`File`] with `O_CLOEXEC` unconditionally, so a freshly opened descriptor
    /// already has the bit set and an assertion on it would pass whether or not the library
    /// did anything at all. Clearing it first is what makes the observation mean something.
    #[cfg(unix)]
    #[test]
    fn the_e_mode_character_sets_close_on_exec_on_an_adopted_descriptor() {
        use std::os::fd::{AsRawFd, IntoRawFd};

        let scratch = Scratch::new("cloexec");
        std::fs::write(&scratch.0, b"contents").unwrap();

        // Both modes answer 0: see this test's own note on `F_SETFD` and `O_CLOEXEC`.
        for (mode, expected) in [("rb", 0), ("rbe", 0)] {
            let handle = std::fs::File::open(&scratch.0).unwrap();
            let raw = handle.as_raw_fd();
            update_flag_word(raw, libc::F_GETFD, libc::F_SETFD, libc::FD_CLOEXEC, false).unwrap();
            assert_eq!(
                descriptor_flags(raw, libc::F_GETFD) & libc::FD_CLOEXEC,
                0,
                "the flag must start clear for {mode:?} to be observable"
            );

            let text = CString::new(mode).unwrap();
            let fd = handle.into_raw_fd();
            let file = dopen(fd, text.as_ptr());
            assert!(!file.is_null(), "gzdopen({mode:?})");
            // Read the flag while the stream still owns the descriptor: `gzclose` closes it,
            // after which the number names nothing.
            assert_eq!(
                descriptor_flags(fd, libc::F_GETFD) & libc::FD_CLOEXEC,
                expected,
                "close-on-exec after gzdopen({mode:?})"
            );
            assert_eq!(close(file), Z_OK);
        }
    }

    /// `gzdopen(fd, "…N…")` must put the descriptor into non-blocking mode.
    ///
    /// `zlib.h` L1398-L1402 spells this out as the supported way to get a non-blocking
    /// stream without a non-blocking *open*: "it can use `open()` without `O_NONBLOCK`, and
    /// then `gzdopen()` with the resulting file descriptor and `N` in the mode, which will
    /// set it to non-blocking." C performs it as `fcntl(fd, F_SETFL, … | O_NONBLOCK)`
    /// (`gzlib.c` L254-L257); here it arrives through
    /// [`super::AdoptedDescriptor::set_nonblocking`], which
    /// `zlib_rs::gz::open::gz_open_handle` calls for exactly this character.
    ///
    /// No pre-clearing step is needed on this side: `std` does not request `O_NONBLOCK`, so
    /// the bit starts clear -- which the assertion checks rather than assumes.
    #[cfg(unix)]
    #[test]
    fn the_n_mode_character_sets_non_blocking_on_an_adopted_descriptor() {
        use std::os::fd::{AsRawFd, IntoRawFd};

        let scratch = Scratch::new("nonblock");
        std::fs::write(&scratch.0, b"contents").unwrap();

        for (mode, expected) in [("rb", 0), ("rbN", libc::O_NONBLOCK)] {
            let handle = std::fs::File::open(&scratch.0).unwrap();
            assert_eq!(
                descriptor_flags(handle.as_raw_fd(), libc::F_GETFL) & libc::O_NONBLOCK,
                0,
                "the flag must start clear for {mode:?} to be observable"
            );

            let text = CString::new(mode).unwrap();
            let fd = handle.into_raw_fd();
            let file = dopen(fd, text.as_ptr());
            assert!(!file.is_null(), "gzdopen({mode:?})");
            assert_eq!(
                descriptor_flags(fd, libc::F_GETFL) & libc::O_NONBLOCK,
                expected,
                "O_NONBLOCK after gzdopen({mode:?})"
            );
            assert_eq!(close(file), Z_OK);
        }
    }

    /// Both flags survive together, and neither disturbs the stream's own behaviour.
    ///
    /// The two `fcntl` calls write different flag words -- `F_SETFD` and `F_SETFL` -- so
    /// setting one must not clear the other, and C sets both when the mode string carries
    /// both characters. The read-back afterwards is what proves the descriptor is still a
    /// working gzip stream rather than merely a correctly flagged number.
    #[cfg(unix)]
    #[test]
    fn the_two_descriptor_flags_are_independent_and_the_stream_still_works() {
        use std::os::fd::{AsRawFd, IntoRawFd};

        let scratch = Scratch::new("bothflags");
        let path = scratch.c_path();
        let payload = b"payload behind two descriptor flags";
        write_file(&path, "wb", payload);

        let handle = std::fs::File::open(&scratch.0).unwrap();
        let raw = handle.as_raw_fd();
        update_flag_word(raw, libc::F_GETFD, libc::F_SETFD, libc::FD_CLOEXEC, false).unwrap();
        let mode = CString::new("rbeN").unwrap();
        let fd = handle.into_raw_fd();
        let file = dopen(fd, mode.as_ptr());
        assert!(!file.is_null());
        // `e` leaves the descriptor flag word alone on an adopted descriptor, exactly as the
        // reference does -- see `the_e_mode_character_...` above for the measurement. `N` does
        // apply, because C performs it as `fcntl(fd, F_SETFL, … | O_NONBLOCK)` where the bit
        // is defined. The two are independent, which is what this test is about.
        assert_eq!(descriptor_flags(fd, libc::F_GETFD) & libc::FD_CLOEXEC, 0);
        assert_eq!(
            descriptor_flags(fd, libc::F_GETFL) & libc::O_NONBLOCK,
            libc::O_NONBLOCK
        );

        let mut out = vec![0_u8; 256];
        let got = read_bytes(file, &mut out);
        assert_eq!(count(got), payload.len());
        assert_eq!(&out[..payload.len()], payload);
        assert_eq!(close(file), Z_OK);
    }

    /// [`update_flag_word`] clears as well as sets, which the trait requires.
    ///
    /// `GzHandle::set_nonblocking` is declared as "sets **or clears** the non-blocking
    /// flag". C only ever ORs a flag in, so the clearing direction has no reference call
    /// site and is exercised directly here rather than through an entry point.
    #[cfg(unix)]
    #[test]
    fn a_descriptor_flag_can_be_cleared_again() {
        use std::os::fd::AsRawFd;

        let scratch = Scratch::new("clearflag");
        std::fs::write(&scratch.0, b"contents").unwrap();
        let handle = std::fs::File::open(&scratch.0).unwrap();
        let fd = handle.as_raw_fd();

        update_flag_word(fd, libc::F_GETFL, libc::F_SETFL, libc::O_NONBLOCK, true).unwrap();
        assert_eq!(
            descriptor_flags(fd, libc::F_GETFL) & libc::O_NONBLOCK,
            libc::O_NONBLOCK
        );
        update_flag_word(fd, libc::F_GETFL, libc::F_SETFL, libc::O_NONBLOCK, false).unwrap();
        assert_eq!(descriptor_flags(fd, libc::F_GETFL) & libc::O_NONBLOCK, 0);

        // An invalid descriptor is refused rather than silently accepted, so the `-1` return
        // of `fcntl` really is propagated. `c_int::MAX` is used rather than a descriptor
        // this test closed: descriptor numbers are recycled per process, and these tests run
        // in parallel, so a just-closed number can legitimately be reopened by a sibling
        // test between the close and the check. `c_int::MAX` exceeds every attainable
        // `RLIMIT_NOFILE` and so is `EBADF` deterministically.
        assert!(
            update_flag_word(
                c_int::MAX,
                libc::F_GETFD,
                libc::F_SETFD,
                libc::FD_CLOEXEC,
                true
            )
            .is_err(),
            "fcntl on an invalid descriptor must fail"
        );
    }

    /// A non-null pointer that must never be dereferenced.
    ///
    /// ★ The point of the guard-ordering tests below. C's `gz_open` returns before it looks
    /// at `mode` when `path` is null (`gzlib.c` L96-L97), its `gzdopen` returns before it
    /// looks at `mode` when `fd` is `-1` (`gzlib.c` L302-L303), and its `gzputs` returns
    /// before `strlen(s)` when the stream is null or not writable (`gzwrite.c` L358-L364).
    /// A caller may therefore legitimately pass a pointer that is stale, misaligned or wild
    /// on those paths. Reading through this address would fault, and under Miri it is caught
    /// as the invalid dereference it is -- which is what makes it evidence rather than a
    /// hope.
    ///
    /// An ordinary integer-to-pointer cast rather than
    /// `core::ptr::without_provenance_mut`, which is `strict_provenance` and so was
    /// stabilised in Rust 1.84 -- past this workspace's 1.80 floor.
    fn wild<T>() -> *const T {
        0xdead_0001_usize as *const T
    }

    /// A null `path` is answered without the mode string being scanned.
    ///
    /// `gzlib.c` L96-L97 is `if (path == NULL || mode == NULL) return NULL;`, and `||`
    /// short-circuits: with a null `path`, C never touches `mode`. The port's guard has to
    /// short-circuit the same way, because its equivalent of the null test walks the string
    /// looking for the terminator.
    #[test]
    fn a_null_path_is_refused_without_the_mode_being_read() {
        assert!(open_raw(core::ptr::null(), wild::<c_char>()).is_null());
        assert!(open64_raw(core::ptr::null(), wild::<c_char>()).is_null());
    }

    /// `fd == -1` is answered without the mode string being scanned.
    ///
    /// `gzlib.c` L302-L303: `if (fd == -1 || (path = malloc(…)) == NULL) return NULL;`. The
    /// descriptor test is C's first, and it precedes `gz_open`'s scan of `mode`.
    #[test]
    fn a_minus_one_descriptor_is_refused_without_the_mode_being_read() {
        assert!(dopen(-1, wild::<c_char>()).is_null());
    }

    /// `gzputs` refuses a null or non-writable stream without measuring the string.
    ///
    /// C's order is `file == NULL`, then `state->mode != GZ_WRITE || state->err != Z_OK`,
    /// and only then `strlen(s)` (`gzwrite.c` L358-L364). A read stream is the reachable
    /// half of the second test, and both are exercised with a pointer that would fault if
    /// it were read.
    #[test]
    fn gzputs_refuses_before_it_measures_the_string() {
        assert_eq!(put_string(core::ptr::null_mut(), wild::<c_char>()), -1);

        let scratch = Scratch::new("putsorder");
        let path = scratch.c_path();
        write_file(&path, "wb", b"payload");
        let file = open(&path, "rb");
        assert_eq!(put_string(file, wild::<c_char>()), -1);
        // The refusal records nothing, exactly as C's bare `return -1` records nothing, so
        // the stream is still usable.
        let mut out = vec![0_u8; 32];
        assert_eq!(count(read_bytes(file, &mut out)), b"payload".len());
        assert_eq!(close(file), Z_OK);
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
        let file = dopen(good.into_raw_fd(), mode.as_ptr());
        assert!(!file.is_null());
        assert_eq!(close(file), Z_OK, "a valid descriptor");

        // Now the hostile case, built exactly as a C caller would build it: give up
        // ownership of the number, close it, and only then hand the stale number over.
        // Reclaiming and dropping is how the number is closed without a raw syscall; at
        // that instant it is still open, so the drop is sound and the only owner.
        let raw = std::fs::File::open(scratch.0.as_path())
            .unwrap()
            .into_raw_fd();
        let file = dopen(raw, mode.as_ptr());
        assert!(!file.is_null(), "gzdopen does not validate the descriptor");
        // Closing the descriptor behind the library's back, which is what this test exists
        // to make `gzclose` report.
        //
        // SAFETY: `raw` was produced by `into_raw_fd`, so it names an open descriptor that no
        // `File` owns; adopting it once and dropping it closes it exactly once. The library
        // holds the same number but has not closed it, and the assertion below is precisely
        // that it notices. Nothing else in this test uses `raw` afterwards.
        drop(unsafe { std::fs::File::from_raw_fd(raw) });

        assert_eq!(
            // SAFETY: `raw` was just produced by `into_raw_fd`, so it is open and unowned;
            // reconstructing the sole owner and dropping it performs exactly one close.
            close(file),
            Z_ERRNO,
            "a descriptor closed behind gzdopen's back"
        );
    }

    #[test]
    fn an_empty_payload_round_trips_and_reads_as_end_of_file() {
        let scratch = Scratch::new("empty");
        let path = scratch.c_path();
        let file = open(&path, "wb");
        assert_eq!(close(file), Z_OK);

        let file = open(&path, "rb");
        let mut out = [0_u8; 16];
        assert_eq!(read_bytes(file, &mut out), 0);
        assert_eq!(eof(file), 1);
        assert_eq!(get_byte(file), -1);
        assert_eq!(close(file), Z_OK);
    }

    #[test]
    fn a_zero_length_request_with_a_null_buffer_is_not_undefined() {
        // `core::slice::from_raw_parts(null, 0)` is undefined behaviour, so the zero-length
        // case is branched on. Under Miri this test is what proves it.
        let scratch = Scratch::new("zerolen");
        let path = scratch.c_path();
        write_file(&path, "wb", b"payload");

        let file = open(&path, "rb");
        assert_eq!(read_raw(file, core::ptr::null_mut(), 0), 0);
        assert_eq!(fread_items(core::ptr::null_mut(), 0, 0, file), 0);
        assert_eq!(close(file), Z_OK);

        let file = open(&path, "wb");
        assert_eq!(write_raw(file, core::ptr::null(), 0), 0);
        assert_eq!(fwrite_items(core::ptr::null(), 0, 0, file), 0);
        assert_eq!(close(file), Z_OK);
    }

    #[test]
    fn a_reader_and_a_writer_refuse_each_others_operations() {
        let scratch = Scratch::new("crossed");
        let path = scratch.c_path();
        write_file(&path, "wb", b"payload");

        // Writing to a read stream.
        let file = open(&path, "rb");
        let text = CString::new("nope").unwrap();
        assert_eq!(write_raw(file, text.as_ptr().cast::<c_void>(), 4), 0);
        assert_eq!(put_byte(file, 0), -1);
        assert_eq!(put_string(file, text.as_ptr()), -1);
        assert_eq!(flush(file, 0), Z_STREAM_ERROR);
        assert_eq!(set_params(file, 9, 0), Z_STREAM_ERROR);
        assert_eq!(close(file), Z_OK);

        // Reading from a write stream.
        let file = open(&path, "wb");
        let mut out = [0_u8; 4];
        assert_eq!(read_bytes(file, &mut out), -1);
        assert_eq!(get_byte(file), -1);
        assert_eq!(unget_byte(0, file), -1);
        assert!(get_line(file, out.as_mut_ptr().cast::<c_char>(), 4).is_null());
        assert_eq!(rewind(file), -1);
        // `gzeof` answers 0 for a write stream rather than failing.
        assert_eq!(eof(file), 0);
        assert_eq!(close(file), Z_OK);
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
        assert_eq!(close(file), Z_OK);

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
        assert_eq!(eof(file), 0);
        assert_eq!(close(file), Z_OK);
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

#[cfg(test)]
// `used_underscore_items` is unavoidable here and only here: the two `gzprintf` helpers MUST
// be named `_zlib_rs_gzprintf_begin` and `_zlib_rs_gzprintf_commit`, because the leading
// underscore is what `zlib.map`'s `local: _*;` pattern hides them by and `zlib.map` is
// immutable. Their real caller is `csrc/gzprintf_shim.c`, which `build.rs` compiles and links
// into every artifact; because reaching them through it needs a `va_list` that no Rust caller
// can build, these tests are the only Rust callers they will ever have.
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::used_underscore_items
)]
mod tests_backend {
    //! Unit tests for the `gzFile` facade, driven through the real C entry points.
    //!
    //! # The single invariant that discharges every `unsafe` call in this module
    //!
    //! Every raw-pointer operation the tests need is performed by the harness below --
    //! one wrapper per `extern "C"` export, per private helper ([`install`],
    //! [`release`], [`block_mut`], [`state_mut`]) and per raw-region operation -- and
    //! **each of those wrappers carries its own `// SAFETY:` comment naming the
    //! invariant it discharges**, exactly as the library half of this file does. The
    //! test bodies contain no `unsafe` at all, so the number of places this module
    //! dereferences a raw pointer is the number of wrappers rather than the number of
    //! assertions, and auditing it is mechanical. `clippy::undocumented_unsafe_blocks`
    //! is denied workspace-wide (root `Cargo.toml`, `[workspace.lints.clippy]`), which
    //! is what keeps it that way as tests are added.
    //!
    //! Three facts about what the wrappers receive hold across the whole module, and
    //! the individual invariants build on them rather than restating them:
    //!
    //! - every `gzFile` is either a null pointer passed on purpose, to exercise the
    //!   entry guard, or the exact non-null value a preceding [`open`], [`open_raw`],
    //!   [`open64_raw`], [`dopen`] or [`install`] returned inside the *same* test and
    //!   has not yet closed or released -- so it addresses a live [`GzBlock`] carrying
    //!   the [`GZ_TAG_LIVE`] tag;
    //! - every buffer reaches a wrapper as a Rust slice, [`CStr`] or [`Vec`], and the
    //!   wrapper derives the length it hands to C from that same object, so the
    //!   readable and writable extents are exact by construction rather than by
    //!   agreement -- the wrappers that pass a length of their caller's choosing
    //!   ([`gets`], [`fread_items`], [`fwrite_items`]) assert the relationship first;
    //! - the deliberately-hostile cases -- a closed handle, a double close, a foreign
    //!   pointer with the wrong tag -- construct a *valid, live* allocation first and
    //!   corrupt only the tag, which is exactly the state the guard is specified to
    //!   reject.
    //!
    //! Tests may panic -- that is how they report failure -- so `clippy.toml` allows
    //! panicking here and only here, and the `#[allow]` above re-enables `unwrap`,
    //! indexing and `panic!` for the same reason.

    use super::{
        _zlib_rs_gzprintf_begin, _zlib_rs_gzprintf_commit, block_mut, commit_block, gzbuffer,
        gzclearerr, gzclose, gzclose_r, gzclose_w, gzdirect, gzdopen, gzeof, gzerror, gzflush,
        gzfread, gzfwrite, gzgetc, gzgetc_, gzgets, gzoffset, gzoffset64, gzopen, gzopen64, gzputc,
        gzputs, gzread, gzrewind, gzseek, gzseek64, gzsetparams, gztell, gztell64, gzungetc,
        gzwrite, release, reserve_block, state_mut, FacadeGzState, GzBlock, GZ_TAG_CLOSED,
        GZ_TAG_LIVE,
    };

    // Both of these serve platform-gated tests only, so importing them unconditionally is an
    // `unused_imports` warning everywhere else -- `Descriptor` is exercised by the `cfg(unix)`
    // descriptor-lifecycle tests and `GzOpenSpec` by the LP64-Linux mode-resolution test.
    #[cfg(unix)]
    use super::Descriptor;
    #[cfg(all(target_os = "linux", target_pointer_width = "64"))]
    use super::GzOpenSpec;

    use core::ffi::{c_char, c_int, c_uint, c_void};
    use core::sync::atomic::{AtomicU32, Ordering};

    use std::ffi::{CStr, CString};
    use std::path::PathBuf;

    use zlib_rs::allocate::GlobalAllocator;
    use zlib_rs::error::ReturnCode;

    use crate::types::{gzFile, gzFile_s, z_off64_t, z_off_t};

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

    /// [`reserve_block`] and [`commit_block`] in one step, for tests that need a block and
    /// not the ordering the two halves exist to enforce.
    ///
    /// The production paths deliberately keep the halves apart -- every allocation must
    /// happen before the file system is touched, which is what `open_target` documents --
    /// so this helper lives here rather than beside them, where it would invite a caller to
    /// undo that ordering.
    fn install(state: FacadeGzState) -> gzFile {
        let block = reserve_block();
        assert!(!block.is_null(), "reserve_block reported exhaustion");
        // SAFETY: `block` is the reservation from the line above, uncommitted.
        unsafe { commit_block(block, state) }
    }

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

    // -----------------------------------------------------------------------
    // The harness -- every raw-pointer operation this module performs
    // -----------------------------------------------------------------------
    //
    // Opening and closing first, then the data path, then the cursors, then the
    // diagnostics, then the block-level helpers, then the two `gzprintf` halves.
    // Each wrapper owns exactly one `unsafe` block and one `// SAFETY:` comment; the
    // module documentation above states the three handle and buffer facts they all
    // build on, and each comment adds what is specific to its own entry point.

    /// `gzopen(path, mode)`, reporting whatever the entry point returns.
    fn open_raw(path: &CStr, mode: &CStr) -> gzFile {
        // SAFETY: unsafe-site category 5 -- two caller-supplied C strings. Both are
        // `CStr`s bound to locals that outlive the call, so each is non-null, valid for
        // reads through its terminator, and unmodified for the duration. Neither is
        // retained: `gz_open` copies the path it needs.
        unsafe { gzopen(path.as_ptr(), mode.as_ptr()) }
    }

    /// `gzopen64(path, mode)` -- the large-file face of the same function.
    fn open64_raw(path: &CStr, mode: &CStr) -> gzFile {
        // SAFETY: category 5, exactly as `open_raw`; the two entry points differ only in
        // the width of the offsets they later accept, not in what they read here.
        unsafe { gzopen64(path.as_ptr(), mode.as_ptr()) }
    }

    /// `gzdopen(fd, mode)` -- hands a descriptor to the library.
    ///
    /// Gated because every caller is: the descriptor-adoption tests are `cfg(unix)`.
    #[cfg(unix)]
    fn dopen(fd: c_int, mode: &CStr) -> gzFile {
        // SAFETY: category 5 for `mode`, which is a live `CStr` as in `open_raw`, plus
        // the documented descriptor-adoption site. Every caller of this helper has
        // already given up ownership of `fd` -- or passes `-1`, or a number it closed on
        // purpose, to exercise the guard -- and the library closes it exactly once, in
        // `gzclose`, which is `gzdopen`'s contract at `zlib.h` L1404-L1416.
        unsafe { gzdopen(fd, mode.as_ptr()) }
    }

    /// `gzopen` with Rust-side strings, panicking rather than returning a null handle.
    fn open(path: &CString, mode: &str) -> gzFile {
        let mode = CString::new(mode).unwrap();
        let file = open_raw(path, &mode);
        assert!(!file.is_null(), "gzopen({mode:?}) returned Z_NULL");
        file
    }

    /// `gzopen` that is expected to fail.
    fn open_expect_null(path: &CString, mode: &str) -> bool {
        let mode = CString::new(mode).unwrap();
        let file = open_raw(path, &mode);
        if file.is_null() {
            return true;
        }
        assert_eq!(close(file), Z_OK);
        false
    }

    /// `gzopen` with a null `path`, which `zlib.h` L1357 admits and the guard refuses.
    fn open_without_path(mode: &CStr) -> gzFile {
        // SAFETY: category 5 with the documented null case. `gz_open` tests `path` for
        // null before it reads a byte of it, so nothing is dereferenced on this path;
        // `mode` is a live `CStr` whose terminator is inside its own allocation.
        unsafe { gzopen(core::ptr::null(), mode.as_ptr()) }
    }

    /// `gzopen64` with a null `path`.
    fn open64_without_path(mode: &CStr) -> gzFile {
        // SAFETY: category 5 with the documented null case, exactly as
        // `open_without_path`; the two entry points share the one `gz_open` body.
        unsafe { gzopen64(core::ptr::null(), mode.as_ptr()) }
    }

    /// `gzopen` with a null `mode`.
    fn open_without_mode(path: &CStr) -> gzFile {
        // SAFETY: category 5 with the documented null case on the other argument. The
        // mode is tested for null before it is parsed, and `path` is a live `CStr`.
        unsafe { gzopen(path.as_ptr(), core::ptr::null()) }
    }

    /// Closes a raw descriptor by reclaiming and dropping its sole owner.
    ///
    /// This is how a descriptor is closed without a raw syscall, and it is what builds
    /// the hostile fixture for `gzclose`: a number the library will be handed *after* it
    /// has already been closed.
    #[cfg(unix)]
    fn close_descriptor(raw: c_int) {
        use std::os::fd::FromRawFd as _;

        // SAFETY: the caller has just obtained `raw` from `into_raw_fd`, so at this
        // instant it is open and unowned; reconstructing the sole owner from it and
        // dropping that owner therefore performs exactly one `close(2)`, and no other
        // `File` exists that could close it a second time.
        drop(unsafe { std::fs::File::from_raw_fd(raw) });
    }

    /// Moves an open descriptor to a number no other test in this binary can be handed.
    ///
    /// ★ Descriptor numbers are process-global and `open(2)` returns the LOWEST free one,
    /// so a test that frees a low number and then asserts that `close(2)` on it FAILS is
    /// hostile to every other test in the binary: another thread's `open` is handed that
    /// number, this test's `close` therefore SUCCEEDS -- the assertion sees `Z_OK` where it
    /// demanded `Z_ERRNO` -- and the other test loses its file underneath it. Measured
    /// before this helper existed: the two tests that build that fixture failed together
    /// in roughly two runs out of five, and under `AddressSanitizer`, which perturbs the
    /// interleaving, the pair failed and then leaked the stream they could no longer close.
    ///
    /// `F_DUPFD` returns the lowest free descriptor **at or above** its argument, so the
    /// number handed over afterwards is far outside the range anything allocates naturally.
    /// It is used in preference to `dup2`, which would silently close whatever occupied the
    /// target. The fixture is not weakened: the number a caller is given is still one that
    /// was genuinely open and genuinely is not any more.
    #[cfg(unix)]
    fn relocate_high(open_fd: c_int) -> c_int {
        /// Far above the handful of descriptors a test binary holds at once, and far below
        /// any plausible `RLIMIT_NOFILE`.
        const FLOOR: c_int = 512;

        // SAFETY: a C call with no pointer arguments. `open_fd` is open and owned by the
        // caller, `F_DUPFD` only reads it, and the third argument is the `c_int` that
        // request expects.
        let high = unsafe { libc::fcntl(open_fd, libc::F_DUPFD, FLOOR) };
        assert!(
            high >= FLOOR,
            "F_DUPFD to a descriptor at or above {FLOOR}; is RLIMIT_NOFILE smaller than that?"
        );
        close_descriptor(open_fd);
        high
    }

    /// `open(2)` for reading, returning the raw descriptor the tests then hand over.
    ///
    /// A descriptor has to be obtained without `std::fs::File` in these tests, because
    /// what is under test is what the library does to a NUMBER a caller adopted -- the
    /// flags on it, who closes it, and when.
    #[cfg(unix)]
    fn sys_open_read(path: &CStr) -> c_int {
        // SAFETY: a C call whose only pointer argument is `path`, a live `CStr` valid for
        // reads through its terminator. `open` with `O_RDONLY` takes no third argument, so
        // the variadic call is well formed, and the result is reported rather than
        // dereferenced.
        unsafe { libc::open(path.as_ptr(), libc::O_RDONLY) }
    }

    /// `open(2)` creating and truncating for writing, with mode `0o666`.
    #[cfg(unix)]
    fn sys_open_write(path: &CStr) -> c_int {
        // SAFETY: as `sys_open_read`, plus the third argument `O_CREAT` requires; `path`
        // is a live `CStr` and the permission bits are a plain integer.
        unsafe {
            libc::open(
                path.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC,
                0o666,
            )
        }
    }

    /// `close(2)` on a descriptor this test still owns.
    #[cfg(unix)]
    fn sys_close(fd: c_int) -> c_int {
        // SAFETY: a C call with no pointer argument. Each caller closes a descriptor it
        // opened and has not yet handed to the library, or closes it deliberately behind
        // the library's back to force the `EBADF` path -- which is the fixture, not a
        // soundness question, because no Rust object owns the number.
        unsafe { libc::close(fd) }
    }

    /// `fcntl(fd, F_GETFD)` -- the descriptor flag word, or `-1`.
    #[cfg(unix)]
    fn sys_descriptor_flags(fd: c_int) -> c_int {
        // SAFETY: a C call with no pointer argument. `F_GETFD` takes no third argument, so
        // the variadic call is well formed; the answer is reported rather than acted on.
        unsafe { libc::fcntl(fd, libc::F_GETFD) }
    }

    /// `read(2)` of a single byte, proving a descriptor is still usable.
    #[cfg(unix)]
    fn sys_read_one(fd: c_int, byte: &mut [u8; 1]) -> isize {
        // SAFETY: a C call whose buffer is an exclusive borrow of a live one-byte array,
        // and the length passed is that array's own length, so `read` cannot write past it.
        unsafe { libc::read(fd, byte.as_mut_ptr().cast::<c_void>(), 1) }
    }

    /// `gzdopen` with a null `mode`, which must be refused without taking the descriptor.
    fn dopen_without_mode(fd: c_int) -> gzFile {
        // SAFETY: category 5 with the documented null case. `gzdopen` tests `mode` for
        // null before it parses it and before it adopts `fd`, so nothing is dereferenced
        // and the descriptor stays the caller's.
        unsafe { gzdopen(fd, core::ptr::null()) }
    }

    /// `gzclose(file)` -- dispatches on the direction and ends the handle's life.
    fn close(file: gzFile) -> c_int {
        // SAFETY: category 3 -- the opaque handle round-trip, and the handle is the only
        // pointer involved. It satisfies the module-wide handle invariant, and on the
        // paths where this returns `Z_OK` it also ends the handle's life, which is why
        // no test uses a handle after a successful close.
        unsafe { gzclose(file) }
    }

    /// `gzclose_r(file)` -- the read half, which refuses a write stream without freeing.
    fn close_read(file: gzFile) -> c_int {
        // SAFETY: category 3, as `close`. The direction guard runs before the block is
        // released, so a refusal leaves the handle live and usable -- which the tests
        // rely on and then close through the correct half.
        unsafe { gzclose_r(file) }
    }

    /// `gzclose_w(file)` -- the write half, symmetrically.
    fn close_write(file: gzFile) -> c_int {
        // SAFETY: category 3, as `close_read`.
        unsafe { gzclose_w(file) }
    }

    /// `gzwrite` with Rust-side types, so no test needs a numeric cast.
    ///
    /// The lint policy denies `clippy::cast_possible_truncation` and
    /// `clippy::cast_sign_loss` in test code as well as in library code, which is the right
    /// policy: a test that silently truncated a length would assert the wrong thing.
    /// `try_from().unwrap()` is the alternative, and panicking is permitted here.
    fn write_bytes(file: gzFile, payload: &[u8]) -> c_int {
        let len = c_uint::try_from(payload.len()).unwrap();
        // SAFETY: category 2 -- the entry point rebuilds a slice from the pair. `len` is
        // `payload.len()` converted without loss, so the readable extent it reconstructs
        // is exactly `payload`, which outlives the call and is not written through.
        unsafe { gzwrite(file, payload.as_ptr().cast::<c_void>(), len) }
    }

    /// `gzread` with Rust-side types, returning the raw count -- negative on error.
    fn read_bytes(file: gzFile, out: &mut [u8]) -> c_int {
        let len = c_uint::try_from(out.len()).unwrap();
        // SAFETY: category 2, in the writing direction. `len` is `out.len()` converted
        // without loss, so the writable extent the entry point reconstructs is exactly
        // `out`; the `&mut` borrow makes it unaliased for the duration of the call.
        unsafe { gzread(file, out.as_mut_ptr().cast::<c_void>(), len) }
    }

    /// `gzwrite(file, NULL, 0)` -- the documented zero-length request.
    fn write_nothing_from_null(file: gzFile) -> c_int {
        // SAFETY: category 2, the zero-length case the facade branches on precisely
        // because `slice::from_raw_parts(null, 0)` would be undefined behaviour. A null
        // buffer with `len == 0` asks for nothing to be copied, so no slice is built and
        // nothing is dereferenced -- which is what this probe exists to pin.
        unsafe { gzwrite(file, core::ptr::null(), 0) }
    }

    /// `gzread(file, NULL, 0)` -- the same request in the reading direction.
    fn read_nothing_into_null(file: gzFile) -> c_int {
        // SAFETY: category 2, the zero-length case, as `write_nothing_from_null`.
        unsafe { gzread(file, core::ptr::null_mut(), 0) }
    }

    /// `gzfwrite(buf, size, nitems, file)` -- counts items, not bytes.
    ///
    /// The product is checked against the slice first, so the only two shapes this
    /// helper can hand to C are an exact extent and a deliberately overflowing one.
    fn fwrite_items(payload: &[u8], size: usize, items: usize, file: gzFile) -> usize {
        if let Some(product) = size.checked_mul(items) {
            assert_eq!(
                product,
                payload.len(),
                "a representable size x nitems must be the whole slice"
            );
        }
        // SAFETY: category 2. `gzfwrite` computes `size * nitems` with `checked_mul` and
        // reconstructs a slice of that length, or of length zero when the product
        // overflows, before it touches anything: so the extent is either exactly
        // `payload` -- guaranteed by the assertion above -- or empty. `payload` outlives
        // the call and is only read.
        unsafe { gzfwrite(payload.as_ptr().cast::<c_void>(), size, items, file) }
    }

    /// `gzfread(buf, size, nitems, file)` -- the reading counterpart.
    fn fread_items(out: &mut [u8], size: usize, items: usize, file: gzFile) -> usize {
        if let Some(product) = size.checked_mul(items) {
            assert_eq!(
                product,
                out.len(),
                "a representable size x nitems must be the whole slice"
            );
        }
        // SAFETY: category 2, in the writing direction, with the same product argument
        // as `fwrite_items`: the writable extent is exactly `out` or empty, never
        // anything else, and the `&mut` borrow keeps it unaliased for the call.
        unsafe { gzfread(out.as_mut_ptr().cast::<c_void>(), size, items, file) }
    }

    /// `gzfwrite(NULL, 0, 0, file)` -- the zero-length item request.
    fn fwrite_nothing_from_null(file: gzFile) -> usize {
        // SAFETY: category 2, the zero-length case: the product is zero, so the slice
        // the entry point builds is empty and the null pointer is never dereferenced.
        unsafe { gzfwrite(core::ptr::null(), 0, 0, file) }
    }

    /// `gzfread(NULL, 0, 0, file)` -- the same in the reading direction.
    fn fread_nothing_into_null(file: gzFile) -> usize {
        // SAFETY: category 2, the zero-length case, as `fwrite_nothing_from_null`.
        unsafe { gzfread(core::ptr::null_mut(), 0, 0, file) }
    }

    /// `gzgetc(file)` -- the real exported function, not the macro.
    fn getc(file: gzFile) -> c_int {
        // SAFETY: category 3 -- the handle is the only pointer involved and satisfies the
        // module-wide invariant. This is the arm a caller's macro falls through to when
        // its `have` is zero, so it must accept exactly the handles the macro sees.
        unsafe { gzgetc(file) }
    }

    /// `gzgetc_(file)` -- the second, backward-compatibility entry point.
    fn getc_compat(file: gzFile) -> c_int {
        // SAFETY: category 3, as `getc`; `zlib.h` L1961 exports both names and they
        // share one body.
        unsafe { gzgetc_(file) }
    }

    /// `gzungetc(c, file)` -- note the inverted argument order, which is C's.
    fn ungetc(byte: c_int, file: gzFile) -> c_int {
        // SAFETY: category 3 -- the handle is the only pointer; `byte` is a plain `int`,
        // including the `-1` that `zlib.h` L1630 documents as the idiom for forcing a
        // pending seek to run.
        unsafe { gzungetc(byte, file) }
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
    fn macro_gzgetc(file: gzFile) -> c_int {
        // SAFETY: category 3 -- one field of the caller-visible prefix. `file` satisfies
        // the module-wide handle invariant, so the block it addresses begins with an
        // initialised `gzFile_s` at offset 0, and `have` is therefore readable. The
        // borrow the read takes ends with the statement.
        let available = unsafe { (*file).have };
        if available == 0 {
            // The macro's function arm, taken while nothing is buffered.
            return getc(file);
        }
        // SAFETY: category 3 -- the prefix's three fields, mutated exactly as caller
        // object code mutates them. `file` addresses a live block as above, so all three
        // fields are initialised; `have` is non-zero here, which is precisely the state
        // in which `GzState::refresh_exposed` guarantees `next` addresses `have` readable
        // bytes inside the library's own output buffer, so the one-byte read and the
        // one-byte advance both stay inside that buffer. No library call is made while
        // the borrow is alive.
        unsafe {
            let exposed: &mut gzFile_s = &mut *file;
            exposed.have -= 1;
            exposed.pos += 1;
            let byte = *exposed.next;
            exposed.next = exposed.next.add(1);
            c_int::from(byte)
        }
    }

    /// `gzgets(file, buf, len)` -- the line, or [`None`] at end of file or on error.
    ///
    /// `len` is passed through verbatim, including the non-positive values the guard has
    /// to refuse, and the returned pointer is compared with the buffer's own address
    /// here so that no test body has to hold a raw pointer to do it.
    fn gets(file: gzFile, buf: &mut [u8], len: c_int) -> Option<Vec<u8>> {
        assert!(
            len <= c_int::try_from(buf.len()).unwrap(),
            "gzgets may not be given a length larger than the buffer"
        );
        let want = buf.as_mut_ptr().cast::<c_char>();
        // SAFETY: category 2 -- `want` is writable for `buf.len()` bytes, the assertion
        // above keeps `len` within that, and `gzgets` writes at most `len` bytes
        // including its terminator (refusing `len < 1` before writing at all). The `&mut`
        // borrow keeps the buffer unaliased for the call.
        let got = unsafe { gzgets(file, want, len) };
        if got.is_null() {
            return None;
        }
        assert_eq!(got, want, "gzgets must return the caller's own buffer");
        // SAFETY: category 5 in the outward direction. `got` is `want`, so it addresses
        // `buf`; on this path `gzgets` has written a zero-terminated string there, so the
        // terminator is inside the buffer and the scan cannot leave it. The borrow ends
        // when the owned copy is built.
        Some(unsafe { CStr::from_ptr(got) }.to_bytes().to_vec())
    }

    /// `gzgets` with a null buffer, which is refused before anything is written.
    fn gets_without_buffer(file: gzFile, len: c_int) -> bool {
        // SAFETY: category 2 with the documented null case -- `gzread.c` L574-L575 tests
        // `buf == NULL` first, so nothing is dereferenced. Only the nullness of the
        // result is inspected, so no pointer is followed here either.
        unsafe { gzgets(file, core::ptr::null_mut(), len) }.is_null()
    }

    /// `gzputc(file, c)` -- masks its result with `0xff` on both return paths.
    fn putc(file: gzFile, byte: c_int) -> c_int {
        // SAFETY: category 3 -- the handle is the only pointer; `byte` is a plain `int`.
        unsafe { gzputc(file, byte) }
    }

    /// `gzputs(file, s)` -- counts the characters written.
    fn puts(file: gzFile, text: &CStr) -> c_int {
        // SAFETY: category 5 -- `text` is a live `CStr`, so it is non-null and valid for
        // reads through its terminator, which is what the `strlen` inside the entry point
        // requires; it is not retained past the call.
        unsafe { gzputs(file, text.as_ptr()) }
    }

    /// `gzputs(file, NULL)`, which is refused rather than faulting inside `strlen`.
    fn puts_without_string(file: gzFile) -> c_int {
        // SAFETY: category 5 with the documented null case -- the entry point tests `s`
        // for null before it measures it, so nothing is dereferenced.
        unsafe { gzputs(file, core::ptr::null()) }
    }

    /// `gztell(file)` -- the narrow face.
    fn tell(file: gzFile) -> z_off_t {
        // SAFETY: category 3 -- the handle is the only pointer, and it satisfies the
        // module-wide invariant. Nothing is written.
        unsafe { gztell(file) }
    }

    /// `gztell64(file)` -- the wide face of the same reading.
    fn tell_wide(file: gzFile) -> z_off64_t {
        // SAFETY: category 3, as `tell`; the two faces share one implementation over
        // `ZOff64` and differ only in the width they report.
        unsafe { gztell64(file) }
    }

    /// `gzoffset(file)` -- the narrow face.
    fn offset(file: gzFile) -> z_off_t {
        // SAFETY: category 3, as `tell`.
        unsafe { gzoffset(file) }
    }

    /// `gzoffset64(file)` -- the wide face.
    fn offset_wide(file: gzFile) -> z_off64_t {
        // SAFETY: category 3, as `tell`.
        unsafe { gzoffset64(file) }
    }

    /// `gzseek(file, offset, whence)` -- the narrow face.
    fn seek(file: gzFile, offset: z_off_t, whence: c_int) -> z_off_t {
        // SAFETY: category 3 -- the handle is the only pointer; the offset and the
        // whence code are plain integers, and an unsupported `whence` is answered with
        // `-1` rather than acted on.
        unsafe { gzseek(file, offset, whence) }
    }

    /// `gzseek64(file, offset, whence)` -- the wide face.
    fn seek_wide(file: gzFile, offset: z_off64_t, whence: c_int) -> z_off64_t {
        // SAFETY: category 3, as `seek`.
        unsafe { gzseek64(file, offset, whence) }
    }

    /// `gzrewind(file)`.
    fn rewind(file: gzFile) -> c_int {
        // SAFETY: category 3 -- the handle is the only pointer involved.
        unsafe { gzrewind(file) }
    }

    /// `gzeof(file)` -- reports `past`, not merely "at the end".
    fn eof(file: gzFile) -> c_int {
        // SAFETY: category 3 -- the handle is the only pointer involved.
        unsafe { gzeof(file) }
    }

    /// `gzdirect(file)` -- may force the transparent-read decision, so it can allocate.
    fn direct(file: gzFile) -> c_int {
        // SAFETY: category 3 for the handle, and category 4 for the allocation the
        // decision may perform: it goes through the same allocator the state was built
        // with, which is this module's `GlobalAllocator` or the default `malloc` path.
        unsafe { gzdirect(file) }
    }

    /// `gzflush(file, flush)` -- a narrower flush range than `deflate`'s.
    fn flush(file: gzFile, code: c_int) -> c_int {
        // SAFETY: category 3 -- the handle is the only pointer; `code` is a plain `int`
        // and an out-of-range value is answered with `Z_STREAM_ERROR`.
        unsafe { gzflush(file, code) }
    }

    /// `gzbuffer(file, size)` -- refused once the buffers exist.
    fn buffer(file: gzFile, size: c_uint) -> c_int {
        // SAFETY: category 3 for the handle, category 4 for the allocation a later read
        // or write makes with the size recorded here; `size` is a plain `unsigned` and
        // the clamp, the doubling-overflow test and the already-allocated test are all
        // performed by the core.
        unsafe { gzbuffer(file, size) }
    }

    /// `gzsetparams(file, level, strategy)`.
    fn setparams(file: gzFile, level: c_int, strategy: c_int) -> c_int {
        // SAFETY: category 3 -- the handle is the only pointer; both parameters are
        // plain `int`s that the core validates.
        unsafe { gzsetparams(file, level, strategy) }
    }

    /// `gzerror(file, &mut errnum)` -- the message, or [`None`] where C returns `NULL`.
    ///
    /// `errnum` stays the caller's own variable, so a test can prove the entry point
    /// left it untouched on the paths where C does not write it.
    fn error_with_code(file: gzFile, errnum: &mut c_int) -> Option<Vec<u8>> {
        // SAFETY: category 2 in the outward direction -- the pointer is derived from a
        // live `&mut c_int`, so it is non-null, aligned and writable for exactly one
        // `int`, which is what a non-null `errnum` must be. The handle satisfies the
        // module-wide invariant.
        let message = unsafe { gzerror(file, core::ptr::addr_of_mut!(*errnum)) };
        message_text(message)
    }

    /// `gzerror(file, NULL)` -- the documented "message only" form.
    fn error_message(file: gzFile) -> Option<Vec<u8>> {
        // SAFETY: category 2 with the documented null case: `zlib.h` L1775 makes
        // `errnum` optional and `gzlib.c` L524 tests it before writing, so passing null
        // writes nothing and dereferences nothing.
        let message = unsafe { gzerror(file, core::ptr::null_mut()) };
        message_text(message)
    }

    /// `gzerror(file, errnum)` returning the pointer itself, not a copy of the text.
    ///
    /// The copying wrappers above are what a test should normally use; this one exists
    /// for the single property they cannot express -- that two calls hand back the SAME
    /// address, which is how `zlib.h` L1783-L1786's "may be invalidated" contract is
    /// satisfied without allocating at query time.
    fn error_ptr_with_code(file: gzFile, errnum: &mut c_int) -> *const c_char {
        // SAFETY: as `error_with_code` -- the pointer is derived from a live `&mut c_int`,
        // so it is non-null, aligned and writable for one `int`, and the handle satisfies
        // the module-wide invariant. The returned pointer is only compared and passed to
        // `message_text`, never freed.
        unsafe { gzerror(file, core::ptr::addr_of_mut!(*errnum)) }
    }

    /// `gzerror(file, NULL)` returning the pointer itself.
    fn error_ptr(file: gzFile) -> *const c_char {
        // SAFETY: as `error_message` -- `errnum` is optional and tested before it is
        // written, so a null writes nothing and dereferences nothing.
        unsafe { gzerror(file, core::ptr::null_mut()) }
    }

    /// Copies the NUL-terminated text a `gzerror` call returned.
    ///
    /// Its only callers are the two wrappers above, which pass it the pointer the entry
    /// point has just returned to them and nothing else.
    fn message_text(message: *const c_char) -> Option<Vec<u8>> {
        if message.is_null() {
            return None;
        }
        // SAFETY: category 5 in the outward direction. A non-null `gzerror` result is
        // either a `'static` literal or the terminated copy the block owns, so it is
        // NUL-terminated and, per `zlib.h` L1783-L1786, valid at least until the next
        // call on the same handle -- which is later than this copy.
        Some(unsafe { CStr::from_ptr(message) }.to_bytes().to_vec())
    }

    /// `gzclearerr(file)` -- the only void-returning export in the crate.
    fn clearerr(file: gzFile) {
        // SAFETY: category 3 -- the handle is the only pointer involved, and a null one
        // is a documented no-op rather than a fault.
        unsafe { gzclearerr(file) };
    }

    /// A block installed with the global allocator, as every block-level test wants it.
    fn install_state() -> gzFile {
        let file = install(FacadeGzState::new(GlobalAllocator));
        assert!(!file.is_null(), "install reported exhaustion");
        file
    }

    /// The tag behind a handle, or [`None`] when the guard refuses the handle.
    fn tag_of(file: gzFile) -> Option<c_int> {
        // SAFETY: category 3 -- the opaque round-trip through `block_mut`, whose contract
        // is the module-wide handle invariant. The borrow it returns is dropped inside
        // this call, so no reference to the block outlives the answer.
        unsafe { block_mut(file) }.map(|block| block.tag)
    }

    /// Overwrites the tag of a handle the guard still accepts.
    fn poison_tag(file: gzFile, tag: c_int) {
        // SAFETY: category 3 -- reached through the guard, which establishes that the
        // block is live, aligned and correctly tagged before it hands out the borrow;
        // that borrow ends inside this call.
        let block = unsafe { block_mut(file) };
        block.expect("poison_tag needs a live handle").tag = tag;
    }

    /// Writes the tag of a handle the guard would refuse, repairing a poisoned block.
    ///
    /// The guard cannot be used for this by construction: it exists precisely to refuse
    /// a tag that is not [`GZ_TAG_LIVE`], so restoring one has to go through the raw
    /// pointer.
    fn force_tag(file: gzFile, tag: c_int) {
        // SAFETY: category 3 -- `file` was produced by `install` in the same test and has
        // not been released, so it addresses an aligned, initialised `GzBlock`; only the
        // `tag` member is written, through a raw place so that no reference to the rest
        // of the block is formed, and no borrow of the block is outstanding.
        unsafe { core::ptr::addr_of_mut!((*file.cast::<GzBlock>()).tag).write(tag) };
    }

    /// Whether the state behind a handle is reachable -- i.e. whether the guard accepts it.
    fn has_state(file: gzFile) -> bool {
        // SAFETY: category 3 -- `state_mut`'s contract is `block_mut`'s, which is the
        // module-wide handle invariant; the borrow is dropped before the answer is.
        unsafe { state_mut(file) }.is_some()
    }

    /// [`release`] -- frees the block a handle addresses.
    fn release_block(file: gzFile) {
        // SAFETY: category 3 and category 4 -- `release`'s contract. `file` came from
        // `install_state` in the same test and has not been released; every other helper
        // drops its borrow before returning, so none is outstanding here; and the handle
        // is dead afterwards, which no test violates.
        unsafe { release(file) };
    }

    /// The 24-byte exposed prefix a caller's `gzgetc` macro reads, copied out.
    fn exposed_prefix(file: gzFile) -> gzFile_s {
        // SAFETY: category 3 -- `file` addresses a live block whose first member is the
        // core state, which itself begins with this prefix, so all three fields are in
        // bounds, aligned and initialised. `gzFile_s` is `Copy`, so the copy holds no
        // borrow of the block.
        unsafe { *file }
    }

    /// The scratch region `_zlib_rs_gzprintf_begin` lends its caller.
    ///
    /// Only [`printf_begin`] constructs one, and only from the pointer and length the
    /// entry point has just published, which is what makes the two accessors below safe
    /// to offer: the extent is the library's own answer rather than a test's guess.
    struct Region {
        /// The first byte of the lent region.
        at: *mut u8,
        /// Its length in bytes, exactly as the entry point reported it.
        size: usize,
    }

    impl Region {
        /// The overflow sentinel -- the last byte, which must arrive zeroed.
        fn sentinel(&self) -> u8 {
            // SAFETY: category 2 -- one byte inside the lent region. `size` is the length
            // the entry point published for `at`, and it is at least one because the core
            // refuses a buffer too small to hold its own sentinel, so `size - 1` is the
            // region's last byte and the read is in bounds.
            unsafe { *self.at.add(self.size - 1) }
        }

        /// Writes `text` and a terminator into the region, exactly as `vsnprintf` would.
        fn write_formatted(&self, text: &[u8]) {
            assert!(
                text.len() < self.size,
                "the fixture and its terminator must fit the lent region"
            );
            // SAFETY: category 2 -- `text.len() + 1` bytes at the start of a region the
            // entry point published as `size` bytes long, and the assertion above keeps
            // that sum inside it. The region belongs to the library and cannot overlap
            // `text`, which is a live slice, so the copy is non-overlapping as required.
            unsafe {
                core::ptr::copy_nonoverlapping(text.as_ptr(), self.at, text.len());
                *self.at.add(text.len()) = 0;
            }
        }
    }

    /// `_zlib_rs_gzprintf_begin(file, &scratch, &size)` -- the C shim's first half.
    ///
    /// `size` stays the caller's own variable so that a test can prove the entry point
    /// left it untouched on a path that must not write it.
    fn printf_begin(file: gzFile, size: &mut usize) -> (c_int, Option<Region>) {
        let mut at: *mut u8 = core::ptr::null_mut();
        // SAFETY: category 2 in the outward direction -- both out-parameters are pointers
        // derived from live locals, so each is non-null, aligned and writable for its own
        // type, which is what the helper requires of a non-null pair. Nothing else is
        // dereferenced, and the region it publishes is not touched here.
        let code = unsafe {
            _zlib_rs_gzprintf_begin(
                file,
                core::ptr::addr_of_mut!(at),
                core::ptr::addr_of_mut!(*size),
            )
        };
        let region = (!at.is_null()).then_some(Region { at, size: *size });
        (code, region)
    }

    /// `_zlib_rs_gzprintf_begin` with a null region out-parameter.
    fn printf_begin_without_region(file: gzFile, size: &mut usize) -> c_int {
        // SAFETY: category 2 with the documented null case -- the helper tests both
        // out-parameters for null before it writes either, so a null `scratch` is refused
        // rather than dereferenced; `size` is derived from a live local.
        unsafe {
            _zlib_rs_gzprintf_begin(file, core::ptr::null_mut(), core::ptr::addr_of_mut!(*size))
        }
    }

    /// `_zlib_rs_gzprintf_begin` with a null size out-parameter.
    fn printf_begin_without_size(file: gzFile, region: &mut *mut u8) -> c_int {
        // SAFETY: category 2 with the documented null case on the other out-parameter,
        // as `printf_begin_without_region`; `region` is derived from a live local.
        unsafe {
            _zlib_rs_gzprintf_begin(
                file,
                core::ptr::addr_of_mut!(*region),
                core::ptr::null_mut(),
            )
        }
    }

    /// `_zlib_rs_gzprintf_commit(file, reported)` -- the shim's second half.
    fn printf_commit(file: gzFile, reported: usize) -> c_int {
        // SAFETY: category 3 -- the handle is the only pointer, and it satisfies the
        // module-wide invariant. `reported` is a plain length: the entry point rejects
        // any value that does not fit the region it lent, and forms no pointer from it
        // before that test.
        unsafe { _zlib_rs_gzprintf_commit(file, reported) }
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
        assert_eq!(close(file), Z_OK);
    }

    /// Reads the whole of the gzip file at `path` back through `gzread`.
    fn read_file(path: &CString) -> Vec<u8> {
        let file = open(path, "rb");
        let mut out = vec![0_u8; 4096];
        let got = read_bytes(file, &mut out);
        assert!(got >= 0, "gzread reported {got}");
        assert_eq!(close(file), Z_OK);
        out.truncate(count(got));
        out
    }

    #[test]
    fn a_block_round_trips_through_install_and_release() {
        let file = install_state();

        // The tag makes the handle usable, and the state is reachable through it.
        assert_eq!(tag_of(file), Some(GZ_TAG_LIVE));
        assert!(has_state(file));

        // The exposed prefix of a state with no output buffer: nothing available, pointing
        // nowhere, which is the pair the `gzgetc` macro answers by taking its function arm.
        let exposed = exposed_prefix(file);
        assert_eq!(exposed.have, 0);
        assert!(exposed.next.is_null());
        assert_eq!(exposed.pos, 0);

        release_block(file);
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
        assert!(tag_of(core::ptr::null_mut()).is_none());
        assert!(!has_state(core::ptr::null_mut()));

        let file = install_state();
        // Poison the tag by hand: this is what `release` does, and it is what a second
        // close would see while the allocator has not yet reused the block.
        poison_tag(file, GZ_TAG_CLOSED);
        assert!(tag_of(file).is_none());
        assert!(!has_state(file));
        // Restore it so the block can be released through the normal path.
        force_tag(file, GZ_TAG_LIVE);
        release_block(file);
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
        assert_eq!(direct(file), 1);
        assert_eq!(read_file(&path), payload);
        assert_eq!(close(file), Z_OK);
    }

    #[test]
    fn gzbuffer_is_refused_once_the_buffers_exist() {
        let scratch = Scratch::new("buffer");
        let path = scratch.c_path();
        write_file(&path, "wb", b"payload");

        let file = open(&path, "rb");
        // Before any read: accepted, and a size below 8 is raised rather than refused.
        assert_eq!(buffer(file, 4), 0);
        assert_eq!(buffer(file, 1 << 15), 0);
        // A size that cannot be doubled is refused (`gzlib.c` L338-L339).
        assert_eq!(buffer(file, 0x8000_0000), -1);
        // Forcing the container decision allocates, after which it is refused.
        assert_eq!(direct(file), 0);
        assert_eq!(buffer(file, 1 << 16), -1);
        assert_eq!(close(file), Z_OK);
    }

    #[test]
    fn gzsetparams_changes_level_midstream() {
        let scratch = Scratch::new("setparams");
        let path = scratch.c_path();
        let file = open(&path, "wb1");
        let head = b"the first half, written at level one";
        assert_eq!(count(write_bytes(file, head)), head.len());
        // Z_BEST_COMPRESSION, Z_DEFAULT_STRATEGY.
        assert_eq!(setparams(file, 9, 0), Z_OK);
        let tail = b"the second half, written at level nine";
        assert_eq!(count(write_bytes(file, tail)), tail.len());
        assert_eq!(close(file), Z_OK);

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
        assert_eq!(macro_gzgetc(file), i32::from(b'h'));
        assert_eq!(tell(file), 1);

        // The next four come through the MACRO's fast path, straight out of the prefix.
        for expected in *b"ello" {
            let before = exposed_prefix(file).have;
            assert!(before > 0, "the macro fast path was not available");
            assert_eq!(macro_gzgetc(file), i32::from(expected));
        }
        // Five bytes consumed in total, four of them without the library being called at
        // all. Both faces of `gztell` must report it.
        assert_eq!(tell(file), 5);
        assert_eq!(tell_wide(file), 5);

        // A Rust-side read must continue from where the macro left off, not from where the
        // library last published a cursor.
        let mut rest = [0_u8; 32];
        let got = read_bytes(file, &mut rest);
        assert_eq!(got, as_count(payload.len() - 5));
        assert_eq!(&rest[..count(got)], &payload[5..]);
        assert_eq!(tell(file), z_off_t::try_from(payload.len()).unwrap());
        // ★ `gzeof` reports `past`, not `eof`: the 32-byte request could not be satisfied
        // from a 13-byte stream, so the flag is already set here. That is C's behaviour --
        // `gz_read` sets `state->past` when it runs out of input mid-request
        // (`gzread.c` L347-L348) -- and it is precisely why `zlib.h` L1713-L1722 tells a
        // caller to test `gzeof` *after* a short read rather than before the next one.
        assert_eq!(eof(file), 1);

        // And once past the end the macro falls through to the function, which reports -1.
        assert_eq!(macro_gzgetc(file), -1);
        assert_eq!(eof(file), 1);
        assert_eq!(close(file), Z_OK);
    }

    #[test]
    fn gzgetc_and_gzgetc_underscore_are_interchangeable() {
        let scratch = Scratch::new("getc");
        let path = scratch.c_path();
        write_file(&path, "wb", b"abcd");

        let file = open(&path, "rb");
        assert_eq!(getc(file), i32::from(b'a'));
        assert_eq!(getc_compat(file), i32::from(b'b'));
        assert_eq!(getc(file), i32::from(b'c'));
        assert_eq!(getc_compat(file), i32::from(b'd'));
        assert_eq!(getc(file), -1);
        assert_eq!(getc_compat(file), -1);
        assert_eq!(close(file), Z_OK);
    }

    #[test]
    fn gzungetc_pushes_back_and_forces_a_pending_seek() {
        let scratch = Scratch::new("ungetc");
        let path = scratch.c_path();
        let payload = b"hello, hello!";
        write_file(&path, "wb", payload);

        let file = open(&path, "rb");
        // A push before anything is read: `gz_look` runs first so the buffers exist.
        assert_eq!(ungetc(i32::from(b'X'), file), i32::from(b'X'));
        assert_eq!(getc(file), i32::from(b'X'));
        assert_eq!(getc(file), i32::from(b'h'));
        // Push it back and read it again.
        assert_eq!(ungetc(i32::from(b'h'), file), i32::from(b'h'));
        assert_eq!(tell(file), 0);
        assert_eq!(getc(file), i32::from(b'h'));

        // `gzungetc(-1, file)` is the documented idiom for forcing a pending seek to run:
        // the skip is carried out BEFORE the negative byte is rejected.
        assert_eq!(seek(file, 0, SEEK_SET), 0);
        assert_eq!(ungetc(-1, file), -1);
        assert_eq!(tell(file), 0);
        assert_eq!(close(file), Z_OK);
    }

    #[test]
    fn gzgets_stops_at_a_newline_and_reports_end_of_file_with_null() {
        let scratch = Scratch::new("gets");
        let path = scratch.c_path();
        write_file(&path, "wb", b"first line\nsecond line\n");

        let file = open(&path, "rb");
        let mut buf = [0_u8; 64];
        // `gets` asserts inside itself that the entry point returned the buffer it was
        // given, which is the pointer identity `zlib.h` L1588 promises.
        assert_eq!(
            gets(file, &mut buf, 64).as_deref(),
            Some(&b"first line\n"[..])
        );
        assert_eq!(
            gets(file, &mut buf, 64).as_deref(),
            Some(&b"second line\n"[..])
        );
        // End of file: null, not an empty string.
        assert!(gets(file, &mut buf, 64).is_none());

        // `len == 1` returns null and writes nothing -- the implementation is the oracle,
        // not the header's prose.
        assert_eq!(rewind(file), 0);
        assert!(gets(file, &mut buf, 1).is_none());
        // A non-positive length, and a null buffer, are both refused.
        assert!(gets(file, &mut buf, 0).is_none());
        assert!(gets(file, &mut buf, -5).is_none());
        assert!(gets_without_buffer(file, 64));
        assert_eq!(close(file), Z_OK);
    }

    #[test]
    fn gzputc_masks_its_result_and_gzputs_counts_characters() {
        let scratch = Scratch::new("putc");
        let path = scratch.c_path();
        let file = open(&path, "wb");
        assert_eq!(putc(file, i32::from(b'h')), i32::from(b'h'));
        // `test/example.c` L105's assertion.
        let ello = CString::new("ello").unwrap();
        assert_eq!(puts(file, &ello), 4);
        // ★ Masked with 0xff on both return paths: -1 becomes 255, not -1.
        assert_eq!(putc(file, -1), 255);
        assert_eq!(close(file), Z_OK);

        assert_eq!(read_file(&path), b"hello\xff");

        // A null string is refused rather than faulting inside `strlen`.
        let file = open(&path, "wb");
        assert_eq!(puts_without_string(file), -1);
        assert_eq!(close(file), Z_OK);
    }

    #[test]
    fn the_item_oriented_entry_points_count_items_not_bytes() {
        let scratch = Scratch::new("fread");
        let path = scratch.c_path();

        // Eight items of four bytes.
        let payload: Vec<u8> = (0_u8..32).collect();
        let file = open(&path, "wb");
        assert_eq!(
            fwrite_items(&payload, 4, 8, file),
            8,
            "gzfwrite reports items"
        );
        assert_eq!(close(file), Z_OK);

        let file = open(&path, "rb");
        let mut out = vec![0_u8; 32];
        assert_eq!(fread_items(&mut out, 4, 8, file), 8);
        assert_eq!(out, payload);
        assert_eq!(close(file), Z_OK);

        // An overflowing product is refused with zero items and nothing read.
        let file = open(&path, "rb");
        let huge = usize::MAX / 2 + 1;
        assert_eq!(fread_items(&mut out, huge, 4, file), 0);
        assert_eq!(close(file), Z_OK);

        let file = open(&path, "wb");
        assert_eq!(fwrite_items(&payload, huge, 4, file), 0);
        assert_eq!(close(file), Z_OK);
    }

    #[test]
    fn gzflush_finishes_a_member_and_refuses_the_wider_flush_codes() {
        let scratch = Scratch::new("flush");
        let path = scratch.c_path();
        let file = open(&path, "wb");
        let head = b"first member";
        assert!(write_bytes(file, head) > 0);
        // Z_FINISH ends the member but leaves the file open.
        assert_eq!(flush(file, 4), Z_OK);
        let tail = b"second member";
        assert!(write_bytes(file, tail) > 0);
        // ★ Narrower than `deflate`'s range: Z_BLOCK (5) and Z_TREES (6) are refused.
        assert_eq!(flush(file, 5), Z_STREAM_ERROR);
        assert_eq!(flush(file, 6), Z_STREAM_ERROR);
        assert_eq!(flush(file, -1), Z_STREAM_ERROR);
        assert_eq!(close(file), Z_OK);

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
                open_raw(&path, &mode)
            } else {
                open64_raw(&path, &mode)
            };
            assert!(!file.is_null());

            assert_eq!(
                to_wide(tell(file)),
                tell_wide(file),
                "gztell disagreement, opener {opener}"
            );
            assert_eq!(
                to_wide(offset(file)),
                offset_wide(file),
                "gzoffset disagreement, opener {opener}"
            );

            // Forwards, then backwards, then absolute -- each through both faces. The
            // loop variable is `delta` rather than `offset` so that it cannot shadow the
            // `offset` harness wrapper the body also calls.
            for (delta, whence) in [(1024_i64, SEEK_SET), (512, SEEK_CUR), (-256, SEEK_CUR)] {
                // `try_from` rather than a cast: `z_off_t` is `c_long`, so this is the
                // identity on LP64 and a genuine narrowing on a 32-bit target, and the
                // test must be portable to both.
                let narrowed = z_off_t::try_from(delta).unwrap();
                let narrow = seek(file, narrowed, whence);
                let wide = seek_wide(file, delta, whence);
                // The two calls are consecutive, so the second starts where the first left
                // off; compare each against the position reported afterwards instead.
                assert!(narrow >= 0 && wide >= 0, "seek refused: {delta} {whence}");
                assert_eq!(to_wide(tell(file)), tell_wide(file));
                assert_eq!(to_wide(offset(file)), offset_wide(file));
            }

            // `SEEK_END` is not supported, in both faces.
            assert_eq!(seek(file, 0, SEEK_END), -1);
            assert_eq!(seek_wide(file, 0, SEEK_END), -1);
            assert_eq!(close(file), Z_OK);
        }
    }

    #[test]
    fn a_seek_then_read_lands_on_the_expected_bytes() {
        let scratch = Scratch::new("seekread");
        let path = scratch.c_path();
        let payload: Vec<u8> = (0_u8..=255).cycle().take(2048).collect();
        write_file(&path, "wb", &payload);

        let file = open(&path, "rb");
        assert_eq!(seek(file, 1000, SEEK_SET), 1000);
        let mut out = [0_u8; 8];
        assert_eq!(read_bytes(file, &mut out), 8);
        assert_eq!(&out, &payload[1000..1008]);
        assert_eq!(tell(file), 1008);

        // Backwards, which only a read stream can do.
        assert_eq!(seek(file, -8, SEEK_CUR), 1000);
        assert_eq!(read_bytes(file, &mut out), 8);
        assert_eq!(&out, &payload[1000..1008]);

        assert_eq!(rewind(file), 0);
        assert_eq!(tell(file), 0);
        assert_eq!(close(file), Z_OK);
    }

    #[test]
    fn gzerror_reports_the_message_and_the_code_and_gzclearerr_discards_them() {
        let scratch = Scratch::new("error");
        let path = scratch.c_path();
        write_file(&path, "wb", b"payload");

        let file = open(&path, "rb");
        let mut code: c_int = 0x5eed;
        let message = error_with_code(file, &mut code);
        // No error yet: the empty string, which is not a null pointer.
        assert_eq!(
            message.as_deref(),
            Some(&b""[..]),
            "a live handle must have a message"
        );
        assert_eq!(code, Z_OK);

        // A null `errnum` is permitted and must not be written.
        assert!(error_message(file).is_some());

        // Reading a write stream is refused, and the refusal is reported through `gzerror`.
        let mut buf = [0_u8; 4];
        assert_eq!(read_bytes(file, &mut buf), 4);
        assert_eq!(eof(file), 0);
        clearerr(file);
        let mut code: c_int = 0x5eed;
        assert!(error_with_code(file, &mut code).is_some());
        assert_eq!(code, Z_OK);
        assert_eq!(close(file), Z_OK);

        // ★ An unusable handle answers with NULL, exactly as `gzlib.c` L517-L521 does.
        let mut code: c_int = 0x5eed;
        assert!(error_with_code(core::ptr::null_mut(), &mut code).is_none());
        assert_eq!(code, 0x5eed, "errnum must not be written for a null handle");
        // `gzclearerr` on a null handle is a no-op rather than a crash.
        clearerr(core::ptr::null_mut());
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
        let message = error_with_code(file, &mut code);
        // Z_DATA_ERROR.
        assert_eq!(code, -3, "a failed data check is a data error");
        let text = message.expect("a recorded error must have a message");
        assert!(!text.is_empty(), "a recorded error must have text");
        // The path is prefixed, exactly as `gz_error` concatenates it.
        assert!(
            text.starts_with(path.as_bytes()),
            "message {:?} is not prefixed with the path",
            String::from_utf8_lossy(&text)
        );
        // A second call reports the same text through a still-valid pointer.
        assert_eq!(error_message(file).as_deref(), Some(&text[..]));
        // And the deferred failure surfaces on the next read, as C defers it.
        assert_eq!(read_bytes(file, &mut buf), -1);
        // The close itself reports `Z_OK`: `gzclose_r` promotes only a lingering
        // `Z_BUF_ERROR` (`gzread.c` L662), and a corrupt header latches `Z_DATA_ERROR`,
        // which stays available through `gzerror` instead. The value is captured rather
        // than asserted because what this test is about is the message, not the code.
        let _closed = close(file);
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
        let message = error_with_code(file, &mut code);
        assert_eq!(code, Z_OK, "trailing garbage is not an error");
        assert_eq!(message.as_deref(), Some(&b""[..]));
        assert_eq!(eof(file), 1);
        assert_eq!(close(file), Z_OK);
    }

    #[test]
    fn the_printf_helpers_drive_the_cores_protocol() {
        // The shim is linked into this binary, but calling it needs a `va_list` that no Rust
        // caller can construct, so the protocol it implements is driven directly instead.
        let scratch = Scratch::new("printf");
        let path = scratch.c_path();
        let file = open(&path, "wb");

        let mut size: usize = 0;
        let (code, region) = printf_begin(file, &mut size);
        assert_eq!(code, Z_OK);
        let region = region.expect("a writable stream must be lent a region");
        // The default buffer is 8192, so the region is 8192 bytes and the documented cap is
        // one less.
        assert_eq!(size, 8192);
        // The last byte is the overflow sentinel and must arrive zeroed.
        assert_eq!(region.sentinel(), 0);

        // Format into it exactly as `vsnprintf` would: bytes, then a terminator, and report
        // the length it WOULD have written.
        let text = b"hello, world";
        region.write_formatted(text);
        assert_eq!(printf_commit(file, text.len()), as_count(text.len()));
        assert_eq!(close(file), Z_OK);
        assert_eq!(read_file(&path), text);
    }

    #[test]
    fn a_printf_result_that_does_not_fit_writes_nothing() {
        let scratch = Scratch::new("printfcap");
        let path = scratch.c_path();
        let file = open(&path, "wb");

        let mut size: usize = 0;
        let (code, region) = printf_begin(file, &mut size);
        assert_eq!(code, Z_OK);
        assert!(region.is_some(), "a writable stream must be lent a region");
        // `size - 1` characters is the documented cap; report exactly `size`, which is one
        // too many, and nothing must be written or counted.
        assert_eq!(printf_commit(file, size), 0);
        // A formatter that failed reports `(size_t)-1`, which folds into "did not fit".
        let (code, region) = printf_begin(file, &mut size);
        assert_eq!(code, Z_OK);
        assert!(region.is_some());
        assert_eq!(printf_commit(file, usize::MAX), 0);
        assert_eq!(close(file), Z_OK);
        assert!(read_file(&path).is_empty(), "nothing may have been written");
    }

    #[test]
    fn the_printf_helpers_refuse_an_unusable_request() {
        // A null handle, a null out-parameter, and a read stream: all refused with
        // `Z_STREAM_ERROR`, and none of them may touch the out-parameters.
        let mut region: *mut u8 = core::ptr::null_mut();
        let mut size: usize = 0xdead;
        let (code, lent) = printf_begin(core::ptr::null_mut(), &mut size);
        assert_eq!(code, Z_STREAM_ERROR);
        assert!(lent.is_none());
        assert_eq!(size, 0xdead);
        assert_eq!(printf_commit(core::ptr::null_mut(), 4), Z_STREAM_ERROR);

        let scratch = Scratch::new("printfbad");
        let path = scratch.c_path();
        write_file(&path, "wb", b"payload");
        let file = open(&path, "rb");
        assert_eq!(printf_begin_without_region(file, &mut size), Z_STREAM_ERROR);
        assert_eq!(printf_begin_without_size(file, &mut region), Z_STREAM_ERROR);
        // A read stream is not writable, so the core's own guard refuses it.
        let (code, lent) = printf_begin(file, &mut size);
        assert_eq!(code, Z_STREAM_ERROR);
        assert!(lent.is_none());
        assert_eq!(close(file), Z_OK);
    }

    #[test]
    fn every_entry_point_answers_a_null_handle_with_its_documented_value() {
        // Each value here is the one `zlib.h` documents for that family, and several
        // neighbours disagree -- which is the reason to assert them all in one place.
        let null: gzFile = core::ptr::null_mut();
        let mut byte = [0_u8; 4];

        assert_eq!(buffer(null, 8192), -1);
        assert_eq!(setparams(null, 6, 0), Z_STREAM_ERROR);
        assert_eq!(read_bytes(null, &mut byte), -1);
        assert_eq!(fread_items(&mut byte, 1, 4, null), 0);
        assert_eq!(write_bytes(null, &byte), 0);
        assert_eq!(fwrite_items(&byte, 1, 4, null), 0);
        assert_eq!(putc(null, 0), -1);
        let text = CString::new("text").unwrap();
        assert_eq!(puts(null, &text), -1);
        assert!(gets(null, &mut byte, 4).is_none());
        assert_eq!(getc(null), -1);
        assert_eq!(getc_compat(null), -1);
        assert_eq!(ungetc(0, null), -1);
        assert_eq!(flush(null, 0), Z_STREAM_ERROR);
        assert_eq!(seek(null, 0, SEEK_SET), -1);
        assert_eq!(seek_wide(null, 0, SEEK_SET), -1);
        assert_eq!(rewind(null), -1);
        assert_eq!(tell(null), -1);
        assert_eq!(tell_wide(null), -1);
        assert_eq!(offset(null), -1);
        assert_eq!(offset_wide(null), -1);
        assert_eq!(eof(null), 0);
        assert_eq!(direct(null), 0);
        assert_eq!(close(null), Z_STREAM_ERROR);
        assert_eq!(close_read(null), Z_STREAM_ERROR);
        assert_eq!(close_write(null), Z_STREAM_ERROR);
        assert!(error_message(null).is_none());
        clearerr(null);

        // A null path or mode is `Z_NULL`, not an empty name.
        let mode = CString::new("rb").unwrap();
        assert!(open_without_path(&mode).is_null());
        assert!(open64_without_path(&mode).is_null());
        assert!(open_without_mode(&text).is_null());
        assert!(dopen_without_mode(0).is_null());
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
        assert_eq!(close_write(file), Z_STREAM_ERROR);
        assert_eq!(getc(file), i32::from(b'p'));
        assert_eq!(close_read(file), Z_OK);

        // And symmetrically for a write stream and `gzclose_r` (`gzread.c` L648-L649).
        let file = open(&path, "wb");
        assert_eq!(close_read(file), Z_STREAM_ERROR);
        assert_eq!(putc(file, i32::from(b'x')), i32::from(b'x'));
        assert_eq!(close_write(file), Z_OK);
        assert_eq!(read_file(&path), b"x");
    }

    #[test]
    fn a_freshly_installed_state_with_no_direction_is_not_released_by_close() {
        // The `GZ_NONE` case: C's `gzclose` dispatches to `gzclose_w`, whose mode guard
        // refuses without freeing. Reproduced exactly, which is why the block has to be
        // released by hand afterwards.
        let file = install_state();
        assert_eq!(close(file), Z_STREAM_ERROR);
        assert_eq!(close_read(file), Z_STREAM_ERROR);
        assert_eq!(close_write(file), Z_STREAM_ERROR);
        // Still live, because nothing was freed.
        assert!(has_state(file));
        release_block(file);
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
        assert_eq!(close(file), -5);
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
        assert_eq!(close(file), Z_OK);
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
        assert_eq!(close(file), Z_OK);

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
        let file = dopen(fd, &mode);
        assert!(!file.is_null());
        assert!(write_bytes(file, payload) > 0);
        assert_eq!(close(file), Z_OK);

        // Read it back through another adopted descriptor.
        let handle = std::fs::File::open(&scratch.0).unwrap();
        let fd = handle.into_raw_fd();
        let mode = CString::new("rb").unwrap();
        let file = dopen(fd, &mode);
        assert!(!file.is_null());
        let mut out = vec![0_u8; 256];
        let got = read_bytes(file, &mut out);
        assert_eq!(count(got), payload.len());
        assert_eq!(&out[..payload.len()], payload);
        assert_eq!(close(file), Z_OK);

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
        assert!(dopen(-1, &mode).is_null());

        // An invalid mode must leave the descriptor untouched -- `zlib.h` L1415-L1416. The
        // descriptor is still usable afterwards, which is what proves it.
        let scratch = Scratch::new("dopenbad");
        std::fs::write(&scratch.0, b"contents").unwrap();
        let handle = std::fs::File::open(&scratch.0).unwrap();
        let raw = handle.as_raw_fd();
        let bad = CString::new("rb+").unwrap();
        assert!(dopen(raw, &bad).is_null());
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
        let file = dopen(fd, &mode);
        assert!(!file.is_null());
        assert_eq!(close(file), Z_OK);
    }

    /// `gzclose` on a descriptor the caller closed behind the library's back must report
    /// `Z_ERRNO`, exactly as C's `ret = close(state->fd); return ret ? Z_ERRNO : err;`
    /// does (`gzread.c` L665-L667).
    ///
    /// [`Descriptor::close`] calls `close(2)` and forwards its result, so this is now the
    /// same answer C reaches by the same route. An earlier revision had no descriptor of
    /// its own -- it held a `std::fs::File`, whose `Drop` discards `close`'s status -- and
    /// had to establish the condition indirectly with `fstat(2)` to avoid answering
    /// `Z_OK`; see `a_close_that_fails_is_reported_as_z_errno`, which covers the same
    /// ground through the public entry points.
    #[test]
    #[cfg(unix)]
    fn closing_a_descriptor_that_was_already_closed_reports_z_errno() {
        use std::os::fd::IntoRawFd as _;

        /// `Z_ERRNO`, the code C returns when `close(2)` fails.
        const Z_ERRNO: c_int = -1;

        let scratch = Scratch::new("dopenclosed");
        let path = scratch.c_path();
        write_file(&path, "wb", b"payload");

        let mode = CString::new("rb").unwrap();

        // A live descriptor must close cleanly: the probe may not invent a failure.
        let good = std::fs::File::open(scratch.0.as_path()).unwrap();
        let file = dopen(good.into_raw_fd(), &mode);
        assert!(!file.is_null());
        assert_eq!(close(file), Z_OK, "a valid descriptor");

        // Now the hostile case, built exactly as a C caller would build it: give up
        // ownership of the number, close it, and only then hand the stale number over.
        // Reclaiming and dropping is how the number is closed without a raw syscall; at
        // that instant it is still open, so the drop is sound and the only owner.
        //
        // The number is relocated out of the range `open(2)` allocates from first, because
        // otherwise freeing it here races every other test in this binary -- see
        // [`relocate_high`], which records what that race was measured to do.
        let raw = relocate_high(
            std::fs::File::open(scratch.0.as_path())
                .unwrap()
                .into_raw_fd(),
        );
        close_descriptor(raw);

        let file = dopen(raw, &mode);
        assert!(!file.is_null(), "gzdopen does not validate the descriptor");
        assert_eq!(
            close(file),
            Z_ERRNO,
            "a descriptor closed behind gzdopen's back"
        );
    }

    #[test]
    fn an_empty_payload_round_trips_and_reads_as_end_of_file() {
        let scratch = Scratch::new("empty");
        let path = scratch.c_path();
        let file = open(&path, "wb");
        assert_eq!(close(file), Z_OK);

        let file = open(&path, "rb");
        let mut out = [0_u8; 16];
        assert_eq!(read_bytes(file, &mut out), 0);
        assert_eq!(eof(file), 1);
        assert_eq!(getc(file), -1);
        assert_eq!(close(file), Z_OK);
    }

    #[test]
    fn a_zero_length_request_with_a_null_buffer_is_not_undefined() {
        // `core::slice::from_raw_parts(null, 0)` is undefined behaviour, so the zero-length
        // case is branched on. Under Miri this test is what proves it.
        let scratch = Scratch::new("zerolen");
        let path = scratch.c_path();
        write_file(&path, "wb", b"payload");

        let file = open(&path, "rb");
        assert_eq!(read_nothing_into_null(file), 0);
        assert_eq!(fread_nothing_into_null(file), 0);
        assert_eq!(close(file), Z_OK);

        let file = open(&path, "wb");
        assert_eq!(write_nothing_from_null(file), 0);
        assert_eq!(fwrite_nothing_from_null(file), 0);
        assert_eq!(close(file), Z_OK);
    }

    #[test]
    fn a_reader_and_a_writer_refuse_each_others_operations() {
        let scratch = Scratch::new("crossed");
        let path = scratch.c_path();
        write_file(&path, "wb", b"payload");

        // Writing to a read stream.
        let file = open(&path, "rb");
        let text = CString::new("nope").unwrap();
        assert_eq!(write_bytes(file, text.as_bytes()), 0);
        assert_eq!(putc(file, 0), -1);
        assert_eq!(puts(file, &text), -1);
        assert_eq!(flush(file, 0), Z_STREAM_ERROR);
        assert_eq!(setparams(file, 9, 0), Z_STREAM_ERROR);
        assert_eq!(close(file), Z_OK);

        // Reading from a write stream.
        let file = open(&path, "wb");
        let mut out = [0_u8; 4];
        assert_eq!(read_bytes(file, &mut out), -1);
        assert_eq!(getc(file), -1);
        assert_eq!(ungetc(0, file), -1);
        assert!(gets(file, &mut out, 4).is_none());
        assert_eq!(rewind(file), -1);
        // `gzeof` answers 0 for a write stream rather than failing.
        assert_eq!(eof(file), 0);
        assert_eq!(close(file), Z_OK);
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
        assert_eq!(close(file), Z_OK);

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
        assert_eq!(eof(file), 0);
        assert_eq!(close(file), Z_OK);
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

    // -----------------------------------------------------------------------
    // The platform descriptor: flags, errno, close status and adoption
    // -----------------------------------------------------------------------

    /// The `oflag` C computes, pinned against numbers measured from the C build.
    ///
    /// ★ These are not derived from the Rust constants -- that would make the test
    /// tautological. They were printed by a C program compiled against the same
    /// `<fcntl.h>` this target's libc binds, composing the flag word exactly as
    /// `gzlib.c` L228-L244 composes it:
    ///
    /// ```text
    /// mode=rb     oflag=0        mode=rbe    oflag=0x80000
    /// mode=wb     oflag=0x241    mode=rbN    oflag=0x800
    /// mode=ab     oflag=0x441    mode=wbeN   oflag=0x80a41
    /// mode=wbx    oflag=0x2c1    mode=abx9   oflag=0x4c1
    /// ```
    ///
    /// with `O_LARGEFILE` measured as **defined and zero** on 64-bit Linux and `O_BINARY`
    /// undefined. The case that matters most is the first: a read-only open composes
    /// exactly zero, so nothing -- in particular not `O_CLOEXEC`, which `std::fs::File`
    /// always adds -- may creep in.
    #[cfg(all(target_os = "linux", target_pointer_width = "64"))]
    #[test]
    fn the_open_flags_match_the_numbers_the_c_build_composes() {
        let cases: [(&str, c_int); 8] = [
            ("rb", 0),
            ("wb", 0x241),
            ("ab", 0x441),
            ("wbx", 0x2c1),
            ("rbe", 0x80000),
            ("rbN", 0x800),
            ("wbeN", 0x8_0a41),
            ("abx9", 0x4c1),
        ];
        for (mode, expected) in cases {
            let spec = GzOpenSpec::parse(mode.as_bytes()).unwrap();
            assert_eq!(
                super::open_flags(&spec),
                expected,
                "oflag for mode {mode:?}"
            );
        }
    }

    /// A failed open leaves the platform's `errno` in place, which `zlib.h` L1394-L1397
    /// promises: "errno can be checked to determine if the reason gzopen failed was that
    /// the file could not be opened".
    ///
    /// The number itself is read through `io::Error`, which is `errno`, and compared by
    /// kind rather than by value so the test names no platform constant.
    #[test]
    fn a_failed_open_leaves_errno_for_the_caller() {
        let missing = CString::new("/nonexistent-directory-for-zlib-rs/inner/file.gz").unwrap();
        let mode = CString::new("rb").unwrap();

        let file = open_raw(&missing, &mode);
        assert!(file.is_null(), "opening a path under a missing directory");
        assert_eq!(
            std::io::Error::last_os_error().kind(),
            std::io::ErrorKind::NotFound,
            "errno must still describe the open that failed"
        );

        // The other half of the promise: a refusal that is *not* the file system leaves no
        // claim on errno, because no syscall was made. An unusable mode is rejected before
        // anything is opened -- which is also what keeps it from creating the file.
        let scratch = Scratch::new("errno_mode");
        let bad_mode = CString::new("wb+").unwrap();
        let file = open_raw(&scratch.c_path(), &bad_mode);
        assert!(file.is_null(), "`+` is rejected (gzlib.c L128-L131)");
        assert!(
            !scratch.0.exists(),
            "a rejected mode string must not create the file"
        );
    }

    /// `close(2)`'s own result reaches the caller as `Z_ERRNO`.
    ///
    /// ★ This is the property a handle built on `std::fs::File` cannot offer, because
    /// `Drop for File` discards `close`'s status. C makes it observable in both close
    /// paths -- `gzclose_r` returns `ret ? Z_ERRNO : err` (`gzread.c` L665-L666) and
    /// `gzclose_w` sets `ret = Z_ERRNO` when `close` reports `-1` (`gzwrite.c` L696-L697)
    /// -- so a port that reported `Z_OK` for a failed close would be telling the caller
    /// its data was safely on disk when it may not be.
    ///
    /// The failure is induced the one way a test can induce it: the descriptor is closed
    /// behind the stream's back, so the library's own `close` answers `EBADF`.
    #[cfg(unix)]
    #[test]
    fn a_close_that_fails_is_reported_as_z_errno() {
        let scratch = Scratch::new("close_errno");
        let path = scratch.c_path();
        let mode = CString::new("rb").unwrap();

        // A real gzip file, so that the read stream is fully set up.
        write_file(&path, "wb", b"payload for a close that will fail");

        let fd = sys_open_read(&path);
        assert!(fd >= 0, "opening the scratch file directly");
        // Relocated before `gzdopen` adopts it, because the number is freed below and
        // `open(2)` hands out the lowest free one; see [`relocate_high`].
        let fd = relocate_high(fd);
        let file = dopen(fd, &mode);
        assert!(!file.is_null(), "gzdopen on a live descriptor");

        // Take the descriptor out from under the stream. Nothing else can make a
        // well-formed `close` fail.
        assert_eq!(sys_close(fd), 0);
        assert_eq!(
            close(file),
            ReturnCode::ERRNO.as_i32(),
            "a failed close must be Z_ERRNO, not Z_OK"
        );
    }

    /// A descriptor adopted by [`gzdopen`] receives exactly the `fcntl` call C makes --
    /// `gzlib.c` L253-L262 -- and therefore exactly C's outcome.
    ///
    /// ★ The expectation here was **measured, not reasoned about**, and the measurement
    /// changed the implementation. C's descriptor-flag call is
    /// `fcntl(fd, F_SETFD, fcntl(fd, F_GETFD) | O_CLOEXEC)`, but `F_SETFD` defines only
    /// `FD_CLOEXEC` (`0x1`) and `O_CLOEXEC` is `0x80000`, so the bit C sets is silently
    /// ignored. Built against the C library, the probe prints:
    ///
    /// ```text
    /// raw  F_SETFD|O_CLOEXEC ret=0 errno=0 getfd=0x0 fd_cloexec=0
    /// C    gzdopen(fd,"rbe")            getfd=0x0 fd_cloexec=0
    /// C    gzdopen(fd,"rb")             getfd=0x0 fd_cloexec=0
    /// ```
    ///
    /// So in the reference implementation `e` does nothing to an adopted descriptor on
    /// Linux, and this asserts that it does nothing here either. An earlier revision
    /// substituted `FD_CLOEXEC`, which made `e` work and made the two libraries differ; the
    /// behaviour of the reference is the specification, so the substitution was reverted.
    /// [`apply_cloexec`] carries the same measurement.
    ///
    /// The negative case is load-bearing for a second reason: without `e` the descriptor
    /// must keep whatever it arrived with. Anything built on `std::fs::File` sets
    /// `O_CLOEXEC` on every descriptor it owns, which would change the behaviour of a
    /// caller that then `exec`s.
    #[cfg(unix)]
    #[test]
    // Miri implements `fcntl` for `F_GETFD` but not for `F_SETFD`: it stops with
    // "unsupported operation: fcntl: unsupported command 0x2", which is the command C
    // itself issues at `gzlib.c` L259-L260. The call is what this test exists to exercise,
    // so there is nothing to salvage by running it there; the other five descriptor tests
    // in this module do run under Miri and cover `open`, `read`, `write`, `close`, `lseek`
    // and `errno`.
    #[cfg_attr(miri, ignore = "Miri does not implement fcntl(F_SETFD)")]
    fn an_adopted_descriptor_receives_the_same_fcntl_c_makes() {
        let scratch = Scratch::new("adopt_flags");
        let path = scratch.c_path();
        write_file(&path, "wb", b"flag probe");

        for mode in ["rb", "rbe"] {
            let fd = sys_open_read(&path);
            assert!(fd >= 0);
            // On a real host this reads `0`: a caller's own `open` without `O_CLOEXEC`
            // produces an inheritable descriptor. It is *recorded* rather than asserted
            // because it is a property of the environment, not of this library -- measured
            // under Miri, whose `open` shim reports `FD_CLOEXEC` set, and asserting the
            // host's default would make the test fail there for no reason. What must hold
            // in every environment is that `gzdopen` does not change it.
            let before = sys_descriptor_flags(fd);
            assert!(before != -1, "the descriptor is open, for mode {mode:?}");

            let mode_c = CString::new(mode).unwrap();
            let file = dopen(fd, &mode_c);
            assert!(!file.is_null(), "gzdopen({mode:?})");

            let after = sys_descriptor_flags(fd);
            assert!(after != -1, "the adopted descriptor is still open");
            assert_eq!(
                after, before,
                "the descriptor flags C leaves unchanged must be unchanged here too, \
                 for mode {mode:?}"
            );

            assert_eq!(close(file), Z_OK);
        }
    }

    /// A `gzdopen` that fails leaves the caller's descriptor open and usable.
    ///
    /// `zlib.h` L1414-L1416: "The duplicated descriptor should be saved to avoid a leak,
    /// since gzdopen does not close fd if it fails." The mode string is the one failure a
    /// test can force, and the property it demonstrates is the same one the reservation
    /// ordering protects for an allocation failure: nothing that can fail happens after the
    /// descriptor has been taken.
    #[cfg(unix)]
    #[test]
    fn a_failed_gzdopen_does_not_close_the_callers_descriptor() {
        let scratch = Scratch::new("dopen_fail");
        let path = scratch.c_path();
        write_file(&path, "wb", b"still mine");

        let fd = sys_open_read(&path);
        assert!(fd >= 0);

        let bad_mode = CString::new("rb+").unwrap();
        let file = dopen(fd, &bad_mode);
        assert!(file.is_null(), "`+` is rejected");

        // Still open: `fcntl` answers, and a read succeeds.
        assert!(
            sys_descriptor_flags(fd) != -1,
            "the descriptor must not have been closed"
        );
        let mut byte = [0_u8; 1];
        let got = sys_read_one(fd, &mut byte);
        assert_eq!(got, 1, "and it must still be readable");
        assert_eq!(sys_close(fd), 0);
    }

    /// `gzdopen` is available on every platform, and a stream opened on a descriptor
    /// round-trips.
    ///
    /// The behavioural half of what used to be "gzdopen returns NULL off Unix": the
    /// descriptor path no longer goes through `std::fs::File`, so a Windows CRT descriptor
    /// is served by the same `Descriptor` as a POSIX one.
    #[cfg(unix)]
    #[test]
    fn a_stream_opened_on_a_descriptor_round_trips() {
        let scratch = Scratch::new("dopen_roundtrip");
        let path = scratch.c_path();
        let payload = b"written through an adopted descriptor";

        let write_mode = CString::new("wb").unwrap();
        let fd = sys_open_write(&path);
        assert!(fd >= 0);
        let file = dopen(fd, &write_mode);
        assert!(!file.is_null());
        assert_eq!(write_bytes(file, payload), as_count(payload.len()));
        assert_eq!(close(file), Z_OK);

        let read_mode = CString::new("rb").unwrap();
        let fd = sys_open_read(&path);
        assert!(fd >= 0);
        let file = dopen(fd, &read_mode);
        assert!(!file.is_null());
        let mut out = vec![0_u8; payload.len()];
        assert_eq!(read_bytes(file, &mut out), as_count(payload.len()));
        assert_eq!(out, payload);
        assert_eq!(close(file), Z_OK);
    }

    /// A closed [`Descriptor`] refuses further work and closes only once.
    ///
    /// The idempotence [`GzHandle`]'s contract requires, exercised directly because the
    /// path that depends on it -- a close path closing explicitly, then `GzState::teardown`
    /// closing again as the state is dropped -- cannot be observed from outside.
    #[cfg(unix)]
    #[test]
    fn a_descriptor_closes_once_and_then_refuses() {
        use zlib_rs::gz::GzHandle;

        let scratch = Scratch::new("descriptor_once");
        let path = scratch.c_path();
        write_file(&path, "wb", b"one close only");

        let fd = sys_open_read(&path);
        assert!(fd >= 0);
        let mut descriptor = Descriptor::new(fd);
        assert!(descriptor.close().is_ok(), "the first close succeeds");
        // A second close must not reach the platform: were it to, it would close whatever
        // descriptor the number has since been reused for.
        assert!(descriptor.close().is_ok(), "and is idempotent");
        // Every other operation now reports the closed handle rather than acting on it.
        assert!(descriptor.read(&mut [0_u8; 1]).is_err());
        assert!(descriptor.write(&[0_u8; 1]).is_err());
        assert!(descriptor.seek(0, zlib_rs::gz::GzSeekFrom::Start).is_err());
    }

    /// `gzerror` returns a pointer into the stream's own storage, allocating nothing.
    ///
    /// ★ Two consecutive calls returning the *same* pointer is what makes this a test of
    /// the fix rather than of the message: it can only hold if the text was built when the
    /// error was recorded, which is C's arrangement (`gzlib.c` L576-L584 allocates, L527
    /// merely returns). The earlier revision built a terminated copy inside `gzerror`, so
    /// the query itself could fail under memory pressure -- on the one call a caller makes
    /// because something has already gone wrong.
    #[test]
    fn gzerror_returns_stored_text_without_allocating() {
        let scratch = Scratch::new("gzerror_stored");
        let path = scratch.c_path();

        // Not a gzip file, and not the empty file that would be read transparently.
        std::fs::write(&scratch.0, b"this is not compressed data at all").unwrap();
        let file = open(&path, "rb");

        let mut out = [0_u8; 64];
        // Transparent reads succeed, so force the error the state machine reports: a
        // stream opened with `G` must find a gzip header, and this file has none.
        assert_eq!(close(file), Z_OK);
        let file = open(&path, "rbG");
        assert!(read_bytes(file, &mut out) < 0, "a non-gzip file with `G`");

        let mut code = 0;
        let first = error_ptr_with_code(file, &mut code);
        assert!(!first.is_null());
        assert_eq!(code, ReturnCode::DATA_ERROR.as_i32());
        let text = message_text(first).expect("a non-null message");
        // C's `"%s%s%s", path, ": ", msg`, so the stored message begins with the path.
        assert!(
            text.starts_with(scratch.0.as_os_str().as_encoded_bytes()),
            "the message is prefixed with the path: {:?}",
            String::from_utf8_lossy(&text)
        );

        let second = error_ptr(file);
        assert_eq!(
            first, second,
            "the same stored bytes, so nothing was built at query time"
        );
        assert_eq!(message_text(second).as_deref(), Some(&text[..]));

        assert_eq!(close(file), Z_OK);
    }

    /// A stream that never fails never stores a message, and `gzerror` answers the empty
    /// string rather than null -- `gzlib.c` L527.
    #[test]
    fn gzerror_answers_the_empty_string_when_nothing_went_wrong() {
        let scratch = Scratch::new("gzerror_empty");
        let path = scratch.c_path();
        let file = open(&path, "wb");

        let mut code = -1;
        let text = error_with_code(file, &mut code);
        assert_eq!(
            text.as_deref(),
            Some(&b""[..]),
            "a usable handle answers the empty string, never null"
        );
        assert_eq!(code, Z_OK);

        assert_eq!(close(file), Z_OK);
    }
}
