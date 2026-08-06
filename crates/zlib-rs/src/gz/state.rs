//! The internal `gzFile` state, and the frozen memory layout its first 24 bytes owe to a macro
//! compiled into somebody else's object code.
//!
//! This is the port of `gz_state` (`gzguts.h` L169-L204) together with the caller-visible
//! `struct gzFile_s` prefix it embeds (`zlib.h` L1956-L1960). Every other module of the `gzFile`
//! layer -- `open.rs`, `read.rs`, `write.rs`, `close.rs`, `printf.rs` and the shared plumbing in
//! `mod.rs` -- operates on the [`GzState`] defined here, exactly as all four `gz*.c` translation
//! units operate on the one `gz_state` that `gzguts.h` declares.
//!
//! # Why the layout of the first 24 bytes may never change
//!
//! `gzgetc` is not only a function. It is also a macro (`zlib.h` L1967-L1968):
//!
//! ```c
//! #define gzgetc(g) \
//!       ((g)->have ? ((g)->have--, (g)->pos++, *((g)->next)++) : (gzgetc)(g))
//! ```
//!
//! Read that carefully, because it is the single highest-risk constraint in this port. The
//! expression loads `g->have`, decrements it, increments `g->pos`, dereferences `g->next` and
//! post-increments it -- and all of that field arithmetic is compiled into the **caller's** object
//! code, at the byte offsets that the caller's copy of `zlib.h` implied when the caller was built.
//! This port cannot recompile those callers; that is the whole point of preserving the ABI. So the
//! three fields must sit at exactly the offsets they have always sat at, at the very start of
//! whatever `gzFile` points to.
//!
//! `gzguts.h` L170-L175 arranges for that by embedding `struct gzFile_s x;` as `gz_state`'s first
//! member, under the comment "exposed contents for `gzgetc()` macro" and with `"x" for exposed`.
//! [`GzState`] reproduces the arrangement: [`GzState`] itself is `#[repr(C)]`, its first declared
//! field is [`GzState::x`], and that field's type [`GzFileExposed`] is `#[repr(C)]` as well.
//!
//! **Both attributes are load-bearing, and the outer one is the one that is easy to forget.** A
//! `repr(Rust)` struct may reorder its fields however the compiler pleases, so a `repr(Rust)`
//! [`GzState`] whose first *declared* field happened to be a `#[repr(C)]` prefix would guarantee
//! nothing at all about where that prefix actually lands. The macro would then decrement some
//! unrelated field as though it were `have`, and dereference some unrelated field as though it were
//! a `*mut u8`. That is silent memory corruption in every caller that uses `gzgetc`, on a code path
//! this crate never executes and therefore can never diagnose. The two attributes are one contract,
//! and the assertions below hold the contract to its measured numbers.
//!
//! Measured on `x86_64-unknown-linux-gnu` from the unmodified headers in this tree:
//! `sizeof(struct gzFile_s)` is 24, with `have` at 0, `next` at 8 and `pos` at 16. The four-byte
//! gap between `have` and `next` is ordinary alignment padding, not a field; it is reproduced by
//! letting `repr(C)` apply C's alignment rules and never by declaring an explicit filler.
//!
//! # The pointer/index split
//!
//! `x.next` is a raw pointer, and the caller's macro advances it. This crate forbids the escape
//! hatch that reading through a raw pointer would require, so the resolution -- and the contract
//! every other module of this layer depends on -- is to separate representation from access:
//!
//! * the **representation** keeps the raw pointer, because the ABI demands a pointer at offset 8;
//! * the **safe code paths** use [`GzState::out_pos`], an integer index into the allocator-owned
//!   output buffer, because an index can be bounds-checked.
//!
//! Both halves of the conversion are ordinary safe operations, which is precisely what makes the
//! split possible. Producing the pointer is `slice::as_mut_ptr` followed by
//! `pointer::wrapping_add`; recovering the index is a `pointer`-to-`usize` cast and a
//! `usize::wrapping_sub`. Declaring, producing, comparing and casting a raw pointer are all safe;
//! only reading or writing *through* one is not, and nothing here does that.
//! [`GzState::refresh_exposed`] performs the first direction, [`GzState::resync_from_exposed`] the
//! second, and the invariant they maintain is that `x.next` always equals
//! `output.as_ptr() + out_pos`, or is null when there is no output buffer.
//!
//! ## The contract for callers
//!
//! **Every public `gz*` entry point must call [`GzState::resync_from_exposed`] immediately on
//! entry and [`GzState::refresh_exposed`] immediately before it returns.** The caller's macro
//! mutates `have`, `pos` and `next` between our calls without telling us, so `out_pos` is stale on
//! entry and `x.next` is stale on exit. Skipping either half is a correctness bug that only shows
//! up in a program that mixes `gzgetc` with the other entry points -- which is exactly what
//! `test/example.c` does.
//!
//! ## Four facts that make the split safe
//!
//! 1. **`have == 0` means `next` is not consulted.** The macro's condition is `(g)->have`, so with
//!    `have` at zero it always takes the `(gzgetc)(g)` function branch and never dereferences
//!    `next` (`zlib.h` L1967-L1968). A null or stale `next` is therefore legitimate whenever
//!    `have` is zero, and both conversion routines accept it.
//! 2. **Decompressing straight into a user buffer transiently moves C's `next` outside `out`.**
//!    `gz_decomp` recomputes the pair as `x.have = had - avail_out; x.next = next_out - x.have`
//!    (`gzread.c` L228-L229), and on the large-read path `next_out` is the *user's* buffer
//!    (`gzread.c` L372-L377), so C's `x.next` legitimately points outside `out` for an instant.
//!    `gz_read` then immediately consumes the count and clears the flag with `n = state->x.have;
//!    state->x.have = 0;` (`gzread.c` L376-L377). This port models that instant with
//!    [`GzState::clear_have`], which sets `have` to zero and nulls `next` together, so `x.next`
//!    never has to denote a location outside the output buffer and
//!    [`GzState::resync_from_exposed`] can treat an out-of-range pointer as corruption.
//! 3. **On the write path `next` is a flush cursor, not a delivery cursor.** `gz_init` seeds it
//!    with `state->x.next = strm->next_out` (`gzwrite.c` L54) and `gz_comp` drains the buffer with
//!    `while (strm->next_out > state->x.next) { write(fd, state->x.next, put); state->x.next +=
//!    writ; }`, resetting it to `state->out` when the buffer is recycled (`gzwrite.c` L110-L127).
//!    `x.have` is never set on the write path at all, which is what keeps `gzgetc` from delivering
//!    bytes out of a write stream.
//! 4. **`gzungetc` parks a pushed byte at the very end of the output buffer.** With the buffer
//!    empty it sets `x.next = state->out + (state->size << 1) - 1` and `x.have = 1`
//!    (`gzread.c` L533-L536), so `out_pos` must be able to hold `2 * size - 1`.
//!    [`GzState::ungetc_park_index`] computes it, and the output buffer is `2 * size` bytes long on
//!    the read path, so the index is always in range.
//!
//! # Freshly allocated buffers are not zeroed
//!
//! The `gzFile` layer allocates with plain `malloc` in C (`gzlib.c` L100 and L206,
//! `gzread.c` L99-L100, `gzwrite.c` L16 and L25) and the default `z_stream` hooks reduce to
//! `malloc` as well, because `zcalloc` takes the `malloc` branch whenever `sizeof(uInt) > 2`
//! (`zutil.c` L299-L303) -- which is every target this port supports. Nothing in this module may
//! assume `in` or `out` arrives zeroed. `test/infcover.c` L84-L87 fills every block it hands out
//! with `0xa5` for exactly that reason, and its `mem_done` reports leaks, non-LIFO frees and rogue
//! frees, so a teardown that returns memory to the wrong allocator or in the wrong order is
//! detected rather than tolerated.
//!
//! # Coordination with the rest of the workspace
//!
//! * `crates/libz-rs-sys/src/types.rs` independently declares a `#[repr(C)] gzFile_s`, and
//!   `crates/libz-rs-sys/src/layout_assertions.rs` asserts the same 24-byte size and the same
//!   0/8/16 offsets. This module is the **core-side half of one shared contract**: the two
//!   declarations describe the same 24 bytes and must agree exactly. If either side changes, both
//!   sides' assertions must be revisited together.
//! * For `crates/libz-rs-sys/src/gz.rs`: `gzFile` is an opaque `*mut GzState`. Obtain the state,
//!   call [`GzState::resync_from_exposed`] before doing anything else, do the work, then call
//!   [`GzState::refresh_exposed`] before handing control back to C. Nothing in this module is
//!   `#[no_mangle]` or `extern "C"`; the exported surface belongs exclusively to that crate. The
//!   internal helpers that correspond to C's `ZLIB_INTERNAL` symbols -- `gz_error` and
//!   `gz_intmax` -- are `pub(crate)` in `mod.rs` and are therefore unreachable from outside the
//!   core, which is what `zlib.map`'s `local:` block requires of them.
//! * `crates/zlib-rs/tests/` hosts the integration suite for this layer; this module carries only
//!   the inline unit tests at the end of the file.
//! * This module is reached through `#[cfg(feature = "std")] mod gz;` in the crate root, so no
//!   per-item feature gate appears below and `--no-default-features` compiles the whole subtree
//!   out. It needs only `core` and `alloc`, never `std`: file access arrives injected through
//!   [`GzHandle`], which is what keeps the crate's `std`-free default build honest.

// `GzState`, `GzStream` and friends "repeat" this module's name because they are the ports of
// `gz_state` and of the `z_stream` embedded in it, and those names are the ones a maintainer
// comparing this file against `gzguts.h` will be looking for.
#![allow(clippy::module_name_repetitions)]

use core::ffi::c_uint;
use core::fmt;

use alloc::boxed::Box;
use alloc::vec::Vec;

use crate::allocate::{Allocator, Buffer};
use crate::config::{Strategy, Z_DEFAULT_COMPRESSION, Z_DEFAULT_STRATEGY};
use crate::deflate::state::DeflateState;
use crate::error::ReturnCode;
use crate::inflate::InflateState;

// -----------------------------------------------------------------------------
//  Offsets and sizes
// -----------------------------------------------------------------------------

/// The signed file offset type of the `gzFile` layer: this port's stand-in for `z_off64_t`.
///
/// `zconf.h` L522-L531 resolves `z_off64_t` differently per platform -- `off64_t` under
/// `Z_LARGE64`, `long long` under MinGW, `__int64` under MSVC, `offset_t` under DJGPP, and
/// otherwise whatever `z_off_t` resolved to, which itself defaults to `long long`
/// (`zconf.h` L518-L520). Every one of those spellings is a signed 64-bit integer on every Tier-1
/// target this port supports, which is why a fixed width is correct *here*, in the core, where no
/// C caller can see the choice.
///
/// **No fixed width may ever be hardcoded for `z_off_t`, `z_off64_t` or `z_size_t` in
/// caller-facing code.** Those are the platform-dependent spellings of the public ABI, and the
/// narrowing between them is deliberate: `gzseek` returns `z_off_t` by calling `gzseek64` and
/// checking `ret == (z_off_t)ret` before narrowing (`gzlib.c` L438-L442), and `gztell` and
/// `gzoffset` do the same (`gzlib.c` L461-L465, L490-L495). That narrowing, and the exact
/// platform types it narrows between, belong to `crates/libz-rs-sys/src/types.rs`, which owns
/// `z_off_t`/`z_off64_t` and must `const`-assert
/// `size_of::<z_off64_t>() == size_of::<ZOff64>()` so that a platform where the two disagree
/// fails to build rather than silently truncating a file position.
pub type ZOff64 = i64;

/// The caller-visible prefix of the `gzFile` state: the port of `struct gzFile_s`.
///
/// Ported verbatim from `zlib.h` L1956-L1960:
///
/// ```c
/// struct gzFile_s {
///     unsigned have;
///     unsigned char *next;
///     z_off64_t pos;
/// };
/// ```
///
/// The field order is the ABI and may not be changed. `core::ffi::c_uint` is the portable mirror
/// of C's `unsigned` and `*mut u8` the mirror of `unsigned char *`; using either a fixed-width
/// integer for `have` or a slice for `next` would change the layout the `gzgetc` macro was
/// compiled against. See this module's documentation for why that would corrupt caller memory
/// rather than merely misbehave.
///
/// `next` is *derived* state. Its only writers are [`GzState::refresh_exposed`], which recomputes
/// it from [`GzState::out_pos`], and [`GzState::clear_have`], which nulls it. Read it freely;
/// never assign to it directly, or the index and the pointer will disagree.
#[derive(Debug)]
#[repr(C)]
pub struct GzFileExposed {
    /// Number of bytes available for delivery at [`GzFileExposed::next`].
    ///
    /// `gzguts.h` L173 documents it as "number of bytes available at `x.next`". The caller's
    /// `gzgetc` macro decrements it, and `gz_error` clears it to zero on a fatal error precisely so
    /// that the macro stops taking its fast path (`gzlib.c` L564-L565).
    pub have: c_uint,
    /// The next output byte to deliver when reading, or the next byte to flush when writing.
    ///
    /// `gzguts.h` L174 documents it as "next output data to deliver or write". It points into the
    /// output buffer, or is null when no output buffer is allocated. It is derived from
    /// [`GzState::out_pos`]; see the type-level note above.
    pub next: *mut u8,
    /// The current position in the *uncompressed* data stream.
    ///
    /// `gzguts.h` L175 documents it as "current position in uncompressed data". The caller's
    /// `gzgetc` macro increments it, `gzungetc` decrements it (`gzread.c` L537), and `gztell`
    /// reports it plus any pending seek (`gzlib.c` L456-L457).
    pub pos: ZOff64,
}

impl GzFileExposed {
    /// The prefix as `gz_open` leaves it: nothing available, no buffer, position zero.
    ///
    /// `gz_open` reaches this state indirectly -- it allocates the structure and then calls
    /// `gz_reset`, which sets `state->x.have = 0` (`gzlib.c` L70) and `state->x.pos = 0`
    /// (`gzlib.c` L82). `next` is left null because no buffer exists until `gz_look` or `gz_init`
    /// allocates one, and fact 1 in this module's documentation explains why a null `next` is
    /// harmless while `have` is zero.
    pub const EMPTY: Self = Self {
        have: 0,
        next: core::ptr::null_mut(),
        pos: 0,
    };
}

// The measured contract, asserted at compile time so that ABI drift is a build failure rather than
// a corrupted caller. The absolute numbers are gated on a 64-bit pointer because the layout is
// genuinely different elsewhere: on a 32-bit target the prefix collapses to `have` at 0, `next` at
// 4 and `pos` at 8 for a total of 16 bytes, with no padding after `have` at all.
#[cfg(target_pointer_width = "64")]
mod layout_64 {
    use super::{GzFileExposed, GzState, ZOff64};
    use crate::allocate::GlobalAllocator;

    const _: () = assert!(size_of::<GzFileExposed>() == 24);
    const _: () = assert!(core::mem::offset_of!(GzFileExposed, have) == 0);
    const _: () = assert!(core::mem::offset_of!(GzFileExposed, next) == 8);
    const _: () = assert!(core::mem::offset_of!(GzFileExposed, pos) == 16);
    const _: () = assert!(size_of::<ZOff64>() == 8);

