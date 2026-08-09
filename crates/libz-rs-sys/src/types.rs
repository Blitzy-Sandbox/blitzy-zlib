//! The `#[repr(C)]` ABI mirror layer: every type, every hook and every
//! pointer-to-slice conversion the C boundary needs, declared once.
//!
//! This is the foundation of the facade. Every other module in this crate
//! imports from it, and nothing in it is `#[no_mangle]`: it declares *types* and
//! *helpers*, never exported symbols. Adding a `#[no_mangle]` here would put an
//! extra name in the shared object and break the 111-symbol parity diff against
//! the reference build.
//!
//! Getting one field order, one integer width or one nullability wrong here
//! corrupts memory in every program that links the result, silently, so every
//! declaration below cites the `zlib.h`, `zconf.h` or `inftrees.h` line it
//! mirrors and every number was measured on the target rather than assumed.
//!
//! # The measured contract, and the platform it was measured on
//!
//! Produced with `gcc -I. -D_LARGEFILE64_SOURCE=1` over the real headers on
//! `x86_64-unknown-linux-gnu`, reading `sizeof` and `offsetof` directly. The
//! permanent compile-time assertions over these numbers live in
//! `layout_assertions.rs`; this module carries only the two *cross-crate
//! agreement* assertions that no other module is in a position to make.
//!
//! ★ **Every absolute number in the table below is an LP64 measurement, not a
//! portable fact.** `uLong`, `z_size_t`, `z_off_t` and pointers all change width
//! between LP64, LLP64 and ILP32, so on another integer model the sizes and the
//! offsets differ while the *structure* -- field order, which field each offset
//! belongs to, and the relations between them -- does not. Quote these numbers
//! only alongside the model they belong to, and derive anything a program depends
//! on from `size_of` and `offset_of!` rather than from the table. The assertions
//! in `layout_assertions.rs` are written the same way: the absolute values are
//! gated on the LP64 predicate and the relations are asserted unconditionally.
//!
//! | Type | Size (LP64) | Align (LP64) | Field offsets (LP64) |
//! |---|---|---|---|
//! | [`z_stream`] | 112 | 8 | 0, 8, 16, 24, 32, 40, 48, 56, 64, 72, 80, 88, 96, 104 |
//! | [`gz_header`] | 80 | 8 | 0, 8, 16, 20, 24, 32, 36, 40, 48, 56, 64, 68, 72 |
//! | [`gzFile_s`] | 24 | 8 | 0, 8, 16 |
//! | [`code`] | 4 | 2 | 0, 1, 2 |
//!
//! [`code`] is the one row that is platform-independent: its three members are
//! `unsigned char`, `unsigned char` and `unsigned short`, whose widths are fixed
//! on every target this port supports.
//!
//! # ★ The `uLong` hazard: the single most consequential decision in this file
//!
//! `uLong` is `unsigned long` (`zconf.h` L406). That is **8 bytes on this LP64
//! target** -- measured: Rust `c_ulong` is 8, C `unsigned long` is 8 -- and
//! **4 bytes on LLP64 Windows**. Hardcoding `u64` silently corrupts
//! [`z_stream::total_in`], [`z_stream::total_out`], [`z_stream::adler`],
//! [`z_stream::reserved`] and every checksum return value on Windows; hardcoding
//! `u32` does exactly the same damage on Linux. **Only [`core::ffi::c_ulong`] is
//! correct.**
//!
//! The identical reasoning applies to two more aliases, and to one that is
//! deliberately different:
//!
//! * [`z_size_t`] resolves to `size_t`, `unsigned long long` or `unsigned long`
//!   depending on the build (`zconf.h` L253-L270).
//! * [`z_off_t`] resolves to `off_t`, `off64_t`, `__int64`, `offset_t` or
//!   `long long` depending on the platform and on the caller's own
//!   `_FILE_OFFSET_BITS` and `_LARGEFILE64_SOURCE` settings
//!   (`zconf.h` L494-L532).
//! * [`z_off64_t`] is the exception: it is **fixed at 64 bits by definition**, on
//!   every target, which is the whole point of the `*64` API family. It is a
//!   concrete `i64` here rather than a `core::ffi` alias, and that is correct --
//!   it is not a platform-varying type and must not be turned into one.
//!
//! **No future maintainer may "simplify" [`uLong`], [`z_size_t`] or [`z_off_t`]
//! to a fixed-width integer.** A fixed-width spelling compiles everywhere and is
//! wrong on half of it. That is why those three are written in terms of types
//! that track the target -- `c_ulong`, `usize`, `c_long` -- selected by `cfg`
//! where the header itself branches, why [`z_off64_t`] is a concrete `i64`
//! instead, and why the size relationships between all four are asserted rather
//! than assumed.
//!
//! # The `Z_NULL` convention
//!
//! C spells the null value `Z_NULL`, which `zlib.h` defines as the integer `0`
//! and then uses for pointers, for function pointers and for the
//! `Bytef *`/`char *` members alike. Rust has no such single spelling, so the
//! convention here is:
//!
//! | C | Rust |
//! |---|---|
//! | `Z_NULL` in a data-pointer slot | [`core::ptr::null`] / [`core::ptr::null_mut`] |
//! | `Z_NULL` in a function-pointer slot ([`alloc_func`], [`free_func`], [`in_func`], [`out_func`]) | [`None`] |
//! | a test such as `strm->zalloc == Z_NULL` | `matches!(strm.zalloc, None)`, i.e. `.is_none()` |
//!
//! `Z_NULL` itself is deliberately **not** declared in this module. It is one of
//! the `zlib.h` object-like macros, and those belong together in the module that
//! owns the `Z_*` constant block; declaring it twice would put two `#define
//! Z_NULL 0` lines in the generated header and fail the header gate.
//!
//! # Nullable function pointers, and why every one is an `Option`
//!
//! A bare `unsafe extern "C" fn(..)` in Rust is a *non-null* function pointer:
//! the niche at zero is what makes `Option<unsafe extern "C" fn(..)>` occupy
//! exactly one pointer with no discriminant. Passing C's `Z_NULL` through a bare
//! `fn` type is therefore instant undefined behaviour, while
//! `Option<unsafe extern "C" fn(..)>` is an FFI-safe, null-pointer-optimised
//! mirror in which `Z_NULL` *is* [`None`]. All four hook aliases use the
//! `Option` form, and `layout_assertions.rs` pins the no-discriminant
//! property so a future change cannot quietly widen them.
//!
//! Every hook is `unsafe extern "C"` -- calling into caller-supplied C code is
//! unsafe, and the `"C"` ABI is required. **Never `extern "C-unwind"`:** a panic
//! reaching a `"C"` boundary aborts, which is the conservative and correct
//! choice at an FFI edge, whereas `"C-unwind"` would let an unwind cross into a
//! caller that was not compiled to receive one.
//!
//! # Unsafe containment
//!
//! This crate is the only one in the workspace permitted `unsafe`, and it
//! carries `#![deny(unsafe_op_in_unsafe_fn)]`, so every unsafe operation sits in
//! an explicit block with a `// SAFETY:` comment naming its invariant. The type
//! and struct declarations below need no `unsafe` at all. The unsafe work in
//! this module is confined to three of the six categories the crate
//! documentation enumerates, and each is provided *once* so that it has exactly
//! one auditable implementation:
//!
//! | Category | Provided by |
//! |---|---|
//! | 1 -- stream-pointer validation | [`StreamAllocator::from_stream_ptr`], [`copy_stream`], [`streams_are_disjoint`] |
//! | 2 -- input/output slice reconstruction | [`input_slice`], [`output_slots_mut`], [`output_region`] |
//! | 3 -- the opaque `state` round-trip | [`StateBlock`], [`install_state`], [`checked_state`], [`checked_state_mut`], [`take_state`] |
//!
//! ★ **No constructor for a `&z_stream` or a `&mut z_stream` exists here, and
//! none may be added.** One did, and it was removed: two `unsafe` helpers that
//! borrowed the caller's stream survived under `#[allow(dead_code)]` without a
//! single call site, which is audit surface bought with no behaviour. Category 1
//! is discharged entirely through raw places instead: every helper above tests
//! `strm` for null and alignment and then reads or writes one member at a time
//! with [`core::ptr::addr_of!`] or [`core::ptr::addr_of_mut!`]. That is a soundness requirement rather than a
//! style, for the reason the next paragraph records, and the absence of a
//! borrowing constructor is what keeps a future entry point from reintroducing
//! the problem by reaching for the convenient thing.
//!
//! ★ **Nothing in the state round-trip takes a `&mut z_stream`.** A mutable
//! reborrow of the caller's stream invalidates the caller's own [`z_streamp`], and
//! that pointer is exactly what [`StatePrefix::strm`] records and what every later
//! call compares against. The helpers therefore read and write
//! [`z_stream::state`] through raw places and take [`z_streamp`] throughout;
//! [`checked_state`] carries the full account, including the Miri diagnostic that
//! a `&mut z_stream` in that position produces.
//! | 4 -- invoking `zalloc` and `zfree` | [`StreamAllocator`] |
//!
//! Categories 5 (C strings and varargs) and 6 (writing through a caller's
//! [`gz_header`]) belong to `gz.rs`, `util.rs` and `inflate.rs`, which own the
//! entry points that need them.
//!
//! # cbindgen
//!
//! `cbindgen --crate libz-rs-sys` renders this module's declarations as C, for
//! comparison against the immutable `zlib.h`, so the *spellings* here are part of
//! the contract. Generation runs; the comparison is not automated -- `cbindgen.toml`
//! specifies a normalised signature-and-constant comparison and explains why a
//! verbatim `diff` can never be empty, and nothing in the tree implements it yet.
//! Treat the spelling rules below as obligations to honour by hand until it does.
//! Every ABI type uses its exact C name -- `z_stream`,
//! `gz_header`, `gzFile_s`, `uInt`, `uLong`, `voidpf` and the rest -- because
//! cbindgen resolves `c_uint` to `unsigned int` and `c_ulong` to
//! `unsigned long` *before* any renaming, which means the facade's own aliases
//! are the mechanism that puts zlib's spellings into the artifact. The crate
//! root carries `#![allow(non_camel_case_types)]` for exactly this reason;
//! renaming any of them to satisfy Rust's casing convention would break the
//! diff.
//!
//! Two further consequences, both verified against cbindgen 0.29.4 rather than
//! assumed:
//!
//! * A type is emitted only when an exported function reaches it. Nothing in
//!   this module is exported, so the idiomatically-named helpers below
//!   ([`StreamAllocator`], [`StateBlock`], [`StatePrefix`], [`StateKind`]) never
//!   appear in the artifact even though they are `pub`.
//! * A `cfg` that cbindgen cannot evaluate makes it emit *both* arms, producing
//!   a duplicate `typedef`. Only `windows` is present in the generator's
//!   `[defines]` map, so [`z_off_t`] -- the one alias whose width genuinely
//!   varies -- is gated on `windows` alone, which renders as
//!   `#if defined(_WIN32)` exactly as `zconf.h` writes it.
//! * ★ **An item doc comment must never contain the character pairs that open or
//!   close a C block comment.** `documentation_style = "c"` wraps each item's
//!   documentation in a C block comment, so a nested one terminates the enclosing
//!   comment early and the prose that follows it lands in the artifact as code.
//!   That makes the generated header fail to compile -- observed, not theorised.
//!   Inside a fenced C example, use a line comment or put the remark outside the
//!   fence, as the note on [`StatePrefix`] does. Module-level `//!` documentation
//!   is not emitted and is therefore unaffected.
//!
//! Compiling the generated artifact is the check that catches a violation of the
//! rule directly above, and it is worth re-running after any change to an exported
//! item's documentation. It compiles under
//! `gcc -std=c89 -pedantic -Wall -Wextra`, `-std=c99`, `-std=c17` and
//! `g++ -std=c++17`, the last of which exercises the generator's `cpp_compat`
//! setting.

// ★ This module is PRIVATE, and the ABI types it declares reach a Rust consumer
// only through the crate root's `pub use` list -- which carries the C contract and
// stops there, leaving `StreamAllocator`, the raw-slice constructors and the state
// round-trip `pub(crate)`. So an outside consumer writes `z::z_stream`, never
// `z::types::z_stream`, and cannot write the latter at all: the crate documentation
// carries `compile_fail` examples that hold that line.
//
// The extern crate name in that path is `z`, not `libz_rs_sys`. `Cargo.toml`
// declares `[lib] name = "z"` so that the build emits `libz.so` and `libz.a`, and a
// library target's name is also its extern crate name, which is what this crate's
// own integration tests and doctests link it under. `zlib-rs-differential`, the fuzz
// targets and the benches rename the dependency and write `libz_rs_sys::…`; the
// package name `libz-rs-sys` appears only in a dependency line or a `cargo -p`
// argument.

use core::ffi::{c_char, c_int, c_uint, c_ulong, c_void};
use core::mem::{align_of, size_of};
// Only the write-only output model names it, and that model belongs to the exported
// surface: the ABI mirrors are ordinary initialised structs.
#[cfg(feature = "libz-compat")]
use core::mem::MaybeUninit;

// The boundary machinery below -- the allocator adapter, the raw-slice constructors
// and the opaque-state round-trip -- serves the exported `extern "C"` surface and
// nothing else, so it is gated on the feature that turns that surface on, and so are
// the imports only it needs. The ABI mirrors are NOT gated: `--no-default-features`
// is a supported configuration that yields exactly them, which is what makes it
// useful to a consumer that wants `z_stream`'s layout without linking any symbol.
#[cfg(feature = "libz-compat")]
use core::ptr::NonNull;

#[cfg(feature = "libz-compat")]
use zlib_rs::allocate::{
    block_len, Allocator, AllocatorId, Buffer, ForeignBlock, Opaque, SENTINEL_FILL,
};
#[cfg(feature = "libz-compat")]
use zlib_rs::deflate::deflate_state_check;
#[cfg(feature = "libz-compat")]
use zlib_rs::error::ReturnCode;
// Gated exactly as the core gates the module: `zlib_rs::gz` requires
// `zlib-rs/std`, which this crate's `gz` feature turns on. The import serves only
// the layout agreement assertions further down; `gzFile_s` itself is
// unconditional, because it is an ABI type and the generated header must always
// carry it.
#[cfg(feature = "gz")]
use zlib_rs::gz::state::GzFileExposed;
#[cfg(feature = "libz-compat")]
use zlib_rs::inflate::inflate_state_check;
use zlib_rs::inflate::inftrees::{Code, CodeType};
// Both serve the write-only output model the exported entry points use, so they are
// gated with that surface: with `libz-compat` off there is no `next_out` to describe.
#[cfg(feature = "libz-compat")]
use zlib_rs::read_buf::{InitView, OutputRegion};

// ---------------------------------------------------------------------------
// Primitive type aliases -- `zconf.h`
// ---------------------------------------------------------------------------
//
// `FAR` is empty on every target this crate supports (`zconf.h` L398-L400
// defines it to nothing whenever it is not already defined, and only the 16-bit
// DOS and OS/2 configurations ever defined it), so the `FAR` in a declaration
// such as `typedef Byte FAR Bytef` contributes nothing to the Rust form. The
// header gate strips the token from the reference side for the same reason.

/// `unsigned char` -- `zconf.h` L403.
///
/// The comment there reads "8 bits", and the guard above it exempts Mac OS
/// Classic's `__MACTYPES__`, which supplies its own `Byte`.
pub type Byte = u8;

/// `Byte FAR` -- `zconf.h` L412.
///
/// Written as an alias *of* [`Byte`] rather than of `u8` directly so that the
/// generated header reproduces `zconf.h`'s own typedef chain
/// (`Bytef` -> `Byte` -> `unsigned char`) instead of collapsing it.
pub type Bytef = Byte;

/// `unsigned int` -- `zconf.h` L405.
///
/// The comment there reads "16 bits or more"; it is 4 bytes on every target this
/// crate supports (measured). This is the type of every `avail_*` count, so it
/// is [`core::ffi::c_uint`] and never a fixed-width integer.
pub type uInt = c_uint;

/// `uInt FAR` -- `zconf.h` L416.
pub type uIntf = uInt;

/// `unsigned long` -- `zconf.h` L406.
///
/// ★ **8 bytes on LP64, 4 bytes on LLP64 Windows.** See the module-level hazard
/// note: [`core::ffi::c_ulong`] is the only correct spelling, and substituting a
/// fixed-width integer corrupts [`z_stream::total_in`],
/// [`z_stream::total_out`], [`z_stream::adler`] and every checksum return value
/// on one platform family or the other.
pub type uLong = c_ulong;

/// `uLong FAR` -- `zconf.h` L417.
pub type uLongf = uLong;

/// `char FAR` -- `zconf.h` L414.
pub type charf = c_char;

/// `int FAR` -- `zconf.h` L415.
pub type intf = c_int;

/// `void const *` -- `zconf.h` L420 (the `STDC` branch).
///
/// The non-`STDC` branch at L424 spells the same slot `Byte const *`; both are
/// one pointer and the distinction cannot reach a Rust build.
pub type voidpc = *const c_void;

/// `void FAR *` -- `zconf.h` L421 (the `STDC` branch).
///
/// This is the type of [`z_stream::opaque`] and of both the argument and the
/// result of [`alloc_func`].
pub type voidpf = *mut c_void;

/// `void *` -- `zconf.h` L422 (the `STDC` branch).
///
/// Distinct from [`voidpf`] only in that the C declaration omits `FAR`, which is
/// empty here. `gzread` and `gzwrite` are declared in terms of it.
pub type voidp = *mut c_void;

/// `Z_U4`, i.e. `unsigned` -- `zconf.h` L429-L444.
///
/// `Z_U4` is chosen by preprocessor arithmetic: `unsigned` when
/// `UINT_MAX == 0xffffffff`, otherwise `unsigned long`, otherwise
/// `unsigned short`, with a fall-through to `unsigned long` when none matches.
/// The first test holds on every target this crate supports -- verified
/// directly, `UINT_MAX == 0xffffffff` is true -- so `z_crc_t` is a 32-bit
/// unsigned integer, which is also what the CRC-32 tables and
/// `get_crc_table` require.
pub type z_crc_t = u32;

/// `size_t` -- `zconf.h` L265 (the `STDC` branch of L253-L270).
///
/// The other branches spell it `unsigned long long` (`Z_SOLO` on `_WIN64`) or
/// `unsigned long`; `STDC` is the branch every ordinary build takes, and there
/// it is `size_t`, whose Rust mirror is [`usize`]. The generator is configured
/// with `usize_is_size_t`, so this alias renders as `typedef size_t z_size_t;`
/// -- matching `zconf.h` exactly.
///
/// This is the type of the `_z` entry points (`compressBound_z`,
/// `deflateBound_z`, `compress_z`, `compress2_z`, `uncompress_z`,
/// `uncompress2_z`), of `adler32_z` and `crc32_z`, and of `gzfread` and
/// `gzfwrite`.
pub type z_size_t = usize;

/// `off_t` on Windows -- `zconf.h` L518-L520, where `z_off_t` falls through to
/// `long long` because `Z_HAVE_UNISTD_H` is never set on `_WIN32` (L483-L487
/// requires `!defined(_WIN32)`).
///
/// See the non-Windows definition for the full rationale and for the one case
/// this alias cannot track.
#[cfg(windows)]
pub type z_off_t = i64;

/// `off_t` -- `zconf.h` L494-L496.
///
/// Under the reference build flags (`-D_LARGEFILE64_SOURCE=1`) `Z_HAVE_UNISTD_H`
/// is set, `<unistd.h>` is included, and `z_off_t` is `#define`d to `off_t`.
/// glibc and the BSDs define `off_t` as `long`, so [`core::ffi::c_long`] is the
/// faithful mirror: 8 bytes on LP64 (measured) and 4 bytes on 32-bit, which is
/// exactly how `off_t` behaves there.
///
/// ⚠ The one case no single Rust alias can track: a **32-bit** caller compiled
/// with `-D_FILE_OFFSET_BITS=64` gets a 64-bit `off_t`. That is a property of
/// the *caller's* translation unit, not of the library, and `zlib.h`
/// L1987-L2013 handles it by redirecting `gzseek`, `gztell`, `gzoffset` and the
/// combine family to their `*64` counterparts, which are declared in terms of
/// the always-64-bit [`z_off64_t`]. Exporting both symbol families -- which this
/// port does -- is what makes the redirection work, and it is why the wide
/// entry points must never be declared in terms of this alias.
#[cfg(not(windows))]
pub type z_off_t = core::ffi::c_long;

/// `off64_t` -- `zconf.h` L522-L532.
///
/// Every branch of that block is 64 bits wide on a platform that has large-file
/// support at all: `off64_t` on non-Windows with `Z_LARGE64`, `long long` on
/// MinGW, `__int64` on MSVC, `offset_t` on DJGPP. Measured at 8 bytes here, and
/// declared unconditionally so that it agrees with the core's `ZOff64`
/// (`zlib_rs::gz::state`), which the core also fixes at 64 bits because
/// [`gzFile_s::pos`] is part of the caller-visible `gzgetc` prefix and cannot be
/// allowed to vary.
///
/// The final `#else` at L531 falls back to `z_off_t`, which is reachable only on
/// a platform with neither large-file support nor a Windows toolchain -- outside
/// the Tier-1 set this port targets, and outside what a 24-byte [`gzFile_s`]
/// could express in any case.
pub type z_off64_t = i64;

// `uInt` is widened to `usize` on every entry path in this crate, so the
// relationship is a build-time guarantee rather than a hope. It holds on every
// target Rust supports for this crate: `uInt` is `unsigned int` (4 bytes) and
// `usize` is at least 4 bytes wherever `std` and a C ABI are available.
/// cbindgen:ignore
const _: () = assert!(size_of::<uInt>() <= size_of::<usize>());

// `z_size_t` must be exactly `size_t`, because the `_z` entry points are
// declared in terms of it and a narrower or wider spelling would change their
// signatures rather than merely their range.
/// cbindgen:ignore
const _: () = assert!(size_of::<z_size_t>() == size_of::<usize>());

// ---------------------------------------------------------------------------
// Function-pointer aliases -- `zlib.h`
// ---------------------------------------------------------------------------

/// `voidpf (*alloc_func)(voidpf opaque, uInt items, uInt size)` -- `zlib.h` L85.
///
/// The caller's allocation hook. [`None`] is C's `Z_NULL`, which asks the library
/// to substitute its own routines (`zlib.h` L151-L153).
///
/// `zlib.h` L149 requires the hook to return `Z_NULL` when it cannot satisfy a
/// request, which arrives here as a null return that
/// `StreamAllocator::allocate_bytes` converts to [`None`] and the caller then
/// reports as [`zlib_rs::error::ReturnCode::MEM_ERROR`].
///
/// `unsafe` because invoking caller-supplied C code is unsafe, and `extern "C"`
/// rather than `extern "C-unwind"` because an unwind must never cross this edge.
//
// The `ReturnCode` link above is spelled in full, and must stay that way. This
// alias is an ABI mirror, so it is compiled in every configuration, whereas the
// `ReturnCode` import is gated with the boundary machinery that uses it; a bare
// `[ReturnCode::MEM_ERROR]` therefore fails to resolve in a
// `--no-default-features` documentation build. Kept as a plain comment rather
// than a doc comment so that the rustdoc mechanics do not reach the generated C
// header, where they would mean nothing to a C reader.
pub type alloc_func = Option<unsafe extern "C" fn(voidpf, uInt, uInt) -> voidpf>;

/// `void (*free_func)(voidpf opaque, voidpf address)` -- `zlib.h` L86.
///
/// The caller's release hook, and the *only* route by which a block obtained
/// from that caller's [`alloc_func`] may be returned. [`None`] is `Z_NULL`.
pub type free_func = Option<unsafe extern "C" fn(voidpf, voidpf)>;

/// `unsigned (*in_func)(void FAR *, z_const unsigned char FAR * FAR *)` --
/// `zlib.h` L1134-L1135.
///
/// `inflateBack`'s input callback. It receives the `in_desc` cookie and a
/// location into which it stores a pointer to the next input block, returning
/// the number of bytes available there, or zero to signal that no more input
/// exists (`zlib.h` L1163-L1166).
pub type in_func = Option<unsafe extern "C" fn(*mut c_void, *mut *const u8) -> c_uint>;

