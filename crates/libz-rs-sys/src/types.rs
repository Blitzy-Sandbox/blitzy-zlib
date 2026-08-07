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
//! # The measured contract
//!
//! Produced with `gcc -I. -D_LARGEFILE64_SOURCE=1` over the real headers on
//! `x86_64-unknown-linux-gnu`, reading `sizeof` and `offsetof` directly. The
//! permanent compile-time assertions over these numbers live in
//! `layout_assertions.rs`; this module carries only the two *cross-crate
//! agreement* assertions that no other module is in a position to make.
//!
//! | Type | Size | Align | Field offsets |
//! |---|---|---|---|
//! | [`z_stream`] | 112 | 8 | 0, 8, 16, 24, 32, 40, 48, 56, 64, 72, 80, 88, 96, 104 |
//! | [`gz_header`] | 80 | 8 | 0, 8, 16, 20, 24, 32, 36, 40, 48, 56, 64, 68, 72 |
//! | [`gzFile_s`] | 24 | 8 | 0, 8, 16 |
//! | [`code`] | 4 | 2 | 0, 1, 2 |
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
//! The identical reasoning applies to [`z_size_t`], [`z_off_t`] and
//! [`z_off64_t`]. `z_size_t` resolves to `size_t`, `unsigned long long` or
//! `unsigned long` depending on the build (`zconf.h` L253-L270); `z_off_t` and
//! `z_off64_t` resolve to `off_t`, `off64_t`, `__int64`, `offset_t` or
//! `long long` depending on the platform and on the caller's own
//! `_FILE_OFFSET_BITS` and `_LARGEFILE64_SOURCE` settings (`zconf.h` L494-L532).
//!
//! **No future maintainer may "simplify" any of these five aliases to a
//! fixed-width integer.** A fixed-width spelling compiles everywhere and is
//! wrong on half of it. That is why the aliases below are written in terms of
//! `core::ffi` types and `cfg`, and why the size relationships are asserted
//! rather than assumed.
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
//! | 1 -- stream-pointer validation | [`stream_ref`], [`stream_mut`] |
//! | 2 -- input/output slice reconstruction | [`input_slice`], [`output_slice_mut`] |
//! | 3 -- the opaque `state` round-trip | [`StateBlock`], [`install_state`], [`checked_state`], [`checked_state_mut`], [`take_state`] |
//!
//! ★ **Nothing in the state round-trip takes a `&mut z_stream`.** A mutable
//! reborrow of the caller's stream invalidates the caller's own [`z_streamp`], and
//! that pointer is exactly what [`StatePrefix::strm`] records and what every later
//! call compares against. The helpers therefore read and write
//! [`z_stream::state`] through raw places and take [`z_streamp`] throughout;
//! [`checked_state`] carries the full account, including the Miri diagnostic that
//! caught an earlier draft getting it wrong.
//! | 4 -- invoking `zalloc` and `zfree` | [`StreamAllocator`] |
//!
//! Categories 5 (C strings and varargs) and 6 (writing through a caller's
//! [`gz_header`]) belong to `gz.rs`, `util.rs` and `inflate.rs`, which own the
//! entry points that need them.
//!
//! # cbindgen
//!
//! The header gate runs `cbindgen --crate libz-rs-sys` and diffs the result
//! against the immutable `zlib.h`, so the *spellings* in this module are part of
//! the contract. Every ABI type uses its exact C name -- `z_stream`,
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
//! The generated artifact was verified to compile under
//! `gcc -std=c89 -pedantic -Wall -Wextra`, `-std=c99`, `-std=c17` and
//! `g++ -std=c++17`, the last of which exercises the generator's `cpp_compat`
//! setting.

// ★ The extern crate name is `z`, not `libz_rs_sys`. `Cargo.toml` declares
// `[lib] name = "z"` so that the build emits `libz.so` and `libz.a`, and a library
// target's name is also its extern crate name. Every consumer of this module --
// the integration tests, the doctests here, `zlib-rs-differential`, the fuzz
// targets and the benches -- therefore writes `use z::types::…`. The package name
// `libz-rs-sys` appears only in a dependency line or a `cargo -p` argument.

use core::ffi::{c_char, c_int, c_uint, c_ulong, c_void};
use core::mem::{align_of, size_of};
use core::ptr::NonNull;

use zlib_rs::allocate::{
    block_len, Allocator, AllocatorId, Buffer, GlobalAllocator, Opaque, Release, SENTINEL_FILL,
};
use zlib_rs::deflate::deflate_state_check;
use zlib_rs::error::ReturnCode;
// Gated exactly as the core gates the module: `zlib_rs::gz` requires
// `zlib-rs/std`, which this crate's `gz` feature turns on. The import serves only
// the layout agreement assertions further down; `gzFile_s` itself is
// unconditional, because it is an ABI type and the generated header must always
// carry it.
#[cfg(feature = "gz")]
use zlib_rs::gz::state::GzFileExposed;
use zlib_rs::inflate::inflate_state_check;
use zlib_rs::inflate::inftrees::{Code, CodeType};

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
/// declared unconditionally so that it agrees with
/// [`zlib_rs::gz::state::ZOff64`], which the core also fixes at 64 bits because
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
const _: () = assert!(size_of::<uInt>() <= size_of::<usize>());