    // The outer struct's contribution to the same contract. `#[repr(C)]` places the first declared
    // field at offset zero for *every* instantiation of a generic type, so proving it for one
    // concrete instantiation proves it generally; `GlobalAllocator` is used because it is a
    // zero-sized allocator that needs no setup.
    const _: () = assert!(core::mem::offset_of!(GzState<'static, GlobalAllocator>, x) == 0);
}

// The general case, asserted unconditionally so that a target this port has not been measured on
// still cannot silently reorder or shrink the prefix. These hold on any target `repr(C)` supports:
// the first field starts at zero, each later field starts at or after the end of its predecessor,
// and the whole prefix is at least as large as its last field's end.
const _: () = assert!(core::mem::offset_of!(GzFileExposed, have) == 0);
const _: () = assert!(core::mem::offset_of!(GzFileExposed, next) >= size_of::<c_uint>());
const _: () = assert!(
    core::mem::offset_of!(GzFileExposed, pos)
        >= core::mem::offset_of!(GzFileExposed, next) + size_of::<*mut u8>()
);
const _: () = assert!(
    size_of::<GzFileExposed>() >= core::mem::offset_of!(GzFileExposed, pos) + size_of::<ZOff64>()
);

// -----------------------------------------------------------------------------
//  Constants
// -----------------------------------------------------------------------------

/// Default size of each working buffer, in bytes.
///
/// Ported from `gzguts.h` L156. The comment above it at `gzguts.h` L154-L155 states the constraint
/// that gives the value its shape: it is the "default i/o buffer size -- double this for output
/// when reading (this and twice this must be able to fit in an unsigned type)". Both buffers are
/// doubled somewhere -- the output buffer when reading (`gzread.c` L100) and the input buffer when
/// writing (`gzwrite.c` L16) -- so `2 * GZBUFSIZE` must remain representable in a `c_uint`.
/// `gzbuffer` enforces the same property for a caller-chosen size by rejecting any `size` for which
/// `(size << 1) < size` (`gzlib.c` L337-L338).
pub const GZBUFSIZE: c_uint = 8192;

/// Mode value for a `gzFile` that has not yet been given a direction.
///
/// Ported from `gzguts.h` L159. `gz_open` starts here (`gzlib.c` L109) and rejects the open if the
/// mode string never selected `r`, `w` or `a` (`gzlib.c` L174-L177).
pub const GZ_NONE: i32 = 0;

/// Mode value for a `gzFile` opened for reading.
///
/// Ported from `gzguts.h` L160. The value is deliberately an arbitrary odd number rather than a
/// small ordinal: `gzguts.h` L158 introduces the mode constants as "gzip modes, also provide a
/// little integrity check on the passed structure", and every entry point tests
/// `state->mode != GZ_READ && state->mode != GZ_WRITE` before trusting the pointer it was handed
/// (for example `gzlib.c` L335-L336). Transcribed exactly; changing it would weaken that check.
pub const GZ_READ: i32 = 7247;

/// Mode value for a `gzFile` opened for writing.
///
/// Ported from `gzguts.h` L161. See [`GZ_READ`] for why the value is what it is.
pub const GZ_WRITE: i32 = 31153;

/// Transient mode value for a `gzFile` opened for appending.
///
/// Ported from `gzguts.h` L162, whose comment records the lifetime of the value: "mode set to
/// `GZ_WRITE` after the file is opened". `gz_open` seeks to the end so that `gzoffset` is correct
/// and then overwrites the mode with [`GZ_WRITE`] "to simplify later checks"
/// (`gzlib.c` L269-L272), so this value is never observed by any entry point.
pub const GZ_APPEND: i32 = 1;

/// Value of `how` meaning "a gzip header has not been looked for yet".
///
/// Ported from `gzguts.h` L165. `gz_reset` installs it for a read stream (`gzlib.c` L74) and
/// `gz_look` replaces it once it has decided between copying and decompressing.
pub const LOOK: i32 = 0;

/// Value of `how` meaning "copy the input through without decompressing".
///
/// Ported from `gzguts.h` L166. This is the transparent path `gzdirect` reports.
pub const COPY: i32 = 1;

/// Value of `how` meaning "decompress a gzip stream".
///
/// Ported from `gzguts.h` L167.
pub const GZIP: i32 = 2;

// -----------------------------------------------------------------------------
//  Typed views over the C integers
// -----------------------------------------------------------------------------

/// The direction a `gzFile` was opened in: an exhaustive view over [`GzState::mode`].
///
/// The four values of `gzguts.h` L159-L162, with their C discriminants, so that a `match` on a
/// recognised mode is exhaustive and a new direction cannot be added without every site being
/// revisited. The stored field remains an `i32` -- see [`GzState::mode`] for why -- and this type is
/// how the rest of the layer reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum GzMode {
    /// [`GZ_NONE`]: no direction chosen yet.
    None = GZ_NONE,
    /// [`GZ_APPEND`]: opened for appending, seen only inside `gz_open` (`gzlib.c` L269-L272).
    Append = GZ_APPEND,
    /// [`GZ_READ`]: opened for reading.
    Read = GZ_READ,
    /// [`GZ_WRITE`]: opened for writing.
    Write = GZ_WRITE,
}

impl GzMode {
    /// Returns the C `int` that names this mode.
    #[must_use]
    pub const fn as_raw(self) -> i32 {
        self as i32
    }

    /// Recognises a stored mode value, returning [`None`] for anything else.
    ///
    /// The [`None`] case is the whole reason the mode constants are odd numbers: `gzguts.h` L158
    /// describes them as "a little integrity check on the passed structure", and a value outside
    /// this set means the pointer handed in was not a `gzFile` at all. Every C entry point performs
    /// the equivalent test, for instance `gzbuffer` at `gzlib.c` L335-L336.
    #[must_use]
    pub const fn from_raw(mode: i32) -> Option<Self> {
        match mode {
            GZ_NONE => Some(Self::None),
            GZ_APPEND => Some(Self::Append),
            GZ_READ => Some(Self::Read),
            GZ_WRITE => Some(Self::Write),
            _ => None,
        }
    }
}

/// What the read path is currently doing with its input: an exhaustive view over [`GzState::how`].
///
/// The three values of `gzguts.h` L165-L167, with their C discriminants. `gzguts.h` L187 documents
/// the field as "0: get header, 1: copy, 2: decompress".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(i32)]
pub enum GzHow {
    /// [`LOOK`]: look for a gzip header before deciding. The state `gz_reset` installs
    /// (`gzlib.c` L74), and therefore the default.
    #[default]
    Look = LOOK,
    /// [`COPY`]: copy input directly, the transparent path.
    Copy = COPY,
    /// [`GZIP`]: decompress a gzip stream.
    Gzip = GZIP,
}

impl GzHow {
    /// Returns the C `int` that names this state.
    #[must_use]
    pub const fn as_raw(self) -> i32 {
        self as i32
    }

    /// Recognises a stored `how` value, returning [`None`] for anything else.
    #[must_use]
    pub const fn from_raw(how: i32) -> Option<Self> {
        match how {
            LOOK => Some(Self::Look),
            COPY => Some(Self::Copy),
            GZIP => Some(Self::Gzip),
            _ => None,
        }
    }
}

// -----------------------------------------------------------------------------
//  The file handle: this port's replacement for `int fd`
// -----------------------------------------------------------------------------

/// Where a seek is measured from: the port of the `whence` argument.
///
/// `zconf.h` L513-L515 defines `SEEK_SET`, `SEEK_CUR` and `SEEK_END` as 0, 1 and 2 when the platform
/// has not already, and those discriminants are reproduced here. `gzseek64` accepts only the first
/// two and rejects `SEEK_END` outright (`gzlib.c` L384-L385), because the uncompressed length of a
/// gzip stream is not known without decompressing it; [`GzSeekFrom::End`] exists because the
/// handle's own positioning still needs it -- `gz_open` uses it to seek to the end of an appended
/// file so that `gzoffset` is correct (`gzlib.c` L270).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum GzSeekFrom {
    /// `SEEK_SET`: measured from the beginning of the file.
    Start = 0,
    /// `SEEK_CUR`: measured from the current position.
    Current = 1,
    /// `SEEK_END`: measured from the end of the file.
    End = 2,
}

impl GzSeekFrom {
    /// Returns the C `int` that names this origin.
    #[must_use]
    pub const fn as_raw(self) -> i32 {
        self as i32
    }

    /// Recognises a `whence` argument, returning [`None`] for anything else.
    #[must_use]
    pub const fn from_raw(whence: i32) -> Option<Self> {
        match whence {
            0 => Some(Self::Start),
            1 => Some(Self::Current),
            2 => Some(Self::End),
            _ => None,
        }
    }
}

/// Why an operation on the underlying file failed.
///
/// The C layer reports such a failure as `gz_error(state, Z_ERRNO, zstrerror())`, where
/// `zstrerror()` expands to `strerror(errno)` (`gzguts.h` L131-L133) -- for instance at
/// `gzread.c` L41 and `gzwrite.c` L119. This type carries the same two pieces of information the C
/// code acts on, and nothing more:
///
/// * `errno`, so that the error can be rendered exactly as `strerror` would render it;
/// * whether the failure was `EAGAIN` or `EWOULDBLOCK`, because that distinction is not an error
///   condition at all but the non-blocking contract. `gz_comp` records it in `state->again` before
///   reporting (`gzwrite.c` L117-L119), and `gz_error` then declines to clear `x.have` for a stream
///   that is merely stalled (`gzlib.c` L564-L565).
///
/// Deliberately allocation-free and [`Copy`]: an error path is the last place that should need a
/// heap allocation to succeed. Turning it into text is the caller's job, which keeps the two
/// `errno`-name sets (`std::io::ErrorKind` and the platform's own) out of this crate entirely.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GzIoError {
    /// The platform error number, or zero when the implementation could not determine one.
    ///
    /// Mirrors C's `errno` immediately after the failed `read`, `write`, `lseek` or `close`.
    pub errno: i32,
    /// True when the failure was `EAGAIN` or `EWOULDBLOCK` on a non-blocking descriptor.
    ///
    /// Ported from the test at `gzwrite.c` L117-L118, which sets `state->again` on exactly this
    /// condition.
    pub would_block: bool,
}

impl GzIoError {
    /// Builds an error from a platform error number.
    ///
    /// `would_block` must be set by the implementation, because only it knows the platform's
    /// `EAGAIN` and `EWOULDBLOCK` values; this crate deliberately does not name them.
    #[must_use]
    pub const fn new(errno: i32, would_block: bool) -> Self {
        Self { errno, would_block }
    }
}

/// The file a `gzFile` reads from or writes to: this port's replacement for `gz_state.fd`.
///
/// C stores a bare descriptor (`gzguts.h` L178) and calls `read`, `write`, `lseek`, `close` and
/// `fcntl` on it directly. A descriptor cannot be adopted in safe Rust -- `FromRawFd::from_raw_fd`
/// requires the escape hatch this crate forbids -- so the descriptor is replaced by an injected
/// object, and the two ways a `gzFile` can come into being are split accordingly:
///
/// | C entry point | How the handle is produced |
/// |---|---|
/// | `gzopen`, `gzopen64`, `gzopen_w` (`gzlib.c` L288, L293 and L316) | opened by path inside the core, from `open.rs` |
/// | `gzdopen` (`gzlib.c` L298-L311) | adopted from a caller's descriptor, and therefore injected by `crates/libz-rs-sys` |
///
/// This module declares only the slot ([`GzState::handle`]) and this trait. The concrete
/// implementation lives in `open.rs`, which is where path-based opening belongs and which is free
/// to build it over `std::fs::OpenOptions`; the facade supplies its own implementation for the
/// adopted-descriptor case. Neither one is named here, which is what lets this file stay free of
/// `std` and free of any platform type.
///
/// # Implementing this trait
///
/// * Every method reports failure as [`GzIoError`], never by panicking. The `gzFile` layer converts
///   a failure into `Z_ERRNO` and keeps going; it never unwinds.
/// * [`GzHandle::read`] returning `Ok(0)` means end of file, exactly as `read` returning 0 does in
///   `gz_load` (`gzread.c` L31-L46).
/// * [`GzHandle::write`] may report a short write. `gz_comp` loops until the buffer is drained
///   (`gzwrite.c` L110-L123), so a partial write is normal rather than exceptional.
/// * [`GzHandle::close`] must be idempotent, because `Drop` may run after an explicit close: both
///   `gzclose_r` and `gzclose_w` close the descriptor and then free the state
///   (`gzread.c` L663-L665, `gzwrite.c` L648-L650).
pub trait GzHandle {
    /// Reads into `buf`, returning the number of bytes read, or zero at end of file.
    ///
    /// The port of the `read(state->fd, buf + *have, len)` call in `gz_load`
    /// (`gzread.c` L30).
    ///
    /// # Errors
    ///
    /// [`GzIoError`] if the underlying read fails. A `would_block` error is the non-blocking
    /// stall that `gz_load` propagates so that `gz_avail` can record it in `state->again`.
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, GzIoError>;

    /// Writes from `buf`, returning the number of bytes written, which may be fewer than requested.
    ///
    /// The port of the `write(state->fd, state->x.next, put)` call in `gz_comp`
    /// (`gzwrite.c` L115).
    ///
    /// # Errors
    ///
    /// [`GzIoError`] if the underlying write fails.
    fn write(&mut self, buf: &[u8]) -> Result<usize, GzIoError>;

    /// Repositions the file, returning the resulting absolute offset.
    ///
    /// The port of the `LSEEK` macro (`gzlib.c` L8-L16), which selects `lseek64`, `_lseeki64`,
    /// `llseek` or `lseek` per platform. Used to record the starting position of a read stream
    /// (`gzlib.c` L276), to rewind (`gzlib.c` L360), to skip forward within raw data
    /// (`gzlib.c` L398) and to report the raw offset (`gzlib.c` L481).
    ///
    /// # Errors
    ///
    /// [`GzIoError`] if the file is not seekable or the request is invalid. C treats a failure as a
    /// `-1` return and its callers fall back accordingly -- `gz_open` substitutes zero for an
    /// unseekable stream (`gzlib.c` L277).
    fn seek(&mut self, offset: ZOff64, whence: GzSeekFrom) -> Result<ZOff64, GzIoError>;

    /// Sets or clears the non-blocking flag.
    ///
    /// The port of the `fcntl(fd, F_SETFL, fcntl(fd, F_GETFL) | O_NONBLOCK)` call that `gz_open`
    /// makes for an adopted descriptor when the mode string contained `N` (`gzlib.c` L255-L257).
    /// For a path-based open the flag is part of the `open` call instead (`gzlib.c` L160-L162), so
    /// an implementation that only ever opens by path may report success without doing anything.
    ///
    /// # Errors
    ///
    /// [`GzIoError`] if the flag cannot be changed.
    fn set_nonblocking(&mut self, nonblocking: bool) -> Result<(), GzIoError>;

    /// Closes the file. Must be idempotent.
    ///
    /// The port of `close(state->fd)` (`gzread.c` L665, `gzwrite.c` L696), whose result becomes
    /// `Z_ERRNO` when it fails.
    ///
    /// # Errors
    ///
    /// [`GzIoError`] if the underlying close fails.
    fn close(&mut self) -> Result<(), GzIoError>;
}

// -----------------------------------------------------------------------------
//  The compression engine, held behind an indirection
// -----------------------------------------------------------------------------