/// `int (*out_func)(void FAR *, unsigned char FAR *, unsigned)` -- `zlib.h`
/// L1136.
///
/// `inflateBack`'s output callback. It receives the `out_desc` cookie, a pointer
/// to the bytes to write and their count, and returns non-zero on failure
/// (`zlib.h` L1167-L1169).
pub type out_func = Option<unsafe extern "C" fn(*mut c_void, *mut u8, c_uint) -> c_int>;

// ---------------------------------------------------------------------------
// `struct internal_state` -- `zlib.h` L88
// ---------------------------------------------------------------------------

/// The opaque compression or decompression state, declared but never defined.
///
/// `zlib.h` L88 is a bare forward declaration -- `struct internal_state;` -- and
/// L100 uses it only as `struct internal_state FAR *state`. It is an *incomplete
/// type*: a caller can hold and copy the pointer but can never look through it,
/// which is precisely the contract the comment at L100 states ("not visible by
/// applications").
///
/// The stable-Rust way to express a type with no layout is a struct whose only
/// field is private and zero-sized, which is what an `extern type` would give
/// once that feature stabilises. Two properties follow, and both are wanted:
/// the type cannot be constructed or read from outside this module, and cbindgen
/// renders it as `typedef struct internal_state internal_state;` -- a genuine C
/// incomplete-type declaration -- so [`z_stream::state`] renders as
/// `internal_state *state;`, matching `zlib.h` once the empty `FAR` is stripped.
///
/// Deliberately **not** `#[repr(C)]`: adding that attribute makes the generator
/// emit the zero-length-array form `typedef struct { uint8_t _opaque[0]; }
/// internal_state;`, which is a GCC extension rather than valid ISO C. Since the
/// type is only ever the pointee of a raw pointer, it has no layout to specify.
///
/// It is equally deliberately **not** a `c_void` alias: `*mut c_void` renders as
/// `void *state;`, which loses the name `zlib.h` publishes and would make the
/// struct's rendered form diverge from the contract.
#[derive(Debug)]
pub struct internal_state {
    /// Occupies no space and cannot be named outside this module, so the type is
    /// uninhabitable in practice: there is no way to obtain a value of it.
    _opaque: [u8; 0],
}

// ---------------------------------------------------------------------------
// `z_stream` -- `zlib.h` L90-L112
// ---------------------------------------------------------------------------

/// The compression and decompression stream: `zlib.h` L90-L110.
///
/// **Measured on LP64: 112 bytes, align 8**, with the field offsets given on each
/// member below. Those absolute numbers belong to that integer model: `uLong` is
/// 4 bytes on LLP64 and pointers are 4 bytes on ILP32, so both the size and the
/// offsets shrink there. What holds on **every** target is the structure: every
/// field is `pub`, in the exact declaration order of the C struct, and nothing may
/// be reordered, added or removed. Derive any number a program depends on from
/// `size_of::<z_stream>()` and `offset_of!`, never from the figures quoted here.
///
/// # Who owns which field
///
/// `zlib.h` L138-L143 divides the struct: the application updates `next_in` and
/// `avail_in` when the input is exhausted and `next_out` and `avail_out` when the
/// output fills, it must initialise `zalloc`, `zfree` and `opaque` before calling
/// an init function, and "all other fields are set by the compression library and
/// must not be updated by the application".
///
/// # `Copy`, and why not `Default`
///
/// [`Clone`] and [`Copy`] are derived because `deflateCopy` and `inflateCopy`
/// duplicate the whole struct, hooks and `opaque` included, before allocating the
/// copy's own buffers. Neither derive affects layout.
///
/// [`Default`] is deliberately **not** derived. A zeroed `z_stream` is not a
/// valid stream: it has no state, and `zlib.h` L248's version check plus the
/// `zalloc`/`zfree` substitution at `deflate.c` L401-L414 must run before any
/// entry point will accept it. Offering a `Default` would advertise the opposite.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct z_stream {
    /// Offset 0. `z_const Bytef *next_in` -- the next input byte.
    ///
    /// `zlib.h` L91. Typed `*const` because the library never writes through it;
    /// with `ZLIB_CONST` defined the C declaration is literally
    /// `const Bytef *`, and `const` is not part of the ABI either way.
    ///
    /// May be null while [`z_stream::avail_in`] is zero, which is why
    /// `input_slice` branches on the length before forming a slice.
    pub next_in: *const Bytef,
    /// Offset 8. `uInt avail_in` -- bytes available at [`z_stream::next_in`].
    ///
    /// `zlib.h` L92.
    pub avail_in: uInt,
    /// Offset 16. `uLong total_in` -- total input bytes read so far.
    ///
    /// `zlib.h` L93. [`uLong`], never a fixed-width integer; see the module
    /// hazard note.
    pub total_in: uLong,
    /// Offset 24. `Bytef *next_out` -- where the next output byte goes.
    ///
    /// `zlib.h` L95. May be null while [`z_stream::avail_out`] is zero.
    pub next_out: *mut Bytef,
    /// Offset 32. `uInt avail_out` -- free space remaining at
    /// [`z_stream::next_out`].
    ///
    /// `zlib.h` L96.
    pub avail_out: uInt,
    /// Offset 40. `uLong total_out` -- total bytes output so far.
    ///
    /// `zlib.h` L97.
    pub total_out: uLong,
    /// Offset 48. `z_const char *msg` -- the last error message, or null.
    ///
    /// `zlib.h` L99. The library stores only `'static` strings here -- the
    /// `z_errmsg` table of `zutil.c` L13 and the literals in the deflate and
    /// inflate sources -- so the pointee always outlives the stream and is never
    /// written through.
    pub msg: *const c_char,
    /// Offset 56. `struct internal_state FAR *state` -- opaque to applications.
    ///
    /// `zlib.h` L100. Caller-visible but caller-opaque: it is read back and
    /// tag-validated on every entry, which is what
    /// `checked_state`/`checked_state_mut` do and why `deflateStateCheck`
    /// (`deflate.c` L538) and `inflateStateCheck` (`inflate.c` L88) exist in C.
    pub state: *mut internal_state,
    /// Offset 64. `alloc_func zalloc` -- used to allocate the internal state.
    ///
    /// `zlib.h` L102. [`None`] is `Z_NULL`.
    pub zalloc: alloc_func,
    /// Offset 72. `free_func zfree` -- used to free the internal state.
    ///
    /// `zlib.h` L103. [`None`] is `Z_NULL`.
    pub zfree: free_func,
    /// Offset 80. `voidpf opaque` -- private data passed to `zalloc` and `zfree`.
    ///
    /// `zlib.h` L104. `zlib.h` L146-L147 states that the library "attaches no
    /// meaning to the `opaque` value", and this port takes that literally: the
    /// pointer is carried and handed back, never dereferenced and never required
    /// to be non-null.
    pub opaque: voidpf,
    /// Offset 88. `int data_type` -- the data-type guess, or the inflate state.
    ///
    /// `zlib.h` L106-L107: "best guess about the data type: binary or text for
    /// deflate, or the decoding state for inflate".
    pub data_type: c_int,
    /// Offset 96. `uLong adler` -- Adler-32 or CRC-32 of the uncompressed data.
    ///
    /// `zlib.h` L108. [`uLong`], so 8 bytes here and 4 on LLP64 Windows.
    pub adler: uLong,
    /// Offset 104. `uLong reserved` -- reserved for future use.
    ///
    /// `zlib.h` L109. Present because it is part of the 112-byte layout; the
    /// library neither reads nor writes it.
    pub reserved: uLong,
}

/// `z_stream FAR *` -- `zlib.h` L112.
///
/// The parameter type of every stream entry point. Nullability is part of the
/// published contract: a null stream yields `Z_STREAM_ERROR`, so every helper in
/// this module tests the pointer for null and alignment before it reads through
/// it, and reads one member at a time through a raw place rather than forming a
/// reference to the whole struct.
pub type z_streamp = *mut z_stream;

// ---------------------------------------------------------------------------
// `gz_header` -- `zlib.h` L118-L135
// ---------------------------------------------------------------------------

/// gzip header information passed to and from the library: `zlib.h` L118-L133.
///
/// **Measured on LP64: 80 bytes, align 8.** Note two layout facts that are easy
/// to get wrong, both of which are consequences of [`uLong`]'s width and therefore
/// LP64-specific: `time` lands at offset **8, not 4**, because an 8-byte `uLong`
/// must be aligned after the leading `int text`; and the struct carries **4 bytes
/// of tail padding** after `done`, which is why 80 rather than 76. Where `uLong`
/// is 4 bytes wide neither adjustment applies and the struct is correspondingly
/// smaller -- which is exactly why nothing may hard-code 80.
///
/// `zlib.h` L115-L116 refers to RFC 1952 for the meaning of the fields.
///
/// # The `*_max` fields are a bound on *the library's* writes
///
/// `extra_max`, `name_max` and `comm_max` are documented as "space at ... (only
/// when reading header)". `inflateGetHeader` fills the three caller buffers and
/// every write must be clamped to the matching maximum -- historically the source
/// of gzip-header overflow defects, and the reason those writes are unsafe-site
/// category 6 rather than ordinary slice work.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct gz_header {
    /// Offset 0. `int text` -- true if the data is believed to be text.
    ///
    /// `zlib.h` L119.
    pub text: c_int,
    /// Offset 8. `uLong time` -- the modification time.
    ///
    /// `zlib.h` L120. At offset 8 rather than 4 because of [`uLong`]'s
    /// alignment; a mirror that placed it at 4 would misread every subsequent
    /// field.
    pub time: uLong,
    /// Offset 16. `int xflags` -- extra flags, unused when writing.
    ///
    /// `zlib.h` L121.
    pub xflags: c_int,
    /// Offset 20. `int os` -- the operating-system identifier.
    ///
    /// `zlib.h` L122.
    pub os: c_int,
    /// Offset 24. `Bytef *extra` -- the extra field, or `Z_NULL` if none.
    ///
    /// `zlib.h` L123.
    pub extra: *mut Bytef,
    /// Offset 32. `uInt extra_len` -- extra-field length, valid when
    /// [`gz_header::extra`] is non-null.
    ///
    /// `zlib.h` L124.
    pub extra_len: uInt,
    /// Offset 36. `uInt extra_max` -- space available at
    /// [`gz_header::extra`], honoured only when reading a header.
    ///
    /// `zlib.h` L125.
    pub extra_max: uInt,
    /// Offset 40. `Bytef *name` -- the NUL-terminated file name, or `Z_NULL`.
    ///
    /// `zlib.h` L126.
    pub name: *mut Bytef,
    /// Offset 48. `uInt name_max` -- space available at [`gz_header::name`].
    ///
    /// `zlib.h` L127.
    pub name_max: uInt,
    /// Offset 56. `Bytef *comment` -- the NUL-terminated comment, or `Z_NULL`.
    ///
    /// `zlib.h` L128.
    pub comment: *mut Bytef,
    /// Offset 64. `uInt comm_max` -- space available at
    /// [`gz_header::comment`].
    ///
    /// `zlib.h` L129.
    pub comm_max: uInt,
    /// Offset 68. `int hcrc` -- true if a header CRC was or will be present.
    ///
    /// `zlib.h` L130.
    pub hcrc: c_int,
    /// Offset 72. `int done` -- true once the header has been fully read.
    ///
    /// `zlib.h` L131-L132; unused when writing a gzip file. Four bytes of tail
    /// padding follow, bringing the struct to 80.
    pub done: c_int,
}

/// `gz_header FAR *` -- `zlib.h` L135.
///
/// The parameter type of `deflateSetHeader` and `inflateGetHeader`.
pub type gz_headerp = *mut gz_header;

// ---------------------------------------------------------------------------
// `struct gzFile_s` -- `zlib.h` L1956-L1960
// ---------------------------------------------------------------------------

/// The caller-visible prefix of the `gzFile` state: `zlib.h` L1956-L1960.
///
/// ```c
/// struct gzFile_s {
///     unsigned have;
///     unsigned char *next;
///     z_off64_t pos;
/// };
/// ```
///
/// **Measured on LP64: 24 bytes**, `have` at 0, `next` at 8, `pos` at 16. The
/// field *order* is the ABI and holds everywhere; the three offsets follow from
/// pointer width and [`z_off64_t`]'s fixed 64 bits, so on a 32-bit target they are
/// 0, 4 and 8 and the struct is 16 bytes. A caller's `gzgetc` macro computes them
/// with its own compiler, so agreement is what matters, not the specific values.
///
/// # ★ Why this is the highest-risk layout in the entire port
///
/// `gzgetc` is a **macro**, not merely a function. `zlib.h` L1966-L1968:
///
/// ```c
/// #define gzgetc(g) \
///       ((g)->have ? ((g)->have--, (g)->pos++, *((g)->next)++) : (gzgetc)(g))
/// ```
///
/// with an identical `z_gzgetc` under `Z_PREFIX_SET` at L1963-L1965, and a
/// `gzgetc_` function at L1961 kept for callers that took the function's address
/// before the macro existed.
///
/// That field arithmetic is compiled into **caller object code**, which this port
/// cannot change and cannot even see. Every program that uses `gzgetc` therefore
/// contains hard-coded loads and stores at offsets 0, 8 and 16 of whatever
/// `gzFile` points at. The real state must consequently *begin* with this exact
/// 24-byte prefix, mirroring `gzguts.h` L169-L172, where `gz_state` embeds
/// `struct gzFile_s x;` as its first member -- "x" for exposed. Any other layout
/// produces silent memory corruption in every such caller rather than a
/// diagnosable failure.
///
/// The header's own warning at L1949-L1954 is worth repeating: the fields exist
/// only for the macro, and their names and behaviour "could change in the future,
/// perhaps even capriciously".
///
/// # Agreement with the core
///
/// The core's `GzFileExposed` (`zlib_rs::gz::state`) is its form of the same
/// prefix and is what actually sits at the head of the allocated state. The two
/// declarations must agree exactly, which the assertions below make a build
/// failure rather than a runtime surprise.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct gzFile_s {
    /// Offset 0. `unsigned have` -- bytes available at [`gzFile_s::next`].
    ///
    /// `zlib.h` L1957; `gzguts.h` L173 documents it as "number of bytes
    /// available at `x.next`". The caller's `gzgetc` macro **decrements** this
    /// field directly, and `gz_error` clears it to zero (`gzlib.c` L564-L565)
    /// specifically so that the macro stops taking its fast path after a fatal
    /// error.
    pub have: c_uint,
    /// Offset 8. `unsigned char *next` -- the next byte to deliver or write.
    ///
    /// `zlib.h` L1958; `gzguts.h` L174. The caller's `gzgetc` macro
    /// **post-increments** this pointer directly. Null while no buffer is
    /// allocated, which is harmless because [`gzFile_s::have`] is then zero and
    /// the macro takes its function branch.
    ///
    /// Spelled `*mut u8` rather than `*mut Bytef` to match the core's
    /// `GzFileExposed::next` exactly; the two are the same type, since [`Bytef`]
    /// is [`Byte`] is `u8`.
    pub next: *mut u8,
    /// Offset 16. `z_off64_t pos` -- the position in the *uncompressed* stream.
    ///
    /// `zlib.h` L1959; `gzguts.h` L175. The caller's `gzgetc` macro
    /// **increments** this field directly; `gzungetc` decrements it
    /// (`gzread.c` L537) and `gztell` reports it plus any pending seek
    /// (`gzlib.c` L456-L457).
    pub pos: z_off64_t,
}

/// `struct gzFile_s *` -- `zlib.h` L1354, "semi-opaque gzip file descriptor".
///
/// Semi-opaque is exact: the three fields above are public so that the `gzgetc`
/// macro can reach them, and everything past them is private.
pub type gzFile = *mut gzFile_s;

// The cross-crate agreement the module documentation promises. Written
// relationally as well as absolutely so that the check still means something on
// a target whose pointer is not 64 bits: the absolute numbers are the measured
// x86_64 contract, while the size and offset equalities against the core's own
// declaration hold everywhere. No other module can make this assertion, because
// no other module imports both declarations.
#[cfg(feature = "gz")]
/// cbindgen:ignore
const _: () = assert!(size_of::<gzFile_s>() == size_of::<GzFileExposed>());
#[cfg(feature = "gz")]
/// cbindgen:ignore
const _: () = assert!(align_of::<gzFile_s>() == align_of::<GzFileExposed>());
#[cfg(feature = "gz")]
/// cbindgen:ignore
const _: () =
    assert!(core::mem::offset_of!(gzFile_s, have) == core::mem::offset_of!(GzFileExposed, have));
#[cfg(feature = "gz")]
/// cbindgen:ignore
const _: () =
    assert!(core::mem::offset_of!(gzFile_s, next) == core::mem::offset_of!(GzFileExposed, next));
#[cfg(feature = "gz")]
/// cbindgen:ignore
const _: () =
    assert!(core::mem::offset_of!(gzFile_s, pos) == core::mem::offset_of!(GzFileExposed, pos));

// ---------------------------------------------------------------------------
// `code` and `codetype` -- `inftrees.h`
// ---------------------------------------------------------------------------

/// One entry of an inflate decoding table: `inftrees.h` L24-L28.
///
/// ```c
/// typedef struct {
///     unsigned char op;
///     unsigned char bits;
///     unsigned short val;
/// } code;
/// ```
///
/// **4 bytes, align 2**, `op` at 0, `bits` at 1, `val` at 2 -- "Each entry is four
/// bytes", as the comment at `inftrees.h` L23 says. Unlike the other layouts in
/// this module these numbers are platform-independent: all three members have
/// fixed widths, so no integer model changes them.
///
/// # Why an ABI mirror of a *private* header type exists here
///
/// `inftrees.h` is private and its opening comment says applications should use
/// `zlib.h` only. `inflate_table` is nevertheless a real symbol: the reference
/// build emits it, `zlib.map` lists it in the `local:` block so it stays hidden,
/// and `test/infcover.c` declares and calls it directly with its own
/// `code table[ENOUGH_DISTS]` array. `crates/libz-rs-sys/src/inflate.rs`
/// therefore has to present the exact C signature
///
/// ```c
/// int inflate_table(codetype, unsigned short *, unsigned, code **, unsigned *,
///                   unsigned short *);
/// ```
///
/// and this is the type that appears in it.
///
/// # Relationship to the core's [`Code`]
///
/// [`zlib_rs::inflate::inftrees::Code`] has the same three fields in the same
/// order, and the assertions below pin that the two agree in size and alignment.
/// The core's type is deliberately **not** `#[repr(C)]`, however, and its own
/// documentation forbids casting a caller's `code *` to a `*mut Code`. Agreement
/// in size is therefore a sanity check, **not** a licence to transmute:
/// conversion goes field by field through the [`From`] implementations below,
/// which are sound whatever layout the core's type happens to have. That is also
/// why a table-link `val` survives the trip -- it is an offset relative to the
/// base of the table holding the link, so it is plain data.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct code {
    /// Offset 0. `unsigned char op` -- operation, extra bits, or table bits.
    ///
    /// `inftrees.h` L25, with the bit assignments listed at L30-L36: `00000000`
    /// literal, `0000tttt` table link where `tttt` is the index width,
    /// `0001eeee` length or distance where `eeee` is the extra-bit count,
    /// `01100000` end of block, `01000000` invalid code.
    pub op: u8,
    /// Offset 1. `unsigned char bits` -- bits of the bit buffer this entry
    /// consumes.
    ///
    /// `inftrees.h` L26.
    pub bits: u8,
    /// Offset 2. `unsigned short val` -- literal value, base length or
    /// distance, or the offset to the next table.
    ///
    /// `inftrees.h` L27.
    pub val: u16,
}

/// cbindgen:ignore
const _: () = assert!(size_of::<code>() == size_of::<Code>());
/// cbindgen:ignore
const _: () = assert!(align_of::<code>() == align_of::<Code>());

impl From<Code> for code {
    /// Copies a core table entry into its ABI mirror, field by field.
    ///
    /// Field-by-field rather than by reinterpretation, because
    /// [`zlib_rs::inflate::inftrees::Code`] carries no `#[repr(C)]` and its
    /// documentation forbids the cast. The three fields are plain integers, so
    /// the copy is exact.
    fn from(value: Code) -> Self {
        Self {
            op: value.op,
            bits: value.bits,
            val: value.val,
        }
    }
}

impl From<code> for Code {
    /// Copies an ABI table entry into the core's form, field by field.
    ///
    /// The counterpart of the other direction, needed where a caller supplies a
    /// table the core must read.
    fn from(value: code) -> Self {
        Self {
            op: value.op,
            bits: value.bits,
            val: value.val,
        }
    }
}

/// Maximum number of table entries for the literal/length code: 852.
///
/// `inftrees.h` L49. The comment at L38-L48 records how the figure was obtained
/// -- `enough 286 9 15`, using `examples/enough.c` -- and warns that it must be
/// recalculated if the root table size changes.
///
/// Typed [`c_uint`] because that is the arithmetic C performs on it:
/// `inftrees.c` compares `used > ENOUGH_LENS` in `unsigned`. The core keeps a
/// [`usize`] form, [`zlib_rs::inflate::inftrees::ENOUGH_LENS`], for indexing.
//
// ★ `#[allow(dead_code)]` on this constant and the two below is required at the
// declared MSRV and is not cosmetic. All three are the ABI-side mirror of
// `inftrees.h` L49-L51 and their consumer is `layout_assertions.rs`, which pins
// each against the header's number; `inflate.rs` reaches for the core's `usize`
// forms instead, because that is what it indexes with. Every use is therefore
// inside a `const _: () = …` item, and **rustc 1.80's dead-code pass does not
// traverse those bodies**: it reports `constant ENOUGH_LENS is never used`.
// Verified here rather than assumed -- `cargo +1.80 build -p libz-rs-sys
// --all-features` reports all three, and 1.97.1 reports none of them -- which is
// the same quirk `layout_assertions.rs` documents for its own `IS_LP64` and
// `crates/zlib-rs/src/gz/header.rs` for its nineteen field constants. Since CI
// builds with `-D warnings`, without these attributes the crate would fail to
// build on exactly the compiler `rust-version = "1.80"` promises to support.
//
// The attributes are inert on newer toolchains and are scoped to these three
// items rather than to the module, so real dead code elsewhere in the file is
// still reported. Remove them only when the MSRV rises past the version that
// fixed the analysis, and re-verify with a build on the new floor first.
/// cbindgen:ignore
#[allow(dead_code)]
pub(crate) const ENOUGH_LENS: c_uint = 852;

/// Maximum number of table entries for the distance code: 592.
///
/// `inftrees.h` L50, from `enough 30 6 15`.
// Const-assert-only consumer; see the note above `ENOUGH_LENS`.
/// cbindgen:ignore
#[allow(dead_code)]
pub(crate) const ENOUGH_DISTS: c_uint = 592;

/// Maximum size of the dynamic table: 1444 entries.
///
/// `inftrees.h` L51, `ENOUGH_LENS + ENOUGH_DISTS`. `struct inflate_state`
/// budgets `4 * ENOUGH` bytes for its inline arena, and `test/infcover.c`
/// accounts for that budget when it caps allocation, so the value is
/// externally load-bearing rather than merely advisory.
// Const-assert-only consumer; see the note above `ENOUGH_LENS`.
/// cbindgen:ignore
#[allow(dead_code)]
pub(crate) const ENOUGH: c_uint = ENOUGH_LENS + ENOUGH_DISTS;

/// `codetype` discriminant `CODES` -- `inftrees.h` L55.
///
/// The code-length code: the 19-symbol alphabet that describes the other two.
///
/// # Why these are `c_int` constants rather than a Rust `enum`
///
/// `codetype` is a C enum (**measured: 4 bytes**) and `inflate_table` takes one
/// by value. A C caller may pass *any* `int` through that parameter -- nothing
/// in the language stops it, and `test/infcover.c` passes the enumerators only
/// by convention. A Rust `enum` parameter would make an out-of-range value
/// instant undefined behaviour, so the exported signature takes [`c_int`] and
/// validates it with [`code_type_from_raw`].
/// cbindgen:ignore
pub(crate) const CODES: c_int = 0;

/// `codetype` discriminant `LENS` -- `inftrees.h` L56.
///
/// The literal/length code: the merged 0..=285 alphabet of RFC 1951 §3.2.5.
/// cbindgen:ignore
pub(crate) const LENS: c_int = 1;