// `z_size_t` must be exactly `size_t`, because the `_z` entry points are
// declared in terms of it and a narrower or wider spelling would change their
// signatures rather than merely their range.
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
/// [`StreamAllocator::allocate_bytes`] converts to [`None`] and the caller then
/// reports as [`ReturnCode::MEM_ERROR`].
///
/// `unsafe` because invoking caller-supplied C code is unsafe, and `extern "C"`
/// rather than `extern "C-unwind"` because an unwind must never cross this edge.
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
/// **Measured: 112 bytes, align 8**, with the field offsets given on each member
/// below. Every field is `pub` and in the exact declaration order of the C
/// struct; nothing may be reordered, added or removed.
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
    /// [`input_slice`] branches on the length before forming a slice.
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
    /// [`checked_state`]/[`checked_state_mut`] do and why `deflateStateCheck`
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
/// published contract: a null stream yields `Z_STREAM_ERROR`, so [`stream_ref`]
/// and [`stream_mut`] check before dereferencing.
pub type z_streamp = *mut z_stream;

// ---------------------------------------------------------------------------
// `gz_header` -- `zlib.h` L118-L135
// ---------------------------------------------------------------------------

/// gzip header information passed to and from the library: `zlib.h` L118-L133.
///
/// **Measured: 80 bytes, align 8.** Note two layout facts that are easy to get
/// wrong: `time` lands at offset **8, not 4**, because [`uLong`] needs 8-byte
/// alignment after the leading `int text`; and the struct carries **4 bytes of
/// tail padding** after `done`, which is why 80 rather than 76.
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
/// **Measured: 24 bytes**, `have` at 0, `next` at 8, `pos` at 16.
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
/// [`zlib_rs::gz::state::GzFileExposed`] is the core's form of the same prefix
/// and is what actually sits at the head of the allocated state. The two
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
    /// Spelled `*mut u8` rather than `*mut Bytef` to match
    /// [`zlib_rs::gz::state::GzFileExposed::next`] exactly; the two are the same
    /// type, since [`Bytef`] is [`Byte`] is `u8`.
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
const _: () = assert!(size_of::<gzFile_s>() == size_of::<GzFileExposed>());
#[cfg(feature = "gz")]
const _: () = assert!(align_of::<gzFile_s>() == align_of::<GzFileExposed>());
#[cfg(feature = "gz")]
const _: () =
    assert!(core::mem::offset_of!(gzFile_s, have) == core::mem::offset_of!(GzFileExposed, have));
#[cfg(feature = "gz")]
const _: () =
    assert!(core::mem::offset_of!(gzFile_s, next) == core::mem::offset_of!(GzFileExposed, next));
#[cfg(feature = "gz")]
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
/// **Measured: 4 bytes, align 2**, `op` at 0, `bits` at 1, `val` at 2 --
/// "Each entry is four bytes", as the comment at `inftrees.h` L23 says.
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

const _: () = assert!(size_of::<code>() == size_of::<Code>());
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
pub const ENOUGH_LENS: c_uint = 852;

/// Maximum number of table entries for the distance code: 592.
///
/// `inftrees.h` L50, from `enough 30 6 15`.
pub const ENOUGH_DISTS: c_uint = 592;

/// Maximum size of the dynamic table: 1444 entries.
///
/// `inftrees.h` L51, `ENOUGH_LENS + ENOUGH_DISTS`. `struct inflate_state`
/// budgets `4 * ENOUGH` bytes for its inline arena, and `test/infcover.c`
/// accounts for that budget when it caps allocation, so the value is
/// externally load-bearing rather than merely advisory.
pub const ENOUGH: c_uint = ENOUGH_LENS + ENOUGH_DISTS;

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
pub const CODES: c_int = 0;

/// `codetype` discriminant `LENS` -- `inftrees.h` L56.
///
/// The literal/length code: the merged 0..=285 alphabet of RFC 1951 §3.2.5.
pub const LENS: c_int = 1;

/// `codetype` discriminant `DISTS` -- `inftrees.h` L57.
///
/// The distance code: the 0..=29 alphabet of RFC 1951 §3.2.5.
pub const DISTS: c_int = 2;