/// One heap-allocated value, obtained fallibly: this port's stand-in for `Box<T>`.
///
/// C reaches its compression engine through `state->strm.state`, a pointer to a separately
/// allocated `deflate_state` or `inflate_state`. Reproducing that shape matters for more than
/// tidiness: both engine states are several kilobytes, and storing one inline would put every byte
/// of it into [`GzState`] itself, which the facade must allocate before it knows which direction the
/// stream will run in.
///
/// `Box::new` cannot be used, for a reason `crate::allocate` records: it aborts the process on
/// allocation failure, and there is no fallible, MSRV-stable way to heap-allocate a single value.
/// Allocation failure is not hypothetical here -- `test/infcover.c` induces it deliberately with
/// `mem_limit` (L176-L181) so that the library's `Z_MEM_ERROR` paths actually run. So the
/// indirection is a [`Vec`] holding exactly one element, reserved through
/// [`Vec::try_reserve_exact`], which reports exhaustion instead of aborting.
///
/// The one-element invariant is maintained by construction: [`EngineBox::try_new`] is the only
/// constructor, it pushes exactly once into a vector reserved for exactly one element, and
/// [`EngineBox::into_inner`] consumes the box. The accessors still return [`Option`] rather than
/// asserting the invariant, because an assertion would be a panic and library code here does not
/// panic.
#[derive(Debug)]
pub struct EngineBox<T> {
    /// The single element. Exactly one on every path that can observe it.
    slot: Vec<T>,
}

impl<T> EngineBox<T> {
    /// Moves `value` onto the heap, reporting an allocation failure rather than aborting.
    ///
    /// The `ZALLOC(strm, 1, sizeof(deflate_state))` / `ZALLOC(strm, 1, sizeof(inflate_state))`
    /// analogue (`deflate.c` L440, `inflate.c` L197-L198), except that the block is obtained from
    /// Rust's allocator. That is the faithful choice for this layer: the `gzFile` code sets
    /// `strm->zalloc`, `strm->zfree` and `strm->opaque` to `Z_NULL` before initialising either
    /// engine (`gzread.c` L110-L112, `gzwrite.c` L32-L34), so the engine state is obtained from
    /// `zcalloc`, which is plain `malloc` (`zutil.c` L299-L303). A `gzFile` has no caller-supplied
    /// hooks to honour, and this block is not caller-visible: C's `strm.state` is opaque by
    /// contract.
    ///
    /// Note that the engine's *own* buffers are a different matter -- they come from the
    /// [`Allocator`] injected into [`DeflateState::new`] or [`InflateState::new`], and go back to
    /// it.
    ///
    /// # Errors
    ///
    /// [`ReturnCode::MEM_ERROR`], which is what both `gz_look` and `gz_init` report when engine
    /// initialisation cannot allocate (`gzread.c` L119, `gzwrite.c` L41).
    pub fn try_new(value: T) -> Result<Self, ReturnCode> {
        let mut slot = Vec::new();
        // The fallible half: reserve room for exactly one element, reporting exhaustion.
        slot.try_reserve_exact(1)
            .map_err(|_| ReturnCode::MEM_ERROR)?;
        // The infallible half: capacity is already sufficient, so this cannot reallocate.
        slot.push(value);
        Ok(Self { slot })
    }

    /// Borrows the boxed value.
    #[must_use]
    pub fn get(&self) -> Option<&T> {
        self.slot.first()
    }

    /// Borrows the boxed value mutably.
    #[must_use]
    pub fn get_mut(&mut self) -> Option<&mut T> {
        self.slot.first_mut()
    }

    /// Consumes the box and yields the value, releasing the heap block.
    #[must_use]
    pub fn into_inner(mut self) -> Option<T> {
        self.slot.pop()
    }
}

/// The engine a `gzFile` drives, or the absence of one: the port of `state->strm.state`.
///
/// A `gzFile` holds at most one engine, and which one is fixed by the direction the file was opened
/// in. C expresses that with an untyped `internal_state *` that `gz_look` initialises for inflate
/// (`gzread.c` L115) and `gz_init` for deflate (`gzwrite.c` L36-L37); this enum expresses it so that
/// asking a read stream for its compressor is a type error rather than a cast.
///
/// [`GzEngine::None`] is the state a freshly opened file is in, and it corresponds exactly to
/// `state->size == 0`: neither `gz_look` nor `gz_init` has run, so neither the working buffers nor
/// the engine exist yet.
pub enum GzEngine<'a, A: Allocator<'a>> {
    /// No engine yet, which is the same condition as `state->size == 0`.
    None,
    /// The compressor of a write stream, initialised by `gz_init` (`gzwrite.c` L36-L37).
    Deflate(EngineBox<DeflateState<'a, A>>),
    /// The decompressor of a read stream, initialised by `gz_look` (`gzread.c` L115).
    Inflate(EngineBox<InflateState<'a, A>>),
}

impl<'a, A: Allocator<'a>> GzEngine<'a, A> {
    /// Reports whether no engine is installed, which is the same condition as `size == 0`.
    #[must_use]
    pub fn is_none(&self) -> bool {
        matches!(self, Self::None)
    }
}

impl<'a, A: Allocator<'a>> Default for GzEngine<'a, A> {
    /// Yields [`GzEngine::None`], the state a freshly opened `gzFile` is in.
    fn default() -> Self {
        Self::None
    }
}

impl<'a, A: Allocator<'a>> fmt::Debug for GzEngine<'a, A> {
    /// Reports which engine is installed, never its contents.
    ///
    /// Written by hand rather than derived so that it places no `Debug` requirement on `A`, matching
    /// the deflate and inflate state modules. It also keeps several kilobytes of engine state -- and
    /// with it the user data in the sliding window -- out of any debug output.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::None => "None",
            Self::Deflate(_) => "Deflate(..)",
            Self::Inflate(_) => "Inflate(..)",
        };
        write!(f, "GzEngine::{name}")
    }
}

/// The `z_stream` a `gzFile` embeds: the port of `gz_state.strm` (`gzguts.h` L202).
///
/// C embeds the whole `z_stream` "in-place (not a pointer)" and hands its address to `deflate` and
/// `inflate`. Two of its members are not reproduced here at all, and that is deliberate:
///
/// * `zalloc`, `zfree` and `opaque` are absent because the `gzFile` layer always sets them to
///   `Z_NULL` (`gzread.c` L110-L112, `gzwrite.c` L32-L34) and never exposes them; the injected
///   [`Allocator`] on [`GzState`] takes their place.
/// * `total_in` and `total_out` are absent because the layer never reads them. That is not an
///   assumption: neither name occurs anywhere in `gzlib.c`,
///   `gzread.c`, `gzwrite.c` or `gzclose.c`. The layer tracks position in `x.pos` instead.
/// * `adler` and `data_type` are absent for the same reason -- the container checksum is the
///   engine's business, and the `gzFile` layer never inspects either.
///
/// What remains is the four cursor fields, the engine's message slot and the engine itself. The two
/// pointer cursors become indices, for the reason this module's documentation gives.
pub struct GzStream<'a, A: Allocator<'a>> {
    /// Bytes of input still unconsumed: the port of `strm.avail_in`.
    ///
    /// `gz_reset` clears it (`gzlib.c` L83), `gz_avail` refills it (`gzread.c` L56-L92), and
    /// `gzoffset64` subtracts it from the raw file offset so that buffered input is not counted
    /// (`gzlib.c` L484-L485).
    pub avail_in: c_uint,
    /// Index of the next input byte within [`GzState::input`]: the port of `strm.next_in`.
    ///
    /// An index rather than a pointer. `gz_avail` slides the remaining input back to the start of
    /// the buffer before refilling (`gzread.c` L63-L71), which is the operation that requires the
    /// cursor to be expressed relative to the buffer in the first place.
    pub next_in: usize,
    /// Space remaining in the output destination: the port of `strm.avail_out`.
    pub avail_out: c_uint,
    /// Index of the next output byte within [`GzState::output`]: the port of `strm.next_out`.
    ///
    /// Meaningful only while the engine is writing into the layer's own output buffer, which is the
    /// buffered path. On the large-read path `gz_read` points C's `next_out` at the *user's* buffer
    /// instead (`gzread.c` L376-L377) and this field is not used; the destination slice travels as
    /// an argument in that case, and fact 2 in this module's documentation explains how the exposed
    /// prefix is kept coherent across it.
    pub next_out: usize,
    /// The engine's last message: the port of `strm.msg`.
    ///
    /// `gz_decomp` forwards it verbatim when inflate reports a data error, falling back to
    /// `"compressed data error"` when it is null (`gzread.c` L221-L222). The shape matches
    /// [`ReturnCode::record_msg`], which is how the engines set it.
    pub msg: Option<&'static str>,
    /// The engine itself, or [`GzEngine::None`] before one has been initialised.
    pub engine: GzEngine<'a, A>,
}

impl<'a, A: Allocator<'a>> GzStream<'a, A> {
    /// The stream as a freshly opened `gzFile` has it: no input, no output, no engine.
    ///
    /// `gz_open` does not touch `strm` beyond what `gz_reset` does, and `gz_reset` sets only
    /// `state->strm.avail_in = 0` (`gzlib.c` L83). The remaining members are established by
    /// `gz_look` or `gz_init` when the buffers are allocated, so they start at zero here.
    #[must_use]
    pub fn new() -> Self {
        Self {
            avail_in: 0,
            next_in: 0,
            avail_out: 0,
            next_out: 0,
            msg: None,
            engine: GzEngine::None,
        }
    }
}

impl<'a, A: Allocator<'a>> Default for GzStream<'a, A> {
    /// Equivalent to [`GzStream::new`].
    fn default() -> Self {
        Self::new()
    }
}

impl<'a, A: Allocator<'a>> fmt::Debug for GzStream<'a, A> {
    /// Reports every cursor and which engine is installed.
    ///
    /// Written by hand for the same reason [`GzEngine`]'s implementation is: so that it places no
    /// `Debug` requirement on `A`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GzStream")
            .field("avail_in", &self.avail_in)
            .field("next_in", &self.next_in)
            .field("avail_out", &self.avail_out)
            .field("next_out", &self.next_out)
            .field("msg", &self.msg)
            .field("engine", &self.engine)
            .finish()
    }
}

// -----------------------------------------------------------------------------
//  The state
// -----------------------------------------------------------------------------