/// `codetype` discriminant `DISTS` -- `inftrees.h` L57.
///
/// The distance code: the 0..=29 alphabet of RFC 1951 §3.2.5.
/// cbindgen:ignore
pub(crate) const DISTS: c_int = 2;

/// Validates a raw `codetype` argument and maps it to the core's enum.
///
/// The guard that makes taking [`c_int`] at the boundary safe: it accepts
/// exactly [`CODES`], [`LENS`] and [`DISTS`] -- the three enumerators of
/// `inftrees.h` L54-L58, in declaration order -- and returns [`None`] for
/// anything else, which the caller reports as an error rather than treating as a
/// valid alphabet.
///
/// # The mapping, pinned
///
/// The five cases below are the whole of the contract, and they are checked at
/// compile time rather than in a doctest. Two reasons, and the first is decisive:
/// this helper is `pub(crate)`, so no doctest -- which rustdoc compiles as a
/// separate crate -- can name it, and it must stay that way, because a caller
/// outside the boundary has nothing to validate a raw `codetype` *for*. The second
/// is that a `const fn` needs no harness to be exercised, so the assertions cost
/// nothing and cannot be skipped by a filtered test run. The same five, plus the
/// two integer extremes, run again at run time in the test module at the bottom of
/// this file, where a failure is reported rather than fatal.
#[must_use]
pub(crate) const fn code_type_from_raw(raw: c_int) -> Option<CodeType> {
    match raw {
        CODES => Some(CodeType::Codes),
        LENS => Some(CodeType::Lens),
        DISTS => Some(CodeType::Dists),
        _ => None,
    }
}

// The three accepted values are `inftrees.h` L54-L58 in declaration order; the two
// rejected ones are the nearest integers on either side of the range, which is where
// an off-by-one in the arms above would show itself.
/// cbindgen:ignore
const _: () = {
    assert!(matches!(code_type_from_raw(CODES), Some(CodeType::Codes)));
    assert!(matches!(code_type_from_raw(LENS), Some(CodeType::Lens)));
    assert!(matches!(code_type_from_raw(DISTS), Some(CodeType::Dists)));
    assert!(code_type_from_raw(3).is_none());
    assert!(code_type_from_raw(-1).is_none());
};

// ---------------------------------------------------------------------------
// The allocator hook -- unsafe-site category 4
// ---------------------------------------------------------------------------

#[cfg(feature = "libz-compat")]
/// The byte a freshly acquired foreign block is filled with.
///
/// [`SENTINEL_FILL`] -- `0xa5` -- in debug builds and zero in release, mirroring
/// the core's own private choice exactly so that the two allocators are
/// indistinguishable to the algorithms.
///
/// A fill is unavoidable rather than optional. C's `zcalloc` is a bare `malloc`
/// (`zutil.c` L299-L303) and writes nothing, but safe Rust cannot hand out a
/// partially initialised slice: reading an uninitialised byte through a
/// `&mut [u8]` is undefined behaviour. Filling is what makes
/// [`Buffer::as_mut_slice`] a slice at all, and it is obligation 1 of the five
/// the core's [`Allocator`] documentation places on an implementation.
///
/// The value is chosen for test signal, not for speed -- both cost one pass over
/// the block. `0xa5` in debug builds is the byte `test/infcover.c` L87 fills with,
/// under the comment "fill memory with a non-zero value to make sure that the
/// code isn't depending on zeros", so a state constructor that mistakenly assumes
/// zeroed memory fails a Rust test rather than only the C coverage harness.
#[cfg(debug_assertions)]
/// cbindgen:ignore
const FILL_BYTE: u8 = SENTINEL_FILL;

/// The byte a freshly acquired foreign block is filled with in release builds.
///
/// See the debug-build definition above for the rationale. Zero is the plainest
/// possible choice for a pass that has to happen anyway.
#[cfg(not(debug_assertions))]
/// cbindgen:ignore
const FILL_BYTE: u8 = {
    // Referenced so that the import is used in both build configurations and the
    // two definitions cannot drift apart unnoticed.
    let _ = SENTINEL_FILL;
    0
};

#[cfg(feature = "libz-compat")]
// ★ `#[allow(dead_code)]` for the reason `ENOUGH_LENS` above carries it: both uses of this
// constant are inside `const _: () = assert!(…)` items, and rustc 1.80's dead-code pass
// does not traverse those bodies -- `make rust-msrv` reports `constant FILL_U16 is never
// used` there while current stable counts the uses. The byte pattern itself is what
// `allocate_u16s` writes through the raw pointer; the assertions are what keep the two
// spellings equivalent.
#[allow(dead_code)]
/// The 16-bit fill pattern, both of whose bytes are `FILL_BYTE`.
///
/// Filling a `u16` block byte-for-byte the way a byte block is filled keeps the
/// two shapes consistent: in a debug build a `u16` block reads back as `0xa5a5`,
/// which is what the bytes `0xa5 0xa5` mean in either byte order. This is the
/// same derivation the core uses for its own `u16` shape.
/// cbindgen:ignore
const FILL_U16: u16 = u16::from_ne_bytes([FILL_BYTE, FILL_BYTE]);

#[cfg(feature = "libz-compat")]
// [`StreamAllocator::allocate_u16s`] fills its block one *byte* at a time, through
// the raw pointer and before any `&mut [u16]` exists, because a reference may not
// address indeterminate memory. That is only equivalent to filling each element
// with [`FILL_U16`] while both of the pattern's bytes are `FILL_BYTE`, so the
// equivalence is asserted rather than assumed: redefining `FILL_U16` to a pattern
// with two different bytes fails the build here instead of silently changing what
// a freshly allocated hash-chain array reads back as.
/// cbindgen:ignore
const _: () = assert!(FILL_U16.to_ne_bytes()[0] == FILL_BYTE);
#[cfg(feature = "libz-compat")]
/// cbindgen:ignore
const _: () = assert!(FILL_U16.to_ne_bytes()[1] == FILL_BYTE);

// ---------------------------------------------------------------------------
// The library's own allocation routines -- `zutil.c` L299-L308
// ---------------------------------------------------------------------------
//
// ★ These exist because C's substitution is OBSERVABLE. `deflate.c` L401-L414,
// `inflate.c` L183-L195 and `infback.c` L37-L49 do not merely *decide* to use the
// library's own routines when a hook is `Z_NULL`: they WRITE `zcalloc` and
// `zcfree` into the caller's `z_stream`, and `zlib.h` L151-L153 documents that --
// "If zalloc and zfree are set to Z_NULL, deflateInit updates them to use default
// allocation functions". A caller may read those members afterwards, pass the
// stream to `inflateCopy` (which copies all three verbatim), or -- as
// `test/infcover.c` L502 does -- initialise a stream whose members `mem_done` has
// just reset to `Z_NULL` and let the library fill them in again.
//
// Publishing a Rust closure or a mode flag cannot satisfy that: the member's type
// is a C function pointer, so the substituted value has to BE one. Hence two
// ordinary `extern "C"` functions with `zalloc`/`zfree`'s exact signatures.
// Neither is `#[no_mangle]`: `zcalloc` and `zcfree` are in `zlib.map`'s `local:`
// block, so the reference does not export them either, and these are reached only
// through the function pointers the library publishes.
//
// They are `malloc`/`free`-backed for one decisive reason: `zcfree` receives a
// pointer and nothing else, exactly as `free` does. Rust's global allocator needs
// the original `Layout` to deallocate, which the C signature has no room to carry,
// so a global-allocator-backed pair could not be published to a caller at all
// without inventing a size prefix -- a layout the reference does not have and that
// a caller mixing `strm->zfree` with `free` would corrupt.
//
// `malloc` and `free` are excluded from header generation in cbindgen.toml: they
// are C-runtime functions this crate IMPORTS, so a rendered declaration would put
// them in a file guarded by ZLIB_H, where they are no part of the contract.  A doc
// comment cannot carry the annotation here, because rustdoc rejects one on an
// extern block (`unused_doc_comments`, an error under -D warnings).
extern "C" {
    /// C's `malloc` -- the allocator `zcalloc` uses (`zutil.c` L301).
    ///
    /// Declared rather than taken from a crate: `libc` is an optional,
    /// default-off dependency of this crate (dependency minimalism, AAP §0.7.1
    /// (i)), and the two signatures below are fixed by C89 7.20.3, so there is
    /// nothing to discover. Every consumer of this library is a C ABI consumer and
    /// therefore already links a C runtime.
    fn malloc(size: z_size_t) -> *mut c_void;

    /// C's `free` -- the routine `zcfree` calls (`zutil.c` L307).
    fn free(address: *mut c_void);
}

#[cfg(feature = "libz-compat")]
/// The library's own `zalloc`, published to the caller when it supplies none.
///
/// The mirror of `zcalloc` (`zutil.c` L299-L304), which on any target whose `uInt`
/// is wider than two bytes -- every target this crate supports -- is
/// `malloc(items * size)`.
///
/// One deliberate difference: the product is computed **checked**, in `usize`,
/// rather than in C's `unsigned`. An unchecked product is the classic route from
/// an allocation bug to a memory-safety bug, because it yields a block far smaller
/// than the length the resulting slice would claim; a request that cannot be
/// expressed becomes `Z_NULL`, which is `zalloc`'s documented failure answer
/// (`zlib.h` L149) and which every caller of this library already handles as
/// `Z_MEM_ERROR`. No request the library itself makes can reach that bound:
/// `MAX_WBITS` (15) and `MAX_MEM_LEVEL` (9) cap the largest at 131072 bytes.
///
/// # Safety
///
/// This is an `extern "C"` function whose address is handed to C. It dereferences
/// nothing: `opaque` is ignored, exactly as `zcalloc` ignores it, and the return
/// value is the allocator's own. Every block it returns must be released through
/// [`zlib_rs_zfree`] and through nothing else.
/// cbindgen:ignore
pub(crate) unsafe extern "C" fn zlib_rs_zalloc(_opaque: voidpf, items: uInt, size: uInt) -> voidpf {
    let Some(bytes) = block_len(widen(items), widen(size)) else {
        return core::ptr::null_mut();
    };

    // SAFETY: unsafe-site category 4, on the library's own side of it. `malloc` is
    // the C runtime's, its single argument is a byte count that `block_len` has
    // proven does not overflow, and a zero count -- which `malloc` may answer with
    // either null or a unique pointer -- is not reachable here because `block_len`
    // rejects it. The result is treated as an address only; nothing is read or
    // written through it.
    let block = unsafe { malloc(bytes) };
    block.cast::<c_void>()
}

/// The library's own `zfree`, published alongside [`zlib_rs_zalloc`].
///
/// The mirror of `zcfree` (`zutil.c` L306-L308): `free(ptr)`, with `opaque`
/// ignored.
///
/// # Safety
///
/// `address` must be null, or a pointer [`zlib_rs_zalloc`] returned and that has
/// not been released yet. That is `free`'s own contract and `zfree`'s
/// (`zlib.h` L86); this function adds nothing to it.
/// cbindgen:ignore
pub(crate) unsafe extern "C" fn zlib_rs_zfree(_opaque: voidpf, address: voidpf) {
    // SAFETY: unsafe-site category 4. `free` accepts null and does nothing with
    // it (C89 7.20.3.2), and by this function's contract any other value came from
    // `zlib_rs_zalloc`, i.e. from `malloc`. Nothing is read through the pointer.
    unsafe { free(address) };
}

#[cfg(feature = "libz-compat")]
/// The pair of routines a [`StreamAllocator`] calls, with the `opaque` they take.
///
/// Private, and deliberately so: nobody outside this module can construct one, so
/// a block obtained from one triple can never be routed to another. The only ways
/// in are [`StreamAllocator::internal`] and [`StreamAllocator::from_hooks`].
///
/// ★ There is no "internal mode" variant. After C's substitution there is no such
/// thing: `deflate.c` L403-L413 replaces a null `zalloc` with `zcalloc` and a null
/// `zfree` with `zcfree` and then allocates through `strm->zalloc` like any other
/// stream, and [`StreamAllocator::internal`] reproduces exactly that by carrying
/// [`zlib_rs_zalloc`] and [`zlib_rs_zfree`] as an ordinary pair. Two consequences,
/// both wanted: the library's own path and a caller's path are the same code, so
/// neither can drift; and [`Allocator::id`] gives the same answer whether the
/// allocator was built by `internal()` or read back from the hooks the library
/// published, which is what lets `deflateEnd` release blocks `deflateInit2_`
/// obtained.
#[derive(Debug, Clone, Copy)]
struct Hooks {
    /// `zalloc` (`zlib.h` L85), known non-null.
    allocate: unsafe extern "C" fn(voidpf, uInt, uInt) -> voidpf,
    /// `zfree` (`zlib.h` L86), known non-null.
    deallocate: unsafe extern "C" fn(voidpf, voidpf),
    /// `opaque` (`zlib.h` L104), passed back to both routines verbatim.
    opaque: Opaque,
}

/// The facade's [`Allocator`]: the raw-pointer half of the core's abstraction.
///
/// [`zlib_rs::allocate`] defines the *shape* of allocation and supplies the
/// implementation that needs no hooks (`zlib_rs::allocate::GlobalAllocator`), but it
/// cannot supply
/// this one: calling a raw C function pointer requires the escape hatch that
/// `#![forbid(unsafe_code)]` denies it. This type is that missing half, and it
/// lives in this module because [`alloc_func`] and [`free_func`] do.
///
/// It is a small [`Copy`] value reconstructed on each entry into the library from
/// the fields of the caller's [`z_stream`], while the blocks it hands out live as
/// long as the stream state does -- which is what the trait's `'a` parameter
/// expresses and why `'a` belongs to the allocations rather than to the allocator.
///
/// # The five obligations
///
/// The core's [`Allocator`] documentation places five requirements on an
/// implementation that hands out blocks it did not obtain from Rust. Each is
/// discharged here:
///
/// 1. **Initialised.** Every block is filled with `FILL_BYTE` (or `FILL_U16`)
///    before it is wrapped, so no uninitialised byte is ever readable through the
///    resulting slice.
/// 2. **Disjoint.** Each block comes from one call to the caller's `zalloc`, whose
///    contract is to return a fresh region, and it is wrapped exactly once.
/// 3. **Valid for `'a`.** A block is released only through
///    [`Allocator::deallocate_bytes`] or [`Allocator::deallocate_u16s`], each of
///    which consumes the [`Buffer`] that holds the borrow.
/// 4. **Aligned.** A `u16` request is rejected -- and the block immediately
///    returned to the caller's `zfree` -- if the address is not `u16`-aligned.
///    `malloc`-backed hooks always satisfy this, including
///    `test/infcover.c`'s (L84), but the check costs one test and closes the case
///    where they do not.
/// 5. **Correctly identified.** [`Allocator::id`] returns
///    [`AllocatorId::foreign`] built from the two hook addresses and the `opaque`
///    value in caller mode, and only ever [`AllocatorId::GLOBAL`] in internal
///    mode -- where the blocks really are Rust's. Claiming `GLOBAL` for a foreign
///    block would defeat the check in [`Buffer::release_to`].
///
/// # How the mismatched-allocator bug class stays closed
///
/// Memory obtained from a caller's `zalloc` is returned **only** through that same
/// caller's `zfree`, never through Rust's global deallocator and never through a
/// different stream's hooks. Two mechanisms enforce it, neither of which is a
/// convention: a [`Buffer`] keeps Rust-owned and foreign storage in separate
/// variants of a private enum, and [`Buffer::release_to`] compares the
/// [`AllocatorId`] recorded at handout against the allocator being released to,
/// refusing the release on any mismatch. A refused release leaks the block -- safe
/// and diagnosable -- rather than corrupting the heap.
///
/// `test/infcover.c` polices exactly this: its `mem_free` counts frees of
/// addresses it never handed out as *rogue*, and `mem_done` reports rogue frees,
/// leaks, and frees that are not in last-in-first-out order. **Deallocation order
/// is the state's responsibility, not this type's:** `deflateEnd` frees "in
/// reverse order of allocations" (`deflate.c` L1300-L1306) and a state that
/// released its buffers in allocation order would increment that harness's
/// `notlifo` counter without this type being able to notice.
///
/// # Freshly allocated memory is not zeroed
///
/// The live default path is `malloc`, not `calloc`: `zcalloc` chooses between them
/// on `sizeof(uInt) > 2` (`zutil.c` L299-L303) and [`uInt`] is four bytes on every
/// supported target, so the branch taken is always `malloc`. A caller-supplied hook
/// is under no obligation to do better. **Nothing may assume a fresh block is
/// zeroed**; initialise what needs initialising, exactly as `deflate.c` L442
/// zeroes the state with `zmemzero` and `deflate.c` L170-L173 clears the hash head
/// array with `CLEAR_HASH`.
///
/// # ★ Why foreign *deallocation* is verified by AddressSanitizer, not by Miri
///
/// This is a limitation of Rust's aliasing models at an FFI boundary, and it is
/// recorded here in full so that nobody spends a day rediscovering it.
///
/// The core's contract for releasing a foreign block is
/// `Allocator::deallocate_bytes(&self, buffer: Buffer<'a, u8>)`, and a foreign
/// [`Buffer`] necessarily holds a `&'a mut [u8]` over the caller's memory --
/// necessarily, because the core is `#![forbid(unsafe_code)]` and a live borrow is
/// the only way it can hand out a slice at all. Passing that buffer by value makes
/// the enclosed `&mut` an argument of `deallocate_bytes`, which both Stacked
/// Borrows and Tree Borrows treat as *protected* for the whole call. Freeing the
/// memory it covers -- which is the entire purpose of the call -- then reads as
/// undefined behaviour:
///
/// * Stacked Borrows: `deallocating while item [Unique for ...] is strongly
///   protected`.
/// * Tree Borrows: `deallocation through ... is forbidden`.
///
/// Both were reproduced on a fifteen-line standalone program, so this is a
/// property of the models rather than of this implementation, and it is *not* an
/// artifact of Stacked Borrows being experimental -- the two independent models
/// agree. The same program accepts the identical `dealloc` when only a raw pointer
/// is passed, which pinpoints the cause exactly: a protected reference, not the
/// deallocation.
///
/// There is no fix available inside this crate. The free would have to happen after
/// every frame that received the borrow has returned, and `deallocate_bytes` is
/// the outermost such frame; and the core cannot describe foreign storage with a
/// raw pointer instead, because rebuilding a slice from one requires the escape
/// hatch it forbids.
///
/// This is precisely why the two tools divide the workspace as they do: **Miri
/// covers `crates/zlib-rs`, the safe core, and AddressSanitizer covers
/// `crates/libz-rs-sys`, the boundary layer where the raw pointers actually
/// exist.** Miri can analyse the core exhaustively because the core contains no
/// FFI; it cannot model this crate's whole job. Accordingly:
///
/// * Every raw-pointer path in this module -- caller-hook allocation, slice
///   reconstruction, stream validation, and the full state round-trip including
///   the caller-hook variant -- is **ASan-clean**, verified with
///   `-Zsanitizer=address`.
/// * Every path that does not free foreign memory is additionally **Miri-clean**
///   under `-Zmiri-strict-provenance`.
/// * A CI job that runs Miri over this crate must therefore skip the tests that
///   release a foreign block, and should cite this note rather than silence the
///   diagnostic.
#[cfg(feature = "libz-compat")]
#[derive(Debug, Clone, Copy)]
pub(crate) struct StreamAllocator {
    /// Which routines to call. See [`Hooks`].
    hooks: Hooks,
}

#[cfg(feature = "libz-compat")]
impl StreamAllocator {
    /// The allocator the library substitutes when a caller hook is `Z_NULL`.
    ///
    /// Carries `zlib_rs_zalloc` and `zlib_rs_zfree` with a null `opaque`,
    /// which is the substitution `deflate.c` L401-L414, `inflate.c` L183-L195 and
    /// `infback.c` L37-L49 perform with `zcalloc`/`zcfree` -- the same two
    /// routines, reached the same way, through a function pointer.
    ///
    /// Also the right choice for buffers that are not reached through a
    /// [`z_stream`] at all -- the `gzFile` layer allocates its state, its path
    /// string and its two working buffers with plain `malloc`
    /// (`gzlib.c` L100 and L206, `gzread.c` L99-L100, `gzwrite.c` L16 and L25),
    /// because a `gzFile` has no caller-supplied hooks to honour.
    #[must_use]
    // One entry point calls it: `_zlib_rs_inflate_table`, which has no `z_stream` and
    // therefore no caller hooks to honour, and needs a fallible block for a `codes`
    // beyond its stack bound. Every stream entry point reaches the same routines
    // through `from_hooks` instead, which performs the substitution inline while
    // reading the caller's triple.
    pub(crate) const fn internal() -> Self {
        Self {
            hooks: Hooks {
                allocate: zlib_rs_zalloc,
                deallocate: zlib_rs_zfree,
                opaque: Opaque::NULL,
            },
        }
    }

    /// The three values [`z_stream`]'s allocator members must hold after this
    /// allocator has been installed -- `(zalloc, zfree, opaque)`.
    ///
    /// This is what makes C's substitution observable to the caller. An init entry
    /// point reads the caller's three members, builds an allocator with
    /// [`StreamAllocator::from_hooks`], and writes these three back; for a caller
    /// that supplied both hooks they are the caller's own values unchanged, and for
    /// each one it left null they are the library's own routine -- exactly the two
    /// outcomes `zlib.h` L151-L153 documents.
    #[must_use]
    pub(crate) const fn published_hooks(&self) -> (alloc_func, free_func, voidpf) {
        (
            Some(self.hooks.allocate),
            Some(self.hooks.deallocate),
            self.hooks.opaque.as_ptr(),
        )
    }

    /// Builds an allocator from a caller's `(zalloc, zfree, opaque)` triple.
    ///
    /// The three arguments are [`z_stream::zalloc`], [`z_stream::zfree`] and
    /// [`z_stream::opaque`] read straight off the caller's stream; see
    /// [`StreamAllocator::from_stream_ptr`] for the reading half.
    ///
    /// | `zalloc` | `zfree` | `zalloc` used | `zfree` used | `opaque` used |
    /// |---|---|---|---|---|
    /// | [`None`] | [`None`] | `zlib_rs_zalloc` | `zlib_rs_zfree` | `Z_NULL` |
    /// | [`Some`] | [`Some`] | the caller's | the caller's | the caller's |
    /// | [`Some`] | [`None`] | the caller's | `zlib_rs_zfree` | the caller's |
    /// | [`None`] | [`Some`] | `zlib_rs_zalloc` | the caller's | `Z_NULL` |
    ///
    /// # ★ The two hooks default INDEPENDENTLY, exactly as C's do
    ///
    /// `deflate.c` L401-L407 replaces a null `zalloc` with `zcalloc` **and clears
    /// `opaque`**, and L410-L414 separately replaces a null `zfree` with `zcfree`;
    /// `inflate.c` L183-L195 and `infback.c` L37-L49 are the same two steps in the
    /// same order. The four rows above are that behaviour, and every one of them is
    /// reachable: a half-supplied pair is a C-valid stream, so this function
    /// answers it rather than rejecting it.
    ///
    /// Two things make the mixed rows safe here, and they are the reason the
    /// substitution can be reproduced faithfully instead of being refused:
    ///
    /// * The library's own routines are `malloc`/`free`-backed
    ///   (`zlib_rs_zalloc`, `zlib_rs_zfree`), which is what `zcalloc` and
    ///   `zcfree` are (`zutil.c` L299-L308). A caller that supplies only `zalloc`
    ///   therefore has its blocks released by `free`, which is precisely what the
    ///   reference does with that stream -- no more and no less.
    /// * Whichever pair results, it is recorded as one triple and
    ///   [`Allocator::id`] is derived from all three values, so a block obtained
    ///   from this allocator can only ever be returned to this allocator.
    ///
    /// `opaque` follows `zalloc`, not `zfree`, because that is where C clears it:
    /// the `strm->opaque = (voidpf)0` assignment lives inside the `zalloc == NULL`
    /// arm. A caller that supplies `zfree` alone keeps a null `opaque` and sees it
    /// passed to its own `zfree`, which is what the reference passes.
    ///
    /// This constructor is for the three **init** entry points only, because they
    /// are the only ones that substitute: every other entry point runs C's state
    /// check first, which *rejects* a stream with a null hook rather than filling
    /// one in. [`StreamAllocator::from_supplied_hooks`] is that path.
    #[must_use]
    pub(crate) fn from_hooks(zalloc: alloc_func, zfree: free_func, opaque: voidpf) -> Self {
        // `deflate.c` L403-L407: a null `zalloc` becomes the library's own and
        // takes the `opaque` clear with it.
        let (allocate, opaque) = match zalloc {
            Some(allocate) => (allocate, Opaque::new(opaque)),
            None => (
                zlib_rs_zalloc as unsafe extern "C" fn(voidpf, uInt, uInt) -> voidpf,
                Opaque::NULL,
            ),
        };
        // `deflate.c` L410-L414: and then, separately, a null `zfree`.
        let deallocate = zfree.unwrap_or(zlib_rs_zfree);

        Self {
            hooks: Hooks {
                allocate,
                deallocate,
                opaque,
            },
        }
    }