/// Validates a raw `codetype` argument and maps it to the core's enum.
///
/// The guard that makes taking [`c_int`] at the boundary safe: it accepts
/// exactly [`CODES`], [`LENS`] and [`DISTS`] -- the three enumerators of
/// `inftrees.h` L54-L58, in declaration order -- and returns [`None`] for
/// anything else, which the caller reports as an error rather than treating as a
/// valid alphabet.
///
/// # Examples
///
/// The crate's `[lib] name` is `z`, so that is the extern crate name a consumer
/// writes -- see the module documentation.
///
/// ```
/// use z::types::{code_type_from_raw, CODES, DISTS, LENS};
/// use zlib_rs::inflate::inftrees::CodeType;
///
/// assert_eq!(code_type_from_raw(CODES), Some(CodeType::Codes));
/// assert_eq!(code_type_from_raw(LENS), Some(CodeType::Lens));
/// assert_eq!(code_type_from_raw(DISTS), Some(CodeType::Dists));
/// assert_eq!(code_type_from_raw(3), None);
/// assert_eq!(code_type_from_raw(-1), None);
/// ```
#[must_use]
pub const fn code_type_from_raw(raw: c_int) -> Option<CodeType> {
    match raw {
        CODES => Some(CodeType::Codes),
        LENS => Some(CodeType::Lens),
        DISTS => Some(CodeType::Dists),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// The allocator hook -- unsafe-site category 4
// ---------------------------------------------------------------------------

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
const FILL_BYTE: u8 = SENTINEL_FILL;

/// The byte a freshly acquired foreign block is filled with in release builds.
///
/// See the debug-build definition above for the rationale. Zero is the plainest
/// possible choice for a pass that has to happen anyway.
#[cfg(not(debug_assertions))]
const FILL_BYTE: u8 = {
    // Referenced so that the import is used in both build configurations and the
    // two definitions cannot drift apart unnoticed.
    let _ = SENTINEL_FILL;
    0
};

/// The 16-bit fill pattern, both of whose bytes are `FILL_BYTE`.
///
/// Filling a `u16` block byte-for-byte the way a byte block is filled keeps the
/// two shapes consistent: in a debug build a `u16` block reads back as `0xa5a5`,
/// which is what the bytes `0xa5 0xa5` mean in either byte order. This is the
/// same derivation the core uses for its own `u16` shape.
const FILL_U16: u16 = u16::from_ne_bytes([FILL_BYTE, FILL_BYTE]);

/// Which pair of routines a [`StreamAllocator`] calls.
///
/// Private, and deliberately so: the two arms are not interchangeable, and
/// keeping them in a closed enum nobody outside this module can construct means a
/// caller's block can never be routed to the internal path or the reverse. The
/// only ways in are [`StreamAllocator::internal`] and
/// [`StreamAllocator::from_hooks`].
#[derive(Debug, Clone, Copy)]
enum Hooks {
    /// Both caller hooks were `Z_NULL`, so the library's own routines apply.
    ///
    /// The substitution `zlib.h` L151-L153 promises and `deflate.c` L401-L414,
    /// `inflate.c` L183-L195 and `infback.c` L37-L49 perform. Delegates to
    /// [`GlobalAllocator`], which the core documents as the mirror of
    /// `zcalloc`/`zcfree` (`zutil.c` L299-L308).
    Internal,
    /// The caller supplied both hooks; every request goes through them.
    Caller {
        /// The caller's `zalloc` (`zlib.h` L85), known non-null.
        allocate: unsafe extern "C" fn(voidpf, uInt, uInt) -> voidpf,
        /// The caller's `zfree` (`zlib.h` L86), known non-null.
        deallocate: unsafe extern "C" fn(voidpf, voidpf),
        /// The caller's `opaque` (`zlib.h` L104), passed back verbatim.
        opaque: Opaque,
    },
}

/// The facade's [`Allocator`]: the raw-pointer half of the core's abstraction.
///
/// [`zlib_rs::allocate`] defines the *shape* of allocation and supplies the
/// implementation that needs no hooks ([`GlobalAllocator`]), but it cannot supply
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
/// yielding [`Release::Refused`] on any mismatch. A refused release leaks the
/// block -- safe and diagnosable -- rather than corrupting the heap.
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
/// This is precisely why AAP §0.6.4.5 assigns the two tools as it does: **Miri
/// covers `crates/zlib-rs`, the safe core, and AddressSanitizer covers
/// `crates/libz-rs-sys`, the boundary layer "where raw pointers actually exist".**
/// The plan's own reasoning is that "Miri can analyze the core exhaustively
/// precisely because the core contains no FFI". Accordingly:
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
#[derive(Debug, Clone, Copy)]
pub struct StreamAllocator {
    /// Which routines to call. See [`Hooks`].
    hooks: Hooks,
}

impl StreamAllocator {
    /// The allocator the library substitutes when both caller hooks are `Z_NULL`.
    ///
    /// Mirrors the substitution at `deflate.c` L401-L414, `inflate.c` L183-L195
    /// and `infback.c` L37-L49, which install `zcalloc`/`zcfree` and clear
    /// `opaque`. Requests are served by [`GlobalAllocator`], which the core
    /// documents as the mirror of those routines.
    ///
    /// Also the right choice for buffers that are not reached through a
    /// [`z_stream`] at all -- the `gzFile` layer allocates its state, its path
    /// string and its two working buffers with plain `malloc`
    /// (`gzlib.c` L100 and L206, `gzread.c` L99-L100, `gzwrite.c` L16 and L25),
    /// because a `gzFile` has no caller-supplied hooks to honour.
    #[must_use]
    pub const fn internal() -> Self {
        Self {
            hooks: Hooks::Internal,
        }
    }

    /// Builds an allocator from a caller's `(zalloc, zfree, opaque)` triple.
    ///
    /// The three arguments are [`z_stream::zalloc`], [`z_stream::zfree`] and
    /// [`z_stream::opaque`] read straight off the caller's stream; see
    /// [`StreamAllocator::from_stream`] for the reading half.
    ///
    /// | `zalloc` | `zfree` | Result |
    /// |---|---|---|
    /// | [`None`] | [`None`] | [`StreamAllocator::internal`] |
    /// | [`Some`] | [`Some`] | the caller's hooks |
    /// | [`Some`] | [`None`] | [`None`] -- rejected |
    /// | [`None`] | [`Some`] | [`None`] -- rejected |
    ///
    /// # ★ Why a half-supplied pair is refused
    ///
    /// C defaults the two hooks *independently*: `deflate.c` L401-L407 replaces a
    /// null `zalloc` with `zcalloc`, and L410-L414 separately replaces a null
    /// `zfree` with `zcfree`. A caller who supplies only `zalloc` therefore has
    /// its blocks released by `free()` -- a genuine mismatched-allocator hazard
    /// that happens to work only because both routines are `malloc`-backed. A
    /// memory-safe port cannot reproduce a hazard, and it does not need to:
    ///
    /// * `zlib.h` L151-L153 documents the two as a *pair* -- "If `zalloc` and
    ///   `zfree` are set to `Z_NULL`, `deflateInit` updates them to use default
    ///   allocation functions" -- so treating them as a pair is the documented
    ///   contract.
    /// * C itself refuses the half-supplied case under `Z_SOLO`, returning
    ///   `Z_STREAM_ERROR` at `deflate.c` L403 and L412, so the status a caller
    ///   sees here is one C already produces in a supported configuration.
    /// * No test in the suite exercises it. `test/example.c` leaves both
    ///   `Z_NULL`; `test/infcover.c` sets both, in `mem_setup`; `test/minigzip.c`
    ///   uses only the `gz*` layer, which has no hooks.
    ///
    /// A rejected triple must be reported as `Z_STREAM_ERROR` by the entry point,
    /// which is the same status `inflateStateCheck` (`inflate.c` L88-L97) yields
    /// for a stream whose `zalloc` or `zfree` is null.
    #[must_use]
    pub fn from_hooks(zalloc: alloc_func, zfree: free_func, opaque: voidpf) -> Option<Self> {
        match (zalloc, zfree) {
            (None, None) => Some(Self::internal()),
            (Some(allocate), Some(deallocate)) => Some(Self {
                hooks: Hooks::Caller {
                    allocate,
                    deallocate,
                    opaque: Opaque::new(opaque),
                },
            }),
            // Exactly one hook supplied: see the note above.
            (Some(_), None) | (None, Some(_)) => None,
        }
    }

    /// Builds an allocator from a borrowed stream.
    ///
    /// Reads only the three allocator members and interprets nothing else. Use it
    /// where a borrow already exists -- for instance inside an entry point that has
    /// called [`stream_ref`] to reach `avail_in` and `next_in` anyway.
    ///
    /// Where the entry point is going on to touch [`z_stream::state`], prefer
    /// [`StreamAllocator::from_stream_ptr`]: a borrow of the stream and a later use
    /// of the caller's raw pointer do not mix, for the reason
    /// [`checked_state`] documents at length.
    ///
    /// Returns [`None`] for a half-supplied hook pair, for the reason
    /// [`StreamAllocator::from_hooks`] documents.
    #[must_use]
    pub fn from_stream(strm: &z_stream) -> Option<Self> {
        Self::from_hooks(strm.zalloc, strm.zfree, strm.opaque)
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
    pub unsafe fn from_stream_ptr(strm: z_streamp) -> Option<Self> {
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
        Self::from_hooks(zalloc, zfree, opaque)
    }

    /// Reports whether this allocator uses the library's internal routines.
    ///
    /// Useful to an entry point that needs to know whether the caller's
    /// `z_stream` still holds `Z_NULL` hooks, and to tests.
    #[must_use]
    pub const fn is_internal(&self) -> bool {
        matches!(self.hooks, Hooks::Internal)
    }

    /// Returns the `opaque` value this allocator passes to its hooks.
    ///
    /// [`Opaque::NULL`] in internal mode, mirroring `strm->opaque = (voidpf)0` at
    /// `deflate.c` L406.
    #[must_use]
    pub const fn opaque_ptr(&self) -> voidpf {
        match self.hooks {
            Hooks::Internal => core::ptr::null_mut(),
            Hooks::Caller { opaque, .. } => opaque.as_ptr(),
        }
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

impl<'a> Allocator<'a> for StreamAllocator {
    /// Returns [`AllocatorId::GLOBAL`] in internal mode and
    /// [`AllocatorId::foreign`] in caller mode.
    ///
    /// The foreign identity records what actually distinguishes two allocators --
    /// the two hook addresses and the `opaque` value -- rather than a digest of
    /// them, so two identities compare equal precisely when the allocators are
    /// interchangeable and there is no collision to worry about. The addresses are
    /// used for comparison only: nothing is ever called or read through the
    /// integers.
    fn id(&self) -> AllocatorId {
        match self.hooks {
            Hooks::Internal => GlobalAllocator.id(),
            Hooks::Caller {
                allocate,
                deallocate,
                opaque,
            } => AllocatorId::foreign(allocate as usize, deallocate as usize, opaque.addr()),
        }
    }

    /// Returns the `opaque` value the hooks receive, or [`Opaque::NULL`] in
    /// internal mode.
    fn opaque(&self) -> Opaque {
        match self.hooks {
            Hooks::Internal => GlobalAllocator.opaque(),
            Hooks::Caller { opaque, .. } => opaque,
        }
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
        match self.hooks {
            Hooks::Internal => GlobalAllocator.allocate_bytes(items, size),
            Hooks::Caller {
                allocate, opaque, ..
            } => {
                let (base, len) = Self::call_zalloc(allocate, opaque, items, size)?;

                // SAFETY: unsafe-site category 2 -- reconstructing a slice from a
                // pointer/length pair. `base` is non-null (a `NonNull`) and
                // trivially aligned for `u8`. `len` is the checked `items * size`
                // product this very call passed to the hook, and `zlib.h` L149's
                // contract is that a non-null return addresses that many writable
                // bytes. The region is freshly allocated, so nothing else aliases
                // it, and the borrow is produced exactly once and handed straight
                // to `Buffer`, which owns it until `release_to` consumes it. The
                // bytes are uninitialised at this instant and are written -- not
                // read -- by the `fill` on the next line, which is what makes the
                // slice sound to expose afterwards.
                let block: &'a mut [u8] =
                    unsafe { core::slice::from_raw_parts_mut(base.as_ptr(), len) };
                block.fill(FILL_BYTE);

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
        match self.hooks {
            Hooks::Internal => GlobalAllocator.allocate_u16s(items),
            Hooks::Caller {
                allocate,
                deallocate,
                opaque,
            } => {
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

                // SAFETY: unsafe-site category 2 -- reconstructing a slice from a
                // pointer/length pair. `elements` is non-null, derived from a
                // `NonNull`, and has just been shown to be `u16`-aligned. The
                // hook was asked for `items * size_of::<u16>()` bytes and
                // returned non-null, so `items` `u16` values fit in the region it
                // addresses; the product was computed checked inside
                // `call_zalloc`. The region is fresh, so nothing aliases it, and
                // the borrow is created once and handed to `Buffer`. The elements
                // are uninitialised here and are written by the `fill` below
                // before any read can occur.
                let block: &'a mut [u16] =
                    unsafe { core::slice::from_raw_parts_mut(elements, items) };
                block.fill(FILL_U16);

                Some(Buffer::from_foreign(self, block))
            }
        }
    }

    /// Releases a byte block obtained from this allocator.
    ///
    /// `ZFREE(strm, addr)` from `zutil.h` L254. Routed through
    /// [`Buffer::release_to`], which is what guarantees the identity check runs
    /// and is also the only way to obtain the borrow to free.
    fn deallocate_bytes(&self, buffer: Buffer<'a, u8>) {
        match self.hooks {
            Hooks::Internal => GlobalAllocator.deallocate_bytes(buffer),
            Hooks::Caller {
                deallocate, opaque, ..
            } => match buffer.release_to(self) {
                Release::Foreign(block) => {
                    Self::call_zfree(deallocate, opaque, block.as_mut_ptr());
                }
                // Two distinct situations, one correct response: do nothing.
                //
                // `Handled` means the block was a Rust allocation that has already
                // released itself, so no caller's `zfree` ever handed this memory
                // out and calling one would be a rogue free. It cannot arise on
                // this arm in any case, because caller mode never hands out
                // Rust-owned storage.
                //
                // `Refused` means the block came from a *different* allocator and
                // has not been released. Dropping the buffer leaks a foreign block
                // -- safe, and visible in `test/infcover.c`'s leak report --
                // whereas passing it to the wrong `zfree` would corrupt the heap. A
                // Rust-owned block in that position still runs its own destructor
                // as it drops, so nothing leaks in that direction.
                Release::Handled | Release::Refused(_) => {}
            },
        }
    }

    /// Releases a `u16` block obtained from this allocator.
    ///
    /// The [`Allocator::deallocate_bytes`] counterpart for the shape
    /// [`Allocator::allocate_u16s`] produces; the base address handed to `zfree`
    /// is the same one `zalloc` returned, because a [`Buffer`] never narrows the
    /// block it holds.
    fn deallocate_u16s(&self, buffer: Buffer<'a, u16>) {
        match self.hooks {
            Hooks::Internal => GlobalAllocator.deallocate_u16s(buffer),
            Hooks::Caller {
                deallocate, opaque, ..
            } => match buffer.release_to(self) {
                Release::Foreign(block) => {
                    Self::call_zfree(deallocate, opaque, block.as_mut_ptr().cast::<u8>());
                }
                // As `deallocate_bytes`: two distinct situations, one correct
                // response. See the comment there.
                Release::Handled | Release::Refused(_) => {}
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Single-value placement, shared by the state round-trip
// ---------------------------------------------------------------------------

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
    fn place<T>(&self, value: T) -> Option<NonNull<T>> {
        // A zero-sized `T` has no valid allocation: `alloc` with a zero-size
        // layout is undefined behaviour, and `malloc(0)` may legitimately return
        // null. No state type is zero-sized -- `StateBlock` always carries its
        // prefix -- so this is a guard rather than a case.
        if size_of::<T>() == 0 {
            return None;
        }

        match self.hooks {
            Hooks::Internal => {
                let layout = core::alloc::Layout::new::<T>();
                // SAFETY: unsafe-site category 3 -- placing the object that
                // `z_stream.state` will point at. `layout` has non-zero size, which is the single
                // precondition of `alloc`; the branch above established it. The
                // returned block is either null, handled by `NonNull::new`
                // immediately below, or `layout.size()` writable bytes aligned to
                // `layout.align()` -- i.e. correctly aligned for `T`.
                let raw = unsafe { std::alloc::alloc(layout) }.cast::<T>();
                let slot = NonNull::new(raw)?;
                // SAFETY: unsafe-site category 3 -- moving the state into its
                // block. `slot` is a fresh, correctly aligned, non-null
                // allocation of exactly `size_of::<T>()` bytes that nothing else
                // references, and its contents are uninitialised -- so writing
                // rather than assigning is required, since assignment would drop
                // whatever the uninitialised bytes were interpreted as.
                unsafe { slot.as_ptr().write(value) };
                Some(slot)
            }
            Hooks::Caller {
                allocate,
                deallocate,
                opaque,
            } => {
                let (base, _len) = Self::call_zalloc(allocate, opaque, 1, size_of::<T>())?;
                let slot = base.cast::<T>();
                if !slot.as_ptr().is_aligned() {
                    Self::call_zfree(deallocate, opaque, base.as_ptr());
                    return None;
                }
                // SAFETY: unsafe-site category 3 -- moving the state into a block
                // the caller's `zalloc` returned. `call_zalloc` was asked for
                // `1 * size_of::<T>()` bytes
                // and returned non-null, so by the `zlib.h` L149 contract the
                // region holds that many writable bytes; the line above proved the
                // address is aligned for `T`. The block is freshly allocated, so
                // nothing else references it, and its contents are uninitialised,
                // so it is written rather than assigned.
                unsafe { slot.as_ptr().write(value) };
                Some(slot)
            }
        }
    }

    /// Moves the `T` at `slot` back out and returns its storage to this
    /// allocator.
    ///
    /// The exact inverse of [`StreamAllocator::place`]: `ZFREE(strm, s)` from
    /// `deflate.c` L1305, or Rust's global deallocator in internal mode.
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

        match self.hooks {
            Hooks::Internal => {
                // SAFETY: unsafe-site category 3 -- releasing the state object's
                // storage. By this function's contract `slot` came from
                // `place::<T>` on an internal-mode allocator, which allocated it
                // with exactly `Layout::new::<T>()` through the same global
                // allocator. `dealloc` requires the identical layout, which is
                // reconstructed here from the same type.
                unsafe {
                    std::alloc::dealloc(
                        slot.as_ptr().cast::<u8>(),
                        core::alloc::Layout::new::<T>(),
                    );
                }
            }
            Hooks::Caller {
                deallocate, opaque, ..
            } => Self::call_zfree(deallocate, opaque, slot.as_ptr().cast::<u8>()),
        }

        value
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
#[allow(clippy::cast_possible_truncation, clippy::cast_lossless)]
pub const fn widen(count: uInt) -> usize {
    count as usize
}

// ---------------------------------------------------------------------------
// Stream-pointer validation -- unsafe-site category 1
// ---------------------------------------------------------------------------

/// Borrows a caller's stream, or reports that the entry point must fail.
///
/// Every exported stream function receives a [`z_streamp`], and nullability is
/// part of the *published* contract rather than an edge case: `zlib.h` documents
/// `Z_STREAM_ERROR` for an invalid stream, and `test/infcover.c` L394-L396
/// asserts it for `inflate(Z_NULL, 0)`, `inflateEnd(Z_NULL)` and
/// `inflateCopy(Z_NULL, Z_NULL)`. The guard therefore always *precedes* the first
/// dereference, and [`None`] means "return `Z_STREAM_ERROR`".
///
/// # What is and is not checked
///
/// Checked: non-null, and aligned for [`z_stream`]. Not checkable from here:
/// whether the caller really allocated 112 bytes of live `z_stream` at that
/// address. No language construct can verify that, which is why it appears as a
/// safety obligation below rather than as a runtime test -- and why the *state*
/// carries a tag that [`checked_state`] validates, giving a second, independent
/// chance to reject a stream that was never initialised.
///
/// # Safety
///
/// If `strm` is non-null it must address a caller-allocated [`z_stream`] --
/// at least 112 bytes, aligned to 8 -- that stays live and unaliased for `'a`.
/// The returned borrow's lifetime is unconstrained by the argument, which is
/// inherent to an FFI boundary: the caller of this function is responsible for
/// not outliving the stream.
#[must_use]
pub unsafe fn stream_ref<'a>(strm: z_streamp) -> Option<&'a z_stream> {
    if strm.is_null() || !strm.is_aligned() {
        return None;
    }
    // SAFETY: unsafe-site category 1 -- stream-pointer validation. The two tests
    // above established non-null and correct alignment; this function's own safety
    // contract supplies the remaining requirement, that a non-null `strm`
    // addresses a live, unaliased `z_stream` for `'a`.
    Some(unsafe { &*strm })
}

/// Borrows a caller's stream mutably, or reports that the entry point must fail.
///
/// The [`stream_ref`] counterpart for the entry points that update the stream --
/// which is nearly all of them, since `next_in`, `avail_in`, `next_out`,
/// `avail_out`, `total_in`, `total_out`, `msg`, `state`, `data_type` and `adler`
/// are all library-written fields.
///
/// # Safety
///
/// As [`stream_ref`], and additionally: no other reference to the same
/// [`z_stream`] may exist for `'a`. Reconstruct the borrow once on entry rather
/// than repeatedly, and do not hold two at the same time -- the two-stream entry
/// points (`deflateCopy`, `inflateCopy`) must additionally establish that their
/// source and destination are distinct.
#[must_use]
pub unsafe fn stream_mut<'a>(strm: z_streamp) -> Option<&'a mut z_stream> {
    if strm.is_null() || !strm.is_aligned() {
        return None;
    }
    // SAFETY: unsafe-site category 1 -- stream-pointer validation. Non-null and
    // aligned are established above; liveness and the absence of any other live
    // borrow are this function's documented obligations on its caller.
    Some(unsafe { &mut *strm })
}

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
#[must_use]
pub unsafe fn input_slice<'a>(next_in: *const Bytef, avail_in: uInt) -> &'a [u8] {
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

/// Rebuilds the caller's output as a mutable slice from `(next_out, avail_out)`.
///
/// The [`input_slice`] counterpart, and the only place
/// [`core::slice::from_raw_parts_mut`] is called for a caller's output buffer.
/// The same zero-length rule applies and for the same reason: a stream with
/// `avail_out == 0` and a null `next_out` must yield an empty slice, not
/// undefined behaviour.
///
/// # Safety
///
/// If `avail_out` is non-zero, `next_out` must be non-null and writable for
/// `avail_out` bytes, and that region must not be aliased by anything else --
/// including by the input slice -- for `'a`. Call this once on entry; two live
/// mutable slices over the same output buffer would be undefined behaviour even
/// if neither were written.
#[must_use]
pub unsafe fn output_slice_mut<'a>(next_out: *mut Bytef, avail_out: uInt) -> &'a mut [u8] {
    let len = widen(avail_out);
    if len == 0 || next_out.is_null() {
        return &mut [];
    }
    // SAFETY: unsafe-site category 2 -- slice reconstruction. `next_out` is
    // non-null by the test above and trivially aligned for `u8`. `len` is
    // `avail_out` widened without loss, and this function's contract makes those
    // bytes writable, unaliased and stable for `'a`.
    unsafe { core::slice::from_raw_parts_mut(next_out, len) }
}

// ---------------------------------------------------------------------------
// The opaque `state` round-trip -- unsafe-site category 3
// ---------------------------------------------------------------------------

/// Which state a tag belongs to, and hence which of the core's validators applies.
///
/// An enum rather than a function argument so that an entry point cannot pass the
/// wrong predicate: there are exactly two, and the mapping to the core's
/// [`deflate_state_check`] and [`inflate_state_check`] is fixed here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StateKind {
    /// A compression state. Its tag is the `status` member of `deflate_state`,
    /// one of the eight values `deflateStateCheck` accepts (`deflate.c`
    /// L546-L555).
    Deflate,
    /// A decompression state. Its tag is the `mode` member of `inflate_state`,
    /// which must lie in `HEAD..=SYNC`, i.e. `16180..=16211`
    /// (`inflate.c` L95).
    Inflate,
}

impl StateKind {
    /// Reports whether `tag` must be **rejected**.
    ///
    /// The polarity matches C's, where a non-zero return from
    /// `deflateStateCheck`/`inflateStateCheck` means "reject", so that the Rust
    /// and C forms read the same way side by side. Routed through the core's own
    /// validators rather than reimplemented, which is what keeps the accepted set
    /// in one place.
    #[must_use]
    pub const fn rejects(self, tag: c_int) -> bool {
        match self {
            Self::Deflate => deflate_state_check(tag),
            Self::Inflate => inflate_state_check(tag),
        }
    }
}

// The core's validators take `i32`. `c_int` is `i32` on every target that has
// both `std` and a C ABI, so the calls above need no conversion; the assertion
// turns a hypothetical target where that fails into a build error rather than a
// silent narrowing.
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
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct StatePrefix {
    /// Offset 0. The stream that owns this state.
    ///
    /// `deflate.h` L105 and `inflate.h` L83, both `z_streamp strm`. C compares it
    /// against the incoming stream -- `state->strm != strm` at `inflate.c` L94 --
    /// to reject a state that belongs to a different stream, and
    /// [`checked_state`] performs the same comparison.
    pub strm: z_streamp,
    /// Offset 8. The state's validity tag: `status` for deflate, `mode` for
    /// inflate.
    ///
    /// Four bytes wide, matching the C `int` and the C enum. See
    /// [`StateKind::rejects`] for the accepted ranges.
    pub tag: c_int,
}

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
/// assertion fails. The core measures `InflateState` at 7296 bytes with a
/// reference allocator, so the block lands at 7312 -- comfortably inside the
/// `7033..=8234` window the core documents, the upper bound coming from the 15%
/// per-stream memory bar.
#[repr(C)]
pub struct StateBlock<S> {
    /// The C-visible prefix. Private so that only the accessors below can touch
    /// it, which keeps the tag's two roles from being confused with ordinary data.
    prefix: StatePrefix,
    /// The Rust state. Invisible to C and free of layout obligations.
    state: S,
}

impl<S> StateBlock<S> {
    /// Wraps a state with the prefix a given stream expects.
    ///
    /// `strm` becomes [`StatePrefix::strm`], which is what makes the
    /// owner-identity check in [`checked_state`] meaningful, and `tag` becomes the
    /// initial validity tag -- `INIT_STATE` for deflate, `HEAD` (16180) for
    /// inflate, mirroring `inflate.c` L205, which pre-seeds the mode precisely so
    /// that a freshly allocated state passes the tag check.
    #[must_use]
    pub const fn new(strm: z_streamp, tag: c_int, state: S) -> Self {
        Self {
            prefix: StatePrefix { strm, tag },
            state,
        }
    }

    /// Returns the stream recorded in the prefix.
    #[must_use]
    pub const fn owner(&self) -> z_streamp {
        self.prefix.strm
    }

    /// Re-points the prefix at a different stream.
    ///
    /// Needed by `deflateCopy` and `inflateCopy`, which duplicate a state into a
    /// second stream and must then make the copy's back-pointer refer to *that*
    /// stream: `copy->strm = dest` at `inflate.c` L1349. Without this the copy
    /// would fail its own owner-identity check on the next call.
    pub fn set_owner(&mut self, strm: z_streamp) {
        self.prefix.strm = strm;
    }

    /// Returns the validity tag currently in the prefix.
    ///
    /// Read this on entry and feed it to
    /// [`zlib_rs::inflate::InflateState::set_mode_tag`] so that a mode written by
    /// caller code -- `test/infcover.c` L330 and L458 -- is honoured rather than
    /// silently discarded.
    #[must_use]
    pub const fn tag(&self) -> c_int {
        self.prefix.tag
    }

    /// Writes the validity tag into the prefix.
    ///
    /// Call this before returning, with
    /// [`zlib_rs::inflate::InflateState::mode_tag`], so that the C-visible slot
    /// always reflects the Rust state. The two are separate storage, so they stay
    /// in step only because every entry point syncs them.
    pub fn set_tag(&mut self, tag: c_int) {
        self.prefix.tag = tag;
    }

    /// Borrows the wrapped state.
    #[must_use]
    pub const fn state(&self) -> &S {
        &self.state
    }

    /// Borrows the wrapped state mutably.
    #[must_use]
    pub fn state_mut(&mut self) -> &mut S {
        &mut self.state
    }

    /// Unwraps the block, discarding the prefix.
    #[must_use]
    pub fn into_state(self) -> S {
        self.state
    }
}

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
            .finish_non_exhaustive()
    }
}

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
pub unsafe fn install_state<S>(
    strm: z_streamp,
    allocator: &StreamAllocator,
    tag: c_int,
    state: S,
) -> Result<NonNull<StateBlock<S>>, ReturnCode> {
    if strm.is_null() || !strm.is_aligned() {
        return Err(ReturnCode::STREAM_ERROR);
    }

    // The prefix records the caller's pointer verbatim, which is what makes the
    // owner-identity check in `checked_state` compare like with like.
    let slot = allocator
        .place(StateBlock::new(strm, tag, state))
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

/// Recovers a validated borrow of the state behind [`z_stream::state`].
///
/// The read half of the round-trip, and the Rust form of the surviving parts of
/// `deflateStateCheck` (`deflate.c` L538) and `inflateStateCheck`
/// (`inflate.c` L88). Four things are established before any state field is
/// touched, in this order:
///
/// 1. `strm` is non-null and aligned -- the `strm == Z_NULL` test.
/// 2. [`z_stream::state`] is non-null and aligned for the block -- the
///    `state == Z_NULL` test.
/// 3. [`StatePrefix::strm`] equals `strm` -- the `state->strm != strm` test at
///    `inflate.c` L94, which rejects a state belonging to another stream.
/// 4. [`StatePrefix::tag`] is in the live range for `kind` -- the
///    `state->mode < HEAD || state->mode > SYNC` test at `inflate.c` L95.
///
/// [`None`] means the entry point must return `Z_STREAM_ERROR`.
///
/// Steps 3 and 4 read the prefix through the raw pointer with
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
/// module records in [`StatePrefix::strm`] -- may no longer be used. An earlier
/// draft of this module took `&mut z_stream` in [`install_state`], and Miri
/// reported the consequence precisely: a later `checked_state` read of
/// `z_stream::state` failed with "that tag does not exist in the borrow stack",
/// pointing at the `&mut` as the invalidating retag. The addresses still compared
/// equal, so the owner test still *passed* -- the defect was invisible to every
/// functional test and visible only to an aliasing model.
///
/// Reading and writing a single member through a raw place has neither problem, and
/// it is also a closer translation of the C it replaces, where every access is
/// spelled `strm->state`. Entry points that additionally need the input or output
/// buffers should reach them the same way, or take [`stream_ref`]/[`stream_mut`]
/// and finish with the borrow *before* touching the state.
///
/// # The remaining check, and where it lives
///
/// C also rejects a stream whose `zalloc` or `zfree` is null. That test is
/// [`StreamAllocator::from_stream_ptr`]'s: it returns [`None`] for a
/// half-supplied pair. It cannot be performed here as C performs it, because this port leaves
/// both hooks `Z_NULL` when the caller did -- C overwrites them with `zcalloc` and
/// `zcfree` (`deflate.c` L401-L414) whereas [`StreamAllocator::internal`] needs no
/// function pointers at all. The difference is unobservable to the suite:
/// `test/example.c` either supplies both hooks or leaves both null, and
/// `test/infcover.c` clears both only in `mem_done` (L231-L233), after the stream
/// has already been ended.
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
pub unsafe fn checked_state<'a, S>(strm: z_streamp, kind: StateKind) -> Option<&'a StateBlock<S>> {
    // SAFETY: unsafe-site category 3 -- the opaque `state` round-trip.
    // `validated_state_ptr` performs steps 1 to 4 and returns a pointer
    // only when all four passed; this function's own contract supplies the
    // liveness and provenance requirements it cannot test. Forming a shared borrow
    // from that pointer is therefore sound, and no other borrow exists for `'a` by
    // contract.
    let slot = unsafe { validated_state_ptr::<S>(strm, kind) }?;
    // SAFETY: unsafe-site category 3, as above -- `slot` addresses an initialised
    // `StateBlock<S>`.
    Some(unsafe { slot.as_ref() })
}

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
pub unsafe fn checked_state_mut<'a, S>(
    strm: z_streamp,
    kind: StateKind,
) -> Option<&'a mut StateBlock<S>> {
    // SAFETY: unsafe-site category 3, as `checked_state`, with the caller
    // additionally guaranteeing that
    // this is the only live borrow of the block for `'a`.
    let mut slot = unsafe { validated_state_ptr::<S>(strm, kind) }?;
    // SAFETY: unsafe-site category 3, as above.
    Some(unsafe { slot.as_mut() })
}

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
/// built by [`StreamAllocator::from_stream`] from the same stream, whose hooks
/// `zlib.h` L140-L142 forbids the application from changing once initialised.
pub unsafe fn take_state<S>(
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

    Some(slot)
}