/// The internal `gzFile` state: the port of `gz_state` (`gzguts.h` L169-L204).
///
/// A `gzFile` is an opaque pointer to one of these. The C fields appear below in `gzguts.h`'s
/// declaration order, each annotated with the line it comes from, followed by a clearly separated
/// tail of members this port needs and C does not.
///
/// # `#[repr(C)]` is mandatory here
///
/// The attribute is not decoration and not an optimisation. [`GzState::x`] must begin at offset
/// zero, because the `gzgetc` macro dereferences its three fields inside caller object code that
/// cannot be recompiled, and only `#[repr(C)]` promises that the first declared field is the first
/// field in memory. This module's documentation explains the consequences of getting it wrong, and
/// the `const` assertions above make a mistake a build failure. Nothing past the first 24 bytes is
/// visible to any caller, so the remaining fields are free to be ordinary Rust values -- owned
/// buffers, an owned handle, owned strings -- and they are.
///
/// # Lifetime and allocator
///
/// `'a` covers the allocator's blocks and the injected file handle. `A` is the [`Allocator`]
/// injected in place of C's direct `malloc` calls; injecting [`crate::allocate::GlobalAllocator`]
/// reproduces C's behaviour exactly, because a `gzFile` has no caller-supplied hooks
/// (`gzlib.c` L100 and L206, `gzread.c` L99-L100, `gzwrite.c` L16 and L25). The allocator is stored
/// by value so that teardown can return every block to the allocator that produced it.
///
/// # Moving a `GzState` is safe
///
/// [`GzFileExposed::next`] points into the heap block behind [`GzState::output`], not into the
/// `GzState` itself, and moving a `GzState` does not move that block. The pointer therefore survives
/// the move the facade performs when it installs a freshly built state at the address it will hand
/// to C. What does invalidate it is replacing the output buffer, which is why every routine below
/// that touches [`GzState::output`] ends by calling [`GzState::refresh_exposed`].
///
/// # Why `refresh_exposed` derives the pointer afresh every time
///
/// The caller's `gzgetc` macro does not merely read through `next`; it reads a byte and
/// post-increments the pointer, so the C side both loads from and stores into the exposed prefix.
/// Rather than cache a pointer once and hope it stays usable across every intervening borrow of the
/// output buffer, [`GzState::refresh_exposed`] recomputes it from a *fresh* mutable borrow
/// immediately before control returns to C. The pointer the C side holds is therefore always the
/// most recently derived one, which is the strongest discipline available at an FFI edge and the
/// reason the resync-on-entry / refresh-on-exit contract is stated as an obligation rather than an
/// optimisation.
#[repr(C)]
pub struct GzState<'a, A: Allocator<'a>> {
    /// The caller-visible prefix, at offset zero. `gzguts.h` L172.
    ///
    /// Read `have` and `pos` freely. Never assign to `next`: it is derived from
    /// [`GzState::out_pos`] and is maintained only by [`GzState::refresh_exposed`] and
    /// [`GzState::clear_have`]. Prefer the accessors on this type over reaching in here, so that
    /// the index and the pointer cannot drift apart.
    pub(crate) x: GzFileExposed,
    /// One of [`GZ_NONE`], [`GZ_READ`], [`GZ_WRITE`] or [`GZ_APPEND`]. `gzguts.h` L177.
    ///
    /// Stored as a plain `i32` rather than as a [`GzMode`] on purpose. C treats it as "a little
    /// integrity check on the passed structure" (`gzguts.h` L158) and tests it against the two
    /// live values before trusting the pointer it was given, so the type must be able to *hold* an
    /// unrecognised value in order to reject it. [`GzState::mode_typed`] is the exhaustive view.
    pub(crate) mode: i32,
    /// The open file, replacing C's `int fd`. `gzguts.h` L178.
    ///
    /// [`None`] before the file is opened and after it has been taken for closing. See [`GzHandle`]
    /// for why a descriptor cannot be stored directly and which side supplies the implementation.
    pub(crate) handle: Option<Box<dyn GzHandle + 'a>>,
    /// The path, or the `"<fd:N>"` stand-in for a descriptor, used only in error messages.
    /// `gzguts.h` L179.
    ///
    /// Bytes rather than a `String`, deliberately: `gz_open` copies the caller's path verbatim with
    /// `snprintf(state->path, len + 1, "%s", path)` (`gzlib.c` L222), and a POSIX path is an
    /// arbitrary byte string that need not be valid UTF-8. Insisting on UTF-8 here would make some
    /// legal filenames unopenable, so the bytes are carried through untouched and
    /// [`GzState::try_set_prefixed_msg`] concatenates them exactly as `gz_error` does.
    pub(crate) path: Vec<u8>,
    /// Working-buffer size, or zero when the buffers have not been allocated. `gzguts.h` L180.
    ///
    /// The zero sentinel is load-bearing and is read by five separate places: `gzbuffer` refuses to
    /// change `want` once buffers exist (`gzlib.c` L333-L334), `gz_look` allocates only when it is
    /// zero (`gzread.c` L97) and resets it to zero if engine initialisation then fails
    /// (`gzread.c` L118), `gz_init` sets it last to mark the write path initialised
    /// (`gzwrite.c` L48), and both close paths free the buffers only when it is non-zero
    /// (`gzread.c` L657-L661, `gzwrite.c` L687-L693).
    pub(crate) size: c_uint,
    /// Requested working-buffer size, [`GZBUFSIZE`] by default. `gzguts.h` L181.
    ///
    /// Set by `gzbuffer`, which floors it at 8 "to behave well with flushing" and rejects a value
    /// that cannot be doubled (`gzlib.c` L337-L340).
    pub(crate) want: c_uint,
    /// The input buffer, C's `in`. `gzguts.h` L182.
    ///
    /// Renamed because `in` is a Rust keyword. `want` bytes on the read path (`gzread.c` L99) and
    /// `2 * want` on the write path, where the comment records the reason -- "double size for
    /// `gzprintf`" (`gzwrite.c` L15-L16).
    pub(crate) input: Option<Buffer<'a, u8>>,
    /// The output buffer, C's `out`. `gzguts.h` L183.
    ///
    /// `2 * want` bytes on the read path (`gzread.c` L100), which is what gives `gzungetc` somewhere
    /// to park a pushed byte, and `want` bytes on the write path, allocated only when actually
    /// compressing (`gzwrite.c` L24-L25).
    pub(crate) output: Option<Buffer<'a, u8>>,
    /// Container handling: 0 gzip, 1 transparent, -1 gzip only. `gzguts.h` L184.
    ///
    /// A genuine tri-state, and all three values are reachable from the mode string:
    /// `T` sets it to 1 and `G` to -1, last one winning (`gzlib.c` L156-L166). `gz_open` then
    /// normalises it: a read stream may not force transparency, so `T` with `r` is rejected, and a
    /// read stream left at 0 becomes 1 to "start with a transparent assumption in case of an empty
    /// file" (`gzlib.c` L180-L190); `G` while writing is rejected outright (`gzlib.c` L191-L195).
    /// `gzdirect` reports `state->direct` as a boolean, so only the value 1 reads as transparent
    /// (`gzread.c` L640-L641).
    pub(crate) direct: i32,
    /// Multi-member tracking: -1 at the start, 1 for a junk candidate, 0 inside a gzip member.
    /// `gzguts.h` L186.
    ///
    /// Specific to this `1.3.2.1-motley` tree rather than inherited from zlib 1.3.1, and
    /// load-bearing: `gz_reset` marks the first member with -1 (`gzlib.c` L75) and `gz_decomp`
    /// clears it to 0 once a member has completed successfully (`gzread.c` L233), which is what lets
    /// the read path tell trailing garbage after a complete member from a corrupt first member.
    pub(crate) junk: i32,
    /// What the read path is doing: [`LOOK`], [`COPY`] or [`GZIP`]. `gzguts.h` L187.
    ///
    /// Stored raw for the same reason [`GzState::mode`] is; [`GzState::how_typed`] is the exhaustive
    /// view.
    pub(crate) how: i32,
    /// Non-zero when the last input or output operation reported `EAGAIN` or `EWOULDBLOCK`.
    /// `gzguts.h` L188.
    ///
    /// Also specific to this tree, and also load-bearing: it is what distinguishes a stalled
    /// non-blocking stream from a broken one. `gz_error` consults it before clearing `x.have`
    /// (`gzlib.c` L564-L565), so a stall leaves the buffered data intact and the caller can retry,
    /// and `gzungetc` treats a stalled stream as usable (`gzread.c` L520).
    pub(crate) again: i32,
    /// Where the gzip data started, for rewinding. `gzguts.h` L189.
    ///
    /// Recorded for a read stream by `gz_open` from the handle's current position, falling back to
    /// zero when the file is not seekable (`gzlib.c` L275-L278), and used by `gzrewind`
    /// (`gzlib.c` L360).
    pub(crate) start: ZOff64,
    /// Non-zero once the end of the input file has been reached. `gzguts.h` L190.
    pub(crate) eof: i32,
    /// Non-zero once a read has been requested past the end of the data. `gzguts.h` L191.
    ///
    /// This, not [`GzState::eof`], is what `gzeof` reports (`gzlib.c` L509), and the distinction
    /// matters: reaching the end of the compressed input is not the same as a caller asking for
    /// data that is not there.
    pub(crate) past: i32,
    /// Compression level for the write path. `gzguts.h` L193.
    ///
    /// Stored as C stores it, including the negative `Z_DEFAULT_COMPRESSION` sentinel, so that
    /// `gzsetparams`' "no change requested" short-circuit -- `level == state->level && strategy ==
    /// state->strategy` (`gzwrite.c` L647) -- compares identically. Validation and the
    /// mapping to an engine configuration are [`crate::config`]'s job.
    pub(crate) level: i32,
    /// Compression strategy for the write path. `gzguts.h` L194.
    ///
    /// Stored raw for the same reason as [`GzState::level`]; the mode-string letters `f`, `h`, `R`
    /// and `F` select `Z_FILTERED`, `Z_HUFFMAN_ONLY`, `Z_RLE` and `Z_FIXED` respectively
    /// (`gzlib.c` L144-L155). [`GzState::strategy_typed`] is the exhaustive view.
    pub(crate) strategy: i32,
    /// Non-zero when a `deflateReset` is pending after a `Z_FINISH`. `gzguts.h` L195.
    ///
    /// Set when `gz_comp` finishes a member and consumed at the start of the next one
    /// (`gzwrite.c` L97-L101), which is how consecutive `gzflush(Z_FINISH)` calls produce
    /// consecutive gzip members in one file.
    pub(crate) reset: i32,
    /// Amount still to skip forward, already rewound if the seek was backwards. `gzguts.h` L197.
    ///
    /// `gzseek64` records the residue here rather than performing it, so that a seek costs nothing
    /// until the data is actually wanted (`gzlib.c` L432-L433); `gztell64` therefore has to add it
    /// back when reporting the position (`gzlib.c` L457).
    pub(crate) skip: ZOff64,
    /// The last error code, as a `Z_*` value. `gzguts.h` L199.
    ///
    /// Reported by `gzerror` through its `errnum` out-parameter (`gzlib.c` L524-L525). Held as an
    /// `i32` rather than a [`ReturnCode`] because `gzerror` hands the raw value straight to the
    /// caller; [`GzState::err_code`] is the typed view.
    pub(crate) err: i32,
    /// The last error message, already prefixed with the path. `gzguts.h` L200.
    ///
    /// [`None`] means "no message", which `gzerror` reports as the empty string
    /// (`gzlib.c` L526-L527). It stays [`None`] for `Z_MEM_ERROR` as well, because `gz_error`
    /// deliberately does not allocate in that case (`gzlib.c` L573-L574) and `gzerror` substitutes
    /// the literal `"out of memory"` instead (`gzlib.c` L526).
    pub(crate) msg: Option<Vec<u8>>,
    /// The embedded stream and its engine. `gzguts.h` L202.
    pub(crate) strm: GzStream<'a, A>,

    // -------------------------------------------------------------------------
    //  Members this port needs and C does not
    // -------------------------------------------------------------------------
    /// Index within [`GzState::output`] that [`GzFileExposed::next`] denotes.
    ///
    /// The safe half of the pointer/index split described in this module's documentation. C keeps
    /// only the pointer; this port keeps both and treats the index as authoritative, because an
    /// index can be bounds-checked and a pointer cannot be dereferenced from inside this crate.
    /// The invariant `x.next == output.as_ptr() + out_pos` is established by
    /// [`GzState::refresh_exposed`] and recovered by [`GzState::resync_from_exposed`].
    pub(crate) out_pos: usize,
    /// The allocator every block in this state came from, and the only one it may go back to.
    ///
    /// C has no equivalent because it calls `malloc` and `free` directly. Keeping the allocator
    /// beside the blocks is what lets teardown -- and, as a safety net, [`Drop`] -- return each block
    /// to its origin, which is the mismatched-allocator bug class that `test/infcover.c`'s
    /// `mem_done` reports as a rogue free.
    pub(crate) allocator: A,
}

/// Widens a C `unsigned` to a `usize` without a lossy cast.
///
/// Every target this port supports has `usize` at least as wide as `c_uint`, so the conversion
/// always succeeds; saturating rather than panicking on a hypothetical narrower target is the
/// conservative choice, because a saturated value can only make a bounds check stricter.
fn widen(value: c_uint) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}

/// Narrows a `usize` to a C `unsigned`, or reports that it does not fit.
///
/// The counterpart of [`widen`]. C performs this narrowing with an unchecked cast -- for instance
/// `(unsigned)offset` at `gzlib.c` L425 -- guarded by a preceding range test; this port makes the
/// test part of the conversion so the guard cannot be forgotten.
fn narrow(value: usize) -> Option<c_uint> {
    c_uint::try_from(value).ok()
}