    /// Builds an allocator from a triple that must already be complete.
    ///
    /// The counterpart of [`StreamAllocator::from_hooks`] for every entry point
    /// that is **not** an init: `deflateStateCheck` (`deflate.c` L536-L539) and
    /// `inflateStateCheck` (`inflate.c` L88-L97) both begin
    ///
    /// ```c
    /// if (strm == Z_NULL || strm->zalloc == (alloc_func)0 || strm->zfree == (free_func)0)
    ///     return 1;
    /// ```
    ///
    /// so a stream whose hooks are null is not substituted into working order at
    /// that point -- it is refused with `Z_STREAM_ERROR`. Returns [`None`] for
    /// exactly that case, which the entry point reports as
    /// [`ReturnCode::STREAM_ERROR`].
    ///
    /// The case is reachable and is exercised by the suite:
    /// `test/infcover.c`'s `mem_done` sets all three members back to `Z_NULL`
    /// (L231-L233), and a stream in that state must be refused by anything other
    /// than a fresh init.
    #[must_use]
    pub(crate) fn from_supplied_hooks(
        zalloc: alloc_func,
        zfree: free_func,
        opaque: voidpf,
    ) -> Option<Self> {
        let (allocate, deallocate) = (zalloc?, zfree?);
        Some(Self {
            hooks: Hooks {
                allocate,
                deallocate,
                opaque: Opaque::new(opaque),
            },
        })
    }

    /// Builds an allocator by reading the three hook members through the caller's
    /// own pointer.
    ///
    /// The shape every exported entry point wants, because it forms no reference to
    /// the stream at all: each member is read through a raw place, so the caller's
    /// [`z_streamp`] keeps its provenance and remains usable for
    /// [`install_state`], [`checked_state`] and [`take_state`] afterwards. See the
    /// note on [`checked_state`] for why that matters.
    ///
    /// Returns [`None`] if `strm` is null or misaligned -- which the entry point
    /// reports as `Z_STREAM_ERROR`, the same status a half-supplied hook pair
    /// yields.
    ///
    /// # Safety
    ///
    /// If `strm` is non-null it must address a live [`z_stream`] that nothing else
    /// is concurrently writing.
    #[must_use]
    pub(crate) unsafe fn from_stream_ptr(strm: z_streamp) -> Option<Self> {
        if strm.is_null() || !strm.is_aligned() {
            return None;
        }
        // SAFETY: unsafe-site category 1 -- reading three members of the caller's
        // stream. `strm` is non-null and aligned by the guard above and addresses a
        // live `z_stream` by this function's contract, so all three members are in
        // bounds and readable. `addr_of!` plus `read` forms raw places rather than a
        // `&z_stream`, so nothing here disturbs the borrow stack of the caller's own
        // pointer. `alloc_func` and `free_func` are `Option<fn>` and `voidpf` is a
        // pointer; all three are plain `Copy` data that is never dereferenced here.
        let (zalloc, zfree, opaque) = unsafe {
            (
                core::ptr::addr_of!((*strm).zalloc).read(),
                core::ptr::addr_of!((*strm).zfree).read(),
                core::ptr::addr_of!((*strm).opaque).read(),
            )
        };
        Self::from_supplied_hooks(zalloc, zfree, opaque)
    }

    /// Performs C's hook substitution on the caller's stream and publishes the
    /// result -- the init-only counterpart of [`StreamAllocator::from_stream_ptr`].
    ///
    /// This is `deflate.c` L400-L414, `inflate.c` L182-L195 and `infback.c`
    /// L36-L49 in one place, and it does all three things they do, in their order:
    ///
    /// 1. `strm->msg = Z_NULL` -- "in case we return an error";
    /// 2. a null `zalloc` becomes the library's own **and** `opaque` is cleared;
    /// 3. a null `zfree` becomes the library's own, decided independently.
    ///
    /// The three members are then written back, so a caller that passed `Z_NULL`
    /// hooks finds working function pointers in its own structure afterwards --
    /// which is what `zlib.h` L151-L153 promises and what makes the substitution
    /// observable rather than internal.
    ///
    /// ★ It is called BEFORE parameter validation, exactly where C calls it: in
    /// `deflateInit2_` the substitution at L401-L414 precedes the `windowBits` and
    /// `memLevel` checks at L433-L437, so even an init that fails with
    /// `Z_STREAM_ERROR` has already filled the hooks in. Reproducing that ordering
    /// is why this is one operation and not two.
    ///
    /// Returns [`None`] only for a null or misaligned `strm`, which the entry point
    /// reports as [`ReturnCode::STREAM_ERROR`]; the substitution itself cannot
    /// fail.
    ///
    /// # Safety
    ///
    /// If `strm` is non-null it must address a live [`z_stream`] that nothing else
    /// is concurrently reading or writing: this function WRITES four of its
    /// members.
    #[must_use]
    pub(crate) unsafe fn adopt_hooks(strm: z_streamp) -> Option<Self> {
        if strm.is_null() || !strm.is_aligned() {
            return None;
        }

        // SAFETY: unsafe-site category 1 -- reading three members of the caller's
        // stream through raw places, forming no reference, so `strm` stays usable
        // for `install_state` afterwards. Non-null and aligned by the guard above,
        // live by this function's contract; all three are plain `Copy` data and
        // none is dereferenced.
        let (zalloc, zfree, opaque) = unsafe {
            (
                core::ptr::addr_of!((*strm).zalloc).read(),
                core::ptr::addr_of!((*strm).zfree).read(),
                core::ptr::addr_of!((*strm).opaque).read(),
            )
        };

        let allocator = Self::from_hooks(zalloc, zfree, opaque);
        let (published_zalloc, published_zfree, published_opaque) = allocator.published_hooks();

        // SAFETY: unsafe-site category 1 -- writing four members of the caller's
        // stream through raw places. `strm` is non-null, aligned and live as
        // established above, so each member is in bounds and writable; each value
        // is plain `Copy` data and `z_stream` has no `Drop`, so overwriting drops
        // nothing. The order is C's: `msg`, then `zalloc` with `opaque`, then
        // `zfree`.
        unsafe {
            core::ptr::addr_of_mut!((*strm).msg).write(core::ptr::null());
            core::ptr::addr_of_mut!((*strm).zalloc).write(published_zalloc);
            core::ptr::addr_of_mut!((*strm).opaque).write(published_opaque);
            core::ptr::addr_of_mut!((*strm).zfree).write(published_zfree);
        }

        Some(allocator)
    }

    /// Reports whether this allocator uses the library's own routines rather than
    /// a caller's.
    ///
    /// True when `zalloc` is `zlib_rs_zalloc`, which is what
    /// [`StreamAllocator::from_hooks`] substitutes for a `Z_NULL` hook. Compared
    /// as function-pointer addresses because that is the only thing there is to
    /// compare after the substitution: the library's routines are published to the
    /// caller like any others, so "internal" is a statement about which function
    /// the pointer names, not about a mode the allocator is in.
    ///
    /// Cast to `usize` rather than compared with `core::ptr::fn_addr_eq`, which is
    /// stable only from Rust 1.85 and would break the declared 1.80 floor -- the
    /// same cast [`Allocator::id`] uses for the same three values. Two distinct
    /// functions may in principle share an address after the linker folds identical
    /// bodies; nothing here depends on the answer for correctness, since every
    /// allocation and release goes through the recorded triple either way.
    #[must_use]
    // As for `internal`: a test-facing question. Every allocation and release goes
    // through the recorded triple, so no entry point needs to ask it.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn is_internal(&self) -> bool {
        self.hooks.allocate as usize
            == zlib_rs_zalloc as unsafe extern "C" fn(voidpf, uInt, uInt) -> voidpf as usize
    }

    /// Returns the `opaque` value this allocator passes to its hooks.
    ///
    /// [`Opaque::NULL`] when `zalloc` was substituted, mirroring
    /// `strm->opaque = (voidpf)0` at `deflate.c` L406.
    #[must_use]
    // Not currently called: the opaque cookie travels inside `Hooks` and is passed
    // straight back to the caller's own routines, so nothing needs to read it out.
    // Kept as the accessor that keeps `Hooks` private.
    #[allow(dead_code)]
    pub(crate) const fn opaque_ptr(&self) -> voidpf {
        self.hooks.opaque.as_ptr()
    }

    /// Calls the caller's `zalloc` for `items * size` bytes and returns the base
    /// address, or [`None`] if the request could not be met.
    ///
    /// Split out so that the one place this crate invokes a caller's allocation
    /// hook is a single named function rather than one occurrence per shape.
    /// Performs, in order: the checked `items * size` product, the two narrowing
    /// conversions to C's [`uInt`], the call, and the null-return test.
    fn call_zalloc(
        allocate: unsafe extern "C" fn(voidpf, uInt, uInt) -> voidpf,
        opaque: Opaque,
        items: usize,
        size: usize,
    ) -> Option<(NonNull<u8>, usize)> {
        // The product C's hook computes for itself -- `items * size` in
        // `unsigned` at `zutil.c` L301, widened first to `size_t` by
        // `test/infcover.c` L76. Computed here checked and in `usize`, because an
        // unchecked product is the classic route from an allocation bug to a
        // memory-safety bug: it yields a block far smaller than the length the
        // resulting slice would claim.
        let len = block_len(items, size)?;
        // The hook's parameters are `uInt`, so a request that cannot be expressed
        // in one is refused rather than truncated. Every request the library
        // actually makes is bounded by `MAX_WBITS` (15) and `MAX_MEM_LEVEL` (9),
        // so the largest is 131072 bytes; the check exists for the general case.
        let items = uInt::try_from(items).ok()?;
        let size = uInt::try_from(size).ok()?;

        // SAFETY: unsafe-site category 4 -- invoking a caller-supplied hook.
        // `allocate` came out of `Hooks::Caller`, which is constructed only from a
        // `Some(..)` `alloc_func`, so it is a non-null C function pointer with the
        // signature `zlib.h` L85 publishes; `Option<unsafe extern "C" fn(..)>` is
        // what makes that guarantee available in the type rather than in a
        // comment. `opaque` is passed back verbatim and is never dereferenced
        // here, which `zlib.h` L146-L147 permits ("the compression library
        // attaches no meaning to the opaque value"). The result is treated as
        // nothing more than an address until it has been null-checked below and
        // length-checked by the caller of this function.
        let raw = unsafe { allocate(opaque.as_ptr(), items, size) };

        // `zlib.h` L149 requires `zalloc` to return `Z_NULL` on failure, and
        // `test/infcover.c` L79-L80 does so deliberately to force the library's
        // error paths to run. A null return is therefore an ordinary outcome, not
        // an anomaly: it becomes `None` here and `Z_MEM_ERROR` at the entry point.
        // The pointer is never dereferenced on this path.
        Some((NonNull::new(raw.cast::<u8>())?, len))
    }

    /// Calls the caller's `zfree` on a block base address.
    ///
    /// The single site at which this crate invokes a release hook.
    fn call_zfree(deallocate: unsafe extern "C" fn(voidpf, voidpf), opaque: Opaque, base: *mut u8) {
        // SAFETY: unsafe-site category 4 -- invoking a caller-supplied hook.
        // `deallocate` came out of `Hooks::Caller` and is therefore a non-null C
        // function pointer with the signature `zlib.h` L86 publishes. `base` is
        // the base address of a block this same allocator obtained from the
        // matching `zalloc`: it is reached only through `Buffer::release_to`,
        // which yields `Release::Foreign` exclusively when the buffer's recorded
        // `AllocatorId` equals this allocator's, so the identity of the
        // `(zalloc, zfree, opaque)` triple is proven rather than assumed. The
        // borrow that described the block was consumed by `release_to`, so
        // nothing can reach the memory after this call.
        unsafe { deallocate(opaque.as_ptr(), base.cast::<c_void>()) };
    }
}

#[cfg(feature = "libz-compat")]
impl<'a> Allocator<'a> for StreamAllocator {
    /// Returns [`AllocatorId::foreign`] built from the triple this allocator holds.
    ///
    /// The identity records what actually distinguishes two allocators -- the two
    /// hook addresses and the `opaque` value -- rather than a digest of them, so
    /// two identities compare equal precisely when the allocators are
    /// interchangeable and there is no collision to worry about. The addresses are
    /// used for comparison only: nothing is ever called or read through the
    /// integers.
    ///
    /// ★ Note that the library's own routines get a foreign identity too, and that
    /// is the point: an allocator built by [`StreamAllocator::internal`] and one
    /// built by reading the hooks that same substitution published into the
    /// caller's `z_stream` compare **equal**, so `deflateEnd` can release what
    /// `deflateInit2_` allocated. It is never [`AllocatorId::GLOBAL`], because no
    /// block this allocator hands out comes from Rust's global allocator.
    fn id(&self) -> AllocatorId {
        AllocatorId::foreign(
            self.hooks.allocate as usize,
            self.hooks.deallocate as usize,
            self.hooks.opaque.addr(),
        )
    }

    /// Returns the `opaque` value the hooks receive.
    fn opaque(&self) -> Opaque {
        self.hooks.opaque
    }

    /// Allocates `items * size` bytes.
    ///
    /// `ZALLOC(strm, items, size)` from `zutil.h` L252-L253, which keeps the two
    /// factors separate all the way to the hook. Returns [`None`] when the product
    /// overflows, when either factor does not fit C's [`uInt`], or when the hook
    /// declines -- all of which the caller reports as [`ReturnCode::MEM_ERROR`].
    ///
    /// The block's contents are unspecified in the sense that matters: they are
    /// `FILL_BYTE`, not zero, and nothing may depend on either.
    fn allocate_bytes(&self, items: usize, size: usize) -> Option<Buffer<'a, u8>> {
        {
            let Hooks {
                allocate, opaque, ..
            } = self.hooks;
            {
                let (base, len) = Self::call_zalloc(allocate, opaque, items, size)?;

                // ★ The fill happens through the RAW pointer, before any reference
                // to the block exists. A caller's `zalloc` is a `malloc` -- it
                // returns writable storage whose contents are indeterminate -- and
                // `&mut [u8]` is not a legal shape for indeterminate bytes: every
                // element a reference addresses must already hold a valid `u8`.
                // Filling first and borrowing second is what makes the borrow
                // sound; filling *through* the borrow would have required the
                // very reference whose validity the fill establishes.
                //
                // SAFETY: unsafe-site category 4 -- writing the block a caller's
                // hook just handed over. `base` is non-null (a `NonNull`) and
                // trivially aligned for `u8`; `len` is the checked `items * size`
                // product this very call passed to the hook, and `zlib.h` L149's
                // contract is that a non-null return addresses that many writable
                // bytes. The region is freshly allocated, so nothing else aliases
                // it, and no reference to it exists yet.
                unsafe {
                    core::ptr::write_bytes(base.as_ptr(), FILL_BYTE, len);
                }

                // SAFETY: unsafe-site category 2 -- reconstructing a slice from a
                // pointer/length pair. Non-null, aligned and `len` bytes writable
                // as established above, unaliased because the block is fresh, and
                // now fully initialised by the `write_bytes` on the line above --
                // which is what makes `u8` a legal element type for it. The borrow
                // is produced exactly once and handed straight to `Buffer`, which
                // owns it until `release_to` consumes it.
                let block: &'a mut [u8] =
                    unsafe { core::slice::from_raw_parts_mut(base.as_ptr(), len) };

                Some(Buffer::from_foreign(self, block))
            }
        }
    }

    /// Allocates `items` unsigned 16-bit values.
    ///
    /// `ZALLOC(strm, items, sizeof(Pos))`, the shape both hash-chain arrays use
    /// (`deflate.c` L459-L460), where `Pos` is an `unsigned short`
    /// (`deflate.h` L96). `size_of::<u16>()` is passed as the hook's `size`
    /// argument so that the C-visible request is byte-for-byte the one C makes.
    ///
    /// Additionally rejects -- and immediately returns -- a block whose address is
    /// not `u16`-aligned, which is obligation 4 of the core's implementation
    /// contract. Returning it rather than merely declining matters: an abandoned
    /// block is a leak, and `test/infcover.c`'s `mem_done` reports leaks.
    fn allocate_u16s(&self, items: usize) -> Option<Buffer<'a, u16>> {
        {
            let Hooks {
                allocate,
                deallocate,
                opaque,
            } = self.hooks;
            {
                let (base, _len) = Self::call_zalloc(allocate, opaque, items, size_of::<u16>())?;

                // Tested on the byte address, before any narrowing, so that no
                // pointer is ever retyped to a stricter alignment than it has been
                // shown to satisfy. `align_offset` reports how far the address is
                // from the next `u16` boundary; zero means it is already there.
                if base.as_ptr().align_offset(align_of::<u16>()) != 0 {
                    // Hand the block straight back rather than leaking it. Every
                    // `malloc`-backed hook satisfies the alignment -- including
                    // `test/infcover.c`'s at L84 -- so this path is unreachable in
                    // practice and is here to keep it unreachable in principle.
                    Self::call_zfree(deallocate, opaque, base.as_ptr());
                    return None;
                }
                let elements = base.cast::<u16>().as_ptr();

                // ★ Filled through the raw pointer before the borrow exists, for
                // the reason [`StreamAllocator::allocate_bytes`] states at length:
                // a hook returns writable storage, not initialised storage, and
                // `&mut [u16]` may only address elements that already hold a valid
                // `u16`. The byte fill is *exactly* the element fill it replaces,
                // because [`FILL_U16`] is `u16::from_ne_bytes([FILL_BYTE; 2])` by
                // construction -- both bytes of every element receive `FILL_BYTE`
                // either way, in either byte order.
                //
                // SAFETY: unsafe-site category 4 -- writing the block a caller's
                // hook just handed over. `base` is non-null (a `NonNull`), the
                // hook was asked for `items * size_of::<u16>()` bytes and returned
                // non-null so that many bytes are writable, and the product was
                // computed checked inside `call_zalloc`. The region is fresh, so
                // nothing aliases it, and no reference to it exists yet. The write
                // goes through the byte pointer, whose alignment requirement is
                // one, so it is independent of the `u16` test above.
                unsafe {
                    core::ptr::write_bytes(
                        base.as_ptr(),
                        FILL_BYTE,
                        items.saturating_mul(size_of::<u16>()),
                    );
                }

                // SAFETY: unsafe-site category 2 -- reconstructing a slice from a
                // pointer/length pair. `elements` is non-null, derived from a
                // `NonNull`, and has just been shown to be `u16`-aligned. `items`
                // `u16` values fit in the region, as established above; the region
                // is fresh, so nothing aliases it; every element is initialised by
                // the `write_bytes` above, which is what makes `u16` a legal
                // element type for the borrow. It is created once and handed to
                // `Buffer`.
                let block: &'a mut [u16] =
                    unsafe { core::slice::from_raw_parts_mut(elements, items) };

                Some(Buffer::from_foreign(self, block))
            }
        }
    }

    /// Hands one byte block back to the caller's `zfree`.
    ///
    /// This *is* `ZFREE(strm, addr)` (`zutil.h` L254). The block arrives as an
    /// address and a length rather than as a borrow, and that is what makes the free
    /// sound: a reference in argument position is protected for the whole call, and
    /// freeing memory a protected reference covers is undefined behaviour. The
    /// borrow was surrendered one frame earlier, inside `Buffer::release_to`, which
    /// `Allocator::deallocate_bytes` calls before reaching this method -- see
    /// `zlib_rs::allocate::ForeignBlock`, which records the whole rule and the Miri
    /// diagnosis behind it.
    ///
    /// The identity check has already run there too, so a block from a different
    /// allocator can never arrive here: it is refused and dropped instead, which
    /// leaks a foreign block -- safe, and visible in `test/infcover.c`'s leak report
    /// -- rather than corrupting a heap by passing it to the wrong `zfree`.
    fn release_foreign_bytes(&self, block: ForeignBlock<'a, u8>) {
        let Hooks {
            deallocate, opaque, ..
        } = self.hooks;
        Self::call_zfree(deallocate, opaque, block.as_mut_ptr());
    }

    /// Hands one `u16` block back to the caller's `zfree`.
    ///
    /// The [`StreamAllocator::release_foreign_bytes`] counterpart for the shape
    /// `Allocator::allocate_u16s` produces, with the same contract. The base address
    /// is the one `zalloc` returned, because a [`Buffer`] never narrows the block it
    /// holds and `ForeignBlock` records the base rather than a cursor.
    fn release_foreign_u16s(&self, block: ForeignBlock<'a, u16>) {
        let Hooks {
            deallocate, opaque, ..
        } = self.hooks;
        Self::call_zfree(deallocate, opaque, block.as_mut_ptr().cast::<u8>());
    }
}

// ---------------------------------------------------------------------------
// Single-value placement, shared by the state round-trip
// ---------------------------------------------------------------------------

#[cfg(feature = "libz-compat")]
impl StreamAllocator {
    /// Allocates room for one `T` through this allocator and moves `value` into
    /// it.
    ///
    /// The Rust form of `ZALLOC(strm, 1, sizeof(deflate_state))`
    /// (`deflate.c` L440) and its inflate counterparts (`inflate.c` L197-L198,
    /// `infback.c` L51). [`Buffer`] cannot express this shape: it holds a slice,
    /// and a `Vec<u8>`-backed slice is byte-aligned, so it could not host a value
    /// whose alignment exceeds one. Placing a typed value is consequently a raw
    /// pointer write, which is why the core delegates it to this crate.
    ///
    /// Returns [`None`] if the request cannot be met, if the block is not aligned
    /// for `T`, or if `T` is zero-sized. A misaligned block is handed straight
    /// back to the allocator rather than abandoned, because an abandoned block is
    /// a leak and `test/infcover.c`'s `mem_done` reports leaks.
    ///
    /// This function is safe: every unsafe step inside it is justified locally,
    /// and a successful result is a fully initialised `T` at a correctly aligned
    /// address. Recovering the value again is not safe, which is why
    /// [`StreamAllocator::displace`] carries the obligations.
    ///
    /// # ★ The move, and what it costs
    ///
    /// C initialises its state *in place*: `deflateInit2_` allocates `sizeof(deflate_state)` and
    /// then assigns each field through `s->` (`deflate.c` L440-L505). This crate cannot, because the
    /// core owns the state type and constructs it as a Rust value; that value is then moved here.
    /// The state is 6144 bytes for deflate and 7296 for inflate, so the move is a copy of that much
    /// -- one pass, once per stream, and it is the second half of the initialisation cost whose first
    /// half is the buffer fill documented on `zlib_rs::allocate::Buffer::try_global`.
    ///
    /// Measured, the two together are 6.10 us per `deflateInit2_` + `deflateEnd` at the default
    /// configuration against C's 1.40 us. The buffer fill dominates: a quarter of a megabyte against
    /// six kilobytes, forty times the bytes. Constructing the state directly in the destination
    /// storage would recover the smaller half, and it would require the core to hand out a
    /// partially-initialised state -- which is exactly what `#![forbid(unsafe_code)]` prevents it
    /// from doing, and what keeps every field's initialisation checkable by the compiler. The trade
    /// is deliberate, the amortisation crossover is measured (a stream that compresses 64 KiB or more
    /// is already faster end to end than C's), and the figure is reported rather than absorbed into a
    /// throughput number.
    fn place<T>(&self, value: T) -> Option<NonNull<T>> {
        // A zero-sized `T` has no valid allocation: `malloc(0)` may legitimately
        // return null, and a hook is entitled to do the same. No state type is
        // zero-sized -- `StateBlock` always carries its prefix -- so this is a
        // guard rather than a case.
        if size_of::<T>() == 0 {
            return None;
        }

        let Hooks {
            allocate,
            deallocate,
            opaque,
        } = self.hooks;

        let (base, _len) = Self::call_zalloc(allocate, opaque, 1, size_of::<T>())?;
        let slot = base.cast::<T>();
        if !slot.as_ptr().is_aligned() {
            Self::call_zfree(deallocate, opaque, base.as_ptr());
            return None;
        }
        // SAFETY: unsafe-site category 3 -- moving the state into a block the
        // stream's `zalloc` returned. `call_zalloc` was asked for
        // `1 * size_of::<T>()` bytes and returned non-null, so by the `zlib.h` L149
        // contract the region holds that many writable bytes; the line above proved
        // the address is aligned for `T`. The block is freshly allocated, so nothing
        // else references it, and its contents are uninitialised, so it is written
        // rather than assigned.
        unsafe { slot.as_ptr().write(value) };
        Some(slot)
    }