impl<'a, A: Allocator<'a>> GzState<'a, A> {
    /// Builds a state in exactly the condition `gz_open` leaves one in before parsing the mode
    /// string.
    ///
    /// The values are `gz_open`'s (`gzlib.c` L103-L112) together with the ones `gz_reset` establishes
    /// (`gzlib.c` L69-L84), because a Rust value must be whole the moment it exists whereas C leaves
    /// part of the structure untouched until later. Struct-literal order is not observable in Rust,
    /// so the fields below are listed in `gz_state`'s declaration order and the C line that
    /// establishes each value is named beside it:
    ///
    /// | Field | Value | C origin |
    /// |---|---|---|
    /// | `size` | 0 | `gzlib.c` L103, "no buffers allocated yet" |
    /// | `want` | [`GZBUFSIZE`] | `gzlib.c` L104 |
    /// | `err` | `Z_OK` | `gzlib.c` L105 |
    /// | `msg` | none | `gzlib.c` L106 |
    /// | `mode` | [`GZ_NONE`] | `gzlib.c` L109 |
    /// | `level` | `Z_DEFAULT_COMPRESSION` | `gzlib.c` L110 |
    /// | `strategy` | `Z_DEFAULT_STRATEGY` | `gzlib.c` L111 |
    /// | `direct` | 0 | `gzlib.c` L112 |
    /// | `x.have`, `x.pos` | 0 | `gzlib.c` L70, L82 |
    /// | `how` | [`LOOK`] | `gzlib.c` L74 |
    /// | `junk` | -1, "mark first member" | `gzlib.c` L75 |
    /// | `again`, `skip` | 0 | `gzlib.c` L79, L80 |
    /// | `eof`, `past`, `reset` | 0 | `gzlib.c` L72, L73, L78 |
    /// | `strm.avail_in` | 0 | `gzlib.c` L83 |
    ///
    /// No buffer is allocated and no engine is created: that is `gz_look`'s or `gz_init`'s job, and
    /// `size == 0` is how they know it still has to be done. `start` is zero, which is also the
    /// fallback `gz_open` uses when the handle turns out not to be seekable (`gzlib.c` L277).
    pub fn new(allocator: A) -> Self {
        Self {
            x: GzFileExposed::EMPTY,
            mode: GZ_NONE,
            handle: None,
            path: Vec::new(),
            size: 0,
            want: GZBUFSIZE,
            input: None,
            output: None,
            direct: 0,
            junk: -1,
            how: LOOK,
            again: 0,
            start: 0,
            eof: 0,
            past: 0,
            level: Z_DEFAULT_COMPRESSION,
            strategy: Z_DEFAULT_STRATEGY,
            reset: 0,
            skip: 0,
            err: ReturnCode::OK.as_i32(),
            msg: None,
            strm: GzStream::new(),
            out_pos: 0,
            allocator,
        }
    }

    // -------------------------------------------------------------------------
    //  The exposed prefix
    // -------------------------------------------------------------------------

    /// Recomputes [`GzFileExposed::next`] from [`GzState::out_pos`] and the output buffer.
    ///
    /// The core-to-prefix half of the pointer/index split. **Call this immediately before returning
    /// control to C from any entry point**, because the caller's `gzgetc` macro will read the
    /// pointer directly.
    ///
    /// The address is produced with `pointer::wrapping_add` rather than `pointer::add`: the wrapping
    /// form has no precondition to violate, so a hypothetical out-of-range `out_pos` yields a
    /// useless pointer instead of undefined behaviour. `out_pos` is nonetheless kept in range by
    /// every routine that sets it.
    ///
    /// With no output buffer there is nothing to point at, so `next` becomes null and `have` is
    /// forced to zero -- the pair that fact 1 in this module's documentation shows the macro handles
    /// by falling back to the `gzgetc` function. That combination is exactly what `gz_open` leaves
    /// behind before `gz_look` or `gz_init` runs.
    pub fn refresh_exposed(&mut self) {
        // Take the base address and end the borrow before touching any other field.
        let base = self
            .output
            .as_mut()
            .map(|buffer| buffer.as_mut_slice().as_mut_ptr());
        if let Some(base) = base {
            self.x.next = base.wrapping_add(self.out_pos);
        } else {
            // Nothing to point at, and therefore nothing available to deliver.
            self.x.have = 0;
            self.out_pos = 0;
            self.x.next = core::ptr::null_mut();
        }
    }

    /// Recovers [`GzState::out_pos`] from [`GzFileExposed::next`].
    ///
    /// The prefix-to-core half of the pointer/index split. **Call this immediately on entry to any
    /// entry point**, because the caller's `gzgetc` macro advances `next`, decrements `have` and
    /// increments `pos` without informing the library, so `out_pos` is stale on arrival.
    ///
    /// The recovery is a pointer-to-integer cast and a wrapping subtraction, both of which are safe
    /// operations; nothing is read through the pointer. A pointer that does not denote a position
    /// within the output buffer, or a `have` that would run past the end of it, means the structure
    /// was corrupted or was never one of ours, and is reported rather than trusted -- this is the
    /// same defensive posture as C's mode check (`gzguts.h` L158).
    ///
    /// A null `next` is accepted when `have` is zero, for the reason fact 1 in this module's
    /// documentation gives, and is treated as position zero.
    ///
    /// # Errors
    ///
    /// [`ReturnCode::STREAM_ERROR`] when `next` and `have` are not consistent with the output
    /// buffer. That is the code every `gzFile` entry point already returns for a structure it does
    /// not recognise, so a caller can propagate it unchanged.
    pub fn resync_from_exposed(&mut self) -> Result<(), ReturnCode> {
        let Some(buffer) = self.output.as_ref() else {
            // No output buffer: only "nothing available, pointing nowhere" is coherent.
            if self.x.next.is_null() && self.x.have == 0 {
                self.out_pos = 0;
                return Ok(());
            }
            return Err(ReturnCode::STREAM_ERROR);
        };
        let slice = buffer.as_slice();
        let len = slice.len();

        if self.x.next.is_null() {
            if self.x.have != 0 {
                return Err(ReturnCode::STREAM_ERROR);
            }
            self.out_pos = 0;
            return Ok(());
        }

        let base = slice.as_ptr() as usize;
        let current = self.x.next as usize;
        // Wrapping, because a pointer from an unrelated allocation must not be able to trap here;
        // the range tests below reject whatever nonsense it produces.
        let offset = current.wrapping_sub(base);
        // `len` itself is legal: after the last byte is consumed C leaves `next` one past the end
        // with `have` at zero (`gzread.c` L344-L345).
        if offset > len || widen(self.x.have) > len - offset {
            return Err(ReturnCode::STREAM_ERROR);
        }
        self.out_pos = offset;
        Ok(())
    }

    /// Number of bytes currently available for delivery: reads `x.have`.
    #[must_use]
    pub const fn have(&self) -> c_uint {
        self.x.have
    }

    /// Sets `x.have` without moving the output cursor.
    ///
    /// The port of the bare assignments C makes to the field, such as `state->x.have =
    /// strm->avail_in` after copying leftover input into the output buffer (`gzread.c` L166). Use
    /// [`GzState::set_output_window`] instead when the cursor moves too, so that the pointer stays
    /// consistent with the index.
    pub fn set_have(&mut self, have: c_uint) {
        self.x.have = have;
    }

    /// Declares the output buffer empty and points `next` nowhere.
    ///
    /// Two distinct C situations reduce to this pair of writes:
    ///
    /// * the end of the large-read path, where `gz_read` takes the count and clears the flag with
    ///   `n = state->x.have; state->x.have = 0;` (`gzread.c` L376-L377) after `gz_decomp` had
    ///   pointed `x.next` into the *user's* buffer -- fact 2 in this module's documentation;
    /// * a fatal error, where `gz_error` sets `state->x.have` to zero specifically "so that the
    ///   `gzgetc()` macro fails" and drops into the function branch (`gzlib.c` L563-L565).
    ///
    /// Nulling `next` alongside `have` is what keeps [`GzState::resync_from_exposed`] able to treat
    /// an out-of-range pointer as corruption: this port never leaves `next` denoting a location
    /// outside the output buffer, which C transiently does.
    pub fn clear_have(&mut self) {
        self.x.have = 0;
        self.out_pos = 0;
        self.x.next = core::ptr::null_mut();
    }

    /// The current position in the uncompressed stream: reads `x.pos`.
    #[must_use]
    pub const fn pos(&self) -> ZOff64 {
        self.x.pos
    }

    /// Sets `x.pos` outright, as `gz_reset` does with `state->x.pos = 0` (`gzlib.c` L82).
    pub fn set_pos(&mut self, pos: ZOff64) {
        self.x.pos = pos;
    }

    /// Adds `delta` to `x.pos`, which may be negative.
    ///
    /// The port of both directions C moves the position in: forwards after delivering bytes
    /// (`state->x.pos += n`, `gzread.c` L384) and backwards when `gzungetc` pushes one
    /// (`state->x.pos--`, `gzread.c` L537 and L560).
    ///
    /// The addition wraps rather than trapping. Overflow is unreachable in practice -- it would take
    /// eight exbibytes of uncompressed data -- and wrapping keeps a debug build from aborting inside
    /// a library that promises never to panic.
    pub fn add_pos(&mut self, delta: ZOff64) {
        self.x.pos = self.x.pos.wrapping_add(delta);
    }

    /// The index within the output buffer that `x.next` denotes.
    #[must_use]
    pub const fn out_pos(&self) -> usize {
        self.out_pos
    }

    /// Points the exposed window at `have` bytes starting at `pos` in the output buffer.
    ///
    /// The single operation behind every C site that assigns the cursor and the count together:
    /// `gz_look` publishing leftover raw input with `state->x.next = state->out;` and
    /// `state->x.have = strm->avail_in;` (`gzread.c` L164-L166), `gz_fetch` publishing a freshly
    /// loaded block with `state->x.next = state->out;` (`gzread.c` L260-L263), and `gz_decomp`
    /// publishing decompressed output (`gzread.c` L228-L229). Doing both in one call is what keeps
    /// the index and the pointer from ever disagreeing.
    ///
    /// # Errors
    ///
    /// [`ReturnCode::STREAM_ERROR`] if the window would extend past the end of the output buffer,
    /// including the case where no buffer is allocated and the window is not empty.
    pub fn set_output_window(&mut self, pos: usize, have: c_uint) -> Result<(), ReturnCode> {
        let end = pos
            .checked_add(widen(have))
            .ok_or(ReturnCode::STREAM_ERROR)?;
        if end > self.out_len() {
            return Err(ReturnCode::STREAM_ERROR);
        }
        self.out_pos = pos;
        self.x.have = have;
        self.refresh_exposed();
        Ok(())
    }

    /// Consumes `n` delivered bytes: advances the cursor and reduces the count.
    ///
    /// The port of the `state->x.next += n; state->x.have -= n;` pair that appears wherever the read
    /// path hands bytes to a caller -- `gzread` after its `memcpy` (`gzread.c` L343-L345), `gzgets`
    /// after finding a line (`gzread.c` L609-L611), and `gz_skip` when discarding buffered output
    /// (`gzread.c` L291-L292). The position is *not* moved: C updates `x.pos` separately, so use
    /// [`GzState::add_pos`] for that and keep the two decisions as separable as they are in the
    /// original.
    ///
    /// # Errors
    ///
    /// [`ReturnCode::STREAM_ERROR`] if `n` exceeds the number of available bytes or would move the
    /// cursor past the end of the output buffer. Both are impossible in a correct caller -- C simply
    /// assumes them -- so the check costs nothing and converts a would-be pointer bug into a status
    /// code.
    pub fn advance_out(&mut self, n: usize) -> Result<(), ReturnCode> {
        let have = widen(self.x.have);
        if n > have {
            return Err(ReturnCode::STREAM_ERROR);
        }
        let next_pos = self
            .out_pos
            .checked_add(n)
            .ok_or(ReturnCode::STREAM_ERROR)?;
        if next_pos > self.out_len() {
            return Err(ReturnCode::STREAM_ERROR);
        }
        // `have - n` cannot exceed the original `have`, so it still fits in a `c_uint`.
        let remaining = narrow(have - n).ok_or(ReturnCode::STREAM_ERROR)?;
        self.out_pos = next_pos;
        self.x.have = remaining;
        self.refresh_exposed();
        Ok(())
    }

    /// The whole output buffer, or an empty slice when none is allocated.
    ///
    /// The bounds-checked replacement for C's bare `state->out` pointer.
    #[must_use]
    pub fn out_slice(&self) -> &[u8] {
        self.output.as_ref().map_or(&[], Buffer::as_slice)
    }

    /// The whole output buffer, mutably, or an empty slice when none is allocated.
    ///
    /// This is where `gz_fetch` loads raw input (`gzread.c` L260) and where `gzungetc` slides
    /// existing data along to make room (`gzread.c` L549-L555).
    pub fn out_slice_mut(&mut self) -> &mut [u8] {
        self.output.as_mut().map_or(&mut [], Buffer::as_mut_slice)
    }

    /// Just the bytes currently available for delivery: `out[out_pos .. out_pos + have]`.
    ///
    /// The bounds-checked replacement for the `(state->x.next, state->x.have)` pair that C copies
    /// from, as in `memcpy(buf, state->x.next, n)` (`gzread.c` L343) and the newline search
    /// `memchr(state->x.next, '\n', n)` (`gzread.c` L604).
    #[must_use]
    pub fn available_out(&self) -> &[u8] {
        let slice = self.out_slice();
        let start = self.out_pos.min(slice.len());
        let end = start.saturating_add(widen(self.x.have)).min(slice.len());
        slice.get(start..end).unwrap_or(&[])
    }

    /// Just the bytes currently available for delivery, mutably.
    ///
    /// Needed by `gzungetc`, which writes the pushed byte through this window
    /// (`gzread.c` L536 and L559).
    pub fn available_out_mut(&mut self) -> &mut [u8] {
        let start = self.out_pos;
        let have = widen(self.x.have);
        let slice = self.out_slice_mut();
        let start = start.min(slice.len());
        let end = start.saturating_add(have).min(slice.len());
        slice.get_mut(start..end).unwrap_or(&mut [])
    }

    /// The index `gzungetc` parks a pushed byte at when the output buffer is empty.
    ///
    /// `gzungetc` handles an empty buffer by placing the byte at the very last position, "allows
    /// more pushing": `state->x.next = state->out + (state->size << 1) - 1` (`gzread.c` L533-L536).
    /// This computes `2 * size - 1` and reports [`None`] when there is no room for it -- when the
    /// buffers have not been allocated, or when the arithmetic would not fit -- so that a caller can
    /// fail cleanly instead of indexing out of range.
    ///
    /// The read path allocates `2 * want` output bytes and sets `size` to `want`
    /// (`gzread.c` L100 and L107), so the index is the last byte of the buffer whenever it is
    /// [`Some`].
    #[must_use]
    pub fn ungetc_park_index(&self) -> Option<usize> {
        let doubled = widen(self.size).checked_mul(2)?;
        let index = doubled.checked_sub(1)?;
        if index < self.out_len() {
            Some(index)
        } else {
            None
        }
    }

    /// Length of the output buffer, or zero when none is allocated.
    fn out_len(&self) -> usize {
        self.output.as_ref().map_or(0, Buffer::len)
    }

    // -------------------------------------------------------------------------
    //  Direction, container handling and read-path flags
    // -------------------------------------------------------------------------

    /// The raw mode value, one of [`GZ_NONE`], [`GZ_READ`], [`GZ_WRITE`] or [`GZ_APPEND`].
    #[must_use]
    pub const fn mode(&self) -> i32 {
        self.mode
    }

    /// Replaces the mode, as the mode-string loop and the append fixup do
    /// (`gzlib.c` L119, L123, L126 and L271).
    pub fn set_mode(&mut self, mode: i32) {
        self.mode = mode;
    }

    /// The mode as an exhaustive [`GzMode`], or [`None`] if the stored value is not a mode at all.
    #[must_use]
    pub const fn mode_typed(&self) -> Option<GzMode> {
        GzMode::from_raw(self.mode)
    }

    /// Whether the mode is one of the two an entry point may act on.
    ///
    /// The port of the integrity test every public `gzFile` function performs before doing anything
    /// else -- `if (state->mode != GZ_READ && state->mode != GZ_WRITE) return -1;`
    /// (`gzlib.c` L335-L336, and the same two lines in `gzseek64`, `gztell64`, `gzoffset64`, `gzeof`,
    /// `gzerror` and `gzclearerr`).
    #[must_use]
    pub const fn has_valid_mode(&self) -> bool {
        self.mode == GZ_READ || self.mode == GZ_WRITE
    }

    /// Whether this file was opened for reading.
    #[must_use]
    pub const fn is_reading(&self) -> bool {
        self.mode == GZ_READ
    }

    /// Whether this file was opened for writing.
    #[must_use]
    pub const fn is_writing(&self) -> bool {
        self.mode == GZ_WRITE
    }

    /// The raw `direct` tri-state: 0 gzip, 1 transparent, -1 gzip only.
    #[must_use]
    pub const fn direct(&self) -> i32 {
        self.direct
    }

    /// Replaces the `direct` tri-state.
    ///
    /// All three values are set from the mode string and then normalised by `gz_open`; see
    /// [`GzState::direct`] for the rules (`gzlib.c` L156-L166 and L179-L197).
    pub fn set_direct(&mut self, direct: i32) {
        self.direct = direct;
    }

    /// Whether data passes through without compression, which is what `gzdirect` reports.
    ///
    /// `gzdirect` returns `state->direct` interpreted as a boolean (`gzread.c` L640-L641), so only
    /// the value 1 counts; the -1 of `"G"` means "gzip only" and is emphatically not transparent.
    #[must_use]
    pub const fn is_transparent(&self) -> bool {
        self.direct == 1
    }

    /// The raw `junk` tri-state: -1 at the start, 1 for a junk candidate, 0 inside a gzip member.
    #[must_use]
    pub const fn junk(&self) -> i32 {
        self.junk
    }

    /// Replaces the `junk` tri-state (`gzlib.c` L75, `gzread.c` L233).
    pub fn set_junk(&mut self, junk: i32) {
        self.junk = junk;
    }

    /// The raw `how` value, one of [`LOOK`], [`COPY`] or [`GZIP`].
    #[must_use]
    pub const fn how(&self) -> i32 {
        self.how
    }

    /// Replaces `how`.
    pub fn set_how(&mut self, how: i32) {
        self.how = how;
    }

    /// `how` as an exhaustive [`GzHow`], or [`None`] if the stored value is not one of the three.
    #[must_use]
    pub const fn how_typed(&self) -> Option<GzHow> {
        GzHow::from_raw(self.how)
    }

    /// Whether the last input or output operation stalled on a non-blocking file.
    ///
    /// The predicate `gz_error` tests before clearing `x.have` (`gzlib.c` L564) and `gzungetc` tests
    /// before rejecting a stream that has an error recorded (`gzread.c` L520).
    #[must_use]
    pub const fn again(&self) -> bool {
        self.again != 0
    }

    /// Records, or clears, the non-blocking stall.
    ///
    /// The port of `state->again = 0` before each write attempt and `state->again = 1` on
    /// `EAGAIN`/`EWOULDBLOCK` (`gzwrite.c` L112 and L118), and of `state->again = 0` in `gz_reset`
    /// (`gzlib.c` L79).
    pub fn set_again(&mut self, again: bool) {
        self.again = i32::from(again);
    }

    /// Where the gzip data started, for rewinding.
    #[must_use]
    pub const fn start(&self) -> ZOff64 {
        self.start
    }

    /// Records the starting position, as `gz_open` does for a read stream (`gzlib.c` L275-L278).
    pub fn set_start(&mut self, start: ZOff64) {
        self.start = start;
    }

    /// Whether the end of the input file has been reached.
    #[must_use]
    pub const fn eof(&self) -> bool {
        self.eof != 0
    }

    /// Records, or clears, end of input (`gzlib.c` L72, L402 and L543, `gzread.c` L45 and L216).
    pub fn set_eof(&mut self, eof: bool) {
        self.eof = i32::from(eof);
    }

    /// Whether a read has been requested past the end of the data, which is what `gzeof` reports.
    #[must_use]
    pub const fn past(&self) -> bool {
        self.past != 0
    }

    /// Records, or clears, the read-past-end condition (`gzread.c` L389 and L598, `gzlib.c` L73, L403 and L544).
    pub fn set_past(&mut self, past: bool) {
        self.past = i32::from(past);
    }

    // -------------------------------------------------------------------------
    //  Write-path parameters
    // -------------------------------------------------------------------------

    /// The requested compression level, possibly `Z_DEFAULT_COMPRESSION`.
    #[must_use]
    pub const fn level(&self) -> i32 {
        self.level
    }

    /// Replaces the compression level.
    ///
    /// Set from a digit in the mode string (`gzlib.c` L114-L115) and by `gzsetparams`. Stored
    /// unvalidated, exactly as C stores it, so that `gzsetparams`' equality short-circuit behaves
    /// identically; the engine validates when it is configured.
    pub fn set_level(&mut self, level: i32) {
        self.level = level;
    }

    /// The requested compression strategy as a raw `Z_*` value.
    #[must_use]
    pub const fn strategy(&self) -> i32 {
        self.strategy
    }

    /// Replaces the compression strategy (`gzlib.c` L144-L155, and `gzsetparams`).
    pub fn set_strategy(&mut self, strategy: i32) {
        self.strategy = strategy;
    }

    /// The strategy as an exhaustive [`Strategy`], or [`None`] if the stored value is not one.
    ///
    /// A stored value outside `Z_DEFAULT_STRATEGY ..= Z_FIXED` can only come from a caller passing
    /// one to `gzsetparams`, which the engine then rejects; this reports it as [`None`] rather than
    /// inventing a strategy.
    #[must_use]
    pub fn strategy_typed(&self) -> Option<Strategy> {
        Strategy::from_raw(self.strategy)
    }

    /// Whether a `deflateReset` is pending after a `Z_FINISH`.
    #[must_use]
    pub const fn reset_pending(&self) -> bool {
        self.reset != 0
    }

    /// Records, or clears, the pending reset (`gzlib.c` L78, `gzwrite.c` L98-L101).
    pub fn set_reset_pending(&mut self, pending: bool) {
        self.reset = i32::from(pending);
    }

    /// The amount still to skip forward before delivering data.
    #[must_use]
    pub const fn skip(&self) -> ZOff64 {
        self.skip
    }

    /// Records a pending skip (`gzlib.c` L80 and L433).
    pub fn set_skip(&mut self, skip: ZOff64) {
        self.skip = skip;
    }

    // -------------------------------------------------------------------------
    //  Buffer sizes
    // -------------------------------------------------------------------------

    /// The working-buffer size, or zero when the buffers have not been allocated.
    #[must_use]
    pub const fn size(&self) -> c_uint {
        self.size
    }

    /// Overwrites the size marker.
    ///
    /// Provided because both C allocation sites manipulate it in an order that matters: `gz_look`
    /// sets it as soon as the buffers exist and puts it back to zero if engine initialisation then
    /// fails (`gzread.c` L107 and L118), while `gz_init` sets it only once everything has succeeded
    /// (`gzwrite.c` L48). Reproducing that sequencing is the caller's business, so the marker is
    /// exposed rather than hidden inside the allocation helpers.
    pub fn set_size(&mut self, size: c_uint) {
        self.size = size;
    }

    /// The requested working-buffer size.
    #[must_use]
    pub const fn want(&self) -> c_uint {
        self.want
    }

    /// Replaces the requested buffer size, as `gzbuffer` does (`gzlib.c` L341).
    ///
    /// The caller performs `gzbuffer`'s own checks -- that no buffer has been allocated yet, that the
    /// size can be doubled, and the floor of 8 (`gzlib.c` L333-L340) -- because they are that
    /// function's policy rather than this structure's invariants.
    pub fn set_want(&mut self, want: c_uint) {
        self.want = want;
    }

    /// Whether the working buffers have been allocated, i.e. whether `size` is non-zero.
    #[must_use]
    pub const fn has_buffers(&self) -> bool {
        self.size != 0
    }

    /// The whole input buffer, or an empty slice when none is allocated.
    #[must_use]
    pub fn in_slice(&self) -> &[u8] {
        self.input.as_ref().map_or(&[], Buffer::as_slice)
    }

    /// The whole input buffer, mutably, or an empty slice when none is allocated.
    ///
    /// This is where `gz_avail` slides leftover input back to the front before refilling
    /// (`gzread.c` L63-L71) and where `gz_load` reads into (`gzread.c` L30).
    pub fn in_slice_mut(&mut self) -> &mut [u8] {
        self.input.as_mut().map_or(&mut [], Buffer::as_mut_slice)
    }

    // -------------------------------------------------------------------------
    //  Error state
    // -------------------------------------------------------------------------

    /// The last error code as a raw `Z_*` value, which is what `gzerror` reports.
    #[must_use]
    pub const fn err(&self) -> i32 {
        self.err
    }

    /// The last error code as a [`ReturnCode`], or [`None`] if the stored value is not one.
    #[must_use]
    pub const fn err_code(&self) -> Option<ReturnCode> {
        ReturnCode::from_i32(self.err)
    }

    /// Replaces the error code without touching the message.
    ///
    /// `gz_error` owns the surrounding policy -- releasing the old message, clearing `x.have` for a
    /// fatal error, declining to allocate for `Z_MEM_ERROR` (`gzlib.c` L555-L590) -- so the two
    /// halves are separate operations here.
    pub fn set_err(&mut self, err: i32) {
        self.err = err;
    }

    /// The last error message, or [`None`] when there is none.
    ///
    /// Already prefixed with the path, because that is the form `gz_error` stores and `gzerror`
    /// returns verbatim (`gzlib.c` L576-L584).
    #[must_use]
    pub fn msg(&self) -> Option<&[u8]> {
        self.msg.as_deref()
    }

    /// Installs, or removes, the error message.
    pub fn set_msg(&mut self, msg: Option<Vec<u8>>) {
        self.msg = msg;
    }

    /// Releases the error message, leaving [`None`].
    ///
    /// The port of the opening lines of `gz_error`, which free any previous message and null the
    /// field (`gzlib.c` L556-L561). C guards the release with `state->err != Z_MEM_ERROR` because a
    /// message recorded for an out-of-memory error was never allocated; here the distinction is
    /// carried by the [`Option`] itself, so no guard is needed.
    pub fn clear_msg(&mut self) {
        self.msg = None;
    }

    /// Builds `"{path}: {msg}"` and installs it as the error message.
    ///
    /// The allocation-and-formatting step of `gz_error`, whose C form is a `malloc` of
    /// `strlen(state->path) + strlen(msg) + 3` bytes followed by
    /// `snprintf(state->msg, ..., "%s%s%s", state->path, ": ", msg)` (`gzlib.c` L576-L584). The
    /// three extra bytes are the two of `": "` plus the terminating NUL; this port stores bytes with
    /// a length instead of a NUL, so it reserves exactly two.
    ///
    /// Everything *around* the formatting stays with `gz_error` in `mod.rs`: which errors get a
    /// message at all, the `Z_MEM_ERROR` early return that deliberately skips allocating
    /// (`gzlib.c` L573-L574), and the promotion of a failed allocation to `Z_MEM_ERROR`
    /// (`gzlib.c` L577-L580).
    ///
    /// # Errors
    ///
    /// [`ReturnCode::MEM_ERROR`] if the message cannot be allocated, which is exactly what
    /// `gz_error` converts such a failure into. The previous message is released first either way,
    /// matching C's order.
    pub fn try_set_prefixed_msg(&mut self, msg: &[u8]) -> Result<(), ReturnCode> {
        self.msg = None;
        let mut built = Vec::new();
        let needed = self
            .path
            .len()
            .checked_add(msg.len())
            .and_then(|len| len.checked_add(2))
            .ok_or(ReturnCode::MEM_ERROR)?;
        built
            .try_reserve_exact(needed)
            .map_err(|_| ReturnCode::MEM_ERROR)?;
        // Capacity is already sufficient for all three pieces, so none of these can reallocate.
        built.extend_from_slice(&self.path);
        built.extend_from_slice(b": ");
        built.extend_from_slice(msg);
        self.msg = Some(built);
        Ok(())
    }

    // -------------------------------------------------------------------------
    //  Path and handle
    // -------------------------------------------------------------------------

    /// The path, or `"<fd:N>"` stand-in, used to prefix error messages.
    #[must_use]
    pub fn path(&self) -> &[u8] {
        &self.path
    }

    /// Copies `path` into the state, replacing whatever was there.
    ///
    /// The port of the path save at `gzlib.c` L199-L226, which allocates `len + 1` bytes and copies
    /// the caller's string in. Reported as a failure rather than aborting, because `gz_open` treats
    /// a failed path allocation as a failed open (`gzlib.c` L207-L209).
    ///
    /// # Errors
    ///
    /// [`ReturnCode::MEM_ERROR`] if the copy cannot be allocated.
    pub fn try_set_path(&mut self, path: &[u8]) -> Result<(), ReturnCode> {
        let mut owned = Vec::new();
        owned
            .try_reserve_exact(path.len())
            .map_err(|_| ReturnCode::MEM_ERROR)?;
        owned.extend_from_slice(path);
        self.path = owned;
        Ok(())
    }

    /// Whether a file handle is installed.
    #[must_use]
    pub const fn has_handle(&self) -> bool {
        self.handle.is_some()
    }

    /// Borrows the file handle mutably, which is how every read, write and seek reaches the file.
    ///
    /// [`None`] once the handle has been taken for closing, which is the state C would express as a
    /// closed descriptor.
    pub fn handle_mut(&mut self) -> Option<&mut (dyn GzHandle + 'a)> {
        // Reborrow through the box so that callers get the trait object rather than the box.
        self.handle.as_deref_mut()
    }

    /// Installs the file handle, returning whatever was there before.
    ///
    /// The port of the two assignments to `state->fd` -- the path-based `open` (`gzlib.c` L248) and
    /// the adopted descriptor (`gzlib.c` L262).
    pub fn set_handle(
        &mut self,
        handle: Option<Box<dyn GzHandle + 'a>>,
    ) -> Option<Box<dyn GzHandle + 'a>> {
        core::mem::replace(&mut self.handle, handle)
    }

    /// Removes the file handle so that a close path can close it and observe the result.
    ///
    /// Both close paths need the outcome of `close` -- it becomes `Z_ERRNO` when it fails
    /// (`gzread.c` L665-L667, `gzwrite.c` L696-L697) -- so the handle is handed over rather than
    /// dropped silently.
    #[must_use]
    pub fn take_handle(&mut self) -> Option<Box<dyn GzHandle + 'a>> {
        self.handle.take()
    }

    // -------------------------------------------------------------------------
    //  The stream and its engine
    // -------------------------------------------------------------------------

    /// Borrows the embedded stream.
    #[must_use]
    pub const fn stream(&self) -> &GzStream<'a, A> {
        &self.strm
    }

    /// Borrows the embedded stream mutably.
    pub fn stream_mut(&mut self) -> &mut GzStream<'a, A> {
        &mut self.strm
    }

    /// Borrows the compressor of a write stream, or [`None`] if this is not one.
    pub fn deflate_state_mut(&mut self) -> Option<&mut DeflateState<'a, A>> {
        match &mut self.strm.engine {
            GzEngine::Deflate(engine) => engine.get_mut(),
            GzEngine::None | GzEngine::Inflate(_) => None,
        }
    }

    /// Borrows the decompressor of a read stream, or [`None`] if this is not one.
    pub fn inflate_state_mut(&mut self) -> Option<&mut InflateState<'a, A>> {
        match &mut self.strm.engine {
            GzEngine::Inflate(engine) => engine.get_mut(),
            GzEngine::None | GzEngine::Deflate(_) => None,
        }
    }

    /// Moves a compressor onto the heap and installs it, replacing any previous engine.
    ///
    /// The state itself is built by `crate::deflate`, exactly as `gz_init` builds it by calling
    /// `deflateInit2(strm, state->level, Z_DEFLATED, MAX_WBITS + 16, DEF_MEM_LEVEL,
    /// state->strategy)` (`gzwrite.c` L36-L37); the `+ 16` is the request for the gzip wrapper, so
    /// the container is the engine's business and none of it is duplicated here.
    ///
    /// # Errors
    ///
    /// [`ReturnCode::MEM_ERROR`] if the indirection cannot be allocated, which is the error
    /// `gz_init` reports for a failed engine initialisation (`gzwrite.c` L38-L43).
    pub fn install_deflate(&mut self, engine: DeflateState<'a, A>) -> Result<(), ReturnCode> {
        self.strm.engine = GzEngine::Deflate(EngineBox::try_new(engine)?);
        Ok(())
    }

    /// Moves a decompressor onto the heap and installs it, replacing any previous engine.
    ///
    /// The counterpart of [`GzState::install_deflate`] for the read path, whose C form is
    /// `inflateInit2(&(state->strm), 15 + 16)` -- "gunzip" (`gzread.c` L115).
    ///
    /// # Errors
    ///
    /// [`ReturnCode::MEM_ERROR`] if the indirection cannot be allocated, which is the error
    /// `gz_look` reports (`gzread.c` L116-L120).
    pub fn install_inflate(&mut self, engine: InflateState<'a, A>) -> Result<(), ReturnCode> {
        self.strm.engine = GzEngine::Inflate(EngineBox::try_new(engine)?);
        Ok(())
    }

    /// Removes the engine and hands it over, leaving [`GzEngine::None`] behind.
    ///
    /// The close paths need the engine by value, because ending it is `deflateEnd`'s or
    /// `inflateEnd`'s job and both consume the state (`gzread.c` L658, `gzwrite.c` L689).
    #[must_use]
    pub fn take_engine(&mut self) -> GzEngine<'a, A> {
        core::mem::take(&mut self.strm.engine)
    }

    /// The allocator every block in this state came from.
    ///
    /// Needed by the modules that build the engines, because an engine's buffers must come from the
    /// same allocator as the rest of the stream's memory.
    #[must_use]
    pub const fn allocator(&self) -> &A {
        &self.allocator
    }

    // -------------------------------------------------------------------------
    //  Buffer allocation and release
    // -------------------------------------------------------------------------

    /// Allocates the read path's working buffers: `want` input bytes and `2 * want` output bytes.
    ///
    /// The port of `gz_look`'s allocation block (`gzread.c` L97-L107). The output buffer is the
    /// doubled one on this path, which is what gives `gzungetc` room to park pushed bytes at
    /// `2 * size - 1` (`gzread.c` L535).
    ///
    /// Both allocations are attempted before either is inspected, exactly as C calls `malloc` twice
    /// and then tests both results, and a partial success is unwound by releasing the output buffer
    /// before the input one -- the last-in-first-out order `gzread.c` L102-L103 uses and
    /// `test/infcover.c`'s `mem_done` checks for.
    ///
    /// `size` is deliberately **not** set here. C sets it only once the surrounding step has
    /// succeeded, and the exact moment differs between the two paths -- `gz_look` sets it before
    /// initialising the engine and puts it back to zero if that fails (`gzread.c` L107 and L118),
    /// whereas `gz_init` sets it afterwards (`gzwrite.c` L48) -- so the marker stays under the
    /// caller's control through [`GzState::set_size`].
    ///
    /// The buffers arrive with unspecified contents; see this module's documentation on why nothing
    /// may assume they are zeroed.
    ///
    /// # Errors
    ///
    /// [`ReturnCode::MEM_ERROR`] if either buffer cannot be allocated, or if `2 * want` does not fit
    /// in a `usize`. This is the `gz_error(state, Z_MEM_ERROR, "out of memory")` of
    /// `gzread.c` L104; recording the message is the caller's step, because `gz_error` owns that
    /// policy.
    pub fn allocate_read_buffers(&mut self) -> Result<(), ReturnCode> {
        let want = widen(self.want);
        let doubled = want.checked_mul(2).ok_or(ReturnCode::MEM_ERROR)?;
        let input = self.allocator.allocate_bytes(want, 1);
        let output = self.allocator.allocate_bytes(doubled, 1);
        match (input, output) {
            (Some(input), Some(output)) => {
                self.input = Some(input);
                self.output = Some(output);
                self.out_pos = 0;
                self.x.have = 0;
                self.refresh_exposed();
                Ok(())
            }
            (input, output) => {
                self.allocator.try_deallocate_bytes(output);
                self.allocator.try_deallocate_bytes(input);
                Err(ReturnCode::MEM_ERROR)
            }
        }
    }

    /// Allocates the write path's working buffers: `2 * want` input bytes, plus `want` output bytes
    /// when actually compressing.
    ///
    /// The port of `gz_init`'s allocation block (`gzwrite.c` L15-L30). The *input* buffer is the
    /// doubled one on this path, for the reason its comment gives -- "double size for `gzprintf`"
    /// (`gzwrite.c` L15) -- and the output buffer is skipped entirely for a transparent stream,
    /// because `if (!state->direct)` guards it (`gzwrite.c` L23): with nothing to compress there is
    /// nothing to stage.
    ///
    /// As with [`GzState::allocate_read_buffers`], `size` is left for the caller to set at the
    /// moment C sets it, and the buffers arrive with unspecified contents.
    ///
    /// # Errors
    ///
    /// [`ReturnCode::MEM_ERROR`] if either buffer cannot be allocated, or if `2 * want` does not fit
    /// in a `usize`. A failure after the input buffer has been obtained releases it before
    /// returning, matching `gzwrite.c` L27.
    pub fn allocate_write_buffers(&mut self) -> Result<(), ReturnCode> {
        let want = widen(self.want);
        let doubled = want.checked_mul(2).ok_or(ReturnCode::MEM_ERROR)?;
        let input = self
            .allocator
            .allocate_bytes(doubled, 1)
            .ok_or(ReturnCode::MEM_ERROR)?;
        self.input = Some(input);

        // `!state->direct` -- a transparent write stream stages nothing, so it needs no output
        // buffer and no engine (`gzwrite.c` L23-L30).
        if self.direct == 0 {
            let Some(output) = self.allocator.allocate_bytes(want, 1) else {
                self.release_buffers();
                return Err(ReturnCode::MEM_ERROR);
            };
            self.output = Some(output);
        }

        self.out_pos = 0;
        self.x.have = 0;
        self.refresh_exposed();
        Ok(())
    }

    /// Releases both working buffers to the allocator that produced them.
    ///
    /// The order is the output buffer and then the input buffer, which is what both close paths do
    /// (`gzread.c` L659-L660, `gzwrite.c` L690-L692) and is the reverse of the allocation order.
    /// Order is observable: `test/infcover.c`'s `mem_done` counts a release that is not
    /// last-in-first-out and reports it (L200-L234).
    ///
    /// The exposed prefix is reset alongside, because a pointer into a buffer that no longer exists
    /// must not be left where the `gzgetc` macro could reach it. `size` is not touched, for the
    /// reason [`GzState::allocate_read_buffers`] gives.
    pub fn release_buffers(&mut self) {
        let output = self.output.take();
        let input = self.input.take();
        self.allocator.try_deallocate_bytes(output);
        self.allocator.try_deallocate_bytes(input);
        self.out_pos = 0;
        self.x.have = 0;
        self.x.next = core::ptr::null_mut();
    }

    /// Releases everything this state owns, in the order the close paths release it.
    ///
    /// The sequence is C's, from `gzclose_r` (`gzread.c` L645-L668) and `gzclose_w`
    /// (`gzwrite.c` L667-L700): end the engine, free the output buffer, free the input buffer, clear
    /// the error, free the path, close the file. What C does last -- `free(state)` -- is the
    /// facade's job, since the facade is what allocated the structure.
    ///
    /// This is the **safety net**, not the primary path. `gzclose_r` and `gzclose_w` need the return
    /// values that two of those steps produce -- `inflateEnd`'s or `deflateEnd`'s status, and
    /// `close`'s, which becomes `Z_ERRNO` when it fails -- so they perform the same steps themselves
    /// with [`GzState::take_engine`], [`GzState::release_buffers`], [`GzState::clear_msg`] and
    /// [`GzState::take_handle`], observing each result. Once they have, this method and [`Drop`]
    /// find nothing left to release, so calling it twice is harmless.
    ///
    /// Ending the engine here means dropping it, which returns its own buffers to the allocator they
    /// came from. It cannot report a status, which is precisely why the close paths do not rely on
    /// this method.
    pub fn teardown(&mut self) {
        drop(self.take_engine());
        self.release_buffers();
        self.err = ReturnCode::OK.as_i32();
        self.msg = None;
        self.path = Vec::new();
        if let Some(mut handle) = self.handle.take() {
            // The result cannot be reported from here; a close path that needs it calls
            // `take_handle` and closes the handle itself. `GzHandle::close` is required to be
            // idempotent, so doing it here as well is safe.
            let _ = handle.close();
        }
    }
}

impl<'a, A: Allocator<'a>> Drop for GzState<'a, A> {
    /// Releases every block, the path, the message and the file, through [`GzState::teardown`].
    ///
    /// C has no equivalent, because every C path that destroys a `gz_state` is a close path that
    /// frees each member by hand. This exists so that a state abandoned on an error path -- a failed
    /// `gz_open` after the path has been copied, say (`gzlib.c` L207-L209) -- still returns its
    /// memory to the allocator that produced it, rather than leaking it where
    /// `test/infcover.c`'s `mem_done` would report it.
    fn drop(&mut self) {
        self.teardown();
    }
}

impl<'a, A: Allocator<'a>> fmt::Debug for GzState<'a, A> {
    /// Reports the state's shape and flags, never buffer contents and never the allocator.
    ///
    /// Buffer contents are user data and the path is a filename, so both are reported as lengths
    /// only -- the same discipline the deflate and inflate state modules follow. The allocator is
    /// omitted because a facade allocator holds a caller's function pointers and need not be
    /// printable, which is also why this implementation places no `Debug` requirement on `A`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GzState")
            .field("x", &self.x)
            .field("mode", &self.mode)
            .field("handle", &self.handle.is_some())
            .field("path_len", &self.path.len())
            .field("size", &self.size)
            .field("want", &self.want)
            .field("in_len", &self.input.as_ref().map(Buffer::len))
            .field("out_len", &self.output.as_ref().map(Buffer::len))
            .field("direct", &self.direct)
            .field("junk", &self.junk)
            .field("how", &self.how)
            .field("again", &self.again)
            .field("start", &self.start)
            .field("eof", &self.eof)
            .field("past", &self.past)
            .field("level", &self.level)
            .field("strategy", &self.strategy)
            .field("reset", &self.reset)
            .field("skip", &self.skip)
            .field("err", &self.err)
            .field("msg_len", &self.msg.as_ref().map(Vec::len))
            .field("strm", &self.strm)
            .field("out_pos", &self.out_pos)
            .finish_non_exhaustive()
    }
}