    /// Moves the `T` at `slot` back out and returns its storage to this
    /// allocator.
    ///
    /// The exact inverse of [`StreamAllocator::place`]: `ZFREE(strm, s)` from
    /// `deflate.c` L1305, through whichever `zfree` this allocator carries.
    ///
    /// # Safety
    ///
    /// * `slot` must have been produced by [`StreamAllocator::place`] on an
    ///   allocator with the same `(zalloc, zfree, opaque)` triple as this one, and
    ///   with the same `T`. Returning a block to a different triple is exactly the
    ///   mismatched-allocator bug this crate exists to prevent, and it is what
    ///   `test/infcover.c` counts as a *rogue* free.
    /// * `slot` must not have been displaced already, and nothing may reference it
    ///   afterwards: this both reads the value out and frees the storage.
    unsafe fn displace<T>(&self, slot: NonNull<T>) -> T {
        // SAFETY: unsafe-site category 3 -- recovering the state object. By this
        // function's contract `slot` addresses an initialised,
        // aligned `T` that nothing else references. The read moves the value out;
        // the storage below is released without running a destructor on it, which
        // is correct because ownership has transferred to the returned value.
        let value = unsafe { slot.as_ptr().read() };

        // SAFETY: unsafe-site category 4 -- the caller's `zfree`. `release`'s
        // contract is satisfied by this function's: `slot` came from `place` or
        // `reserve` on this triple, it has not been released before, nothing
        // references it, and the value was moved out by the read above, so no
        // destructor is owed.
        unsafe { self.release(slot) };

        value
    }

    /// Requests room for one `T` without initialising it.
    ///
    /// [`StreamAllocator::place`] split at the seam: the allocation happens here
    /// and the move happens in [`StreamAllocator::occupy`]. The split exists
    /// because C makes the state request *before* the requests for the buffers the
    /// state owns -- `ZALLOC(strm, 1, sizeof(deflate_state))` at `deflate.c` L440
    /// precedes the window, `prev`, `head` and `pending_buf` requests at L468-L479
    /// -- while a Rust state value cannot be built until the buffers it owns
    /// exist. Reserving first and moving in afterwards reproduces C's order of
    /// calls into the caller's `zalloc` exactly, which matters because
    /// `test/infcover.c`'s tracking allocator records the order and reports any
    /// non-last-in-first-out release against it.
    ///
    /// Returns [`None`] on the same three conditions as
    /// [`StreamAllocator::place`]: a refused request, a block misaligned for `T`,
    /// or a zero-sized `T`. A misaligned block is handed straight back rather than
    /// abandoned, for the reason given there.
    ///
    /// The block is deliberately **not** filled with [`FILL_BYTE`]: unlike the
    /// buffer path, every byte of it is written by [`StreamAllocator::occupy`]
    /// before anything reads it, and C does not pre-fill this allocation either.
    ///
    /// A reservation is uninitialised storage that this allocator now expects
    /// back. It must reach exactly one of [`StreamAllocator::occupy`] followed
    /// eventually by [`StreamAllocator::displace`], or
    /// [`StreamAllocator::release`] -- anything else leaks the block, and
    /// `test/infcover.c`'s `mem_done` reports leaks as defects.
    fn reserve<T>(&self) -> Option<NonNull<T>> {
        // As `place`: a zero-sized `T` has no valid allocation.
        if size_of::<T>() == 0 {
            return None;
        }

        let Hooks {
            allocate,
            deallocate,
            opaque,
        } = self.hooks;

        let (base, _len) = Self::call_zalloc(allocate, opaque, 1, size_of::<T>())?;
        let slot = base.cast::<T>();
        if !slot.as_ptr().is_aligned() {
            Self::call_zfree(deallocate, opaque, base.as_ptr());
            return None;
        }
        Some(slot)
    }

    /// Moves `value` into a block [`StreamAllocator::reserve`] produced.
    ///
    /// The second half of [`StreamAllocator::place`]. Afterwards `slot` addresses
    /// an initialised `T` and is recoverable with
    /// [`StreamAllocator::displace`], exactly as though `place` had produced it.
    ///
    /// # Safety
    ///
    /// * `slot` must have come from [`StreamAllocator::reserve`] on an allocator
    ///   carrying the same `(zalloc, zfree, opaque)` triple, with the same `T`.
    /// * The block must still be uninitialised -- that is, `slot` must not have
    ///   been occupied already -- because the write does not drop what it
    ///   overwrites.
    // The receiver is not read, and is kept anyway: it is what names *which*
    // allocator the block must have come from, both in the safety contract above and
    // at every call site, and it keeps this method beside `reserve`, `release`,
    // `place` and `displace` instead of turning one step of the sequence into a free
    // function. Written as a comment rather than the lint's `reason` field, which
    // needs Rust 1.81 and this workspace declares `rust-version = "1.80"`.
    #[allow(clippy::unused_self)]
    unsafe fn occupy<T>(&self, slot: NonNull<T>, value: T) {
        // SAFETY: unsafe-site category 3 -- moving the state into a block the
        // stream's `zalloc` returned. `reserve` asked for `1 * size_of::<T>()`
        // bytes and returned non-null, so by the `zlib.h` L149 contract the region
        // holds that many writable bytes, and it proved the address aligned for
        // `T`. This function's contract makes the block still uninitialised and
        // unreferenced, so it is written rather than assigned.
        unsafe { slot.as_ptr().write(value) };
    }

    /// Returns a block's storage to this allocator without reading it.
    ///
    /// The counterpart of [`StreamAllocator::reserve`] for a reservation that was
    /// never occupied -- the `ZFREE(strm, s)` C reaches when a later allocation
    /// fails -- and the tail of [`StreamAllocator::displace`], which reads the
    /// value out first and then releases the storage through this.
    ///
    /// Nothing is dropped, which is why it is sound for uninitialised storage and
    /// why a caller holding an initialised block must take the value out first.
    ///
    /// # Safety
    ///
    /// * `slot` must have come from [`StreamAllocator::reserve`] or
    ///   [`StreamAllocator::place`] on an allocator carrying the same
    ///   `(zalloc, zfree, opaque)` triple. Returning a block to a different triple
    ///   is the mismatched-allocator bug this crate exists to prevent, and what
    ///   `test/infcover.c` counts as a *rogue* free.
    /// * The block must not have been released already, and nothing may reference
    ///   it afterwards.
    /// * If the block was occupied, the value must already have been moved out:
    ///   this runs no destructor, so anything the value owned would leak.
    unsafe fn release<T>(&self, slot: NonNull<T>) {
        let Hooks {
            deallocate, opaque, ..
        } = self.hooks;
        Self::call_zfree(deallocate, opaque, slot.as_ptr().cast::<u8>());
    }
}

// ---------------------------------------------------------------------------
// Widening -- `uInt` counts to Rust lengths
// ---------------------------------------------------------------------------

/// Widens a C `uInt` count to a Rust length.
///
/// Every `avail_in`, `avail_out` and buffer size at the boundary is a [`uInt`],
/// and every Rust slice length is a [`usize`]. The conversion cannot lose a bit:
/// `uInt` is `unsigned int`, four bytes on every target this crate supports, and
/// `usize` is at least that wide wherever `std` and a C ABI are both available --
/// a relationship the module asserts at compile time rather than assumes.
///
/// Provided as a named function so that the widening reads the same at every call
/// site and so that the justification lives in one place instead of beside every
/// cast.
// Why these two are allowed, rather than worked around:
//
// * `cast_possible_truncation` -- cannot occur. `size_of::<uInt>() <=
//   size_of::<usize>()` is asserted at compile time near the top of this module, so
//   a target on which this could truncate fails to build.
// * `cast_lossless` -- there is nothing to convert to. No `From<u32> for usize`
//   impl exists, because the conversion is not lossless on a 16-bit target, and
//   this crate supports none.
//
// Written as a comment rather than as the attribute's `reason` field: lint reasons
// were stabilised in Rust 1.81 and this workspace declares
// `rust-version = "1.80"`, so `reason = "..."` fails to compile on the declared
// MSRV with "lint reasons are experimental" -- verified against 1.80.0 rather than
// assumed.
#[must_use]
#[cfg(feature = "libz-compat")]
#[allow(clippy::cast_possible_truncation, clippy::cast_lossless)]
pub(crate) const fn widen(count: uInt) -> usize {
    count as usize
}

// ---------------------------------------------------------------------------
// Byte-range disjointness -- the precondition every slice pair depends on
// ---------------------------------------------------------------------------

/// Reports whether the two byte ranges `(a, a_len)` and `(b, b_len)` share no byte.
///
/// ★ **This is the test that makes [`input_slice`] and the output helpers
/// [`output_region`] and [`output_slots_mut`] sound when both are called for the
/// same stream.** A `&[u8]` and a `&mut [u8]` may not
/// alias, and it is undefined behaviour for them to do so *even if neither is
/// dereferenced*. The C API, however, does not forbid a caller from pointing
/// `next_in` and `next_out` into one buffer: `zlib.h` L138-L139 describes the two
/// pairs independently and says nothing about them being distinct. So overlap is
/// an input this library can receive, and it has to be rejected **before** the two
/// borrows exist rather than tolerated afterwards.
///
/// Two ranges are disjoint when one ends at or before the other begins. A zero
/// length is disjoint from everything, which is the right answer twice over: an
/// empty slice borrows nothing, and the helpers above return an empty slice
/// without calling [`core::slice::from_raw_parts`] at all in that case.
///
/// The addresses are compared as integers because that is the only way to express
/// "these two ranges are disjoint". Nothing is dereferenced and no integer is
/// turned back into a pointer, so no provenance is created or lost, and this
/// function is safe to call with wild, dangling or null pointers -- which is
/// exactly why it can run before validation rather than after.
#[must_use]
pub(crate) fn ranges_are_disjoint(
    a: *const Bytef,
    a_len: usize,
    b: *const Bytef,
    b_len: usize,
) -> bool {
    if a_len == 0 || b_len == 0 {
        return true;
    }
    let a_start = a as usize;
    let b_start = b as usize;
    // `checked_add` rather than `+` or `saturating_add`, and the overflow arm answers
    // "not disjoint". A caller's length is arbitrary, so a range whose end cannot be
    // represented is an input this gate can receive -- and it describes no real
    // allocation, because Rust and C both bound an object at `isize::MAX` bytes. A
    // saturating end would be *wrong* rather than merely conservative here: the
    // wrapped range covers low addresses, so the pair can genuinely overlap while the
    // saturated comparison says otherwise. Refusing is the fail-closed answer.
    let (Some(a_end), Some(b_end)) = (a_start.checked_add(a_len), b_start.checked_add(b_len))
    else {
        return false;
    };
    a_end <= b_start || b_end <= a_start
}

/// Reports whether two `T` pointers address regions that do not overlap at all.
///
/// `deflateCopy` and `inflateCopy` copy `sizeof(z_stream)` bytes from one stream to
/// another (`deflate.c` L1333, `inflate.c` L1352), which is defined only for
/// non-overlapping regions -- in C as much as in Rust, since `memcpy`'s own contract
/// requires it. Two correctly aligned pointers can still overlap *partially*, at any
/// multiple of the alignment below the struct size, so a plain equality test is not
/// sufficient and the full range has to be compared.
///
/// Shares [`ranges_are_disjoint`]'s reasoning about provenance: this is arithmetic on
/// addresses, not on pointers, and nothing is dereferenced.
#[must_use]
pub(crate) fn structs_are_disjoint<T>(first: *const T, second: *const T) -> bool {
    (first as usize).abs_diff(second as usize) >= size_of::<T>()
}

/// Reports whether two whole [`z_stream`] structures are disjoint.
///
/// The [`structs_are_disjoint`] instantiation `deflateCopy` and `inflateCopy` need,
/// named so that neither has to spell the type parameter and so that the two cannot
/// drift apart.
#[must_use]
pub(crate) fn streams_are_disjoint(dest: z_streamp, source: z_streamp) -> bool {
    structs_are_disjoint(dest.cast_const(), source.cast_const())
}

// ---------------------------------------------------------------------------
// Alias-tolerant input capture -- unsafe-site category 2
// ---------------------------------------------------------------------------

/// An owned copy of a caller's input range, used when that range overlaps the
/// caller's output range.
///
/// ★ **Why a copy, rather than a refusal.** [`ranges_are_disjoint`] can *detect* an
/// overlapping `(next_in, avail_in)` / `(next_out, avail_out)` pair, and for a while the
/// obvious response was to answer `Z_STREAM_ERROR`. That is wrong, and `test/example.c`
/// proves it: `test_large_deflate` sets `next_out = compr`, deflates 40 000 bytes into it,
/// and then at L275-L277 sets `next_in = compr` with `avail_in = uncomprLen/2` while
/// `next_out` still points a few hundred bytes into that same `compr` — deliberately
/// feeding already-compressed data back through the compressor. Reference zlib runs it,
/// and `test_large_inflate` then asserts `total_out == 2*uncomprLen + uncomprLen/2`
/// (L327), which only holds if all `uncomprLen/2` overlapping bytes were consumed by that
/// one call. Refusing the pair fails the suite the port is required to pass unmodified,
/// so shrinking a range, splitting the call, or returning an error are all ruled out: the
/// call has to consume the whole input in one pass.
///
/// Copying the input is what makes that possible without aliasing. The snapshot is taken
/// while no mutable borrow of the output exists, so the shared read is sound; afterwards
/// the core is handed the snapshot plus a `&mut [u8]` over the caller's output, and those
/// two never overlap. Consumption accounting is unaffected, because the snapshot is
/// exactly `avail_in` bytes long and the caller's `next_in` is advanced by the count the
/// core reports, not by anything derived from the snapshot's address.
///
/// The one observable difference from C is *which* bytes get compressed when the output
/// actually catches up with the unread input: C reads whatever it has just written over,
/// whereas this reads the values the buffer held when the call began. C's own answer there
/// is unspecified — it depends on the interleaving of `read_buf` and `flush_pending` — so
/// this is a divergence within the space C leaves undefined, and it is the more defined of
/// the two. Every disjoint call, which is every call the differential corpus makes, is
/// completely untouched by this path.
///
/// ★ **Allocation goes through the stream's own allocator, not Rust's.** `zlib.h`
/// L140-L153 makes `zalloc`/`zfree`/`opaque` the caller's resource policy for a stream,
/// and a buffer whose size is `avail_in` -- the caller's own number -- is exactly the kind
/// of allocation that policy exists to govern: a custom arena has to be able to see it,
/// and `test/infcover.c`'s `mem_limit()` has to be able to refuse it. Reaching past the
/// hooks to Rust's global allocator would put a caller-sized request outside both, which
/// is CWE-789 in the small. So [`AliasScratch::capture`] takes the allocator the entry
/// point already read off the stream and returns the block through that same allocator,
/// which is the discipline `zlib.h` L151-L153 states and [`Buffer::release_to`] enforces.
///
/// The release is the other half of the contract and is why [`AliasScratch::release`]
/// exists rather than a `Drop` implementation: the block must go back to *this*
/// allocator, and a `Drop` has no way to name it. It is released before the entry point
/// returns and after every borrow of it has ended, so a tracking allocator sees a strict
/// last-in, first-out pair around one call -- which is what `test/infcover.c`'s
/// `mem_done()` checks for.
///
/// Allocation failure is reported rather than fatal, for the same reason a system library
/// should not abort on a large `avail_in`: [`AliasScratch::capture`] answers [`None`] and
/// the caller maps it to the outcome its own entry point documents.
///
/// Gated on `libz-compat`, which is what turns on the two modules that use it. Every other
/// helper in this module is `pub` and therefore exempt from `dead_code` by construction; this
/// one is deliberately crate-private -- an internal scratch buffer has no business in the
/// crate's Rust surface -- so the gate is what keeps a `--no-default-features` build free of
/// `dead_code`, rather than an `allow` that would also hide a genuine future disuse.
#[cfg(feature = "libz-compat")]
pub(crate) struct AliasScratch {
    /// The captured bytes, in a block this value owns until [`AliasScratch::release`]
    /// hands it back. Exactly as long as the count [`AliasScratch::capture`] was given.
    ///
    /// An [`Option`] because the release takes the block out through the slot: a
    /// reference in argument position is protected for the duration of the call, and
    /// freeing memory a protected reference covers is undefined behaviour, so
    /// [`Allocator::deallocate_bytes`] is documented to take the *slot*.
    bytes: Option<Buffer<'static, u8>>,
}

#[cfg(feature = "libz-compat")]
impl AliasScratch {
    /// Copies `len` bytes from `src` into a block from `allocator`, or reports that the
    /// allocation failed.
    ///
    /// The block is `len` bytes exactly, requested as `len` items of one byte, which is
    /// the shape `ZALLOC(strm, len, 1)` would have.
    ///
    /// # Safety
    ///
    /// `src` and `len` must satisfy [`input_slice`]'s contract: either `len` is zero, or
    /// `len` bytes are readable at `src` and nothing mutates them for the duration of this
    /// call. No mutable borrow of any part of that region may exist, which is what makes
    /// the shared read sound even when the region overlaps the caller's output.
    ///
    /// The value must be released with [`AliasScratch::release`], passing the same
    /// `allocator`, before it is dropped.
    pub(crate) unsafe fn capture(
        allocator: &StreamAllocator,
        src: *const Bytef,
        len: usize,
    ) -> Option<Self> {
        let mut block = allocator.allocate_bytes(len, 1)?;
        // SAFETY: unsafe-site category 2 -- one shared slice over the caller's input,
        // formed under this function's contract, which is `input_slice`'s. It is the only
        // borrow of that region in existence at this point: the mutable borrow of the
        // output is created by the caller *after* this returns, precisely so that the two
        // never coexist. The slice dies at the end of this statement.
        let source = unsafe { core::slice::from_raw_parts(src, len) };
        // The block arrives holding `FILL_BYTE`, never zero, so every byte is written
        // rather than appended to. `copy_from_slice` panics on a length mismatch, so the
        // lengths are reconciled first: the block is `len` bytes by construction and the
        // slice is `len` bytes by this function's contract, and the `get_mut` states that
        // rather than asserting it.
        let written = block.as_mut_slice().get_mut(..len).map(|window| {
            window.copy_from_slice(source);
        });
        if written.is_none() {
            // Unreachable: the block was requested as `len` bytes. Releasing rather than
            // leaking is what makes it unreachable *and* harmless.
            allocator.deallocate_bytes(&mut Some(block));
            return None;
        }
        Some(Self { bytes: Some(block) })
    }

    /// Returns the block to the allocator it came from.
    ///
    /// Must be called with the allocator [`AliasScratch::capture`] was given, and must be
    /// called after every borrow taken through [`AliasScratch::view`] has ended.
    pub(crate) fn release(&mut self, allocator: &StreamAllocator) {
        allocator.deallocate_bytes(&mut self.bytes);
    }

    /// The captured bytes, with the `'static` lifetime the core's stream views require.
    ///
    /// # Safety
    ///
    /// The returned slice must not be used after `self` is dropped. Callers satisfy this
    /// by keeping the [`AliasScratch`] alive in a binding that outlives the stream view
    /// built from it -- the same discipline [`input_slice`] already relies on, and the
    /// reason this returns `'static` at all: the core's stream types take owned lifetimes
    /// that the raw C boundary cannot otherwise supply.
    #[must_use]
    pub(crate) unsafe fn view(&self) -> &'static [u8] {
        let Some(block) = self.bytes.as_ref() else {
            // Released already, which this function's contract forbids. An empty slice is
            // the one answer that cannot be unsound.
            return &[];
        };
        let captured = block.as_slice();
        // SAFETY: unsafe-site category 2 -- one shared slice over memory this value owns.
        // The pointer is non-null, aligned for `u8` and valid for `len` initialised bytes,
        // because `capture` wrote every one of them; so the slice is well formed. The
        // `'static` lifetime is fabricated and is exactly what this function's contract
        // makes the caller responsible for.
        unsafe { core::slice::from_raw_parts(captured.as_ptr(), captured.len()) }
    }
}

/// The allocator an overlap snapshot for `strm` must be taken from.
///
/// [`StreamAllocator::from_stream_ptr`] reads the caller's published triple, which for any
/// stream this library initialised is either the caller's own hooks or the library's own
/// substituted routines -- exactly the pair `zlib.h` L151-L153 says a caller may observe.
/// The fallback matters only for a stream that has no hooks at all, which no initialised
/// stream is: [`StreamAllocator::internal`] then names the same two routines the
/// substitution would have installed, so the snapshot is never taken from Rust's global
/// allocator on any path.
///
/// # Safety
///
/// `strm` must be null or address a live [`z_stream`], as
/// [`StreamAllocator::from_stream_ptr`] requires.
#[cfg(feature = "libz-compat")]
#[must_use]
pub(crate) unsafe fn scratch_allocator(strm: z_streamp) -> StreamAllocator {
    // SAFETY: unsafe-site category 4 -- reads the three hook members through raw places.
    // `strm` is live or null by this function's contract, which is the helper's own.
    let published = unsafe { StreamAllocator::from_stream_ptr(strm) };
    match published {
        Some(allocator) => allocator,
        None => StreamAllocator::internal(),
    }
}

/// [`AliasScratch::view`] lifted over [`Option`], which is the shape every caller wants.
///
/// The overlap check yields an `Option<AliasScratch>` -- [`None`] on the ordinary disjoint
/// path -- and the stream builders take an `Option<&'static [u8]>`, so every call site would
/// otherwise spell the same three-line `map` with the same safety comment. Naming it once
/// keeps the invariant in one place.
///
/// # Safety
///
/// The returned slice must not be used after `scratch` is dropped, exactly as
/// [`AliasScratch::view`] requires. Callers satisfy this by passing a borrow of a binding
/// that outlives the stream view built from the result.
///
/// Gated on `libz-compat` for the reason [`AliasScratch`] is.
#[cfg(feature = "libz-compat")]
#[must_use]
pub(crate) unsafe fn scratch_view(scratch: Option<&AliasScratch>) -> Option<&'static [u8]> {
    scratch.map(|scratch| {
        // SAFETY: unsafe-site category 2 -- `AliasScratch::view`'s contract is this
        // function's, and this only forwards it: the caller owns the requirement that the
        // slice not outlive the scratch it borrows.
        unsafe { scratch.view() }
    })
}

// ---------------------------------------------------------------------------
// Stream-pointer validation -- unsafe-site category 1
// ---------------------------------------------------------------------------

// ★ This section is deliberately empty of helpers, and must stay that way.
//
// Category 1 is discharged without ever forming a `&z_stream` or a `&mut z_stream`:
// [`StreamAllocator::from_stream_ptr`] reads the three hook members, `copy_stream`
// copies the twelve `deflateCopy`/`inflateCopy` members, and the state helpers at the
// end of this module write `state` -- each of them testing `strm` for null and
// alignment and then touching one member at a time through
// [`core::ptr::addr_of!`]/[`core::ptr::addr_of_mut!`]. A borrowing constructor did
// live here; it was removed because nothing called it and unreachable `unsafe` is
// audit surface with no counterpart in behaviour. Do not restore it: a reference to
// the whole struct cannot coexist with the later use of the caller's own pointer that
// the opaque-`state` round-trip needs, which is the soundness argument the module
// header sets out above [`checked_state`] and repeats at length below.

// ---------------------------------------------------------------------------
// Slice reconstruction -- unsafe-site category 2
// ---------------------------------------------------------------------------

/// Rebuilds the caller's input as a slice from `(next_in, avail_in)`.
///
/// The pair is the whole input contract: `zlib.h` L138-L139 has the application
/// update both when the input is exhausted. This is the *only* place in the crate
/// that turns them into a slice, so that [`core::slice::from_raw_parts`] appears
/// once and can be audited once. Call it immediately on entry; only the resulting
/// safe slice travels inward to [`zlib_rs`].
///
/// # ★ The zero-length case is not a formality
///
/// `core::slice::from_raw_parts(null, 0)` is **undefined behaviour** -- the
/// pointer must be non-null and aligned even for an empty slice. A `z_stream`
/// with `avail_in == 0` and `next_in == Z_NULL` is entirely ordinary, however:
/// `zlib.h` L138-L139 invites exactly that state, `test/infcover.c` L399-L400
/// sets it deliberately, and `inflate`'s own guard only rejects a null `next_in`
/// when `avail_in` is non-zero. So the zero case is branched on and yields a
/// genuine empty slice rather than a dangling one.
///
/// # Safety
///
/// If `avail_in` is non-zero, `next_in` must be non-null, aligned (trivially, for
/// bytes) and readable for `avail_in` bytes, and that region must stay valid and
/// unwritten by anything else for `'a`.
#[cfg(feature = "libz-compat")]
#[must_use]
pub(crate) unsafe fn input_slice<'a>(next_in: *const Bytef, avail_in: uInt) -> &'a [u8] {
    let len = widen(avail_in);
    if len == 0 || next_in.is_null() {
        // Either there is nothing to read, or there is nowhere to read it from --
        // and in the second case the entry point's own guard reports the error.
        // Returning the empty slice keeps a null pointer out of `from_raw_parts`.
        return &[];
    }
    // SAFETY: unsafe-site category 2 -- slice reconstruction. `next_in` is
    // non-null by the test above and trivially aligned for `u8`. `len` is
    // `avail_in` widened without loss, and this function's contract makes those
    // bytes readable and stable for `'a`. The library never writes through this
    // slice, which is why the shared borrow is the right shape.
    unsafe { core::slice::from_raw_parts(next_in, len) }
}

/// Rebuilds the caller's output as **write-only** storage from `(next_out, avail_out)`.
///
/// The [`input_slice`] counterpart, and the only place
/// [`core::slice::from_raw_parts_mut`] is called for a caller's output buffer.
/// The same zero-length rule applies and for the same reason: a stream with
/// `avail_out == 0` and a null `next_out` must yield an empty slice, not
/// undefined behaviour.
///
/// # ★ `MaybeUninit<u8>`, not `u8`, and this is not a formality
///
/// `zlib.h` L94-L95 promises `avail_out` bytes of *room* at `next_out` and says nothing
/// whatever about their contents: `test/example.c` L86 passes a buffer straight from
/// `malloc`, and every application that reuses a buffer passes one whose tail holds
/// whatever the previous call left. Rust does not allow a `&mut [u8]` to address a byte
/// that holds no value -- uninitialised memory is not a valid `u8` -- so presenting a
/// caller's output buffer as a byte slice would be undefined behaviour on entirely
/// ordinary, documented usage, however carefully the library then wrote to it.
///
/// The other route, initialising the buffer here so that the byte slice becomes legal,
/// would add an `avail_out`-sized write to **every** call. On the decompression side that
/// is a second pass over the whole output and is exactly the cost the AAP's ≤10%
/// throughput budget (§0.8.4) has no room for.
///
/// So the buffer keeps the shape it actually has. `zlib_rs`'s [`OutputRegion`] writes
/// through it without ever forming a byte reference to an unwritten slot, tracks how far
/// it has written, and reinterprets only that prefix -- through [`init_view`] -- when the
/// decoder needs to read its own output back. Nothing is initialised that the library was
/// not going to write anyway.
///
/// # Safety
///
/// If `avail_out` is non-zero, `next_out` must be non-null and writable for
/// `avail_out` bytes, and that region must not be aliased by anything else --
/// including by the input slice -- for `'a`. Call this once on entry; two live
/// mutable slices over the same output buffer would be undefined behaviour even
/// if neither were written.
#[cfg(feature = "libz-compat")]
#[must_use]
pub(crate) unsafe fn output_slots_mut<'a>(
    next_out: *mut Bytef,
    avail_out: uInt,
) -> &'a mut [MaybeUninit<u8>] {
    let len = widen(avail_out);
    if len == 0 || next_out.is_null() {
        return &mut [];
    }
    // SAFETY: unsafe-site category 2 -- slice reconstruction. `next_out` is
    // non-null by the test above and trivially aligned for `u8`, which
    // `MaybeUninit<u8>` shares because it is `repr(transparent)` over it. `len` is
    // `avail_out` widened without loss, and this function's contract makes those
    // bytes writable, unaliased and stable for `'a`. No initialisation is required for
    // this element type, which is the whole point of it.
    unsafe { core::slice::from_raw_parts_mut(next_out.cast::<MaybeUninit<u8>>(), len) }
}

#[cfg(feature = "libz-compat")]
/// Rebuilds the caller's output as the region the core writes through.
///
/// [`output_slots_mut`] plus the [`init_view`] the region needs, in the one shape every
/// stream entry point wants. Kept as a separate function so that the pairing of a
/// caller's buffer with *this crate's* view happens in exactly one place.
///
/// # Safety
///
/// [`output_slots_mut`]'s, unchanged.
#[must_use]
pub(crate) unsafe fn output_region<'a>(next_out: *mut Bytef, avail_out: uInt) -> OutputRegion<'a> {
    // SAFETY: this function's contract is `output_slots_mut`'s contract.
    let slots = unsafe { output_slots_mut(next_out, avail_out) };
    OutputRegion::write_only(slots, init_view())
}

#[cfg(feature = "libz-compat")]
/// Rebuilds `inflateBack`'s caller-supplied window as the write-only storage it is.
///
/// # ★ The window is storage, and the reference never writes it
///
/// `inflateBackInit_` is handed a bare `unsigned char *`. `zlib.h` L1163-L1166 asks only
/// for room -- "window is a user-supplied window and output buffer that is `2**windowBits`
/// bytes" -- and `test/infcover.c` L475 duly passes an *uninitialised* stack array,
/// `unsigned char win[32768];`. `infback.c` L25-L64 records the pointer and the extent and
/// touches not one byte, so whatever the caller left in that buffer is still there when
/// `inflateBackInit_` returns, and stays there until the decoder writes output over it.
///
/// So a `&mut [u8]` is the wrong shape twice over: the memory may hold nothing, and
/// filling it to manufacture one would destroy bytes the caller is entitled to keep and
/// would make the visible contents depend on which fill byte a build chose. The slots go
/// to [`zlib_rs::read_buf::OutputRegion::write_only`] instead, exactly as a C caller's
/// `next_out` does, and the region hands the decoder back only the prefix it has written.
///
/// # Safety
///
/// If `extent` is non-zero, `window` must be non-null and writable for `extent` bytes, and
/// that region must stay valid and unaliased by anything else for `'a` -- which for
/// `inflateBack` means for the duration of the one call the slice is built for. Only one
/// slice may be live at a time; the previous call's was dropped before it returned.
///
/// Nothing about the *contents* is required, which is the whole point: the elements are
/// [`MaybeUninit`], so the borrow asserts writability and nothing more.
#[must_use]
pub(crate) unsafe fn window_slots_mut<'a>(
    window: *mut Bytef,
    extent: uInt,
) -> &'a mut [MaybeUninit<u8>] {
    let len = widen(extent);
    if len == 0 || window.is_null() {
        // `from_raw_parts_mut` may not be called with a null pointer even for a zero
        // length; `inflateBackInit_` rejects both cases anyway.
        return &mut [];
    }
    // SAFETY: unsafe-site category 2 -- slice reconstruction. `window` is non-null by the
    // test above, and `MaybeUninit<u8>` is `repr(transparent)` over `u8`, so it needs no
    // alignment beyond one and the cast changes neither the address nor the length. `len` is
    // the extent this stream was initialised with, which this function's contract makes
    // writable and unaliased for `'a`. No initialisation is required or claimed.
    unsafe { core::slice::from_raw_parts_mut(window.cast::<MaybeUninit<u8>>(), len) }
}

/// Reinterprets written slots as the bytes they hold.
///
/// # ★ The one place `MaybeUninit<u8>` becomes `u8` in this crate
///
/// `zlib_rs` cannot perform this step: its crate root asserts
/// `#![forbid(unsafe_code)]`, and no *safe* stable API turns `[MaybeUninit<u8>]` into
/// `[u8]`. It also genuinely needs the step -- `inflate.c` L1080 folds the bytes just
/// written into the check value, L1136 copies them into the sliding window -- so the
/// operation is supplied from here, as a function pointer, and the unsafe reinterpretation
/// stays inside the one crate the AAP permits it in.
///
/// The obligation this discharges is that every slot passed in has been written.
/// [`OutputRegion`] discharges it structurally: it raises its own high-water mark in its
/// write methods and nowhere else, and clamps every read to it, so this function is never
/// reached with a slot the library has not stored a byte into. Its documentation carries
/// the argument in full.
#[must_use]
#[cfg(feature = "libz-compat")]
pub(crate) fn init_view() -> InitView {
    InitView::new(view_written_slots)
}

/// The one reinterpretation [`init_view`] supplies. Separate so that the pointer taken is a
/// plain `fn` item rather than a closure, which is what [`InitView`] holds.
///
/// ★ Shared only. An exclusive counterpart existed, for the gzip transparent read path, which
/// needed somewhere the operating system could write and obtained it by initialising the
/// caller's buffer first -- destroying the bytes past the count `gzread` returns, which C
/// leaves untouched. That path stages its read through the layer's own buffer now, so no
/// write-only region is ever handed out as writable bytes.
#[cfg(feature = "libz-compat")]
fn view_written_slots(slots: &[MaybeUninit<u8>]) -> &[u8] {
    // SAFETY: unsafe-site category 2 -- reinterpreting written output slots as the bytes
    // they hold. `MaybeUninit<u8>` is `repr(transparent)` over `u8`, so the pointer cast
    // changes neither the address, the length nor the alignment requirement, and the
    // lifetime of the result is tied to the borrow it came from. The elements are
    // initialised because `OutputRegion` calls this only on the prefix it has itself
    // written -- the clamp against its high-water mark is what establishes that, and it is
    // the reason this function is sound rather than merely conventional.
    unsafe { core::slice::from_raw_parts(slots.as_ptr().cast::<u8>(), slots.len()) }
}

/// Copies all `sizeof(z_stream)` bytes of one stream over another.
///
/// `zmemcpy(dest, source, sizeof(z_stream))` (`deflate.c` L1333, `inflate.c` L1352)
/// exactly: a **byte** copy, not a typed one. The distinction matters and is not
/// stylistic. [`z_stream::reserved`] (`zlib.h` L109) is a member the library never
/// writes and a caller need never initialise, and `msg` may hold indeterminate bytes
/// on a stream that has never reported an error, so reading the struct as a *value* --
/// `let snapshot: z_stream = source.read()` -- would be reading uninitialised memory
/// and interpreting it as a `c_ulong` and a pointer. That is undefined behaviour on a
/// perfectly ordinary C caller. A byte copy carries whatever is there without
/// interpreting any of it, which is what C does and what makes `dest` a complete copy
/// including any `reserved` the caller had set.
///
/// Provided once here rather than in each of the two entry points, so that the one
/// place the whole structure is duplicated can be audited in one place.
///
/// # Safety
///
/// Both pointers must be non-null, aligned, and address `size_of::<z_stream>()` bytes
/// -- `source` readable, `dest` writable -- and the two regions must not overlap, which
/// [`streams_are_disjoint`] establishes. Neither struct need be fully initialised.
pub(crate) unsafe fn copy_stream(dest: z_streamp, source: z_streamp) {
    // SAFETY: unsafe-site category 1 -- copying the caller's stream structure byte for
    // byte. Both pointers are non-null, aligned and cover `size_of::<z_stream>()`
    // readable or writable bytes by this function's contract, and the regions are
    // disjoint, which is `copy_nonoverlapping`'s remaining requirement. The element
    // type is `u8`, so the copy reinterprets nothing and requires no member to be
    // initialised; and it is a raw-pointer copy, so no reference to either stream is
    // formed and the caller's own pointers keep their provenance for the state install
    // that follows.
    unsafe {
        core::ptr::copy_nonoverlapping(
            source.cast::<u8>(),
            dest.cast::<u8>(),
            size_of::<z_stream>(),
        );
    }
}

// ---------------------------------------------------------------------------
// The opaque `state` round-trip -- unsafe-site category 3
// ---------------------------------------------------------------------------

/// Which state a tag belongs to, and hence which of the core's validators applies.
///
/// An enum rather than a function argument so that an entry point cannot pass the
/// wrong predicate: there are exactly two, and the mapping to the core's
/// [`deflate_state_check`] and [`inflate_state_check`] is fixed here.
#[cfg(feature = "libz-compat")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum StateKind {
    /// A compression state. Its tag is the `status` member of `deflate_state`,
    /// one of the eight values `deflateStateCheck` accepts (`deflate.c`
    /// L546-L555).
    Deflate,
    /// A decompression state driven by `inflate()`. Its tag is the `mode` member
    /// of `inflate_state`, which must lie in `HEAD..=SYNC`, i.e. `16180..=16211`
    /// (`inflate.c` L95).
    Inflate,
    /// A decompression state driven by `inflateBack()`.
    ///
    /// ★ **A distinct kind even though it shares `inflate`'s tag range, and that
    /// distinction is a soundness requirement rather than tidiness.** `inflateBack`
    /// installs its own payload behind `z_stream::state` -- a window borrowed from
    /// the caller rather than one the state owns -- so the block has a *different
    /// Rust type* from an `inflate()` block while carrying an indistinguishable
    /// `{strm, mode}` prefix. Without a separate kind, `inflate(strm, ...)` on an
    /// `inflateBack` stream (or `inflateBackEnd` on an ordinary one) would pass
    /// every one of C's checks, reinterpret the block at the wrong type and then
    /// read fields -- or free storage -- at the wrong layout. C has the same hole
    /// and gets away with it because both of its payloads really are
    /// `struct inflate_state`; here they are not, so the hole has to be closed.
    InflateBack,
}

#[cfg(feature = "libz-compat")]
impl StateKind {
    /// Reports whether `tag` must be **rejected**.
    ///
    /// The polarity matches C's, where a non-zero return from
    /// `deflateStateCheck`/`inflateStateCheck` means "reject", so that the Rust
    /// and C forms read the same way side by side. Routed through the core's own
    /// validators rather than reimplemented, which is what keeps the accepted set
    /// in one place.
    ///
    /// [`Self::InflateBack`] shares [`Self::Inflate`]'s predicate because it shares
    /// the same `inflate_mode` tag space; the two are told apart by
    /// `StateKind::cookie` (private), not by the tag.
    #[must_use]
    pub(crate) const fn rejects(self, tag: c_int) -> bool {
        match self {
            Self::Deflate => deflate_state_check(tag),
            Self::Inflate | Self::InflateBack => inflate_state_check(tag),
        }
    }

    /// The exact payload discriminator stored in [`StatePrefix::flavor`].
    ///
    /// Three distinct, deliberately unlikely 32-bit values rather than `0`, `1`, `2`:
    /// the field also has to reject *uninitialised* and *recycled* memory, and a small
    /// integer is a value stale storage plausibly already holds. The high byte spells
    /// `Z` and the low byte numbers the kind, so a hex dump of a live prefix is
    /// readable while a random word almost never matches.
    ///
    /// The value is private to this crate in the sense that matters: nothing exports
    /// it, no C header mentions it, and it occupies four bytes that lie **inside the
    /// padding of `deflate_state`** and at `inflate_state`'s `int last` -- neither of
    /// which `test/infcover.c` reads. See [`StatePrefix::flavor`].
    #[must_use]
    const fn cookie(self) -> u32 {
        match self {
            Self::Deflate => 0x5A_72_44_01,
            Self::Inflate => 0x5A_72_49_02,
            Self::InflateBack => 0x5A_72_42_03,
        }
    }
}

// The core's validators take `i32`. `c_int` is `i32` on every target that has
// both `std` and a C ABI, so the calls above need no conversion; the assertion
// turns a hypothetical target where that fails into a build error rather than a
// silent narrowing.
/// cbindgen:ignore
const _: () = assert!(size_of::<c_int>() == size_of::<i32>());

/// The C-visible head of every block [`z_stream::state`] points at.
///
/// **This exists because `test/infcover.c` writes through it.** Both C state
/// structs begin with the same two members -- `deflate.h` L105-L106 has
/// `z_streamp strm; int status;` and `inflate.h` L82-L84 has
/// `z_streamp strm; inflate_mode mode;` -- and the coverage harness includes
/// `inflate.h` directly (its L18) and then reaches straight through the opaque
/// pointer:
///
/// ```c
/// ((struct inflate_state *)strm.state)->mode = DICT;
/// state->mode = SYNC;
/// ```
///
/// -- `test/infcover.c` L330 and L458 respectively.
///
/// Measured layout those writes assume: `sizeof(struct inflate_state)` is 7160,
/// `strm` is at offset **0** and `mode` is at offset **8** with width **4**. The
/// `inflate_mode` values are deliberately not zero-based -- `HEAD` is `16180` and
/// runs consecutively to `SYNC` at `16211` (`inflate.h` L20-L52) -- so `DICT` is
/// `16190` and `SYNC` is `16211`. Those writes happen in *caller-compiled code*
/// that this port cannot change, so the prefix must be present, at offset 0, with
/// exactly that shape.
///
/// [`zlib_rs::inflate::state`] states the division explicitly: `InflateState`
/// carries no `#[repr(C)]` and no back-pointer, and the facade "must present a
/// synced `{ z_streamp strm; inflate_mode mode; }` prefix at the head of whatever
/// block `z_stream.state` points at". [`zlib_rs::inflate::InflateState::mode_tag`]
/// and `set_mode_tag` are the two primitives that make the syncing possible
/// without reinterpreting memory: read the tag out before acting on a call, and
/// write it back before returning.
///
/// # The two roles the tag plays
///
/// It is a *synchronisation slot* for the harness above, and it is the
/// *validity tag* that [`StateKind::rejects`] tests -- the reason
/// [`zlib_rs::inflate::Mode::Head`] starts at 16180 rather than at zero. A live
/// state, an uninitialised one and one whose memory has been overwritten are very
/// unlikely to hold a number in the accepted range, so the test rejects the
/// second and third. It is a heuristic and C says as much by pairing it with the
/// owner-identity check, which [`checked_state`] also performs.
#[cfg(feature = "libz-compat")]
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub(crate) struct StatePrefix {
    /// Offset 0. The stream that owns this state.
    ///
    /// `deflate.h` L105 and `inflate.h` L83, both `z_streamp strm`. C compares it
    /// against the incoming stream -- `state->strm != strm` at `inflate.c` L94 --
    /// to reject a state that belongs to a different stream, and
    /// [`checked_state`] performs the same comparison.
    pub(crate) strm: z_streamp,
    /// Offset 8. The state's validity tag: `status` for deflate, `mode` for
    /// inflate.
    ///
    /// Four bytes wide, matching the C `int` and the C enum. See
    /// [`StateKind::rejects`] for the accepted ranges.
    pub(crate) tag: c_int,
    /// Offset 12. Which *Rust* payload follows the prefix: [`StateKind::cookie`].
    ///
    /// ★ **This field costs nothing and closes a type-confusion hole the tag cannot.**
    /// [`StatePrefix::tag`] is four bytes at offset 8 inside a structure whose
    /// alignment is 8, so offsets 12 to 15 were already *padding* -- adding this field
    /// leaves `size_of::<StatePrefix>()` at 16, which `layout_assertions` asserts.
    ///
    /// Nothing in C looks here. `deflate_state` has `Bytef *pending_buf` next, which
    /// the ABI places at offset 16, leaving 12 to 15 as padding; `inflate_state` has
    /// `int last` at offset 12, and `test/infcover.c` -- the only consumer that
    /// reaches through the opaque pointer at all -- touches nothing but `->mode`
    /// (its L330 and L459). Both slots are therefore this port's to use.
    ///
    /// What it buys: [`StateKind::Inflate`] and [`StateKind::InflateBack`] put
    /// *different Rust types* behind `z_stream::state` while presenting the same
    /// prefix and the same tag range, so the four checks C performs cannot tell them
    /// apart. Requiring an exact cookie match before the block is reinterpreted turns
    /// "call `inflate()` on an `inflateBack` stream" from a wrong-layout read and a
    /// wrong-layout `zfree` into a plain `Z_STREAM_ERROR`.
    flavor: u32,
}

#[cfg(feature = "libz-compat")]
// ★ The prefix must stay exactly `{ pointer, int, int }` with no member beyond the ABI's
// own padding. That is what keeps `strm` at offset 0 and `tag` immediately after it --
// the two positions `test/infcover.c` reaches through when it assigns `->mode` (its L330
// and L459) -- and it is what bounds the flavor's cost.
//
// The cost is target-dependent and is stated rather than assumed. Where a pointer is
// eight bytes, `{ pointer, int }` already carried four bytes of tail padding, so the
// flavor is free and the prefix stays 16 bytes: the Rust payload does not move at all.
// On a 32-bit target `{ pointer, int }` has no tail padding, so the prefix grows from 8
// to 12 and the payload moves by four bytes -- invisible to C, which reads only offset 0
// and offset `sizeof(z_streamp)`, and comfortably inside the window
// `test/infcover.c` L428 leaves when it caps its zone at
// `(sizeof(struct inflate_state) << 1) + 256` and requires `inflateCopy` to fail.
//
// Written relationally so the assertions hold on every target rather than only on the
// verified one; the measured LP64 numbers are then pinned separately below. A future
// edit that reorders the members, widens `tag` or inserts a field is a build failure.
/// cbindgen:ignore
const _: () = assert!(core::mem::offset_of!(StatePrefix, strm) == 0);
#[cfg(feature = "libz-compat")]
/// cbindgen:ignore
const _: () = assert!(core::mem::offset_of!(StatePrefix, tag) == size_of::<z_streamp>());
#[cfg(feature = "libz-compat")]
/// cbindgen:ignore
const _: () = assert!(
    core::mem::offset_of!(StatePrefix, flavor) == size_of::<z_streamp>() + size_of::<c_int>()
);
#[cfg(feature = "libz-compat")]
/// cbindgen:ignore
const _: () = assert!(size_of::<StatePrefix>() == size_of::<z_streamp>() + 2 * size_of::<c_int>());
#[cfg(feature = "libz-compat")]
// The measured layout on the verified `x86_64-unknown-linux-gnu` target, and on LLP64
// Windows, where the pointer is also eight bytes: `strm` 0, `tag` 8, `flavor` 12, size 16.
#[cfg(target_pointer_width = "64")]
/// cbindgen:ignore
const _: () = assert!(size_of::<StatePrefix>() == 16);
#[cfg(feature = "libz-compat")]
#[cfg(target_pointer_width = "64")]
/// cbindgen:ignore
const _: () = assert!(core::mem::offset_of!(StatePrefix, tag) == 8);
#[cfg(feature = "libz-compat")]
#[cfg(target_pointer_width = "64")]
/// cbindgen:ignore
const _: () = assert!(core::mem::offset_of!(StatePrefix, flavor) == 12);
#[cfg(feature = "libz-compat")]
// The three cookies must stay pairwise distinct, or the check that tells an `inflate`
// block from an `inflateBack` block would accept both.
/// cbindgen:ignore
const _: () = assert!(StateKind::Deflate.cookie() != StateKind::Inflate.cookie());
#[cfg(feature = "libz-compat")]
/// cbindgen:ignore
const _: () = assert!(StateKind::Deflate.cookie() != StateKind::InflateBack.cookie());
#[cfg(feature = "libz-compat")]
/// cbindgen:ignore
const _: () = assert!(StateKind::Inflate.cookie() != StateKind::InflateBack.cookie());