// -----------------------------------------------------------------------------
//  Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    // The crate denies the panic-prone lints in library code, which is the right policy there and
    // the wrong one here: a test asserts, and an assertion that fails panics. The relaxation is
    // scoped to this module and applies to nothing that ships.
    #![allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

    use super::{
        narrow, widen, EngineBox, GzEngine, GzFileExposed, GzHandle, GzHow, GzIoError, GzMode,
        GzSeekFrom, GzState, ZOff64, COPY, GZBUFSIZE, GZIP, GZ_APPEND, GZ_NONE, GZ_READ, GZ_WRITE,
        LOOK,
    };
    use crate::allocate::GlobalAllocator;
    use crate::config::{Strategy, Z_DEFAULT_COMPRESSION, Z_DEFAULT_STRATEGY};
    use crate::error::ReturnCode;
    use alloc::boxed::Box;
    use alloc::vec::Vec;
    use core::cell::Cell;
    use core::ffi::c_uint;

    /// A handle that does nothing but count how often it was closed.
    ///
    /// Enough to exercise the slot and the teardown order without touching a real file, which keeps
    /// these tests runnable under Miri.
    struct CountingHandle<'c> {
        closes: &'c Cell<usize>,
    }

    impl GzHandle for CountingHandle<'_> {
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
            Ok(())
        }
    }

    /// A fresh state over the global allocator, which is what C's plain `malloc` corresponds to.
    fn fresh() -> GzState<'static, GlobalAllocator> {
        GzState::new(GlobalAllocator)
    }

    /// A read-configured state whose buffers are `want` and `2 * want` bytes.
    fn with_read_buffers(want: c_uint) -> GzState<'static, GlobalAllocator> {
        let mut state = fresh();
        state.set_want(want);
        state.allocate_read_buffers().unwrap();
        state.set_size(want);
        state
    }

    #[test]
    fn exposed_prefix_layout_matches_the_c_measurement() {
        // The `const` assertions in this module already fail the build on drift; this repeats them
        // at run time so that a test run reports the numbers rather than only refusing to compile.
        assert_eq!(core::mem::offset_of!(GzFileExposed, have), 0);
        assert!(core::mem::offset_of!(GzFileExposed, next) >= size_of::<c_uint>());
        assert!(
            core::mem::offset_of!(GzFileExposed, pos)
                >= core::mem::offset_of!(GzFileExposed, next) + size_of::<*mut u8>()
        );
        assert_eq!(
            core::mem::offset_of!(GzState<'static, GlobalAllocator>, x),
            0,
            "the gzgetc macro reads the prefix at offset zero"
        );
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn exposed_prefix_matches_the_lp64_numbers() {
        // Printed by a C program compiled against the unmodified headers in this tree:
        // "gzFile_s: size=24 have=0 next=8 pos=16".
        assert_eq!(size_of::<GzFileExposed>(), 24);
        assert_eq!(core::mem::offset_of!(GzFileExposed, have), 0);
        assert_eq!(core::mem::offset_of!(GzFileExposed, next), 8);
        assert_eq!(core::mem::offset_of!(GzFileExposed, pos), 16);
        assert_eq!(size_of::<ZOff64>(), 8);
    }

    #[test]
    fn constants_match_gzguts_h() {
        assert_eq!(GZBUFSIZE, 8192);
        assert_eq!(GZ_NONE, 0);
        assert_eq!(GZ_APPEND, 1);
        assert_eq!(GZ_READ, 7247);
        assert_eq!(GZ_WRITE, 31153);
        assert_eq!(LOOK, 0);
        assert_eq!(COPY, 1);
        assert_eq!(GZIP, 2);
        // `GZBUFSIZE` and twice it must both fit in a `c_uint` (`gzguts.h` L154-L155).
        assert!(GZBUFSIZE.checked_mul(2).is_some());
    }

    #[test]
    fn new_reproduces_gz_open_initial_values() {
        let state = fresh();
        assert_eq!(state.size(), 0, "gzlib.c L103");
        assert_eq!(state.want(), GZBUFSIZE, "gzlib.c L104");
        assert_eq!(state.err(), ReturnCode::OK.as_i32(), "gzlib.c L105");
        assert!(state.msg().is_none(), "gzlib.c L106");
        assert_eq!(state.mode(), GZ_NONE, "gzlib.c L109");
        assert_eq!(state.level(), Z_DEFAULT_COMPRESSION, "gzlib.c L110");
        assert_eq!(state.strategy(), Z_DEFAULT_STRATEGY, "gzlib.c L111");
        assert_eq!(state.direct(), 0, "gzlib.c L112");
        assert_eq!(state.have(), 0, "gzlib.c L70");
        assert_eq!(state.pos(), 0, "gzlib.c L82");
        assert_eq!(state.how(), LOOK, "gzlib.c L74");
        assert_eq!(state.junk(), -1, "gzlib.c L75");
        assert!(!state.again(), "gzlib.c L79");
        assert_eq!(state.skip(), 0, "gzlib.c L80");
        assert!(!state.eof(), "gzlib.c L72");
        assert!(!state.past(), "gzlib.c L73");
        assert!(!state.reset_pending(), "gzlib.c L78");
        assert_eq!(state.stream().avail_in, 0, "gzlib.c L83");
        assert_eq!(state.start(), 0);
        assert!(!state.has_buffers());
        assert!(!state.has_handle());
        assert!(state.path().is_empty());
        assert!(state.stream().engine.is_none());
        // No buffer yet, so the prefix must point nowhere -- see fact 1.
        assert!(state.x.next.is_null());
    }

    #[test]
    fn refresh_then_resync_round_trips_every_position() {
        let mut state = with_read_buffers(16);
        let len = state.out_slice().len();
        assert_eq!(len, 32, "the read path doubles the output buffer");

        for position in 0..=len {
            state.out_pos = position;
            state.x.have = 0;
            state.refresh_exposed();
            // Scramble the index so that only the pointer can restore it.
            state.out_pos = usize::MAX;
            state.resync_from_exposed().unwrap();
            assert_eq!(state.out_pos, position);
        }
    }

    #[test]
    fn refresh_then_resync_round_trips_a_window_with_data() {
        let mut state = with_read_buffers(8);
        state.set_output_window(3, 5).unwrap();
        assert_eq!(state.out_pos(), 3);
        assert_eq!(state.have(), 5);

        state.out_pos = usize::MAX;
        state.resync_from_exposed().unwrap();
        assert_eq!(state.out_pos(), 3);
        assert_eq!(state.have(), 5);
    }

    #[test]
    fn resync_rejects_a_pointer_outside_the_output_buffer() {
        let mut state = with_read_buffers(8);
        let len = state.out_slice().len();
        // One byte past the end is out of range; `len` itself is not, because C leaves the cursor
        // there once the last byte has been consumed.
        state.x.next = state.out_slice_mut().as_mut_ptr().wrapping_add(len);
        state.x.have = 0;
        state.resync_from_exposed().unwrap();
        assert_eq!(state.out_pos(), len);

        state.x.next = state.out_slice_mut().as_mut_ptr().wrapping_add(len + 1);
        assert_eq!(
            state.resync_from_exposed(),
            Err(ReturnCode::STREAM_ERROR),
            "a pointer past the end must be reported, not trusted"
        );

        // A pointer from an unrelated allocation, which is what a foreign or stale `gzFile` would
        // supply. Never dereferenced -- only compared.
        state.x.next = core::ptr::null_mut::<u8>().wrapping_add(1);
        assert_eq!(state.resync_from_exposed(), Err(ReturnCode::STREAM_ERROR));
    }

    #[test]
    fn resync_rejects_a_count_running_past_the_end() {
        let mut state = with_read_buffers(8);
        let len = state.out_slice().len();
        state.out_pos = len - 2;
        state.refresh_exposed();
        state.x.have = 3;
        assert_eq!(
            state.resync_from_exposed(),
            Err(ReturnCode::STREAM_ERROR),
            "three available bytes cannot start two bytes from the end"
        );
    }

    #[test]
    fn resync_accepts_a_null_pointer_when_nothing_is_available() {
        // Fact 1: with `have` at zero the macro never dereferences `next`, so a null pointer is
        // legitimate -- and it is exactly what a freshly opened file has.
        let mut state = fresh();
        state.resync_from_exposed().unwrap();
        assert_eq!(state.out_pos(), 0);

        let mut state = with_read_buffers(8);
        state.clear_have();
        assert!(state.x.next.is_null());
        state.resync_from_exposed().unwrap();
        assert_eq!(state.out_pos(), 0);
        assert_eq!(state.have(), 0);
    }

    #[test]
    fn resync_rejects_a_null_pointer_when_bytes_are_claimed() {
        let mut state = with_read_buffers(8);
        state.clear_have();
        state.set_have(1);
        assert_eq!(state.resync_from_exposed(), Err(ReturnCode::STREAM_ERROR));

        // The same contradiction without any buffer at all.
        let mut state = fresh();
        state.set_have(1);
        assert_eq!(state.resync_from_exposed(), Err(ReturnCode::STREAM_ERROR));
    }

    #[test]
    fn ungetc_park_index_is_two_size_minus_one() {
        assert!(
            fresh().ungetc_park_index().is_none(),
            "there is nowhere to park a byte before the buffers exist"
        );

        for want in [8_u32, 16, 64, GZBUFSIZE] {
            let state = with_read_buffers(want);
            let index = state.ungetc_park_index().unwrap();
            assert_eq!(index, widen(want) * 2 - 1, "gzread.c L535");
            assert!(
                index < state.out_slice().len(),
                "the park index must be the last byte of the doubled output buffer"
            );
        }

        // The park position must be reachable through the ordinary window machinery.
        let mut state = with_read_buffers(8);
        let index = state.ungetc_park_index().unwrap();
        state.set_output_window(index, 1).unwrap();
        state.available_out_mut()[0] = b'Z';
        state.add_pos(-1);
        assert_eq!(state.available_out(), b"Z");
        assert_eq!(state.pos(), -1, "gzread.c L537 decrements the position");
    }

    #[test]
    fn advance_out_moves_the_cursor_and_reduces_the_count() {
        let mut state = with_read_buffers(8);
        state.out_slice_mut()[..4].copy_from_slice(b"abcd");
        state.set_output_window(0, 4).unwrap();

        state.advance_out(1).unwrap();
        assert_eq!(state.out_pos(), 1);
        assert_eq!(state.have(), 3);
        assert_eq!(state.available_out(), b"bcd");
        // The position is deliberately untouched: C updates `x.pos` in a separate statement.
        assert_eq!(state.pos(), 0);

        state.advance_out(3).unwrap();
        assert_eq!(state.have(), 0);
        assert!(state.available_out().is_empty());
    }

    #[test]
    fn advance_out_rejects_more_than_is_available() {
        let mut state = with_read_buffers(8);
        state.set_output_window(0, 2).unwrap();
        assert_eq!(state.advance_out(3), Err(ReturnCode::STREAM_ERROR));
        assert_eq!(state.advance_out(usize::MAX), Err(ReturnCode::STREAM_ERROR));
        // The rejected calls must leave the window exactly as it was.
        assert_eq!(state.out_pos(), 0);
        assert_eq!(state.have(), 2);
    }

    #[test]
    fn set_output_window_rejects_windows_past_the_end() {
        let mut state = with_read_buffers(8);
        let len = state.out_slice().len();
        state.set_output_window(len, 0).unwrap();
        assert_eq!(
            state.set_output_window(len, 1),
            Err(ReturnCode::STREAM_ERROR)
        );
        assert_eq!(
            state.set_output_window(usize::MAX, 1),
            Err(ReturnCode::STREAM_ERROR)
        );

        // With no buffer only the empty window at zero is coherent.
        let mut state = fresh();
        state.set_output_window(0, 0).unwrap();
        assert_eq!(state.set_output_window(0, 1), Err(ReturnCode::STREAM_ERROR));
    }

    #[test]
    fn available_out_exposes_exactly_the_delivered_window() {
        let mut state = with_read_buffers(8);
        state.out_slice_mut().fill(b'.');
        state.out_slice_mut()[4..7].copy_from_slice(b"xyz");
        state.set_output_window(4, 3).unwrap();
        assert_eq!(state.available_out(), b"xyz");
        state.available_out_mut()[1] = b'Y';
        assert_eq!(state.available_out(), b"xYz");
        assert_eq!(state.out_slice()[5], b'Y');

        assert!(
            fresh().available_out().is_empty(),
            "with no buffer there is nothing to deliver"
        );
    }

    #[test]
    fn read_buffers_double_the_output_and_write_buffers_double_the_input() {
        let mut read = fresh();
        read.set_want(64);
        read.allocate_read_buffers().unwrap();
        assert_eq!(read.in_slice().len(), 64, "gzread.c L99");
        assert_eq!(read.out_slice().len(), 128, "gzread.c L100");

        let mut write = fresh();
        write.set_want(64);
        write.allocate_write_buffers().unwrap();
        assert_eq!(write.in_slice().len(), 128, "gzwrite.c L16");
        assert_eq!(write.out_slice().len(), 64, "gzwrite.c L25");

        // A transparent write stream stages nothing, so it gets no output buffer at all.
        let mut transparent = fresh();
        transparent.set_want(64);
        transparent.set_direct(1);
        transparent.allocate_write_buffers().unwrap();
        assert_eq!(transparent.in_slice().len(), 128);
        assert!(transparent.out_slice().is_empty(), "gzwrite.c L24");
        assert!(transparent.x.next.is_null());
    }

    #[test]
    fn allocation_helpers_leave_the_size_marker_alone() {
        // C sets `state->size` at a different moment on each path, so the marker stays with the
        // caller: `gz_look` before initialising the engine (`gzread.c` L107), `gz_init` after
        // (`gzwrite.c` L48).
        let mut state = fresh();
        state.set_want(32);
        state.allocate_read_buffers().unwrap();
        assert_eq!(state.size(), 0);
        assert!(!state.has_buffers());
        state.set_size(state.want());
        assert!(state.has_buffers());
    }

    #[test]
    fn release_buffers_clears_the_exposed_prefix() {
        let mut state = with_read_buffers(8);
        state.set_output_window(2, 4).unwrap();
        assert!(!state.x.next.is_null());

        state.release_buffers();
        assert!(state.out_slice().is_empty());
        assert!(state.in_slice().is_empty());
        assert_eq!(state.have(), 0);
        assert_eq!(state.out_pos(), 0);
        assert!(
            state.x.next.is_null(),
            "a pointer into a released buffer must never be left where the macro can reach it"
        );
        // Releasing twice is harmless, which is what makes `Drop` a safe net after an explicit
        // close.
        state.release_buffers();
        assert!(state.out_slice().is_empty());
    }

    #[test]
    fn prefixed_message_matches_the_gz_error_format() {
        let mut state = fresh();
        state.try_set_path(b"/tmp/example.gz").unwrap();
        assert_eq!(state.path(), b"/tmp/example.gz");

        state.try_set_prefixed_msg(b"out of memory").unwrap();
        assert_eq!(
            state.msg(),
            Some(&b"/tmp/example.gz: out of memory"[..]),
            "gzlib.c L570-L575 formats \"%s%s%s\" with path, \": \" and the message"
        );

        state.clear_msg();
        assert!(state.msg().is_none());

        // A path need not be valid UTF-8, which is why the field holds bytes.
        let mut state = fresh();
        state.try_set_path(&[0x66, 0xFF, 0x2E, 0x67, 0x7A]).unwrap();
        state.try_set_prefixed_msg(b"bad").unwrap();
        assert_eq!(
            state.msg(),
            Some(&[0x66, 0xFF, 0x2E, 0x67, 0x7A, b':', b' ', b'b', b'a', b'd'][..])
        );
    }

    #[test]
    fn typed_views_round_trip_and_reject_unknown_values() {
        for mode in [GzMode::None, GzMode::Append, GzMode::Read, GzMode::Write] {
            assert_eq!(GzMode::from_raw(mode.as_raw()), Some(mode));
        }
        assert_eq!(GzMode::from_raw(1234), None);
        assert_eq!(GzMode::Read.as_raw(), GZ_READ);
        assert_eq!(GzMode::Write.as_raw(), GZ_WRITE);

        for how in [GzHow::Look, GzHow::Copy, GzHow::Gzip] {
            assert_eq!(GzHow::from_raw(how.as_raw()), Some(how));
        }
        assert_eq!(GzHow::from_raw(3), None);
        assert_eq!(GzHow::default(), GzHow::Look);

        for whence in [GzSeekFrom::Start, GzSeekFrom::Current, GzSeekFrom::End] {
            assert_eq!(GzSeekFrom::from_raw(whence.as_raw()), Some(whence));
        }
        assert_eq!(GzSeekFrom::from_raw(-1), None);

        let mut state = fresh();
        state.set_mode(GZ_READ);
        assert_eq!(state.mode_typed(), Some(GzMode::Read));
        assert!(state.has_valid_mode() && state.is_reading() && !state.is_writing());
        state.set_mode(GZ_WRITE);
        assert!(state.has_valid_mode() && state.is_writing() && !state.is_reading());
        state.set_mode(GZ_NONE);
        assert!(!state.has_valid_mode(), "gzlib.c L335-L336");
        state.set_mode(4321);
        assert_eq!(state.mode_typed(), None);
        assert!(!state.has_valid_mode());

        state.set_how(GZIP);
        assert_eq!(state.how_typed(), Some(GzHow::Gzip));
        state.set_strategy(Strategy::Rle.as_raw());
        assert_eq!(state.strategy_typed(), Some(Strategy::Rle));
        state.set_strategy(99);
        assert_eq!(state.strategy_typed(), None);

        assert_eq!(state.err_code(), Some(ReturnCode::OK));
        state.set_err(ReturnCode::MEM_ERROR.as_i32());
        assert_eq!(state.err_code(), Some(ReturnCode::MEM_ERROR));
        state.set_err(42);
        assert_eq!(state.err_code(), None);
    }

    #[test]
    fn boolean_flags_store_c_integers() {
        let mut state = fresh();
        for flag in [true, false] {
            state.set_again(flag);
            assert_eq!(state.again(), flag);
            state.set_eof(flag);
            assert_eq!(state.eof(), flag);
            state.set_past(flag);
            assert_eq!(state.past(), flag);
            state.set_reset_pending(flag);
            assert_eq!(state.reset_pending(), flag);
        }
        state.set_junk(0);
        assert_eq!(state.junk(), 0);
        state.set_direct(-1);
        assert_eq!(state.direct(), -1);
        assert!(
            !state.is_transparent(),
            "-1 means gzip only, not transparent"
        );
        state.set_direct(1);
        assert!(state.is_transparent(), "gzread.c L640-L641");
        state.set_start(1234);
        assert_eq!(state.start(), 1234);
        state.set_skip(-9);
        assert_eq!(state.skip(), -9);
        state.set_level(9);
        assert_eq!(state.level(), 9);
    }

    #[test]
    fn position_moves_in_both_directions() {
        let mut state = fresh();
        state.add_pos(10);
        assert_eq!(state.pos(), 10);
        state.add_pos(-1);
        assert_eq!(state.pos(), 9, "gzread.c L537");
        state.set_pos(0);
        assert_eq!(state.pos(), 0, "gzlib.c L82");
        // Wrapping rather than trapping, so a debug build cannot abort inside the library.
        state.set_pos(ZOff64::MAX);
        state.add_pos(1);
        assert_eq!(state.pos(), ZOff64::MIN);
    }

    #[test]
    fn engine_box_holds_exactly_one_value() {
        let mut boxed = EngineBox::try_new(7_u32).unwrap();
        assert_eq!(boxed.get(), Some(&7));
        *boxed.get_mut().unwrap() = 9;
        assert_eq!(boxed.into_inner(), Some(9));

        let state = fresh();
        assert!(state.stream().engine.is_none());
        assert!(matches!(state.stream().engine, GzEngine::None));
    }

    #[test]
    fn engine_slots_are_direction_specific() {
        let mut state = fresh();
        assert!(state.deflate_state_mut().is_none());
        assert!(state.inflate_state_mut().is_none());
        // Installing an engine is the business of `read.rs` and `write.rs`, which build the states;
        // what this module owns is that an empty slot answers `None` for both directions and that
        // taking the engine leaves the slot empty again.
        let taken = state.take_engine();
        assert!(taken.is_none());
        assert!(state.stream().engine.is_none());
    }

    #[test]
    fn handle_slot_installs_takes_and_closes() {
        let closes = Cell::new(0);
        let mut state: GzState<'_, GlobalAllocator> = GzState::new(GlobalAllocator);
        assert!(!state.has_handle());

        let previous = state.set_handle(Some(Box::new(CountingHandle { closes: &closes })));
        assert!(previous.is_none());
        assert!(state.has_handle());
        assert_eq!(state.handle_mut().unwrap().write(b"abc").unwrap(), 3);
        assert_eq!(state.handle_mut().unwrap().read(&mut [0_u8; 4]).unwrap(), 0);
        assert_eq!(
            state
                .handle_mut()
                .unwrap()
                .seek(5, GzSeekFrom::Start)
                .unwrap(),
            5
        );
        state.handle_mut().unwrap().set_nonblocking(true).unwrap();

        let mut handle = state.take_handle().unwrap();
        assert!(!state.has_handle());
        handle.close().unwrap();
        assert_eq!(closes.get(), 1);
        drop(handle);
        // The state no longer owns a handle, so teardown must not close anything again.
        state.teardown();
        assert_eq!(closes.get(), 1);
    }

    #[test]
    fn teardown_releases_everything_and_is_idempotent() {
        let closes = Cell::new(0);
        {
            let mut state: GzState<'_, GlobalAllocator> = GzState::new(GlobalAllocator);
            state.set_want(16);
            state.allocate_read_buffers().unwrap();
            state.set_size(16);
            state.try_set_path(b"file.gz").unwrap();
            state.set_err(ReturnCode::DATA_ERROR.as_i32());
            state.try_set_prefixed_msg(b"broken").unwrap();
            state.set_handle(Some(Box::new(CountingHandle { closes: &closes })));

            state.teardown();
            assert!(state.out_slice().is_empty());
            assert!(state.in_slice().is_empty());
            assert!(state.path().is_empty());
            assert!(state.msg().is_none());
            assert_eq!(state.err(), ReturnCode::OK.as_i32());
            assert!(!state.has_handle());
            assert!(state.x.next.is_null());
            assert_eq!(closes.get(), 1);

            state.teardown();
            assert_eq!(closes.get(), 1, "teardown must be idempotent");
        }
        // Dropping the state after an explicit teardown must not close the handle a second time.
        assert_eq!(closes.get(), 1);
    }

    #[test]
    fn drop_is_the_safety_net_for_an_abandoned_state() {
        let closes = Cell::new(0);
        {
            let mut state: GzState<'_, GlobalAllocator> = GzState::new(GlobalAllocator);
            state.set_want(16);
            state.allocate_read_buffers().unwrap();
            state.try_set_path(b"abandoned.gz").unwrap();
            state.set_handle(Some(Box::new(CountingHandle { closes: &closes })));
            // No explicit teardown: this models `gz_open` failing after the path has been copied
            // (`gzlib.c` L207-L209).
        }
        assert_eq!(closes.get(), 1, "Drop must close the file it still owns");
    }

    #[test]
    fn widen_and_narrow_agree() {
        assert_eq!(widen(0), 0);
        assert_eq!(widen(GZBUFSIZE), 8192);
        assert_eq!(widen(c_uint::MAX), usize::try_from(c_uint::MAX).unwrap());
        assert_eq!(narrow(0), Some(0));
        assert_eq!(narrow(8192), Some(8192));
        assert_eq!(narrow(usize::MAX), None);
        assert_eq!(narrow(widen(GZBUFSIZE)), Some(GZBUFSIZE));
    }

    #[test]
    fn debug_reports_shape_without_contents() {
        let mut state = with_read_buffers(8);
        state.try_set_path(b"private-user-path.gz").unwrap();
        state.out_slice_mut().fill(b'Q');
        state.set_output_window(0, 4).unwrap();

        let rendered = alloc::format!("{state:?}");
        assert!(rendered.starts_with("GzState {"));
        assert!(rendered.contains(&alloc::format!(
            "path_len: {}",
            b"private-user-path.gz".len()
        )));
        assert!(rendered.contains("out_len: Some(16)"));
        assert!(rendered.contains("GzEngine::None"));
        assert!(
            !rendered.contains("private-user-path"),
            "the path is reported as a length, never as bytes"
        );
        assert!(
            !rendered.contains("QQQ"),
            "buffer contents are user data and are never rendered"
        );

        // The message slot and the empty-state rendering, for completeness.
        let empty: Vec<u8> = Vec::new();
        assert!(empty.is_empty());
        assert!(alloc::format!("{:?}", fresh()).contains("msg_len: None"));
        assert!(alloc::format!("{:?}", GzIoError::new(11, true)).contains("would_block: true"));
    }
}