/// A state object with the C-visible [`StatePrefix`] at offset 0.
///
/// `#[repr(C)]`, so `prefix` really is first and `strm` and `tag` really are at
/// offsets 0 and 8 -- which is the entire point. The Rust state follows at the
/// next suitably aligned offset (16 for an 8-aligned `S`), where no C caller looks
/// and where its layout is nobody's business.
///
/// # Footprint
///
/// The prefix costs 16 bytes for an 8-aligned `S`, and that cost is *not* free of
/// consequence. `test/infcover.c` L428 caps allocation at
/// `(sizeof(struct inflate_state) << 1) + 256` -- 14576 bytes, computed from the
/// **C** header -- and then requires `inflateCopy` to fail. As
/// [`zlib_rs::inflate::state`] works out, that puts a *floor* under the state
/// size as well as a ceiling: below 7033 bytes both allocations succeed and the
/// assertion fails. The core measures `InflateState` at 7312 bytes with a
/// reference allocator, so the block lands at 7328 -- comfortably inside the
/// `7033..=8234` window the core documents, the upper bound coming from the 15%
/// per-stream memory bar.
#[cfg(feature = "libz-compat")]
#[repr(C)]
pub(crate) struct StateBlock<S> {
    /// The C-visible prefix. Private so that only the accessors below can touch
    /// it, which keeps the tag's two roles from being confused with ordinary data.
    prefix: StatePrefix,
    /// The Rust state. Invisible to C and free of layout obligations.
    state: S,
}

#[cfg(feature = "libz-compat")]
impl<S> StateBlock<S> {
    /// Wraps a state with the prefix a given stream expects.
    ///
    /// `strm` becomes [`StatePrefix::strm`], which is what makes the
    /// owner-identity check in [`checked_state`] meaningful, and `tag` becomes the
    /// initial validity tag -- `INIT_STATE` for deflate, `HEAD` (16180) for
    /// inflate, mirroring `inflate.c` L205, which pre-seeds the mode precisely so
    /// that a freshly allocated state passes the tag check.
    ///
    /// `kind` records which Rust payload `S` is, as `StatePrefix`'s private `flavor`
    /// member. It must
    /// name the same kind every later [`checked_state`], [`checked_state_mut`] and
    /// [`take_state`] call passes for this block, because that is the comparison that
    /// keeps an `inflate` block and an `inflateBack` block from being mistaken for
    /// each other.
    #[must_use]
    pub(crate) const fn new(strm: z_streamp, kind: StateKind, tag: c_int, state: S) -> Self {
        Self {
            prefix: StatePrefix {
                strm,
                tag,
                flavor: kind.cookie(),
            },
            state,
        }
    }

    /// Returns the stream recorded in the prefix.
    #[must_use]
    // Not currently called: `checked_state`/`checked_state_mut` compare the prefix
    // against the incoming stream themselves, reading `prefix.strm` directly, so no
    // export has to ask for it. Kept as the read counterpart of `set_owner` and the
    // only way to observe the back-pointer without touching the private prefix.
    #[allow(dead_code)]
    pub(crate) const fn owner(&self) -> z_streamp {
        self.prefix.strm
    }

    /// Re-points the prefix at a different stream.
    ///
    /// Needed by `deflateCopy` and `inflateCopy`, which duplicate a state into a
    /// second stream and must then make the copy's back-pointer refer to *that*
    /// stream: `copy->strm = dest` at `inflate.c` L1349. Without this the copy
    /// would fail its own owner-identity check on the next call.
    // Not currently called: `deflateCopy` and `inflateCopy` build the duplicate
    // with `StateBlock::new(dest, tag, state)`, which sets the back-pointer at
    // construction, so there is no second stream to re-point afterwards. Kept for a
    // future path that copies a block rather than rebuilding one -- `copy->strm =
    // dest` at `inflate.c` L1349 is the operation it names.
    #[allow(dead_code)]
    pub(crate) fn set_owner(&mut self, strm: z_streamp) {
        self.prefix.strm = strm;
    }

    /// Returns the validity tag currently in the prefix.
    ///
    /// Read this on entry and feed it to
    /// [`zlib_rs::inflate::InflateState::set_mode_tag`] so that a mode written by
    /// caller code -- `test/infcover.c` L330 and L458 -- is honoured rather than
    /// silently discarded.
    #[must_use]
    pub(crate) const fn tag(&self) -> c_int {
        self.prefix.tag
    }

    /// Writes the validity tag into the prefix.
    ///
    /// Call this before returning, with
    /// [`zlib_rs::inflate::InflateState::mode_tag`], so that the C-visible slot
    /// always reflects the Rust state. The two are separate storage, so they stay
    /// in step only because every entry point syncs them.
    pub(crate) fn set_tag(&mut self, tag: c_int) {
        self.prefix.tag = tag;
    }

    /// Borrows the wrapped state.
    #[must_use]
    pub(crate) const fn state(&self) -> &S {
        &self.state
    }

    /// Borrows the wrapped state mutably.
    #[must_use]
    pub(crate) fn state_mut(&mut self) -> &mut S {
        &mut self.state
    }
}

#[cfg(feature = "libz-compat")]
impl<S> core::fmt::Debug for StateBlock<S> {
    /// Reports the prefix only, never the state.
    ///
    /// Written out rather than derived for two reasons: a derived implementation
    /// would require `S: Debug`, which needlessly constrains every state type, and
    /// dumping a 7 KiB decoder state into a log is not useful diagnostics.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("StateBlock")
            .field("owner", &self.prefix.strm)
            .field("tag", &self.prefix.tag)
            .field("flavor", &self.prefix.flavor)
            .finish_non_exhaustive()
    }
}

#[cfg(feature = "libz-compat")]
/// Allocates a state block through `allocator`, moves `state` into it, and
/// records its address in [`z_stream::state`].
///
/// The Rust form of the state allocation every init function performs --
/// `ZALLOC(strm, 1, sizeof(deflate_state))` at `deflate.c` L440,
/// `inflate.c` L197-L198, `infback.c` L51 -- followed by
/// `strm->state = (struct internal_state FAR *)s`.
///
/// ★ The state **must** be allocated through the caller's hooks when the caller
/// supplied any, and not from the Rust heap. `test/infcover.c` L428 limits the
/// tracked zone to `(sizeof(struct inflate_state) << 1) + 256` and then requires
/// `inflateCopy` to return `Z_MEM_ERROR`; a state the zone never sees would leave
/// the total far below the limit, the copy would succeed, and the assertion would
/// fail. Routing through [`StreamAllocator`] is what keeps the accounting honest.
///
/// The prefix is seeded from `strm` and `tag`, so the block passes its own
/// owner-identity and tag checks from the moment it exists.
///
/// # Errors
///
/// [`ReturnCode::MEM_ERROR`] if the allocator declines the request or returns a
/// block that is not aligned for the state -- the same status C reports at
/// `deflate.c` L443-L444 when `ZALLOC` yields `Z_NULL`.
///
/// ★ Takes the raw [`z_streamp`], **not** a `&mut z_stream`, and that is not a
/// stylistic choice -- see the note on [`checked_state`]. The pointer stored in the
/// prefix is exactly the one the caller passed, so the owner-identity comparison on
/// every later call is against the caller's own pointer rather than against
/// something derived from it.
///
/// # Errors
///
/// [`ReturnCode::STREAM_ERROR`] if `strm` is null or misaligned, matching the
/// `strm == Z_NULL` rejection every init function performs.
///
/// [`ReturnCode::MEM_ERROR`] if the allocator declines the request or returns a
/// block that is not aligned for the state -- the same status C reports at
/// `deflate.c` L443-L444 when `ZALLOC` yields `Z_NULL`.
///
/// # Safety
///
/// If `strm` is non-null it must address a live [`z_stream`] that nothing else is
/// concurrently writing. Any state already installed must have been taken with
/// [`take_state`] first; overwriting a live [`z_stream::state`] leaks it, and
/// `test/infcover.c`'s `mem_done` reports leaks.
pub(crate) unsafe fn install_state<S>(
    strm: z_streamp,
    allocator: &StreamAllocator,
    kind: StateKind,
    tag: c_int,
    state: S,
) -> Result<NonNull<StateBlock<S>>, ReturnCode> {
    if strm.is_null() || !strm.is_aligned() {
        return Err(ReturnCode::STREAM_ERROR);
    }

    // The prefix records the caller's pointer verbatim, which is what makes the
    // owner-identity check in `checked_state` compare like with like, and `kind`
    // verbatim, which is what makes the flavor check able to tell this payload from
    // the other two.
    let slot = allocator
        .place(StateBlock::new(strm, kind, tag, state))
        .ok_or(ReturnCode::MEM_ERROR)?;

    // SAFETY: unsafe-site category 1 -- writing one member of the caller's stream.
    // `strm` is non-null and aligned by the guard above and addresses a live
    // `z_stream` by this function's contract, so the `state` member is in bounds
    // and writable. `addr_of_mut!` forms a raw place rather than a reference, so no
    // `&mut z_stream` is materialised and the caller's own pointer keeps its
    // provenance -- which is the whole reason this function takes `z_streamp`.
    unsafe {
        core::ptr::addr_of_mut!((*strm).state).write(slot.as_ptr().cast::<internal_state>());
    }
    Ok(slot)
}

#[cfg(feature = "libz-compat")]
/// Requests a state block through `allocator` without initialising it.
///
/// [`install_state`] performs three steps at once -- allocate, move the state in,
/// record the address -- which is the right shape wherever the state value already
/// exists. `deflateInit2_` is the case where it is not: C takes the state object
/// *first* (`ZALLOC(strm, 1, sizeof(deflate_state))`, `deflate.c` L440) and the
/// four buffers the state owns *after* it (L468-L479), then releases them in the
/// exact reverse order (`deflateEnd`, L1300-L1306). A Rust state value cannot be
/// built before the buffers it owns exist, so reproducing C's sequence of calls
/// into the caller's `zalloc` means splitting the three steps: reserve here,
/// publish with [`publish_state`], build the state, then move it in with
/// [`commit_state`].
///
/// That sequence is observable. `test/infcover.c`'s tracking allocator records
/// every block in allocation order and reports a release that is not
/// last-in-first-out as a `notlifo` defect, so an implementation that asked for
/// the state object last and returned it first would be visibly wrong even though
/// it leaked nothing.
///
/// # Errors
///
/// [`ReturnCode::MEM_ERROR`] if the request is refused, if the block is misaligned
/// for the block type, or if the block type is zero-sized -- which is C's
/// `if (s == Z_NULL) return Z_MEM_ERROR;` (L443-L444).
///
/// A reservation is uninitialised storage the caller now owns. It must reach
/// exactly one of [`commit_state`] or [`discard_reserved_state`]; anything else
/// leaks the block.
pub(crate) fn reserve_state<S>(
    allocator: &StreamAllocator,
) -> Result<NonNull<StateBlock<S>>, ReturnCode> {
    allocator
        .reserve::<StateBlock<S>>()
        .ok_or(ReturnCode::MEM_ERROR)
}

#[cfg(feature = "libz-compat")]
/// Records a reserved state block's address in [`z_stream::state`].
///
/// `strm->state = (struct internal_state FAR *)s;` on its own -- `deflate.c` L445,
/// which C reaches *before* it allocates the state's buffers. Publishing at the
/// same point means a caller's `zalloc`, called for those buffers, sees the same
/// non-null `state` member it would see in C.
///
/// The block does not have to be initialised yet, and nothing in this library
/// reads through the member until [`commit_state`] has run. Every subsequent
/// recovery goes through [`checked_state`] or [`take_state`], which validate the
/// tag and the owner first, so a stream published but never committed cannot be
/// mistaken for a usable one -- but it must still be resolved: either
/// [`commit_state`] initialises it or the member is cleared again on the failure
/// path, exactly as C's `deflateEnd` clears it at L1307.
///
/// # Safety
///
/// `strm` must be non-null, aligned, and address a live [`z_stream`] with nothing
/// else concurrently accessing it. `slot` must have come from [`reserve_state`].
pub(crate) unsafe fn publish_state<S>(strm: z_streamp, slot: NonNull<StateBlock<S>>) {
    // SAFETY: unsafe-site category 1 -- writing one member of the caller's stream.
    // `strm` is non-null, aligned and live by this function's contract, so the
    // `state` member is in bounds and writable. `addr_of_mut!` forms a raw place
    // rather than a reference, so no `&mut z_stream` is materialised and the
    // caller's own pointer keeps its provenance. The member may hold indeterminate
    // bytes beforehand, which is sound because it is written, not read.
    unsafe {
        core::ptr::addr_of_mut!((*strm).state).write(slot.as_ptr().cast::<internal_state>());
    }
}

#[cfg(feature = "libz-compat")]
/// Moves a finished state into a block [`reserve_state`] produced.
///
/// The last of the three steps [`install_state`] performs together, and the point
/// at which the block becomes a valid state: the prefix records `strm` as the
/// owner and `tag` as the initial validity tag, so the block passes its own
/// owner-identity and tag checks from this moment on. `s->strm = strm` and
/// `s->status = INIT_STATE` at `deflate.c` L446-L447 are the same two
/// assignments, and C's comment there says why the status is set so early: "to
/// pass state test in `deflateReset()`".
///
/// # Safety
///
/// * `slot` must have come from [`reserve_state`] on an allocator carrying the
///   same `(zalloc, zfree, opaque)` triple as the one that will eventually release
///   it, and with the same `S`.
/// * The block must not have been committed already: the write does not drop what
///   it overwrites.
pub(crate) unsafe fn commit_state<S>(
    slot: NonNull<StateBlock<S>>,
    allocator: &StreamAllocator,
    strm: z_streamp,
    kind: StateKind,
    tag: c_int,
    state: S,
) {
    // SAFETY: unsafe-site category 3 -- initialising the state block. `occupy`'s
    // contract is this function's: `slot` came from `reserve` -- through
    // `reserve_state` -- on this allocator's triple, and is still uninitialised.
    // `kind` is stamped into the prefix exactly as `install_state` stamps it, so a
    // block placed in two phases is indistinguishable from one placed in one and the
    // flavor check in `checked_state` keeps working.
    unsafe { allocator.occupy(slot, StateBlock::new(strm, kind, tag, state)) }
}

#[cfg(feature = "libz-compat")]
/// Returns a reserved but uncommitted state block to `allocator`.
///
/// The `ZFREE(strm, s)` C performs when a later step fails -- `deflate.c` L509-L513
/// reaches it through `deflateEnd`, `inflate.c` L212 spells it out -- for the case
/// where the state value was never built, so there is nothing to drop and only the
/// storage to give back.
///
/// # Safety
///
/// * `slot` must have come from [`reserve_state`] on an allocator carrying this
///   same `(zalloc, zfree, opaque)` triple, and must not have been committed or
///   released.
/// * Nothing may reference the block afterwards, and any [`z_stream::state`]
///   member that [`publish_state`] pointed at it must already have been cleared.
pub(crate) unsafe fn discard_reserved_state<S>(
    allocator: &StreamAllocator,
    slot: NonNull<StateBlock<S>>,
) {
    // SAFETY: unsafe-site category 4 -- the caller's `zfree`. `release`'s contract
    // is this function's: the block came from `reserve` on this triple, has not
    // been released, is unreferenced, and holds no value that owes a destructor
    // because it was never committed.
    unsafe { allocator.release(slot) }
}

#[cfg(feature = "libz-compat")]
/// Recovers a validated borrow of the state behind [`z_stream::state`].
///
/// The read half of the round-trip, and the Rust form of the surviving parts of
/// `deflateStateCheck` (`deflate.c` L538) and `inflateStateCheck`
/// (`inflate.c` L88). Five things are established before any state field is
/// touched, in this order:
///
/// 1. `strm` is non-null and aligned -- the `strm == Z_NULL` test.
/// 2. [`z_stream::state`] is non-null and aligned for the block -- the
///    `state == Z_NULL` test.
/// 3. [`StatePrefix::strm`] equals `strm` -- the `state->strm != strm` test at
///    `inflate.c` L94, which rejects a state belonging to another stream.
/// 4. [`StatePrefix::tag`] is in the live range for `kind` -- the
///    `state->mode < HEAD || state->mode > SYNC` test at `inflate.c` L95.
/// 5. `StatePrefix`'s private `flavor` member is exactly `kind`'s cookie. This one has **no C
///    counterpart**, because C does not need one: its `inflate` and `inflateBack`
///    payloads are the same `struct inflate_state`, while this port's are different
///    Rust types behind an identical prefix and an identical tag range. It is what
///    turns "an ordinary inflate call on an `inflateBack` stream" into
///    `Z_STREAM_ERROR` instead of a wrong-layout access.
///
/// [`None`] means the entry point must return `Z_STREAM_ERROR`.
///
/// Steps 3 to 5 read the prefix through the raw pointer with
/// [`core::ptr::read`] rather than forming a reference first. That ordering is
/// deliberate: forming `&StateBlock<S>` over memory that has not yet been shown
/// to hold one would already be undefined behaviour, so the cheapest, most
/// plain-data view is used for the tests and the reference is created only after
/// they pass.
///
/// # ★ Why this takes `z_streamp` and never `&mut z_stream`
///
/// Every function in the state round-trip -- [`install_state`], this one,
/// [`checked_state_mut`] and [`take_state`] -- takes the caller's raw
/// [`z_streamp`] and reads or writes [`z_stream::state`] through a raw place
/// (`addr_of!` / `addr_of_mut!`). None of them ever materialises a
/// `&mut z_stream`, and that is a soundness requirement rather than a preference.
///
/// Creating a `&mut z_stream` from the caller's pointer **invalidates that
/// pointer**: the mutable reborrow takes exclusive control of the stream, so every
/// pointer derived earlier -- including the very one C handed in and the one this
/// module records in [`StatePrefix::strm`] -- may no longer be used. Taking
/// `&mut z_stream` in [`install_state`] produces exactly that failure: a later
/// `checked_state` read of `z_stream::state` is rejected by Miri with "that tag
/// does not exist in the borrow stack", naming the `&mut` as the invalidating
/// retag. The addresses still compare equal, so the owner test still *passes* --
/// which is why the defect is invisible to every functional test and visible only
/// to an aliasing model, and why the rule above is stated as a requirement rather
/// than left to judgement.
///
/// Reading and writing a single member through a raw place has neither problem, and
/// it is also a closer translation of the C it replaces, where every access is
/// spelled `strm->state`. Entry points that additionally need the input or output
/// buffers reach them the same way -- read `next_in`/`avail_in` through a raw place
/// and hand the pair to [`input_slice`] -- which is why this module offers no
/// borrowing constructor for a whole [`z_stream`] and why adding one would
/// reintroduce exactly the defect above.
///
/// # The remaining check, and where it lives
///
/// C also rejects a stream whose `zalloc` or `zfree` is null. That test is
/// [`StreamAllocator::from_stream_ptr`]'s, which returns [`None`] for exactly that
/// case, and it is the same test C performs -- not an approximation of it. The
/// three init entry points substitute a null hook and write the substitution back
/// into the caller's structure ([`StreamAllocator::adopt_hooks`], mirroring
/// `deflate.c` L401-L414), so an initialised stream never has a null hook; a stream
/// that arrives here with one was never initialised, or had its members cleared
/// afterwards, which is what `test/infcover.c`'s `mem_done` does (L231-L233).
///
/// # Safety
///
/// If `strm` is non-null it must address a live [`z_stream`], and if that
/// stream's [`z_stream::state`] is non-null it must address a
/// `StateBlock<S>` produced by [`install_state`] with the same `S`. The tag and
/// owner tests make a *foreign or uninitialised* block overwhelmingly likely to
/// be rejected, but they cannot detect a freed one -- freed memory may still hold
/// its old tag, and reading it is already undefined behaviour. That is exactly
/// the limitation C has, and why C pairs the tag test with the owner test too.
/// No other borrow of the block may exist for `'a`.
#[must_use]
pub(crate) unsafe fn checked_state<'a, S>(
    strm: z_streamp,
    kind: StateKind,
) -> Option<&'a StateBlock<S>> {
    // SAFETY: unsafe-site category 3 -- the opaque `state` round-trip.
    // `validated_state_ptr` performs steps 1 to 4 and returns a pointer
    // only when all four passed; this function's own contract supplies the
    // liveness and provenance requirements it cannot test. Forming a shared borrow
    // from that pointer is therefore sound, and no other borrow exists for `'a` by
    // contract.
    let slot = unsafe { validated_state_ptr::<S>(strm, kind) }?;
    // SAFETY: unsafe-site category 3 -- the opaque `state` round-trip. `slot` is
    // non-null and aligned for `StateBlock<S>`, because `validated_state_ptr`
    // returned `Some` only after testing both, and it addresses a fully initialised
    // `StateBlock<S>` written by `install_state` for this same `S`, which the tag
    // and owner tests just confirmed. This function's contract supplies the two
    // facts no test can establish: that the block is still live rather than freed,
    // and that no other borrow of it exists for `'a`. A shared reference is
    // therefore sound and cannot alias a `&mut`.
    Some(unsafe { slot.as_ref() })
}

#[cfg(feature = "libz-compat")]
/// Recovers a validated mutable borrow of the state behind [`z_stream::state`].
///
/// The [`checked_state`] counterpart for the entry points that advance the state,
/// which is nearly all of them. The same four checks run in the same order and the
/// same reasoning applies.
///
/// # Safety
///
/// As [`checked_state`], and additionally: no other borrow of the block --
/// shared or mutable -- may exist for `'a`. Recover the borrow once on entry.
#[must_use]
pub(crate) unsafe fn checked_state_mut<'a, S>(
    strm: z_streamp,
    kind: StateKind,
) -> Option<&'a mut StateBlock<S>> {
    // SAFETY: unsafe-site category 3 -- the opaque `state` round-trip. `strm` is
    // null-and-alignment tested inside `validated_state_ptr` before any
    // dereference, and this function's contract guarantees a non-null `strm`
    // addresses a live `z_stream` whose `state`, if non-null, is a `StateBlock<S>`
    // from `install_state`. The helper forms no reference to the block, so calling
    // it cannot invalidate the caller's own pointer.
    let mut slot = unsafe { validated_state_ptr::<S>(strm, kind) }?;
    // SAFETY: unsafe-site category 3 -- `slot` is non-null, aligned for
    // `StateBlock<S>` and addresses an initialised one, all established by the
    // helper returning `Some`. This function's contract additionally guarantees the
    // block is live and that **no** other borrow of it -- shared or mutable --
    // exists for `'a`, which is what makes a unique reference sound here.
    Some(unsafe { slot.as_mut() })
}

#[cfg(feature = "libz-compat")]
/// Takes the state back out of a stream and returns its storage to `allocator`.
///
/// The teardown half of the round-trip: `ZFREE(strm, strm->state)` followed by
/// `strm->state = Z_NULL` (`deflate.c` L1305-L1306, `inflate.c` L1305-L1306).
/// The same four validity checks [`checked_state`] performs run first, so a
/// foreign or uninitialised stream is refused rather than freed -- which is why
/// `deflateEnd` and `inflateEnd` are able to return `Z_STREAM_ERROR` at all.
///
/// [`z_stream::state`] is cleared *whether or not* the caller goes on to use the
/// returned state, so the pointer can never be used again through this stream.
///
/// # Safety
///
/// As [`checked_state`]: `strm` must be a live, uniquely borrowed [`z_stream`],
/// and its state must be a `StateBlock<S>` produced by [`install_state`] with the
/// same `S`. `allocator` must have the same `(zalloc, zfree, opaque)` triple that
/// the matching [`install_state`] used -- ordinarily guaranteed because both are
/// built by [`StreamAllocator::from_stream_ptr`] from the same stream, whose hooks
/// `zlib.h` L140-L142 forbids the application from changing once initialised.
pub(crate) unsafe fn take_state<S>(
    strm: z_streamp,
    allocator: &StreamAllocator,
    kind: StateKind,
) -> Option<StateBlock<S>> {
    // SAFETY: unsafe-site category 3 -- the opaque `state` round-trip. The four
    // validity checks run inside, and this function's contract supplies the
    // liveness and provenance requirements they cannot test.
    let slot = unsafe { validated_state_ptr::<S>(strm, kind) }?;

    // SAFETY: unsafe-site category 1 -- clearing one member of the caller's stream.
    // `validated_state_ptr` returning `Some` established that `strm` is non-null and
    // aligned, and this function's contract makes it a live `z_stream`. Written
    // through a raw place, so no `&mut z_stream` is materialised and the caller's
    // pointer keeps its provenance. Cleared *before* the block is released, so that
    // no path can leave the stream holding a dangling pointer -- not even if the
    // release itself is what fails.
    unsafe {
        core::ptr::addr_of_mut!((*strm).state).write(core::ptr::null_mut());
    }

    // SAFETY: unsafe-site category 3 -- releasing the state object. `slot` was
    // produced by `install_state`, hence by
    // `StreamAllocator::place` with the same `S`, and `allocator` carries the same
    // triple by this function's contract. The pointer has just been removed from
    // the stream and no borrow of it survives, so displacing it -- reading the
    // value out and releasing the storage -- is sound and cannot be repeated.
    Some(unsafe { allocator.displace(slot) })
}

#[cfg(feature = "libz-compat")]
/// Recovers the state, hands it to `body`, and releases the block's own storage
/// only after `body` returns.
///
/// [`take_state`] with the two release steps in C's order. It has to be a separate
/// function rather than a flag, because the ordering follows from *where the value
/// is dropped*: a function that returns the state cannot free the block first
/// without freeing it before the value's own buffers are released, and cannot free
/// it last without keeping the storage alive past its own return.
///
/// `deflateEnd` releases `pending_buf`, `head`, `prev` and `window` and *then* the
/// state object (`deflate.c` L1300-L1306); `inflateEnd` releases the window and
/// then the state (`inflate.c` L1281-L1287). In both the state block -- the first
/// allocation made -- is the last one returned, so a caller's `zalloc`/`zfree` pair
/// sees a strictly last-in-first-out sequence. `test/infcover.c`'s tracking
/// allocator records that order and reports any departure from it as a `notlifo`
/// defect, which is what makes this observable rather than cosmetic.
///
/// `body` receives the state **by mutable reference** and is expected to release its
/// buffers explicitly -- typically `|block| core_deflate_end(&mut block.state_mut()
/// .state)`, whose own return value is the status C reads at L1298, before the
/// teardown. The value is then dropped here, in this frame, with every buffer slot
/// already empty.
///
/// ★ The reference is not a convenience. A state moved into a call -- `body(block)`
/// included -- carries its buffers' borrows in argument position, where they are
/// protected for the whole call, and freeing memory a protected reference covers is
/// undefined behaviour. Passing `&mut` retags only the state block, which is a
/// different allocation from the buffers, so the frees inside `body` are sound.
/// `zlib_rs::allocate::ForeignBlock` records the rule and the Miri diagnosis behind it.
///
/// Returns [`None`], having touched nothing, if the stream carries no state this
/// library installed -- the same four validity checks [`take_state`] applies.
///
/// # Safety
///
/// As [`take_state`], plus one obligation on `body`: whatever it returns must not
/// own storage obtained from `allocator`, because that storage would then be
/// released after the state block rather than before it. A status code, which is
/// what both `End` entry points return, satisfies that trivially.
pub(crate) unsafe fn take_state_with<S, R>(
    strm: z_streamp,
    allocator: &StreamAllocator,
    kind: StateKind,
    body: impl FnOnce(&mut StateBlock<S>) -> R,
) -> Option<R> {
    // SAFETY: unsafe-site category 3 -- the opaque `state` round-trip. The four
    // validity checks run inside, and this function's contract supplies the
    // liveness and provenance requirements they cannot test.
    let slot = unsafe { validated_state_ptr::<S>(strm, kind) }?;

    // SAFETY: unsafe-site category 1 -- clearing one member of the caller's stream.
    // `validated_state_ptr` returning `Some` established that `strm` is non-null
    // and aligned, and this function's contract makes it a live `z_stream`. Written
    // through a raw place, so no `&mut z_stream` is materialised and the caller's
    // pointer keeps its provenance. Cleared before anything is released, so no path
    // can leave the stream holding a dangling pointer.
    unsafe {
        core::ptr::addr_of_mut!((*strm).state).write(core::ptr::null_mut());
    }

    // SAFETY: unsafe-site category 3 -- recovering the state object. `slot`
    // addresses an initialised, aligned `StateBlock<S>` that this library installed
    // and that the clear above has just detached from the stream, so nothing else
    // references it. The read moves the value out; the storage is released below
    // without running a destructor on it, which is correct because ownership has
    // transferred to `block`.
    let mut block = unsafe { slot.as_ptr().read() };

    // The state's buffers go back to the allocator here, inside `body`, which releases
    // them through the borrow. `block` itself is dropped at the end of this frame --
    // an in-place drop of a local, with every slot already empty.
    let result = body(&mut block);

    // SAFETY: unsafe-site category 4 -- the caller's `zfree`. `release`'s contract
    // is satisfied by this function's: the block came from `install_state` on this
    // triple, has not been released, is unreferenced now that the read above moved
    // the value out, and owes no destructor for the same reason.
    unsafe { allocator.release(slot) };

    Some(result)
}

/// Runs the four validity checks and yields the state pointer, or [`None`].
///
/// The single implementation behind [`checked_state`], [`checked_state_mut`] and
/// [`take_state`], so that the check order exists in exactly one place and a
/// future change cannot apply to two of the three.
///
/// # Safety
///
/// As [`checked_state`]. This function forms no reference to the block; it reads
/// only the plain-data [`StatePrefix`] at offset 0, which `#[repr(C)]`
/// guarantees is where it lies.
#[cfg(feature = "libz-compat")]
unsafe fn validated_state_ptr<S>(
    strm: z_streamp,
    kind: StateKind,
) -> Option<NonNull<StateBlock<S>>> {
    // Check 1: the stream. Precedes every dereference, because a null stream is a
    // documented input rather than an error condition.
    if strm.is_null() || !strm.is_aligned() {
        return None;
    }

    // SAFETY: unsafe-site category 1 -- the pointer is non-null and aligned by the
    // test above, and addresses a live `z_stream` by this function's contract. Only
    // the `state` member is read, which is plain pointer data. `addr_of!` plus
    // `read` forms a raw place rather than a `&z_stream`, so nothing here disturbs
    // the borrow stack of the caller's own pointer.
    let raw = unsafe { core::ptr::addr_of!((*strm).state).read() }.cast::<StateBlock<S>>();

    // Check 2: the state pointer. `NonNull::new` handles the null half; the
    // alignment test is what makes reading the prefix below well defined.
    let slot = NonNull::new(raw)?;
    if !slot.as_ptr().is_aligned() {
        return None;
    }

    // SAFETY: unsafe-site category 3 -- the opaque state round-trip. `slot` is
    // non-null and aligned for `StateBlock<S>`, and `StatePrefix` is the first
    // member of a `#[repr(C)]` struct, so it lies at offset 0 and the block's
    // alignment is a multiple of the prefix's. By this function's contract the
    // block came from `install_state`, so those bytes hold an initialised
    // `StatePrefix`. It is `Copy` plain data, so it is *read* rather than
    // borrowed: no reference to the block exists yet, which matters because the
    // block has not been shown to be valid until the two tests below pass.
    let prefix: StatePrefix = unsafe { core::ptr::read(slot.as_ptr().cast::<StatePrefix>()) };

    // Check 3: owner identity -- `state->strm != strm`, `inflate.c` L94. Rejects a
    // state that belongs to a different stream, which no tag test could catch.
    if !core::ptr::eq(prefix.strm, strm) {
        return None;
    }

    // Check 4: the validity tag -- `inflate.c` L95 and `deflate.c` L546-L555.
    if kind.rejects(prefix.tag) {
        return None;
    }

    // Check 5: the exact payload flavor. ★ Not a C check, and it has no C
    // counterpart to be faithful to: C's two decompression payloads are both
    // `struct inflate_state`, whereas this port's `inflate` and `inflateBack` blocks
    // are different Rust types behind an identical prefix and an identical tag
    // range. Without this comparison, reinterpreting one as the other would read --
    // and later free -- at the wrong layout. See `StatePrefix::flavor`.
    if prefix.flavor != kind.cookie() {
        return None;
    }

    Some(slot)
}

#[cfg(test)]
// Gated with the machinery it exercises. Every case below reaches for the allocator
// adapter, the overlap predicates or the tagged state block, none of which is compiled
// without `libz-compat`, so with the feature off there is nothing here to test rather
// than something to skip. What the ABI mirrors themselves promise is asserted at
// compile time in `layout_assertions.rs`, which is unconditional, so the layouts stay
// verified in every configuration.
#[cfg(feature = "libz-compat")]
// The workspace denies the panic family and slice indexing everywhere, which is the
// property this module exists to uphold at the boundary; a test that cannot assert is
// useless, so the harness opts back in here only. `clippy.toml` permits exactly this
// through its `allow-*-in-tests` keys.
#[allow(clippy::panic, clippy::unwrap_used)]
mod tests {
    use super::{
        ranges_are_disjoint, streams_are_disjoint, structs_are_disjoint, z_stream, z_streamp,
        StateKind, StatePrefix,
    };
    use core::ffi::c_int;

    /// A range shares no byte with itself when either side is empty.
    ///
    /// Both helpers that build slices return an empty slice for a zero length without
    /// calling `from_raw_parts` at all, so a zero-length pair borrows nothing and can
    /// never alias -- which is why the overlap gate must not refuse it.
    #[test]
    fn a_zero_length_range_is_disjoint_from_everything() {
        let buffer = [0_u8; 16];
        let base = buffer.as_ptr();
        assert!(ranges_are_disjoint(base, 0, base, 16));
        assert!(ranges_are_disjoint(base, 16, base, 0));
        assert!(ranges_are_disjoint(base, 0, base, 0));
        assert!(ranges_are_disjoint(core::ptr::null(), 0, base, 8));
    }

    /// Ranges that touch but do not share a byte are disjoint.
    #[test]
    fn adjacent_ranges_are_disjoint_and_touching_ones_are_not() {
        let buffer = [0_u8; 16];
        let base = buffer.as_ptr();
        // SAFETY: both offsets are inside a 16-byte array, so the arithmetic stays in
        // bounds of one allocation.
        let middle = unsafe { base.add(8) };

        // [0,8) and [8,16) share nothing.
        assert!(ranges_are_disjoint(base, 8, middle, 8));
        assert!(ranges_are_disjoint(middle, 8, base, 8));

        // [0,9) and [8,16) share byte 8.
        assert!(!ranges_are_disjoint(base, 9, middle, 8));
        assert!(!ranges_are_disjoint(middle, 8, base, 9));

        // A range always overlaps itself.
        assert!(!ranges_are_disjoint(base, 1, base, 1));
        assert!(!ranges_are_disjoint(base, 16, middle, 1));
    }

    /// A length that would run past the top of the address space is refused, not wrapped.
    ///
    /// `avail_in` and `avail_out` are caller-supplied, so a range whose end cannot be
    /// represented is an input this gate can receive. Saturating means the end never
    /// compares as "before the other start", so the pair is reported as overlapping and
    /// the entry point refuses it -- fail-closed rather than accidentally accepted.
    #[test]
    fn a_range_that_would_wrap_the_address_space_is_reported_as_overlapping() {
        let high = usize::MAX as *const u8;
        let low = 1_usize as *const u8;
        assert!(!ranges_are_disjoint(high, usize::MAX, low, 1));
    }

    /// Two structures overlap unless they are a whole `T` apart.
    ///
    /// Equality is not a sufficient test: `deflateCopy` and `inflateCopy` copy
    /// `sizeof(z_stream)` bytes, so two distinct but *partially* overlapping streams
    /// would still make the copy undefined.
    #[test]
    fn struct_disjointness_covers_partial_overlap_not_just_equality() {
        // Backed by real `MaybeUninit<z_stream>` storage rather than a byte array, so the
        // addresses below carry `z_stream`'s own alignment and no pointer cast widens an
        // alignment. Nothing is ever read out of it.
        let pair = [const { core::mem::MaybeUninit::<z_stream>::uninit() }; 4];
        let base: *const z_stream = pair.as_ptr().cast();
        // SAFETY: the array holds four `z_stream`s, so both derived addresses stay inside
        // it; `byte_add(8)` keeps the 8-byte alignment because `z_stream`'s alignment is
        // 8. Neither pointer is dereferenced -- only its address is read.
        let (near, far) = unsafe { (base.byte_add(8), base.add(1)) };

        assert!(!structs_are_disjoint(base, base));
        assert!(!structs_are_disjoint(base, near));
        assert!(structs_are_disjoint(base, far));
        assert!(structs_are_disjoint(far, base));

        // `streams_are_disjoint` is the same predicate, spelled for the one type both
        // copy entry points need.
        assert!(!streams_are_disjoint(
            base.cast_mut(),
            near.cast_mut().cast()
        ));
        assert!(streams_are_disjoint(base.cast_mut(), far.cast_mut()));
    }

    /// The private prefix keeps the layout `test/infcover.c` writes through.
    ///
    /// The harness casts `strm.state` to `struct inflate_state *` and assigns `->mode`
    /// (its L330 and L459), so `strm` must stay at offset 0 and the tag immediately
    /// after it -- offset 8 on a 64-bit target, offset 4 on a 32-bit one, exactly where
    /// the C compiler puts `inflate_mode mode`. Asserted relationally for that reason,
    /// with the measured 64-bit numbers pinned separately.
    #[test]
    fn the_prefix_layout_matches_the_c_visible_head() {
        assert_eq!(core::mem::offset_of!(StatePrefix, strm), 0);
        assert_eq!(
            core::mem::offset_of!(StatePrefix, tag),
            size_of::<z_streamp>()
        );
        assert_eq!(
            core::mem::offset_of!(StatePrefix, flavor),
            size_of::<z_streamp>() + size_of::<c_int>()
        );
        assert_eq!(
            size_of::<StatePrefix>(),
            size_of::<z_streamp>() + 2 * size_of::<c_int>()
        );
        assert_eq!(size_of::<c_int>(), 4);

        #[cfg(target_pointer_width = "64")]
        {
            assert_eq!(size_of::<StatePrefix>(), 16);
            assert_eq!(core::mem::offset_of!(StatePrefix, tag), 8);
            assert_eq!(core::mem::offset_of!(StatePrefix, flavor), 12);
        }
    }

    /// The three payload kinds carry three different cookies.
    ///
    /// This is the whole mechanism that stops an `inflateBack` block being reinterpreted
    /// as an `inflate` block: the two share a tag range, so only the cookie separates
    /// them.
    #[test]
    fn every_state_kind_has_its_own_cookie() {
        let deflate = StateKind::Deflate.cookie();
        let inflate = StateKind::Inflate.cookie();
        let back = StateKind::InflateBack.cookie();
        assert_ne!(deflate, inflate);
        assert_ne!(deflate, back);
        assert_ne!(inflate, back);
        // A recycled or uninitialised word is very unlikely to match: none of the three
        // is a small integer.
        for cookie in [deflate, inflate, back] {
            assert!(
                cookie > 0x0100_0000,
                "{cookie:#010x} is too small to screen"
            );
        }
    }

    /// `inflateBack` shares `inflate`'s tag predicate, and deflate has its own.
    ///
    /// The polarity is C's: a `true` return means *reject*, matching a non-zero return
    /// from `deflateStateCheck`/`inflateStateCheck`.
    #[test]
    fn the_tag_predicate_follows_the_kind_and_inflate_back_shares_inflates() {
        // `HEAD` is 16180 and `SYNC` is 16211 (`inflate.h` L20-L52).
        for tag in [16180_i32, 16190, 16211] {
            assert!(!StateKind::Inflate.rejects(tag));
            assert!(!StateKind::InflateBack.rejects(tag));
        }
        for tag in [0_i32, 16179, 16212] {
            assert!(StateKind::Inflate.rejects(tag));
            assert!(StateKind::InflateBack.rejects(tag));
        }
        // `INIT_STATE` is 42 (`deflate.h` L58) and is not an inflate mode.
        assert!(!StateKind::Deflate.rejects(42));
        assert!(StateKind::Deflate.rejects(16180));
    }

    use super::{
        alloc_func, free_func, uInt, voidpf, zlib_rs_zalloc, zlib_rs_zfree, StreamAllocator,
    };
    use core::ffi::c_void;
    use zlib_rs::allocate::Allocator;

    /// A caller hook that is never called: only its address matters here.
    unsafe extern "C" fn stub_alloc(_opaque: voidpf, _items: uInt, _size: uInt) -> voidpf {
        core::ptr::null_mut()
    }

    /// The `zfree` counterpart of [`stub_alloc`].
    unsafe extern "C" fn stub_free(_opaque: voidpf, _address: voidpf) {}

    fn opaque_marker() -> voidpf {
        // A non-null value that is never dereferenced -- `zlib.h` L146-L147 says the
        // library attaches no meaning to it, and this test holds it to that.
        //
        // Written as a cast rather than with `core::ptr::without_provenance_mut`, which is
        // stable only from Rust 1.84 and would break the declared 1.80 floor: `make
        // rust-msrv` reports E0658 for it. The cast yields the same address with no
        // provenance, which is all a value that is only ever compared needs.
        0x5a5a_5a5a_usize as *mut c_void
    }

    /// `deflate.c` L401-L414: each hook is defaulted on its own, and the `opaque`
    /// clear belongs to the `zalloc` arm.
    #[test]
    fn each_hook_defaults_independently_and_opaque_follows_zalloc() {
        let marker = opaque_marker();
        let stub_a: alloc_func = Some(stub_alloc);
        let stub_f: free_func = Some(stub_free);

        // Both null: both substituted, `opaque` cleared.
        let both = StreamAllocator::from_hooks(None, None, marker);
        let (zalloc, zfree, opaque) = both.published_hooks();
        assert!(zalloc.is_some() && zfree.is_some());
        assert!(opaque.is_null(), "deflate.c L406 clears opaque");
        assert!(both.is_internal());

        // Both supplied: nothing substituted, `opaque` kept.
        let caller = StreamAllocator::from_hooks(stub_a, stub_f, marker);
        let (_, _, opaque) = caller.published_hooks();
        assert_eq!(opaque, marker);
        assert!(!caller.is_internal());

        // `zalloc` only: `zfree` substituted, `opaque` kept, because C's clear lives
        // in the other arm.
        let alloc_only = StreamAllocator::from_hooks(stub_a, None, marker);
        let (zalloc, zfree, opaque) = alloc_only.published_hooks();
        assert_eq!(zalloc.map(|f| f as usize), stub_a.map(|f| f as usize));
        assert_eq!(
            zfree.map(|f| f as usize),
            Some(zlib_rs_zfree as unsafe extern "C" fn(voidpf, voidpf) as usize)
        );
        assert_eq!(opaque, marker, "deflate.c L410-L414 does not touch opaque");
        assert!(!alloc_only.is_internal());

        // `zfree` only: `zalloc` substituted, and the clear comes with it.
        let free_only = StreamAllocator::from_hooks(None, stub_f, marker);
        let (zalloc, zfree, opaque) = free_only.published_hooks();
        assert_eq!(
            zalloc.map(|f| f as usize),
            Some(zlib_rs_zalloc as unsafe extern "C" fn(voidpf, uInt, uInt) -> voidpf as usize)
        );
        assert_eq!(zfree.map(|f| f as usize), stub_f.map(|f| f as usize));
        assert!(opaque.is_null());
        assert!(free_only.is_internal());
    }

    /// The two state checks reject a stream with a null hook -- `deflate.c` L536-L539
    /// and `inflate.c` L90-L92 -- which is the state `test/infcover.c`'s `mem_done`
    /// leaves behind (L231-L233). Nothing is substituted on that path.
    #[test]
    fn a_null_hook_is_refused_outside_the_init_entry_points() {
        let stub_a: alloc_func = Some(stub_alloc);
        let stub_f: free_func = Some(stub_free);
        let marker = opaque_marker();

        assert!(StreamAllocator::from_supplied_hooks(None, None, marker).is_none());
        assert!(StreamAllocator::from_supplied_hooks(stub_a, None, marker).is_none());
        assert!(StreamAllocator::from_supplied_hooks(None, stub_f, marker).is_none());
        assert!(StreamAllocator::from_supplied_hooks(stub_a, stub_f, marker).is_some());
    }

    /// An allocator built by `internal()` and one rebuilt from the hooks that same
    /// substitution publishes are the SAME allocator. This is what lets `deflateEnd`
    /// release blocks `deflateInit2_` obtained after the caller's stream has been
    /// rewritten, and it is the property the old internal-mode design did not have.
    #[test]
    fn the_published_hooks_rebuild_an_identical_allocator() {
        let internal = StreamAllocator::internal();
        let (zalloc, zfree, opaque) = internal.published_hooks();
        let rebuilt = StreamAllocator::from_supplied_hooks(zalloc, zfree, opaque)
            .expect("both published hooks are non-null");
        assert_eq!(internal.id(), rebuilt.id());
        assert_eq!(internal.opaque_ptr(), rebuilt.opaque_ptr());
    }

    /// The published routines are real, callable C functions whose blocks release
    /// through each other -- the property that makes publishing them safe at all.
    /// `zcalloc`/`zcfree` are `malloc`/`free` (`zutil.c` L299-L308) and so are these.
    #[test]
    fn the_published_routines_allocate_and_release_through_each_other() {
        let (zalloc, zfree, opaque) = StreamAllocator::internal().published_hooks();
        let (zalloc, zfree) = (zalloc.unwrap(), zfree.unwrap());

        // SAFETY: the two published hooks with the published `opaque`, called exactly
        // as the library calls them. The request is 64 bytes, the block is written
        // through its whole length before being released, and it is released once.
        unsafe {
            let block = zalloc(opaque, 16, 4);
            assert!(!block.is_null(), "64 bytes is always available");
            core::ptr::write_bytes(block.cast::<u8>(), 0x5a, 64);
            assert_eq!(block.cast::<u8>().read(), 0x5a);
            zfree(opaque, block);

            // `zalloc`'s documented failure answer is `Z_NULL` (`zlib.h` L149): on a
            // 32-bit target the product is unrepresentable and `block_len` refuses it
            // without calling `malloc`, and on a 64-bit target the product is
            // representable and `malloc` refuses it. Either way the answer is null.
            //
            // ★ `black_box` is load-bearing, not decoration. Measured on rustc 1.97.1
            // at `opt-level = 3` -- the release profile `make rust-test` builds with:
            // when the returned pointer is *only* null-tested and never otherwise
            // used, LLVM removes the `malloc` call as a dead allocation and folds the
            // comparison to `false`, so the assertion fires against a request that was
            // never made. Observing the pointer keeps the call, and the answer is
            // `Z_NULL` in both profiles. The library itself is unaffected: every block
            // it obtains is written and released, so none is ever dead.
            let huge = core::hint::black_box(zalloc(opaque, uInt::MAX, uInt::MAX));
            assert!(
                huge.is_null(),
                "a product that cannot be honoured must answer Z_NULL"
            );
            // `zcfree` forwards to `free`, which accepts a null pointer.
            zfree(opaque, core::ptr::null_mut());
        }
    }
}

// ---------------------------------------------------------------------------
// `inflate_table` alphabet mapping -- tests
// ---------------------------------------------------------------------------

#[cfg(feature = "libz-compat")]
#[cfg(test)]
mod tests_config {
    use core::ffi::c_int;

    use zlib_rs::inflate::inftrees::CodeType;

    use super::{code_type_from_raw, CODES, DISTS, LENS};

    /// The three `inftrees.h` L54-L58 enumerators map to the core's `CodeType`, in
    /// declaration order, and nothing else maps at all.
    ///
    /// This is the guard that makes taking a bare `c_int` at the boundary safe, so
    /// the rejection half matters as much as the acceptance half: `inflate_table`
    /// indexes its own tables by the alphabet it is handed, and `test/infcover.c`
    /// L570-L573 calls the symbol directly, which is exactly the path a bad value
    /// would arrive on.
    #[test]
    fn code_type_from_raw_accepts_the_three_alphabets_and_nothing_else() {
        assert_eq!(code_type_from_raw(CODES), Some(CodeType::Codes));
        assert_eq!(code_type_from_raw(LENS), Some(CodeType::Lens));
        assert_eq!(code_type_from_raw(DISTS), Some(CodeType::Dists));

        assert_eq!(code_type_from_raw(3), None);
        assert_eq!(code_type_from_raw(-1), None);
        assert_eq!(code_type_from_raw(c_int::MAX), None);
        assert_eq!(code_type_from_raw(c_int::MIN), None);
    }

    /// The three constants carry the values `inftrees.h` gives them, which is what
    /// lets a caller pass `CODES`/`LENS`/`DISTS` straight through to the C symbol.
    #[test]
    fn the_alphabet_constants_match_inftrees_h() {
        assert_eq!(CODES, 0);
        assert_eq!(LENS, 1);
        assert_eq!(DISTS, 2);
    }
}
