//! Bindings to the C reference implementation, compiled as a test-only oracle.
//!
//! The point is to make the C implementation callable from Rust so that a test can run both
//! implementations against the same input, in the same process, and compare the bytes.
//!
//! The module is in two parts, and they differ in what a caller has to prove. The bulk of it is
//! `extern "C"` *declarations* -- no logic, and `unsafe` to call, because a declaration cannot
//! discharge an FFI obligation on its caller's behalf. The exception is the block of safe wrappers
//! over the generated tables (`oracle_crc_table` and its eight siblings), which are ordinary safe
//! functions: each turns a table pointer and that table's own length accessor into a
//! `&'static [T]`, so the unsafety is discharged once here rather than at every comparison. There
//! is no other logic anywhere in the file.
//!
//! # Where the symbols come from
//!
//! This crate's `build.rs` compiles the fifteen in-tree C translation units, plus a small
//! generated shim, into `libzlib_c_oracle.a` in `OUT_DIR` and emits the `rustc-link-lib` directive
//! that links it. Nothing here reaches the network, vendors a copy, or needs a submodule; the C
//! sources are read-only inputs sitting in the repository already. The archive is consumed only by
//! this crate's tests and benches and is never a dependency of `zlib-rs` or `libz-rs-sys`, which is
//! the mechanical proof that reference zlib is an *oracle* rather than a build dependency.
//!
//! # Why every name carries a `c_` prefix
//!
//! Both implementations are linked into one test binary, so `deflate` cannot mean two things.
//! `build.rs` renames every externally visible symbol in the archive to carry a `c_` prefix -- with
//! `-D<name>=c_<name>` where the preprocessor can reach the name, and with
//! `objcopy --redefine-syms` for the handful it cannot, `gzgetc` chief among them, since `gzread.c`
//! undefines the macro before defining the real function. It then audits the finished archive with
//! `nm` and fails the build if any export is left unprefixed. So `c_deflate` here is exactly
//! `deflate` in `deflate.c`, and `libz_rs_sys::deflate` is the port; a test that calls both is
//! comparing the two implementations and nothing else.
//!
//! # Calling these functions
//!
//! Every one of them is `unsafe` to call, for the ordinary FFI reasons: the pointers must be valid,
//! the lengths must describe memory that really is there, and a `z_stream` must be initialised by
//! the matching `c_*Init*_` before any other entry point touches it. The oracle is the reference
//! implementation, so it is as trustworthy as C code gets -- but it is still C code, and nothing
//! about that is checked by the compiler.
//!
//! Two practical notes for callers:
//!
//! * The `deflateInit`, `deflateInit2`, `inflateInit`, `inflateInit2` and `inflateBackInit` spellings
//!   are macros in `zlib.h`, not symbols, so they are absent here by construction. Call the
//!   `_`-suffixed forms and pass what the macros pass: the version string, which
//!   [`c_zlibVersion`] returns, and `size_of::<z_stream>()` as the `stream_size`.
//! * `gzprintf` and `gzvprintf` are the two entry points in `zlib.h` that are deliberately **not**
//!   declared here, and the omission is a safety decision rather than an oversight. Both are
//!   declared with `ZEXPORTVA`, not `ZEXPORT` -- a distinction that exists precisely because the two
//!   macros can expand to *different calling conventions*: under `ZLIB_WINAPI`, `ZEXPORT` becomes
//!   `WINAPI` (`__stdcall`) while `ZEXPORTVA` stays `WINAPIV` (`__cdecl`), since a variadic function
//!   cannot use a callee-cleanup convention. A blanket `extern "C"` declaration would therefore be
//!   silently wrong on that configuration. `gzvprintf` has a second, independent blocker: its
//!   `va_list` parameter has no portable spelling on stable Rust (`core::ffi::VaList` is unstable,
//!   and the underlying type is a pointer on one Tier-1 target and a by-value struct on another).
//!   Both symbols are present in the archive under their `c_` names, so a future test that needs
//!   them can declare them locally, with the convention pinned per target and the hazard stated at
//!   the declaration site. Neither is in the required surface, and formatted output is not a
//!   byte-identity concern: `gzprintf` is a one-line forward to `gzvprintf`, which formats into a
//!   buffer and hands the result to `gzwrite`, so [`c_gzwrite`] already covers the compression path
//!   that differential testing actually measures.
//!
//! # Type mirrors
//!
//! The `#[repr(C)]` structs and the type aliases below are transcribed from `zlib.h`, `zconf.h`,
//! `deflate.h`, `inftrees.h` and `zutil.h`. They use the C spellings on purpose -- `z_stream`, not
//! `ZStream` -- so that a reader comparing this file against the header sees one thing, not two.
//! That is what the two `allow`s below are for, and they are scoped to this module.
//!
//! Integer widths follow the headers rather than convenience. `uLong` is `unsigned long` and must
//! therefore be [`c_ulong`], which is 8 bytes on LP64 and 4 on LLP64 Windows; hardcoding `u64`
//! would silently corrupt `total_in`, `total_out` and every checksum return value on Windows, and
//! `u32` would do the same on Linux. The layout assertions at the end of this module fail the build
//! if any of it drifts.
//!
//! # Why the mirrors are declared here rather than imported
//!
//! `crates/libz-rs-sys` defines the same `#[repr(C)]` types, and importing them would be the obvious
//! way to guarantee they agree. It is not available: this crate's `[dependencies]` table is
//! deliberately empty, and `libz_rs_sys`, `zlib-rs` and `criterion` are **dev-dependencies**. Cargo
//! resolves dev-dependencies for test, example and bench targets but *not* for the crate's own `lib`
//! target, so a `use libz_rs_sys::…` in this file would not compile. Keeping the table empty is what
//! keeps the oracle out of the shipped crates' graphs, so the mirrors are transcribed locally
//! instead and held to agreement by two independent mechanisms: the compile-time layout assertions
//! below, which encode the same measured numbers as
//! `crates/libz-rs-sys/src/layout_assertions.rs`, and the [`c_oracle_ct_data_size`] /
//! [`c_oracle_code_size`] accessors, which report what the C compiler actually produced. `ct_data`
//! has no counterpart in the facade at all -- it is an internal `deflate.h` type, not part of the
//! public ABI -- so it could only ever have been declared here.
//!
//! # How this crate divides up
//!
//! ```text
//! src/oracle.rs  local #[repr(C)] mirrors + `extern "C"` c_/c_oracle_ declarations + safe slice
//!                wrappers over the generated tables. Depends on nothing outside `core`/`std`.
//! src/lib.rs     `pub mod oracle;` plus the crate-level documentation.
//! tests/*.rs     NOT YET WRITTEN. The directory does not exist, so no differential,
//!                interoperability or table-equality assertion runs anywhere today.
//! ```
//!
//! The split is a design decision about where such assertions *belong*, not a description of
//! coverage that exists: a suite under `tests/` would import `zlib_rs_differential::oracle::*`
//! (this lib) alongside `zlib_rs::*` and `libz_rs_sys::*`, which is the only place those
//! dev-dependencies resolve. Until it is written, the only thing this crate executes is the
//! deliberately minimal smoke module at the foot of this file, which proves that the archive links
//! and that the renaming worked -- and nothing about either implementation's output.

#![allow(non_camel_case_types)]
#![allow(non_snake_case)]

use core::ffi::{c_char, c_int, c_long, c_uchar, c_uint, c_ulong, c_void};

// =============================================================================
//  Scalar and pointer aliases (zconf.h)
// =============================================================================

/// C `Byte`: `unsigned char` (`zconf.h`).
pub type Byte = c_uchar;

/// C `Bytef`: `Byte FAR`, and `FAR` is empty on every target this crate supports (`zconf.h`).
pub type Bytef = Byte;

/// C `uInt`: `unsigned int` (`zconf.h`).
pub type uInt = c_uint;

/// C `uLong`: `unsigned long` (`zconf.h`). Never a fixed-width integer -- see the module docs.
pub type uLong = c_ulong;

/// C `uLongf`: `uLong FAR` (`zconf.h`).
pub type uLongf = uLong;

/// C `uch`: `unsigned char` (`zutil.h`).
pub type uch = c_uchar;

/// C `ush`: `unsigned short` (`zutil.h`).
pub type ush = u16;

/// C `voidp`: `void *` (`zconf.h`).
pub type voidp = *mut c_void;

/// C `voidpc`: `void const *` (`zconf.h`).
pub type voidpc = *const c_void;

/// C `voidpf`: `void FAR *` (`zconf.h`).
pub type voidpf = *mut c_void;

/// C `z_crc_t`: the 32-bit unsigned type `zconf.h` selects for CRC values.
pub type z_crc_t = u32;

/// C `z_size_t`: `size_t` under `STDC`, which every supported target is (`zconf.h`).
pub type z_size_t = usize;

/// C `wchar_t`, needed only for the Windows-only `gzopen_w`.
#[cfg(windows)]
pub type wchar_t = u16;

/// C `z_off_t`.
///
/// `zconf.h` resolves it to `off_t` whenever `<unistd.h>` is available and falls back to
/// `long long` otherwise. The oracle is compiled with `-D_LARGEFILE64_SOURCE=1` and deliberately
/// *without* `-D_FILE_OFFSET_BITS=64`, so `Z_WANT64` is not defined and `z_off_t` stays `off_t` --
/// which is `long` on the Unix targets in scope. Everywhere else the header's `long long` fallback
/// applies.
#[cfg(unix)]
pub type z_off_t = c_long;

/// C `z_off_t`; see the `unix` definition for the derivation.
#[cfg(not(unix))]
pub type z_off_t = i64;

/// C `z_off64_t`: `off64_t`, `__int64` or `long long` depending on target, all 64-bit (`zconf.h`).
pub type z_off64_t = i64;

/// C `struct internal_state`: declared but never defined in `zlib.h`, so opaque here too.
///
/// A zero-sized `#[repr(C)]` struct is the faithful mirror of an incomplete C type: it can be
/// pointed at and never dereferenced, which is exactly the contract `zlib.h` offers.
#[repr(C)]
#[derive(Debug)]
pub struct internal_state {
    _opaque: [u8; 0],
}

/// C `alloc_func`: `voidpf (*)(voidpf opaque, uInt items, uInt size)` (`zlib.h`).
///
/// `Option` is the nullable mirror: C's `Z_NULL` is `None`, which selects the library's own
/// `malloc`-based default.
pub type alloc_func = Option<unsafe extern "C" fn(voidpf, uInt, uInt) -> voidpf>;

/// C `free_func`: `void (*)(voidpf opaque, voidpf address)` (`zlib.h`).
pub type free_func = Option<unsafe extern "C" fn(voidpf, voidpf)>;

/// C `in_func`: `unsigned (*)(void FAR *, z_const unsigned char FAR * FAR *)` (`zlib.h`).
pub type in_func = Option<unsafe extern "C" fn(*mut c_void, *mut *const c_uchar) -> c_uint>;

/// C `out_func`: `int (*)(void FAR *, unsigned char FAR *, unsigned)` (`zlib.h`).
pub type out_func = Option<unsafe extern "C" fn(*mut c_void, *mut c_uchar, c_uint) -> c_int>;

// =============================================================================
//  Structures (zlib.h, deflate.h, inftrees.h)
// =============================================================================

/// C `z_stream` / `struct z_stream_s` (`zlib.h`).
///
/// Field order and types are transcribed exactly; the layout assertions below check the result
/// against the numbers measured from the C build.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct z_stream {
    /// Next input byte.
    pub next_in: *const Bytef,
    /// Number of bytes available at `next_in`.
    pub avail_in: uInt,
    /// Total number of input bytes read so far.
    pub total_in: uLong,
    /// Next output byte will go here.
    pub next_out: *mut Bytef,
    /// Remaining free space at `next_out`.
    pub avail_out: uInt,
    /// Total number of bytes output so far.
    pub total_out: uLong,
    /// Last error message, or null when there is none.
    pub msg: *const c_char,
    /// The library's private state; opaque to callers.
    pub state: *mut internal_state,
    /// Used to allocate the internal state.
    pub zalloc: alloc_func,
    /// Used to free the internal state.
    pub zfree: free_func,
    /// Private data object passed back to `zalloc` and `zfree` untouched.
    pub opaque: voidpf,
    /// Best guess about the data type for deflate, or the decoding state for inflate.
    pub data_type: c_int,
    /// Adler-32 or CRC-32 value of the uncompressed data.
    pub adler: uLong,
    /// Reserved for future use.
    pub reserved: uLong,
}

impl Default for z_stream {
    /// The `memset(&strm, 0, sizeof strm)` a C caller performs before `deflateInit_`.
    ///
    /// `Z_NULL` is 0 for every field that has one, so an all-zero stream is precisely what the
    /// documented initialisation sequence expects: null `zalloc`/`zfree`/`opaque` selects the
    /// library's own allocator.
    fn default() -> Self {
        Self {
            next_in: core::ptr::null(),
            avail_in: 0,
            total_in: 0,
            next_out: core::ptr::null_mut(),
            avail_out: 0,
            total_out: 0,
            msg: core::ptr::null(),
            state: core::ptr::null_mut(),
            zalloc: None,
            zfree: None,
            opaque: core::ptr::null_mut(),
            data_type: 0,
            adler: 0,
            reserved: 0,
        }
    }
}

/// C `z_streamp`: `z_stream FAR *` (`zlib.h`).
pub type z_streamp = *mut z_stream;

/// C `gz_header` / `struct gz_header_s` (`zlib.h`).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct gz_header {
    /// True if the compressed data is believed to be text.
    pub text: c_int,
    /// Modification time.
    pub time: uLong,
    /// Extra flags; not used when writing a gzip file.
    pub xflags: c_int,
    /// Operating system.
    pub os: c_int,
    /// Extra field, or null if none.
    pub extra: *mut Bytef,
    /// Extra field length, valid when `extra` is non-null.
    pub extra_len: uInt,
    /// Space at `extra`; only meaningful when reading a header.
    pub extra_max: uInt,
    /// Zero-terminated file name, or null if none.
    pub name: *mut Bytef,
    /// Space at `name`; only meaningful when reading a header.
    pub name_max: uInt,
    /// Zero-terminated comment, or null if none.
    pub comment: *mut Bytef,
    /// Space at `comment`; only meaningful when reading a header.
    pub comm_max: uInt,
    /// True if there was, or will be, a header CRC.
    pub hcrc: c_int,
    /// True when the gzip header has been read; not used when writing.
    pub done: c_int,
}

impl Default for gz_header {
    /// An all-zero header, which is what a caller passes to `inflateGetHeader` after setting only
    /// the buffers and their maxima it actually wants filled.
    fn default() -> Self {
        Self {
            text: 0,
            time: 0,
            xflags: 0,
            os: 0,
            extra: core::ptr::null_mut(),
            extra_len: 0,
            extra_max: 0,
            name: core::ptr::null_mut(),
            name_max: 0,
            comment: core::ptr::null_mut(),
            comm_max: 0,
            hcrc: 0,
            done: 0,
        }
    }
}

/// C `gz_headerp`: `gz_header FAR *` (`zlib.h`).
pub type gz_headerp = *mut gz_header;

/// C `struct gzFile_s` (`zlib.h`): the partially exposed prefix of the gzip file state.
///
/// This is the one structure in `zlib.h` whose layout is baked into *caller* object code, because
/// `gzgetc` is a macro that dereferences `have`, `next` and `pos` directly. It is mirrored here so
/// a differential test can assert the layout the port has to reproduce.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct gzFile_s {
    /// Bytes available in the output buffer.
    pub have: c_uint,
    /// Next output byte.
    pub next: *mut c_uchar,
    /// Position relative to the start of the uncompressed data.
    pub pos: z_off64_t,
}

/// C `gzFile`: `struct gzFile_s *` (`zlib.h`), semi-opaque to callers.
pub type gzFile = *mut gzFile_s;

/// C `ct_data` (`deflate.h`): one Huffman tree node.
///
/// C declares two single-typed unions, `{ ush freq; ush code; } fc` and `{ ush dad; ush len; } dl`.
/// Both arms of each union are `ush`, so a pair of `ush` fields is byte-for-byte the same layout
/// with none of the union's ergonomic cost -- and it lets the type derive `PartialEq`, which is
/// what a table-equality test needs. [`c_oracle_ct_data_size`] reports what the C compiler actually
/// produced, so the equivalence is checked rather than assumed.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ct_data {
    /// C `fc`: the frequency count while building, the bit string once built.
    pub fc: ush,
    /// C `dl`: the father node while building, the bit-string length once built.
    pub dl: ush,
}

/// C `code` (`inftrees.h`): one entry of an inflate decoding table.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct code {
    /// Operation, extra bits, or table bits.
    pub op: c_uchar,
    /// Bits consumed by this part of the code.
    pub bits: c_uchar,
    /// Offset into the table, or the code value.
    pub val: u16,
}

// =============================================================================
//  Enumerations and dimension constants (inftrees.h, deflate.h)
// =============================================================================
//
//  These are `#define`s and an `enum` in the C headers, so they have no symbols and cannot be
//  linked: they are transcribed, and the transcriptions are checked against the C compiler's own
//  arithmetic by the table-length accessors in the shim block below, which derive their answers
//  from `sizeof(array) / sizeof(array[0])` rather than from any number written here.

/// C `codetype` (`inftrees.h` L53-L58): which table `inflate_table` is being asked to build.
///
/// ★ **A newtype, not an alias and not a Rust `enum`, and the choice is forced from both sides.**
///
/// C spells this as a `typedef enum { CODES, LENS, DISTS } codetype;`, so the values are 0, 1 and 2
/// in declaration order and the representation is implementation-defined -- GCC, Clang and MSVC all
/// give an all-non-negative enum `unsigned int`, which is why [`c_uint`] is the inner type here.
/// Both spellings are ABI-identical for a 32-bit enum on every target in scope, but the *name*
/// matters as much as the width: the prototype this harness calls through is
/// `inflate_table(codetype type, ...)`, and a declaration that says `int` there is a different
/// declaration -- a C translation unit that redeclares it alongside `inftrees.h` fails with
/// `conflicting types for 'inflate_table'`. Naming the parameter `codetype` on the Rust side is what
/// makes the two agree by inspection.
///
/// It is `#[repr(transparent)]` over an integer rather than a Rust `enum` for the opposite reason: a
/// Rust `enum` would make any other value instant undefined behaviour, and the point of a
/// fuzz-adjacent oracle harness is to be able to pass a deliberately invalid discriminant to
/// `inflate_table` and watch what the reference does. `#[repr(transparent)]` guarantees the ABI of
/// the inner type exactly, so an arbitrary bit pattern is a legal value of this type and travels
/// unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
#[allow(non_camel_case_types)]
pub struct codetype(pub c_uint);

impl codetype {
    /// C `CODES` (`inftrees.h`): build the code-length code table.
    pub const CODES: Self = Self(0);

    /// C `LENS` (`inftrees.h`): build the literal/length code table.
    pub const LENS: Self = Self(1);

    /// C `DISTS` (`inftrees.h`): build the distance code table.
    pub const DISTS: Self = Self(2);
}

/// C `ENOUGH_LENS` (`inftrees.h`): maximum [`code`] entries for literal/length codes.
///
/// The header records that this is `enough 286 9 15`, an exhaustive search performed by
/// `examples/enough.c`, and that it must be recalculated if the root table size changes.
pub const ENOUGH_LENS: c_uint = 852;

/// C `ENOUGH_DISTS` (`inftrees.h`): maximum [`code`] entries for distance codes, `enough 30 6 15`.
pub const ENOUGH_DISTS: c_uint = 592;

/// C `ENOUGH` (`inftrees.h`): `ENOUGH_LENS + ENOUGH_DISTS`, the size of the dynamic-table arena.
///
/// A caller of [`c_inflate_table`] must supply a `table` region of at least this many [`code`]
/// entries to reproduce the reference's own bound.
pub const ENOUGH: c_uint = ENOUGH_LENS + ENOUGH_DISTS;

/// C `LENGTH_CODES` (`deflate.h`): length codes, not counting the special `END_BLOCK` code.
///
/// This is the element count of the `base_length` table; [`oracle_base_length`] returns the count
/// the C compiler measured, which is the value actually to be trusted.
pub const LENGTH_CODES: c_uint = 29;

/// C `LITERALS` (`deflate.h`): the literal bytes `0..=255`.
pub const LITERALS: c_uint = 256;

/// C `L_CODES` (`deflate.h`): `LITERALS + 1 + LENGTH_CODES`, literal/length codes including
/// `END_BLOCK`.
pub const L_CODES: c_uint = LITERALS + 1 + LENGTH_CODES;

/// C `D_CODES` (`deflate.h`): distance codes, and the element count of `static_dtree` and
/// `base_dist`.
pub const D_CODES: c_uint = 30;

/// C `BL_CODES` (`deflate.h`): codes used to transfer the bit lengths.
pub const BL_CODES: c_uint = 19;

/// C `MAX_BITS` (`deflate.h`): no Huffman code may exceed this many bits.
pub const MAX_BITS: c_uint = 15;

// =============================================================================
//  Layout assertions
// =============================================================================
//
//  These run at compile time, so ABI drift is a build failure rather than a test failure.  The
//  absolute sizes are asserted only where the platform's integer model makes them meaningful:
//  `sizeof(z_stream)` is 112 on LP64 and something else on LLP64 Windows, so the unconditional
//  assertions are the relational ones and the exact numbers are gated on the model they were
//  measured under.

use core::mem::{align_of, offset_of, size_of};

// Unconditional: these two are the same on every supported target.
const _: () = assert!(size_of::<ct_data>() == 4);
const _: () = assert!(size_of::<code>() == 4);
const _: () = assert!(offset_of!(code, op) == 0);
const _: () = assert!(offset_of!(code, bits) == 1);
const _: () = assert!(offset_of!(code, val) == 2);

// Unconditional: field ORDER, which is what a mis-transcription would break, independently of any
// platform's widths.
const _: () = assert!(offset_of!(z_stream, next_in) == 0);
const _: () = assert!(offset_of!(z_stream, avail_in) < offset_of!(z_stream, total_in));
const _: () = assert!(offset_of!(z_stream, total_in) < offset_of!(z_stream, next_out));
const _: () = assert!(offset_of!(z_stream, next_out) < offset_of!(z_stream, avail_out));
const _: () = assert!(offset_of!(z_stream, avail_out) < offset_of!(z_stream, total_out));
const _: () = assert!(offset_of!(z_stream, total_out) < offset_of!(z_stream, msg));
const _: () = assert!(offset_of!(z_stream, msg) < offset_of!(z_stream, state));
const _: () = assert!(offset_of!(z_stream, state) < offset_of!(z_stream, zalloc));
const _: () = assert!(offset_of!(z_stream, zalloc) < offset_of!(z_stream, zfree));
const _: () = assert!(offset_of!(z_stream, zfree) < offset_of!(z_stream, opaque));
const _: () = assert!(offset_of!(z_stream, opaque) < offset_of!(z_stream, data_type));
const _: () = assert!(offset_of!(z_stream, data_type) < offset_of!(z_stream, adler));
const _: () = assert!(offset_of!(z_stream, adler) < offset_of!(z_stream, reserved));
const _: () = assert!(offset_of!(gzFile_s, have) == 0);
const _: () = assert!(offset_of!(gzFile_s, have) < offset_of!(gzFile_s, next));
const _: () = assert!(offset_of!(gzFile_s, next) < offset_of!(gzFile_s, pos));

// A nullable function pointer must be exactly one pointer wide, or `Z_NULL` could not be encoded
// as `None` and the struct would not match C at all.
// The four function-pointer aliases are `Option<unsafe extern "C" fn(…)>`, and these assertions are
// what prove the null-pointer optimization applies to them: a nullable C function pointer is a
// single machine word, `None` is the null pattern, and no discriminant is added. If it ever were,
// `zalloc` and `zfree` would land at the wrong offsets in `z_stream` and every allocator hook in
// `test/infcover.c` would be called through garbage.
const _: () = assert!(size_of::<alloc_func>() == size_of::<voidpf>());
const _: () = assert!(size_of::<free_func>() == size_of::<voidpf>());
const _: () = assert!(size_of::<in_func>() == size_of::<voidpf>());
const _: () = assert!(size_of::<out_func>() == size_of::<voidpf>());
const _: () = assert!(size_of::<alloc_func>() == size_of::<*const c_void>());
const _: () = assert!(size_of::<free_func>() == size_of::<*const c_void>());

// The scalar aliases, related to each other rather than to absolute numbers, so these hold on every
// target. `Bytef` being one byte is what makes a `*const Bytef` interchangeable with a byte-slice
// pointer, and `z_crc_t` being exactly four bytes is what makes the CRC table comparison meaningful.
const _: () = assert!(size_of::<Byte>() == 1);
const _: () = assert!(size_of::<Bytef>() == 1);
const _: () = assert!(size_of::<uch>() == 1);
const _: () = assert!(size_of::<ush>() == 2);
const _: () = assert!(size_of::<z_crc_t>() == 4);
const _: () = assert!(size_of::<z_off64_t>() == 8);
const _: () = assert!(size_of::<uLong>() == size_of::<c_ulong>());
const _: () = assert!(size_of::<z_size_t>() == size_of::<usize>());
const _: () = assert!(size_of::<voidp>() == size_of::<*const c_void>());
const _: () = assert!(size_of::<voidpc>() == size_of::<*const c_void>());
const _: () = assert!(size_of::<voidpf>() == size_of::<*const c_void>());
// `z_off_t` is the type the non-`64` gz seek/tell/offset entry points traffic in. It is never
// narrower than an `int`, and on the targets in scope it is either `long` or `long long`.
const _: () = assert!(size_of::<z_off_t>() >= size_of::<c_int>());
const _: () = assert!(size_of::<z_off64_t>() >= size_of::<z_off_t>());

/// True when the target uses the integer model the reference numbers were measured under: 64-bit
/// pointers with a 64-bit `unsigned long`, i.e. LP64.
//
// `#[allow(dead_code)]` is required at the declared MSRV. Every use of this constant sits inside a
// `const _: () = …` item, and rustc 1.80's dead-code pass does not traverse those bodies: it reports
// "constant `IS_LP64` is never used". Verified — the warning appears under rustc 1.80.1 and is absent
// under 1.97.1. Since the project's lint gate is `cargo clippy -- -D warnings`, the warning would be
// an error on exactly the compiler `rust-version = "1.80"` promises, so without this the crate fails
// to build there. The attribute is inert on newer toolchains.
// `crates/libz-rs-sys/src/layout_assertions.rs` carries the same note against its own gate.
#[allow(dead_code)]
const IS_LP64: bool = size_of::<*const c_void>() == 8 && size_of::<c_ulong>() == 8;

/// True when the target uses the other 64-bit model: 64-bit pointers with a **32-bit**
/// `unsigned long`, i.e. LLP64, which is `x86_64` Windows.
///
/// The oracle's mirrors have to be pinned on that model too, and for a reason specific to this
/// crate: a differential test compares BYTES between the C oracle and the Rust port through these
/// declarations. If the mirror's idea of where `total_out` lives were wrong on a target, the harness
/// would not report a layout bug — it would compare the wrong four bytes and report a *compression*
/// bug, or worse, agree by accident. The relational assertions above cannot catch a narrowing
/// re-type that padding absorbs, so the exact numbers are asserted on both 64-bit models.
///
/// The numbers are derived from the `#[repr(C)]` placement rule with `unsigned long` at 4 bytes and
/// pointers at 8 — `z_stream` 88 bytes, `gz_header` 72, `struct gzFile_s` unchanged at 24 — and were
/// confirmed by re-declaring all three structs on an LP64 host with `unsigned long` substituted by
/// `u32` and reading `size_of`/`offset_of!` back.
/// `crates/libz-rs-sys/src/layout_assertions.rs` carries the same gate, the same numbers and the
/// full derivation; the facade mirror and this one are deliberately checked independently, so an
/// edit to one is not silently blessed by the other.
//
// `#[allow(dead_code)]` for the same measured MSRV reason as `IS_LP64` above.
#[allow(dead_code)]
const IS_LLP64: bool = size_of::<*const c_void>() == 8 && size_of::<c_ulong>() == 4;

// The two models are mutually exclusive: `c_ulong` cannot be both widths.
const _: () = assert!(!(IS_LP64 && IS_LLP64));

// The exact numbers measured from the C build on x86_64-unknown-linux-gnu, asserted only where the
// integer model matches.
const _: () = assert!(!IS_LP64 || size_of::<z_stream>() == 112);
const _: () = assert!(!IS_LP64 || align_of::<z_stream>() == 8);
const _: () = assert!(!IS_LP64 || offset_of!(z_stream, avail_in) == 8);
const _: () = assert!(!IS_LP64 || offset_of!(z_stream, total_in) == 16);
const _: () = assert!(!IS_LP64 || offset_of!(z_stream, next_out) == 24);
const _: () = assert!(!IS_LP64 || offset_of!(z_stream, avail_out) == 32);
const _: () = assert!(!IS_LP64 || offset_of!(z_stream, total_out) == 40);
const _: () = assert!(!IS_LP64 || offset_of!(z_stream, msg) == 48);
const _: () = assert!(!IS_LP64 || offset_of!(z_stream, state) == 56);
const _: () = assert!(!IS_LP64 || offset_of!(z_stream, zalloc) == 64);
const _: () = assert!(!IS_LP64 || offset_of!(z_stream, zfree) == 72);
const _: () = assert!(!IS_LP64 || offset_of!(z_stream, opaque) == 80);
const _: () = assert!(!IS_LP64 || offset_of!(z_stream, data_type) == 88);
const _: () = assert!(!IS_LP64 || offset_of!(z_stream, adler) == 96);
const _: () = assert!(!IS_LP64 || offset_of!(z_stream, reserved) == 104);

// The same struct under LLP64. `avail_in`+`total_in` now share the eight bytes LP64 spends on
// `avail_in` alone, and the same happens at `avail_out`/`total_out` and at
// `data_type`/`adler`/`reserved`, so the struct is 88 bytes with four of tail padding.
const _: () = assert!(!IS_LLP64 || size_of::<z_stream>() == 88);
const _: () = assert!(!IS_LLP64 || align_of::<z_stream>() == 8);
const _: () = assert!(!IS_LLP64 || offset_of!(z_stream, next_in) == 0);
const _: () = assert!(!IS_LLP64 || offset_of!(z_stream, avail_in) == 8);
const _: () = assert!(!IS_LLP64 || offset_of!(z_stream, total_in) == 12);
const _: () = assert!(!IS_LLP64 || offset_of!(z_stream, next_out) == 16);
const _: () = assert!(!IS_LLP64 || offset_of!(z_stream, avail_out) == 24);
const _: () = assert!(!IS_LLP64 || offset_of!(z_stream, total_out) == 28);
const _: () = assert!(!IS_LLP64 || offset_of!(z_stream, msg) == 32);
const _: () = assert!(!IS_LLP64 || offset_of!(z_stream, state) == 40);
const _: () = assert!(!IS_LLP64 || offset_of!(z_stream, zalloc) == 48);
const _: () = assert!(!IS_LLP64 || offset_of!(z_stream, zfree) == 56);
const _: () = assert!(!IS_LLP64 || offset_of!(z_stream, opaque) == 64);
const _: () = assert!(!IS_LLP64 || offset_of!(z_stream, data_type) == 72);
const _: () = assert!(!IS_LLP64 || offset_of!(z_stream, adler) == 76);
const _: () = assert!(!IS_LLP64 || offset_of!(z_stream, reserved) == 80);

// `gz_header`, field by field. The absolute numbers matter here for the same reason they matter for
// `z_stream`: `inflateGetHeader` writes through a caller-allocated one, so a single wrong offset
// means the reference and the port disagree about where `extra_max` lives and the differential test
// silently compares the wrong bytes. `crates/libz-rs-sys/src/layout_assertions.rs` asserts the same
// numbers against the facade's mirror; the two files are checked against each other by construction.
const _: () = assert!(!IS_LP64 || size_of::<gz_header>() == 80);
const _: () = assert!(!IS_LP64 || align_of::<gz_header>() == 8);
const _: () = assert!(!IS_LP64 || offset_of!(gz_header, text) == 0);
const _: () = assert!(!IS_LP64 || offset_of!(gz_header, time) == 8);
const _: () = assert!(!IS_LP64 || offset_of!(gz_header, xflags) == 16);
const _: () = assert!(!IS_LP64 || offset_of!(gz_header, os) == 20);
const _: () = assert!(!IS_LP64 || offset_of!(gz_header, extra) == 24);
const _: () = assert!(!IS_LP64 || offset_of!(gz_header, extra_len) == 32);
const _: () = assert!(!IS_LP64 || offset_of!(gz_header, extra_max) == 36);
const _: () = assert!(!IS_LP64 || offset_of!(gz_header, name) == 40);
const _: () = assert!(!IS_LP64 || offset_of!(gz_header, name_max) == 48);
const _: () = assert!(!IS_LP64 || offset_of!(gz_header, comment) == 56);
const _: () = assert!(!IS_LP64 || offset_of!(gz_header, comm_max) == 64);
const _: () = assert!(!IS_LP64 || offset_of!(gz_header, hcrc) == 68);
const _: () = assert!(!IS_LP64 || offset_of!(gz_header, done) == 72);

// The same struct under LLP64. `time` is the only `unsigned long` here, so it shrinks to four bytes
// and packs against `text` with no padding, moving everything up to `extra` down by eight and
// settling the struct at 72 bytes.
const _: () = assert!(!IS_LLP64 || size_of::<gz_header>() == 72);
const _: () = assert!(!IS_LLP64 || align_of::<gz_header>() == 8);
const _: () = assert!(!IS_LLP64 || offset_of!(gz_header, text) == 0);
const _: () = assert!(!IS_LLP64 || offset_of!(gz_header, time) == 4);
const _: () = assert!(!IS_LLP64 || offset_of!(gz_header, xflags) == 8);
const _: () = assert!(!IS_LLP64 || offset_of!(gz_header, os) == 12);
const _: () = assert!(!IS_LLP64 || offset_of!(gz_header, extra) == 16);
const _: () = assert!(!IS_LLP64 || offset_of!(gz_header, extra_len) == 24);
const _: () = assert!(!IS_LLP64 || offset_of!(gz_header, extra_max) == 28);
const _: () = assert!(!IS_LLP64 || offset_of!(gz_header, name) == 32);
const _: () = assert!(!IS_LLP64 || offset_of!(gz_header, name_max) == 40);
const _: () = assert!(!IS_LLP64 || offset_of!(gz_header, comment) == 48);
const _: () = assert!(!IS_LLP64 || offset_of!(gz_header, comm_max) == 56);
const _: () = assert!(!IS_LLP64 || offset_of!(gz_header, hcrc) == 60);
const _: () = assert!(!IS_LLP64 || offset_of!(gz_header, done) == 64);

// `struct gzFile_s`, the single most load-bearing layout in the whole port: `zlib.h` defines
// `gzgetc(g)` as a macro that decrements `g->have`, post-increments `g->next` and increments
// `g->pos` inside *caller-compiled* object code, which this port cannot recompile. `gzguts.h` embeds
// this struct as `gz_state`'s first member so those three fields sit at 0, 8 and 16 of the whole
// state. Any other layout is silent memory corruption in every caller that uses the macro.
const _: () = assert!(!IS_LP64 || size_of::<gzFile_s>() == 24);
const _: () = assert!(!IS_LP64 || align_of::<gzFile_s>() == 8);
const _: () = assert!(!IS_LP64 || offset_of!(gzFile_s, next) == 8);
const _: () = assert!(!IS_LP64 || offset_of!(gzFile_s, pos) == 16);

// Unchanged under LLP64, which is the finding rather than a duplicated line: this struct holds no
// `unsigned long`, so the only model-dependent quantity is the pointer width and both 64-bit models
// agree on it. Asserted anyway, because `gzgetc` does this arithmetic in caller-compiled code.
const _: () = assert!(!IS_LLP64 || size_of::<gzFile_s>() == 24);
const _: () = assert!(!IS_LLP64 || align_of::<gzFile_s>() == 8);
const _: () = assert!(!IS_LLP64 || offset_of!(gzFile_s, have) == 0);
const _: () = assert!(!IS_LLP64 || offset_of!(gzFile_s, next) == 8);
const _: () = assert!(!IS_LLP64 || offset_of!(gzFile_s, pos) == 16);

// The scalar widths measured from the C build, asserted only under the integer model they were
// measured under. `uLong` is the one that matters most: it is `unsigned long`, so it is 8 bytes here
// and 4 on LLP64 Windows, and it is the return type of every checksum entry point.
const _: () = assert!(!IS_LP64 || size_of::<uInt>() == 4);
const _: () = assert!(!IS_LP64 || size_of::<uLong>() == 8);
const _: () = assert!(!IS_LP64 || size_of::<voidpf>() == 8);
const _: () = assert!(!IS_LP64 || size_of::<z_off_t>() == 8);
const _: () = assert!(!IS_LP64 || size_of::<z_size_t>() == 8);

// And under LLP64, where `uLong` is four bytes -- the difference that makes every checksum entry
// point return a narrower value on Windows and the reason `uLong` must be `c_ulong` rather than a
// fixed-width integer.
//
// `z_off_t` is asserted here as well, and the two declarations above are why it can be: on a
// non-`unix` target this mirror declares it `i64`, matching the header's `long long` fallback at
// `zconf.h` L518-L520 (`Z_HAVE_UNISTD_H` is never set on `_WIN32`). So on LLP64 it is eight bytes,
// unlike `uLong`, and the pair of assertions records that the two types diverge there -- which is
// the whole hazard the `*64` symbol family exists to manage.
const _: () = assert!(!IS_LLP64 || size_of::<uInt>() == 4);
const _: () = assert!(!IS_LLP64 || size_of::<uLong>() == 4);
const _: () = assert!(!IS_LLP64 || size_of::<voidpf>() == 8);
const _: () = assert!(!IS_LLP64 || size_of::<z_size_t>() == 8);
const _: () = assert!(!IS_LLP64 || size_of::<z_off_t>() == 8);
const _: () = assert!(!IS_LLP64 || size_of::<z_off64_t>() == 8);

// =============================================================================
//  The public zlib.h surface
// =============================================================================
//
//  95 prototypes.  That figure is `zlib.h`'s 96 real entry points, less the two variadic ones --
//  `gzprintf` and `gzvprintf`, whose `...` and `va_list` parameters have no portable stable-Rust
//  spelling (see the module docs) -- plus one symbol that is not in `zlib.h` at all: the hidden
//  `inflate_table`, declared here because `tests` of the ported decode-table builder need something
//  to compare against.  Each doc line is the C prototype verbatim, so this block can be read
//  against the header line by line.
//
//  Two entries deserve a note.  `gzopen_w` is declared only under `_WIN32` in `zlib.h` and is
//  therefore compiled into the oracle only there, so its declaration is gated the same way.
//  `gzgetc` reaches the archive through the `objcopy` rename rather than the `-D` rename, because
//  `gzread.c` undefines the macro before defining the function and no command-line define can
//  survive that; the resulting symbol is `c_gzgetc` all the same.

// SAFETY: every declaration in this block asserts that its Rust signature is the exact ABI
// equivalent of the corresponding `zlib.h` prototype -- same parameter types in the same order, same
// return type, same C calling convention. That assertion is the invariant, and it is load-bearing
// because nothing checks it: a mismatched declaration is not a compile error but silent undefined
// behaviour at the first call, since the compiler will happily marshal arguments into the wrong
// registers. Three things hold it up. Each declaration carries the C prototype verbatim in its doc
// comment, so the block can be diffed against the header line by line. Every type is spelled through
// the aliases above, which resolve to `core::ffi` types rather than fixed-width integers, so
// `uLong` follows `unsigned long` across integer models instead of being frozen at one width. And
// the compile-time layout assertions above fail the build if any mirrored struct drifts.
//
// `extern "C"` is used throughout and never `extern "C-unwind"`: the reference
// implementation is C and does not unwind, and pinning the convention to the non-unwinding form
// means a panic crossing this boundary aborts rather than becoming undefined behaviour.
//
// Calling any of these is `unsafe` for the ordinary FFI reasons, which no declaration can discharge
// on the caller's behalf: pointers must be non-null, aligned and valid for the length passed
// alongside them; `*const c_char` arguments must be NUL-terminated; a `z_streamp` must have been
// initialised by the matching `c_*Init*_` before any other entry point touches it, and must not be
// used after its `c_*End`; and returned `*const c_char` values point at storage owned by the library
// or by the stream, so they are only valid while that owner is. Wherever a returned pointer's
// lifetime is narrower than `'static` the individual doc comment says so.
extern "C" {
    /// `uLong adler32(uLong adler, const Bytef *buf, uInt len);`
    pub fn c_adler32(adler: uLong, buf: *const Bytef, len: uInt) -> uLong;

    /// `uLong adler32_combine(uLong adler1, uLong adler2, z_off_t len2);`
    pub fn c_adler32_combine(adler1: uLong, adler2: uLong, len2: z_off_t) -> uLong;

    /// `uLong adler32_combine64(uLong, uLong, z_off64_t);`
    pub fn c_adler32_combine64(adler1: uLong, adler2: uLong, len2: z_off64_t) -> uLong;

    /// `uLong adler32_z(uLong adler, const Bytef *buf, z_size_t len);`
    pub fn c_adler32_z(adler: uLong, buf: *const Bytef, len: z_size_t) -> uLong;

    /// `int compress(Bytef *dest, uLongf *destLen, const Bytef *source, uLong sourceLen);`
    pub fn c_compress(
        dest: *mut Bytef,
        destLen: *mut uLongf,
        source: *const Bytef,
        sourceLen: uLong,
    ) -> c_int;

    /// `int compress2(Bytef *dest, uLongf *destLen, const Bytef *source, uLong sourceLen, int level);`
    pub fn c_compress2(
        dest: *mut Bytef,
        destLen: *mut uLongf,
        source: *const Bytef,
        sourceLen: uLong,
        level: c_int,
    ) -> c_int;

    /// `int compress2_z(Bytef *dest, z_size_t *destLen, const Bytef *source, z_size_t sourceLen, int level);`
    pub fn c_compress2_z(
        dest: *mut Bytef,
        destLen: *mut z_size_t,
        source: *const Bytef,
        sourceLen: z_size_t,
        level: c_int,
    ) -> c_int;

    /// `uLong compressBound(uLong sourceLen);`
    pub fn c_compressBound(sourceLen: uLong) -> uLong;

    /// `z_size_t compressBound_z(z_size_t sourceLen);`
    pub fn c_compressBound_z(sourceLen: z_size_t) -> z_size_t;

    /// `int compress_z(Bytef *dest, z_size_t *destLen, const Bytef *source, z_size_t sourceLen);`
    pub fn c_compress_z(
        dest: *mut Bytef,
        destLen: *mut z_size_t,
        source: *const Bytef,
        sourceLen: z_size_t,
    ) -> c_int;

    /// `uLong crc32(uLong crc, const Bytef *buf, uInt len);`
    pub fn c_crc32(crc: uLong, buf: *const Bytef, len: uInt) -> uLong;

    /// `uLong crc32_combine(uLong crc1, uLong crc2, z_off_t len2);`
    pub fn c_crc32_combine(crc1: uLong, crc2: uLong, len2: z_off_t) -> uLong;

    /// `uLong crc32_combine64(uLong, uLong, z_off64_t);`
    pub fn c_crc32_combine64(crc1: uLong, crc2: uLong, len2: z_off64_t) -> uLong;

    /// `uLong crc32_combine_gen(z_off_t len2);`
    pub fn c_crc32_combine_gen(len2: z_off_t) -> uLong;

    /// `uLong crc32_combine_gen64(z_off64_t);`
    pub fn c_crc32_combine_gen64(len2: z_off64_t) -> uLong;

    /// `uLong crc32_combine_op(uLong crc1, uLong crc2, uLong op);`
    pub fn c_crc32_combine_op(crc1: uLong, crc2: uLong, op: uLong) -> uLong;

    /// `uLong crc32_z(uLong crc, const Bytef *buf, z_size_t len);`
    pub fn c_crc32_z(crc: uLong, buf: *const Bytef, len: z_size_t) -> uLong;

    /// `int deflate(z_streamp strm, int flush);`
    pub fn c_deflate(strm: z_streamp, flush: c_int) -> c_int;

    /// `uLong deflateBound(z_streamp strm, uLong sourceLen);`
    pub fn c_deflateBound(strm: z_streamp, sourceLen: uLong) -> uLong;

    /// `z_size_t deflateBound_z(z_streamp strm, z_size_t sourceLen);`
    pub fn c_deflateBound_z(strm: z_streamp, sourceLen: z_size_t) -> z_size_t;

    /// `int deflateCopy(z_streamp dest, z_streamp source);`
    pub fn c_deflateCopy(dest: z_streamp, source: z_streamp) -> c_int;

    /// `int deflateEnd(z_streamp strm);`
    pub fn c_deflateEnd(strm: z_streamp) -> c_int;

    /// `int deflateGetDictionary(z_streamp strm, Bytef *dictionary, uInt *dictLength);`
    pub fn c_deflateGetDictionary(
        strm: z_streamp,
        dictionary: *mut Bytef,
        dictLength: *mut uInt,
    ) -> c_int;

    /// `int deflateInit2_(z_streamp strm, int level, int method, int windowBits, int memLevel, int strategy, const char *version, int stream_size);`
    pub fn c_deflateInit2_(
        strm: z_streamp,
        level: c_int,
        method: c_int,
        windowBits: c_int,
        memLevel: c_int,
        strategy: c_int,
        version: *const c_char,
        stream_size: c_int,
    ) -> c_int;

    /// `int deflateInit_(z_streamp strm, int level, const char *version, int stream_size);`
    pub fn c_deflateInit_(
        strm: z_streamp,
        level: c_int,
        version: *const c_char,
        stream_size: c_int,
    ) -> c_int;

    /// `int deflateParams(z_streamp strm, int level, int strategy);`
    pub fn c_deflateParams(strm: z_streamp, level: c_int, strategy: c_int) -> c_int;

    /// `int deflatePending(z_streamp strm, unsigned *pending, int *bits);`
    pub fn c_deflatePending(strm: z_streamp, pending: *mut c_uint, bits: *mut c_int) -> c_int;

    /// `int deflatePrime(z_streamp strm, int bits, int value);`
    pub fn c_deflatePrime(strm: z_streamp, bits: c_int, value: c_int) -> c_int;

    /// `int deflateReset(z_streamp strm);`
    pub fn c_deflateReset(strm: z_streamp) -> c_int;

    /// `int deflateResetKeep(z_streamp);`
    pub fn c_deflateResetKeep(strm: z_streamp) -> c_int;

    /// `int deflateSetDictionary(z_streamp strm, const Bytef *dictionary, uInt dictLength);`
    pub fn c_deflateSetDictionary(
        strm: z_streamp,
        dictionary: *const Bytef,
        dictLength: uInt,
    ) -> c_int;

    /// `int deflateSetHeader(z_streamp strm, gz_headerp head);`
    pub fn c_deflateSetHeader(strm: z_streamp, head: gz_headerp) -> c_int;

    /// `int deflateTune(z_streamp strm, int good_length, int max_lazy, int nice_length, int max_chain);`
    pub fn c_deflateTune(
        strm: z_streamp,
        good_length: c_int,
        max_lazy: c_int,
        nice_length: c_int,
        max_chain: c_int,
    ) -> c_int;

    /// `int deflateUsed(z_streamp strm, int *bits);`
    pub fn c_deflateUsed(strm: z_streamp, bits: *mut c_int) -> c_int;

    /// `const z_crc_t FAR * get_crc_table(void);`
    pub fn c_get_crc_table() -> *const z_crc_t;

    /// `int gzbuffer(gzFile file, unsigned size);`
    pub fn c_gzbuffer(file: gzFile, size: c_uint) -> c_int;

    /// `void gzclearerr(gzFile file);`
    pub fn c_gzclearerr(file: gzFile);

    /// `int gzclose(gzFile file);`
    pub fn c_gzclose(file: gzFile) -> c_int;

    /// `int gzclose_r(gzFile file);`
    pub fn c_gzclose_r(file: gzFile) -> c_int;

    /// `int gzclose_w(gzFile file);`
    pub fn c_gzclose_w(file: gzFile) -> c_int;

    /// `int gzdirect(gzFile file);`
    pub fn c_gzdirect(file: gzFile) -> c_int;

    /// `gzFile gzdopen(int fd, const char *mode);`
    pub fn c_gzdopen(fd: c_int, mode: *const c_char) -> gzFile;

    /// `int gzeof(gzFile file);`
    pub fn c_gzeof(file: gzFile) -> c_int;

    /// `const char * gzerror(gzFile file, int *errnum);`
    pub fn c_gzerror(file: gzFile, errnum: *mut c_int) -> *const c_char;

    /// `int gzflush(gzFile file, int flush);`
    pub fn c_gzflush(file: gzFile, flush: c_int) -> c_int;

    /// `z_size_t gzfread(voidp buf, z_size_t size, z_size_t nitems, gzFile file);`
    pub fn c_gzfread(buf: voidp, size: z_size_t, nitems: z_size_t, file: gzFile) -> z_size_t;

    /// `z_size_t gzfwrite(voidpc buf, z_size_t size, z_size_t nitems, gzFile file);`
    pub fn c_gzfwrite(buf: voidpc, size: z_size_t, nitems: z_size_t, file: gzFile) -> z_size_t;

    /// `int gzgetc(gzFile file);`
    pub fn c_gzgetc(file: gzFile) -> c_int;

    /// `int gzgetc_(gzFile file);`
    pub fn c_gzgetc_(file: gzFile) -> c_int;

    /// `char * gzgets(gzFile file, char *buf, int len);`
    pub fn c_gzgets(file: gzFile, buf: *mut c_char, len: c_int) -> *mut c_char;

    /// `z_off_t gzoffset(gzFile file);`
    pub fn c_gzoffset(file: gzFile) -> z_off_t;

    /// `z_off64_t gzoffset64(gzFile);`
    pub fn c_gzoffset64(file: gzFile) -> z_off64_t;

    /// `gzFile gzopen(const char *path, const char *mode);`
    pub fn c_gzopen(path: *const c_char, mode: *const c_char) -> gzFile;

    /// `gzFile gzopen64(const char *, const char *);`
    pub fn c_gzopen64(path: *const c_char, mode: *const c_char) -> gzFile;

    /// `gzFile gzopen_w(const wchar_t *path, const char *mode);`
    #[cfg(windows)]
    pub fn c_gzopen_w(path: *const wchar_t, mode: *const c_char) -> gzFile;

    // `gzprintf` and `gzvprintf` are intentionally absent from this block. Both are `ZEXPORTVA`
    // rather than `ZEXPORT`, which is a calling-convention distinction and not a stylistic one, and
    // `gzvprintf`'s `va_list` has no stable portable spelling. The module docs give the full
    // reasoning; the short version is that a plain `extern "C"` declaration for either would be an
    // ABI hazard on at least one supported configuration, and neither is needed to measure
    // byte-identity.

    /// `int gzputc(gzFile file, int c);`
    pub fn c_gzputc(file: gzFile, c: c_int) -> c_int;

    /// `int gzputs(gzFile file, const char *s);`
    pub fn c_gzputs(file: gzFile, s: *const c_char) -> c_int;

    /// `int gzread(gzFile file, voidp buf, unsigned len);`
    pub fn c_gzread(file: gzFile, buf: voidp, len: c_uint) -> c_int;

    /// `int gzrewind(gzFile file);`
    pub fn c_gzrewind(file: gzFile) -> c_int;

    /// `z_off_t gzseek(gzFile file, z_off_t offset, int whence);`
    pub fn c_gzseek(file: gzFile, offset: z_off_t, whence: c_int) -> z_off_t;

    /// `z_off64_t gzseek64(gzFile, z_off64_t, int);`
    pub fn c_gzseek64(file: gzFile, offset: z_off64_t, whence: c_int) -> z_off64_t;

    /// `int gzsetparams(gzFile file, int level, int strategy);`
    pub fn c_gzsetparams(file: gzFile, level: c_int, strategy: c_int) -> c_int;

    /// `z_off_t gztell(gzFile file);`
    pub fn c_gztell(file: gzFile) -> z_off_t;

    /// `z_off64_t gztell64(gzFile);`
    pub fn c_gztell64(file: gzFile) -> z_off64_t;

    /// `int gzungetc(int c, gzFile file);`
    pub fn c_gzungetc(c: c_int, file: gzFile) -> c_int;

    /// `int gzwrite(gzFile file, voidpc buf, unsigned len);`
    pub fn c_gzwrite(file: gzFile, buf: voidpc, len: c_uint) -> c_int;

    /// `int inflate(z_streamp strm, int flush);`
    pub fn c_inflate(strm: z_streamp, flush: c_int) -> c_int;

    /// `int inflateBack(z_streamp strm, in_func in, void FAR *in_desc, out_func out, void FAR *out_desc);`
    pub fn c_inflateBack(
        strm: z_streamp,
        in_: in_func,
        in_desc: *mut c_void,
        out: out_func,
        out_desc: *mut c_void,
    ) -> c_int;

    /// `int inflateBackEnd(z_streamp strm);`
    pub fn c_inflateBackEnd(strm: z_streamp) -> c_int;

    /// `int inflateBackInit_(z_streamp strm, int windowBits, unsigned char FAR *window, const char *version, int stream_size);`
    pub fn c_inflateBackInit_(
        strm: z_streamp,
        windowBits: c_int,
        window: *mut c_uchar,
        version: *const c_char,
        stream_size: c_int,
    ) -> c_int;

    /// `unsigned long inflateCodesUsed(z_streamp);`
    pub fn c_inflateCodesUsed(strm: z_streamp) -> c_ulong;

    /// `int inflateCopy(z_streamp dest, z_streamp source);`
    pub fn c_inflateCopy(dest: z_streamp, source: z_streamp) -> c_int;

    /// `int inflateEnd(z_streamp strm);`
    pub fn c_inflateEnd(strm: z_streamp) -> c_int;

    /// `int inflateGetDictionary(z_streamp strm, Bytef *dictionary, uInt *dictLength);`
    pub fn c_inflateGetDictionary(
        strm: z_streamp,
        dictionary: *mut Bytef,
        dictLength: *mut uInt,
    ) -> c_int;

    /// `int inflateGetHeader(z_streamp strm, gz_headerp head);`
    pub fn c_inflateGetHeader(strm: z_streamp, head: gz_headerp) -> c_int;

    /// `int inflateInit2_(z_streamp strm, int windowBits, const char *version, int stream_size);`
    pub fn c_inflateInit2_(
        strm: z_streamp,
        windowBits: c_int,
        version: *const c_char,
        stream_size: c_int,
    ) -> c_int;

    /// `int inflateInit_(z_streamp strm, const char *version, int stream_size);`
    pub fn c_inflateInit_(strm: z_streamp, version: *const c_char, stream_size: c_int) -> c_int;

    /// `long inflateMark(z_streamp strm);`
    pub fn c_inflateMark(strm: z_streamp) -> c_long;

    /// `int inflatePrime(z_streamp strm, int bits, int value);`
    pub fn c_inflatePrime(strm: z_streamp, bits: c_int, value: c_int) -> c_int;

    /// `int inflateReset(z_streamp strm);`
    pub fn c_inflateReset(strm: z_streamp) -> c_int;

    /// `int inflateReset2(z_streamp strm, int windowBits);`
    pub fn c_inflateReset2(strm: z_streamp, windowBits: c_int) -> c_int;

    /// `int inflateResetKeep(z_streamp);`
    pub fn c_inflateResetKeep(strm: z_streamp) -> c_int;

    /// `int inflateSetDictionary(z_streamp strm, const Bytef *dictionary, uInt dictLength);`
    pub fn c_inflateSetDictionary(
        strm: z_streamp,
        dictionary: *const Bytef,
        dictLength: uInt,
    ) -> c_int;

    /// `int inflateSync(z_streamp strm);`
    pub fn c_inflateSync(strm: z_streamp) -> c_int;

    /// `int inflateSyncPoint(z_streamp);`
    pub fn c_inflateSyncPoint(strm: z_streamp) -> c_int;

    /// `int inflateUndermine(z_streamp, int);`
    pub fn c_inflateUndermine(strm: z_streamp, subvert: c_int) -> c_int;

    /// `int inflateValidate(z_streamp, int);`
    pub fn c_inflateValidate(strm: z_streamp, check: c_int) -> c_int;

    /// `int uncompress(Bytef *dest, uLongf *destLen, const Bytef *source, uLong sourceLen);`
    pub fn c_uncompress(
        dest: *mut Bytef,
        destLen: *mut uLongf,
        source: *const Bytef,
        sourceLen: uLong,
    ) -> c_int;

    /// `int uncompress2(Bytef *dest, uLongf *destLen, const Bytef *source, uLong *sourceLen);`
    pub fn c_uncompress2(
        dest: *mut Bytef,
        destLen: *mut uLongf,
        source: *const Bytef,
        sourceLen: *mut uLong,
    ) -> c_int;

    /// `int uncompress2_z(Bytef *dest, z_size_t *destLen, const Bytef *source, z_size_t *sourceLen);`
    pub fn c_uncompress2_z(
        dest: *mut Bytef,
        destLen: *mut z_size_t,
        source: *const Bytef,
        sourceLen: *mut z_size_t,
    ) -> c_int;

    /// `int uncompress_z(Bytef *dest, z_size_t *destLen, const Bytef *source, z_size_t sourceLen);`
    pub fn c_uncompress_z(
        dest: *mut Bytef,
        destLen: *mut z_size_t,
        source: *const Bytef,
        sourceLen: z_size_t,
    ) -> c_int;

    /// `const char * zError(int);`
    pub fn c_zError(err: c_int) -> *const c_char;

    /// `uLong zlibCompileFlags(void);`
    pub fn c_zlibCompileFlags() -> uLong;

    /// `const char * zlibVersion(void);`
    pub fn c_zlibVersion() -> *const c_char;
}

// =============================================================================
//  Internal surface (inftrees.h)
// =============================================================================
//
//  `inflate_table` is `ZLIB_INTERNAL` and appears in `zlib.map`'s `local:` block, so it is hidden in
//  the shipped shared library and the port's counterpart is `pub(crate)` and unreachable from here.
//  It reaches this archive through `build.rs`'s `objcopy --redefine-syms` pass, which is what makes
//  the reference's table builder callable directly -- and therefore what lets a test compare decoding
//  tables against the reference rather than only comparing decoded output.

// SAFETY: the same signature-fidelity invariant as the public block above; this prototype is
// transcribed from `inftrees.h`. The additional obligations specific to this entry point are the
// reference's own documented preconditions: `lens` must be valid for `codes` elements; `work` must be
// valid for `codes` elements and is used as scratch; `bits` is in/out and must be a valid pointer;
// and `*table` must have room for the reference's worst case -- [`ENOUGH_LENS`] entries for
// [`codetype::LENS`], [`ENOUGH_DISTS`] for [`codetype::DISTS`], and 19 for [`codetype::CODES`] -- with
// `*table` advanced by the number of entries consumed on return. Supplying less is a buffer overflow
// inside the C code, which no Rust-side check can prevent.
extern "C" {
    /// `int inflate_table(codetype type, unsigned short FAR *lens, unsigned codes,
    /// code FAR * FAR *table, unsigned FAR *bits, unsigned short FAR *work);`
    ///
    /// `type` is the [`codetype`] newtype, which reproduces the C enum's name and ABI exactly; its
    /// three associated constants are the enumerators. See [`codetype`] for why the parameter is not
    /// spelled `c_int` and not spelled as a Rust `enum`.
    pub fn c_inflate_table(
        type_: codetype,
        lens: *mut u16,
        codes: c_uint,
        table: *mut *mut code,
        bits: *mut c_uint,
        work: *mut u16,
    ) -> c_int;
}

// =============================================================================
//  The generated table shim
// =============================================================================
//
//  Every generated table in the C implementation is `static`, because `zutil.h` defines `local` as
//  `static` and the tables are declared with it -- `crc_table` and its siblings in `crc32.h`,
//  `static_ltree` and `static_dtree` in `trees.h`, `lenfix` and `distfix` in `inffixed.h`.  There
//  is no symbol for a table-equality test to bind to, so `build.rs` generates a small C file that
//  includes those three headers and hands the addresses out through the accessors below, compiled
//  with the same flags and archived alongside the reference objects.
//
//  Every array accessor is paired with a length accessor so that the Rust side never hardcodes a
//  bound.  The braided-CRC accessors are the ones to be careful with: `crc32.h` selects its
//  variants behind `#if`, and when the braided path is compiled out the shim's `#else` branch
//  returns a null pointer and a zero length rather than failing to compile.  A caller must treat
//  null as a legal answer meaning "this target has no braided table", not as an error.
//
//  One table is out of that shim's reach and is handled by a second generated file; see the
//  configuration-table section further down.
//
//  Prefer the safe wrappers in the section after this one.  They pair each pointer with its own
//  length accessor and hand back a `&'static [T]`, so a table comparison needs no `unsafe` of its
//  own.  That is what the wrappers are for; no such comparison exists in the tree yet.

// SAFETY: the signature-fidelity invariant again, against the generated shim in
// `build.rs`'s `TABLE_SHIM_SOURCE` rather than against a checked-in header. Two properties of that
// shim are what make these declarations sound. Every accessor is a niladic function returning either
// a pointer or an `unsigned`, so there are no argument-marshalling concerns at all. And every length
// is computed by the shim as `sizeof(array) / sizeof(array[0])` in the same translation unit that
// includes the table header, so a length can never disagree with the array it describes even if the
// C headers are regenerated with different dimensions.
//
// Calling an accessor is sound for any input, because there is no input; the obligations land on what
// is done with the result. Each returned pointer addresses a `static const` array with a lifetime of
// the whole program, so `'static` is honest, and the memory is read-only, so no `&mut` may ever be
// formed from it. A pointer may legitimately be null -- the `#else` branch of the shim returns null
// with a zero length when the braided path is compiled out -- so it must be null-checked before use
// rather than assumed. The wrappers below discharge exactly these obligations.
extern "C" {
    /// Returns the `distfix` from `inffixed.h`.
    pub fn c_oracle_distfix() -> *const code;

    /// Returns the `lenfix` from `inffixed.h`.
    pub fn c_oracle_lenfix() -> *const code;

    /// Returns the `static_dtree` from `trees.h`.
    pub fn c_oracle_static_dtree() -> *const ct_data;

    /// Returns the `static_ltree` from `trees.h`.
    pub fn c_oracle_static_ltree() -> *const ct_data;

    /// Returns the `base_dist` from `trees.h`.
    pub fn c_oracle_base_dist() -> *const c_int;

    /// Returns the `base_length` from `trees.h`.
    pub fn c_oracle_base_length() -> *const c_int;

    /// Returns the `_dist_code` from `trees.h`, as the shim's own copy.
    pub fn c_oracle_dist_code() -> *const uch;

    /// Returns the `_length_code` from `trees.h`, as the shim's own copy.
    pub fn c_oracle_length_code() -> *const uch;

    /// Returns the `crc_big_table` from `crc32.h`, or null when the braided path is compiled out.
    pub fn c_oracle_crc_big_table() -> *const c_void;

    /// Returns the first element of `crc_braid_big_table`, or null.
    pub fn c_oracle_crc_braid_big_table() -> *const c_void;

    /// Returns the first element of `crc_braid_table`, or null.
    pub fn c_oracle_crc_braid_table() -> *const z_crc_t;

    /// Returns the `crc_table` from `crc32.h`.
    pub fn c_oracle_crc_table() -> *const z_crc_t;

    /// Returns the `x2n_table` from `crc32.h`.
    pub fn c_oracle_x2n_table() -> *const z_crc_t;

    /// Returns the element count of `base_dist`.
    pub fn c_oracle_base_dist_len() -> c_uint;

    /// Returns the element count of `base_length`.
    pub fn c_oracle_base_length_len() -> c_uint;

    /// Returns `sizeof(code)` as the C compiler laid it out.
    pub fn c_oracle_code_size() -> c_uint;

    /// Returns the element count of `crc_big_table`, or 0.
    pub fn c_oracle_crc_big_table_len() -> c_uint;

    /// Returns the column count of `crc_braid_big_table`, or 0.
    pub fn c_oracle_crc_braid_big_table_cols() -> c_uint;

    /// Returns the row count of `crc_braid_big_table`, or 0.
    pub fn c_oracle_crc_braid_big_table_rows() -> c_uint;

    /// Returns `crc32.c`'s braid width `N`, always defined.
    pub fn c_oracle_crc_braid_n() -> c_uint;

    /// Returns the column count of `crc_braid_table`, or 0.
    pub fn c_oracle_crc_braid_table_cols() -> c_uint;

    /// Returns the row count of `crc_braid_table`, or 0.
    pub fn c_oracle_crc_braid_table_rows() -> c_uint;

    /// Returns `crc32.c`'s word width `W`, or 0 when the braided path is compiled out.
    pub fn c_oracle_crc_braid_w() -> c_uint;

    /// Returns the element count of `crc_table`.
    pub fn c_oracle_crc_table_len() -> c_uint;

    /// Returns `sizeof(ct_data)` as the C compiler laid it out.
    pub fn c_oracle_ct_data_size() -> c_uint;

    /// Returns the element count of `_dist_code`.
    pub fn c_oracle_dist_code_len() -> c_uint;

    /// Returns the element count of `distfix`.
    pub fn c_oracle_distfix_len() -> c_uint;

    /// Returns the element count of `lenfix`.
    pub fn c_oracle_lenfix_len() -> c_uint;

    /// Returns the element count of `_length_code`.
    pub fn c_oracle_length_code_len() -> c_uint;

    /// Returns the element count of `static_dtree`.
    pub fn c_oracle_static_dtree_len() -> c_uint;

    /// Returns the element count of `static_ltree`.
    pub fn c_oracle_static_ltree_len() -> c_uint;

    /// Returns `sizeof(z_word_t)`, or 0 when the braided path is compiled out.
    pub fn c_oracle_word_size() -> c_uint;

    /// Returns the element count of `x2n_table`.
    pub fn c_oracle_x2n_table_len() -> c_uint;
}

// =============================================================================
//  The generated deflate.c wrapper: configuration_table
// =============================================================================
//
//  `configuration_table` -- the per-level tuning rows at `deflate.c` L112-L124 -- is `local` like
//  the other generated tables, but unlike them it is declared in no header at all: it lives inside
//  `deflate.c`.  So the table shim above cannot reach it, because there is nothing for it to
//  include, and no `extern "C"` declaration can bind to it, because a `static` object has no
//  symbol.  `build.rs` closes that gap with a second generated file, `zlib_c_oracle_deflate.c`,
//  which `#include`s the authoritative `deflate.c` and appends the accessors below; that file is
//  compiled *instead of* plain `deflate.c`, so the reference translation unit is still in the
//  archive exactly once and the accessors sit in the same translation unit as the table.
//
//  Why this table is worth a whole second generated file: its four numbers per level decide which
//  candidate matches `longest_match` examines and accepts, and therefore which literal, length and
//  distance symbols reach the Huffman coder.  A single altered digit does not make the encoder
//  better or worse in any way a decompressor could detect -- it makes it emit *different bytes*, at
//  that level, for essentially every input.  It is the first of the byte-identity decision points,
//  and the cheapest one to check.
//
//  The accessors are also why nothing here transcribes the forty numbers.  A shim that restated
//  them would let a table-equality test compare one transcription against another, and a value
//  copied wrongly would be copied wrongly into both sides.  These read the C object.
//
//  Prefer the safe wrappers in the section after the table views: [`oracle_config`] pairs a row
//  with [`oracle_config_table_len`] and hands back an [`OracleConfig`], so a test never indexes
//  past the end of a table whose length the C side owns.

// SAFETY: the signature-fidelity invariant again, against `build.rs`'s `CONFIG_TU_SOURCE` rather
// than against a checked-in header. Three properties of that generated file make these declarations
// sound. Every accessor takes either nothing or one `unsigned` by value and returns `unsigned` or
// `int`, so there are no pointers anywhere in this block and no argument-marshalling concerns at
// all -- a wrong value can be a wrong answer but never a memory error. The row count is computed as
// `sizeof(configuration_table) / sizeof(configuration_table[0])` in the same translation unit as the
// table, so it cannot disagree with the array it describes. And every field accessor range-checks
// its `level` against that same count before indexing, so no argument value can make the C side
// read past the end of the table: an out-of-range level yields 0 from a field accessor and -2 from
// `c_oracle_config_func`.
extern "C" {
    /// Returns the row count of `configuration_table` (`deflate.c` L112-L124).
    ///
    /// Ten in the default configuration. Two under `FASTEST`, which the oracle deliberately does
    /// not define -- so a value other than ten means the oracle was built as a *different encoder*
    /// and a byte-identity comparison against it would be meaningless.
    pub fn c_oracle_config_table_len() -> c_uint;

    /// Returns `sizeof` one numeric field of a `configuration_table` row, as the C compiler laid
    /// it out.
    ///
    /// `struct config_s` declares all four as `ush` (`deflate.c` L99-L102), so this is the C
    /// compiler's own answer for the width the port transcribes them at.
    pub fn c_oracle_config_field_size() -> c_uint;

    /// Returns `configuration_table[level].good_length`, or 0 when `level` is out of range.
    pub fn c_oracle_config_good_length(level: c_uint) -> c_uint;

    /// Returns `configuration_table[level].max_lazy`, or 0 when `level` is out of range.
    pub fn c_oracle_config_max_lazy(level: c_uint) -> c_uint;

    /// Returns `configuration_table[level].nice_length`, or 0 when `level` is out of range.
    pub fn c_oracle_config_nice_length(level: c_uint) -> c_uint;

    /// Returns `configuration_table[level].max_chain`, or 0 when `level` is out of range.
    pub fn c_oracle_config_max_chain(level: c_uint) -> c_uint;

    /// Returns which compressor `configuration_table[level].func` points at, as a discriminator.
    ///
    /// 0 `deflate_stored`, 1 `deflate_fast`, 2 `deflate_slow`, 3 `deflate_rle`, 4 `deflate_huff`;
    /// -1 if the stored address matches none of the five, and -2 if `level` is out of range.
    /// [`OracleCompressFunc`] is the safe reading of the result.
    ///
    /// The C side compares the function *address*, which is the same test `deflateParams` makes at
    /// `deflate.c` L791 when it decides whether a level change must first flush the open block.
    pub fn c_oracle_config_func(level: c_uint) -> c_int;
}

// =============================================================================
//  Safe views over the generated tables
// =============================================================================
//
//  One wrapper per table, each returning a `&'static [T]` built from the table's own pointer
//  accessor and its own length accessor.  This is the only place in the crate where a raw pointer is
//  turned into a slice, so a comparison of the ported Rust `const` arrays against the C arrays can
//  be written without a single `unsafe` block of its own.  Only the shape checks in the smoke module
//  call these today; the content comparison they were built for is not yet written.
//
//  Three properties make every one of these sound, and they are stated once here rather than
//  repeated in nine near-identical comments:
//
//  1. LIFETIME.  Each pointer addresses a `static const` array inside the linked oracle archive.
//     Its storage has the lifetime of the process, so `'static` is not a widening -- it is the true
//     lifetime.  The data is read-only and no wrapper hands out `&mut`, so no aliasing rule can be
//     broken by a caller holding one of these slices for as long as it likes.
//  2. LENGTH.  Each length comes from the companion accessor, which the shim implements as
//     `sizeof(array) / sizeof(array[0])` in the same translation unit that includes the table
//     header.  The bound therefore cannot drift from the array it describes, and no bound is
//     hardcoded on the Rust side.  Converting it with `as usize` is lossless: it is a C `unsigned`,
//     which is 32-bit on every supported target, and `usize` is at least 32-bit.
//  3. ELEMENT TYPE.  The Rust element type is layout-identical to the C one.  For `u32` and
//     `c_int` that is a primitive equivalence; for [`ct_data`] and [`code`] it is enforced by the
//     compile-time assertions above and independently reported at run time by
//     [`c_oracle_ct_data_size`] and [`c_oracle_code_size`], which the smoke module checks.
//
//  A null pointer is a legal answer, not an error: the shim's `#else` branch returns null with a
//  zero length when `crc32.h` compiled the braided path out.  Every wrapper therefore null-checks
//  and yields an empty slice, so a caller never has to reason about it and a test that iterates the
//  braid table on a non-braided target simply iterates nothing.

/// Builds a `&'static [T]` from an oracle table pointer and its companion length.
///
/// Returns an empty slice when `ptr` is null, which is the shim's documented way of saying "this
/// target does not have this table".
///
/// # Safety
///
/// The caller must guarantee that `ptr` is either null or a pointer to a `static` array of at least
/// `len` correctly initialised `T` values, living for the whole program and never mutated. Every
/// call site below satisfies this by pairing a shim pointer accessor with that same table's own
/// length accessor; see the section comment above for the full argument.
unsafe fn table_slice<'a, T>(ptr: *const T, len: c_uint) -> &'a [T] {
    if ptr.is_null() {
        return &[];
    }
    // SAFETY: `ptr` is non-null by the branch above, and the caller's contract guarantees it points
    // at a `static` array of at least `len` initialised `T`s that outlives every possible `'a` and is
    // never mutated. `len as usize` cannot truncate, because a C `unsigned` is 32-bit on every
    // supported target and `usize` is at least that wide. The total size cannot overflow `isize`
    // either, and that holds without appealing to any particular table's measured size. Every call
    // site derives `len` from the array's own `sizeof/sizeof`: directly for the one-dimensional
    // tables, and as the product of the row and column counts -- each its own `sizeof/sizeof` -- for
    // the two-dimensional braid table, which is the same element total. So `len * size_of::<T>()` is
    // exactly that array's `sizeof`, a size the C implementation had already laid out, and therefore
    // within `isize::MAX`. For scale, the largest of these tables is `crc_braid_table`, whose
    // declared geometry of at most 8 rows of 256 `z_crc_t` is 8 KiB.
    unsafe { core::slice::from_raw_parts(ptr, len as usize) }
}

/// The reference's `crc_table` from `crc32.h`, 256 entries of `z_crc_t`.
///
/// This is the byte-at-a-time CRC-32 table, and it is the cheapest high-signal check in the whole
/// port: it begins `0x00000000, 0x77073096, 0xee0e612c, 0x990951ba`, so a transcription error shows
/// up immediately.
///
/// Which copy this returns depends on how the oracle was built, and it is worth being precise
/// because `crc32.c` has two strategies. Under `DYNAMIC_CRC_TABLE` it generates the table at run
/// time behind a `z_once_t`; otherwise it `#include`s the committed `crc32.h`. This oracle is built
/// without that define, so the table returned here is the committed one -- the same data the port
/// transcribes into a `const` array. Comparing the two therefore checks the transcription, and the
/// smoke module pins the build choice by asserting bit 13 of `zlibCompileFlags` is clear, so a
/// switch to run-time generation shows up as a test failure rather than as a silent change in what
/// this function means.
#[must_use]
pub fn oracle_crc_table() -> &'static [z_crc_t] {
    // SAFETY: `c_oracle_crc_table` returns `crc_table`, a `static const` array in the oracle
    // archive, and `c_oracle_crc_table_len` is that same array's `sizeof/sizeof`. Both are niladic,
    // so the calls themselves have no preconditions.
    unsafe { table_slice(c_oracle_crc_table(), c_oracle_crc_table_len()) }
}

/// The reference's `x2n_table` from `crc32.h`, the powers of x mod p(x) used by the combine
/// operations.
///
/// `crc32.c` fills 32 entries, and `crc32_combine_gen`/`crc32_combine_op` read them, so this table
/// is what the combine family's correctness rests on.
#[must_use]
pub fn oracle_x2n_table() -> &'static [z_crc_t] {
    // SAFETY: `c_oracle_x2n_table` returns `x2n_table`, a `static const z_crc_t` array in the
    // oracle archive, paired with that same array's own `sizeof/sizeof` length. Static storage
    // duration makes the `'static` the true lifetime, the data is read-only and never handed out
    // as `&mut`, and `z_crc_t` is `u32` on both sides. Both accessors are niladic, so the calls
    // themselves have no preconditions.
    unsafe { table_slice(c_oracle_x2n_table(), c_oracle_x2n_table_len()) }
}

/// The reference's `crc_braid_table` from `crc32.h`, flattened row-major.
///
/// The C array is `crc_braid_table[W][256]`, and this returns all `W * 256` entries in one slice
/// because a `&[[u32; 256]]` would bake the column count into the Rust type when the C side decides
/// it behind `#if`. Index it exactly as `crc32.c` does -- `crc_braid_table[k][b]` becomes
/// `slice[k * cols + b]`, with `cols` from [`oracle_crc_braid_dimensions`]:
///
/// ```text
/// crc0 ^= crc_braid_table[k][(word0 >> (k << 3)) & 0xff];   // crc32.c
/// crc0 ^= table[k * cols + ((word0 >> (k << 3)) & 0xff)];   // the same access here
/// ```
///
/// Empty when the target compiled the braided path out, in which case
/// [`oracle_crc_braid_dimensions`] reports `(0, 0)` and there is nothing to compare.
#[must_use]
pub fn oracle_crc_braid_table() -> &'static [z_crc_t] {
    // SAFETY: `c_oracle_crc_braid_table` returns `&crc_braid_table[0][0]`, the first element of a
    // `static const` two-dimensional array. A C array is contiguous and row-major by definition, so
    // `rows * cols` elements are addressable from that pointer as one flat run. Both dimensions come
    // from the shim's own `sizeof/sizeof` on the array and on its first row, and `rows * cols` cannot
    // overflow: it is at most 8 * 256. When the braided path is compiled out the accessor returns
    // null and `table_slice` yields an empty slice.
    unsafe {
        let rows = c_oracle_crc_braid_table_rows();
        let cols = c_oracle_crc_braid_table_cols();
        table_slice(c_oracle_crc_braid_table(), rows.saturating_mul(cols))
    }
}

/// The braided-CRC geometry the oracle was actually compiled with: `(rows, cols)`.
///
/// `rows` is `crc32.c`'s word width `W` -- 8 where a 64-bit word type is available and 4 otherwise --
/// and `cols` is 256. Both are reported at run time and neither may be hardcoded: `crc32.h` holds a
/// separate table for every `(N, W)` pair and picks between them with `#if`, so assuming a width
/// would mean comparing against a table the C code did not compile.
///
/// `(0, 0)` means this target has no braided table.
#[must_use]
pub fn oracle_crc_braid_dimensions() -> (c_uint, c_uint) {
    // SAFETY: two niladic accessors returning `unsigned`; no pointers and no preconditions.
    unsafe {
        (
            c_oracle_crc_braid_table_rows(),
            c_oracle_crc_braid_table_cols(),
        )
    }
}

/// The reference's `static_ltree` from `trees.h`, `L_CODES + 2` = 288 entries.
///
/// The static literal/length tree, emitted whenever `_tr_flush_block` decides a static block is no
/// larger than a dynamic one. It begins `{{12},{8}}, {{140},{8}}, {{76},{8}}`, and because every
/// static block in the output stream is encoded with it, any discrepancy here changes emitted bytes
/// directly.
#[must_use]
pub fn oracle_static_ltree() -> &'static [ct_data] {
    // SAFETY: `c_oracle_static_ltree` returns `static_ltree`, a `static const ct_data` array, paired
    // with its own `sizeof/sizeof` length. `ct_data` is layout-identical to the C type: both fields
    // are `ush`, `size_of::<ct_data>() == 4` is asserted at compile time above, and
    // `c_oracle_ct_data_size` reports the C compiler's own answer for cross-checking.
    unsafe { table_slice(c_oracle_static_ltree(), c_oracle_static_ltree_len()) }
}

/// The reference's `static_dtree` from `trees.h`, `D_CODES` = 30 entries.
#[must_use]
pub fn oracle_static_dtree() -> &'static [ct_data] {
    // SAFETY: `c_oracle_static_dtree` returns `static_dtree`, a `static const ct_data` array in
    // the oracle archive, paired with that same array's own `sizeof/sizeof` length. Static storage
    // duration makes the `'static` the true lifetime, and the data is read-only. `ct_data` is
    // layout-identical to the C type: both fields are `ush`, `size_of::<ct_data>() == 4` is
    // asserted at compile time above, and `c_oracle_ct_data_size` reports the C compiler's own
    // answer for cross-checking. Both accessors are niladic.
    unsafe { table_slice(c_oracle_static_dtree(), c_oracle_static_dtree_len()) }
}

/// The reference's `base_length` from `trees.h`, `LENGTH_CODES` = 29 entries.
///
/// The first length for each length code; `compress_block` subtracts it to recover the extra bits to
/// emit, so it participates directly in the encoded bitstream.
#[must_use]
pub fn oracle_base_length() -> &'static [c_int] {
    // SAFETY: `c_oracle_base_length` returns `base_length`, a `static const int` array, paired with
    // its own `sizeof/sizeof` length. The element type is C `int`, spelled `c_int`.
    unsafe { table_slice(c_oracle_base_length(), c_oracle_base_length_len()) }
}

/// The reference's `base_dist` from `trees.h`, `D_CODES` = 30 entries.
#[must_use]
pub fn oracle_base_dist() -> &'static [c_int] {
    // SAFETY: `c_oracle_base_dist` returns `base_dist`, a `static const int` array in the oracle
    // archive, paired with that same array's own `sizeof/sizeof` length. Static storage duration
    // makes the `'static` the true lifetime, the data is read-only, and the element type is C
    // `int`, spelled `c_int`. Both accessors are niladic.
    unsafe { table_slice(c_oracle_base_dist(), c_oracle_base_dist_len()) }
}

/// The reference's `_dist_code` from `trees.h`, 512 entries.
///
/// Maps a match distance to its distance code. The C array is `ZLIB_INTERNAL` rather than `local`, so
/// `build.rs` renames the shim's own view of it to avoid colliding with the renamed global; the
/// contents are the same array either way.
#[must_use]
pub fn oracle_dist_code() -> &'static [uch] {
    // SAFETY: `c_oracle_dist_code` returns `_dist_code`, a `static const uch` array, paired with its
    // own `sizeof/sizeof` length. `uch` is `unsigned char`, one byte, asserted above.
    unsafe { table_slice(c_oracle_dist_code(), c_oracle_dist_code_len()) }
}

/// The reference's `_length_code` from `trees.h`, `MAX_MATCH - MIN_MATCH + 1` = 256 entries.
///
/// Maps a match length to its length code, the companion of [`oracle_dist_code`].
#[must_use]
pub fn oracle_length_code() -> &'static [uch] {
    // SAFETY: `c_oracle_length_code` returns `_length_code`, a `static const uch` array in the
    // oracle archive, paired with that same array's own `sizeof/sizeof` length. Static storage
    // duration makes the `'static` the true lifetime, the data is read-only, and `uch` is
    // `unsigned char`, one byte, as asserted above. Both accessors are niladic.
    unsafe { table_slice(c_oracle_length_code(), c_oracle_length_code_len()) }
}

/// The reference's `lenfix` from `inffixed.h`, 512 [`code`] entries.
///
/// The fixed literal/length decoding table, used for blocks encoded with the fixed Huffman codes of
/// RFC 1951 section 3.2.6. It begins `{96,7,0}, {0,8,80}, {0,8,16}, {20,8,115}`.
#[must_use]
pub fn oracle_lenfix() -> &'static [code] {
    // SAFETY: `c_oracle_lenfix` returns `lenfix`, a `static const code` array, paired with its own
    // `sizeof/sizeof` length. `code` is layout-identical to the C struct: `size_of::<code>() == 4`
    // with fields at 0, 1 and 2 is asserted at compile time above, and `c_oracle_code_size` reports
    // the C compiler's own answer for cross-checking.
    unsafe { table_slice(c_oracle_lenfix(), c_oracle_lenfix_len()) }
}

/// The reference's `distfix` from `inffixed.h`, 32 [`code`] entries.
#[must_use]
pub fn oracle_distfix() -> &'static [code] {
    // SAFETY: `c_oracle_distfix` returns `distfix`, a `static const code` array in the oracle
    // archive, paired with that same array's own `sizeof/sizeof` length. Static storage duration
    // makes the `'static` the true lifetime, and the data is read-only. `code` is layout-identical
    // to the C struct: `size_of::<code>() == 4` with fields at 0, 1 and 2 is asserted at compile
    // time above, and `c_oracle_code_size` reports the C compiler's own answer for cross-checking.
    // Both accessors are niladic.
    unsafe { table_slice(c_oracle_distfix(), c_oracle_distfix_len()) }
}

/// The element sizes the C compiler produced for the two mirrored table element types:
/// `(sizeof(ct_data), sizeof(code))`.
///
/// The compile-time assertions above already require both to be 4, but they assert it of the *Rust*
/// mirrors. This reports it of the *C* structs, so a test can confirm the two agree rather than
/// assuming the transcription was faithful -- which is the whole point of having an oracle.
#[must_use]
pub fn oracle_element_sizes() -> (c_uint, c_uint) {
    // SAFETY: two niladic accessors returning `unsigned`; no pointers and no preconditions.
    unsafe { (c_oracle_ct_data_size(), c_oracle_code_size()) }
}

// =============================================================================
//  Safe view over configuration_table
// =============================================================================
//
//  The other tables are arrays of one element type and are handed out as slices.  A
//  `configuration_table` row cannot be, because its fifth member is a `compress_func` -- a function
//  pointer whose *value* is meaningless across implementations and whose only useful property is
//  which of the five compressors it names.  So a row is handed out as an [`OracleConfig`]: the four
//  numbers read straight from the C object, and the compressor resolved to an
//  [`OracleCompressFunc`] by the C side, which is the only side that can compare the address.

/// Which of `deflate.c`'s five compressors a `configuration_table` row names.
///
/// The variants mirror the `local` functions of `deflate.c`: `deflate_stored` and `deflate_fast`
/// (L73-L74), `deflate_slow` (L76), `deflate_rle` and `deflate_huff` (L78-L79).
///
/// # What the table actually holds, and why all five are represented
///
/// `configuration_table` names only the first three -- [`Self::Stored`] at level 0, [`Self::Fast`]
/// at levels 1 to 3 and [`Self::Slow`] at levels 4 to 9. [`Self::Rle`] and [`Self::Huff`] are
/// reachable through the *strategy* (`Z_RLE` and `Z_HUFFMAN_ONLY`) rather than through the level,
/// so no row names them; they are represented anyway so that this enum describes the whole
/// `compress_func` family and would still name the compressor if a future row ever held one.
///
/// # Why the grouping is byte-identity critical
///
/// `deflateParams` compares the current level's `func` against the requested level's **by
/// identity** (`deflate.c` L791) to decide whether the level change must first flush the open
/// block. Two levels therefore behave the same there precisely when they name the same compressor,
/// which makes this grouping -- not the pointer value -- the property a port has to reproduce. Get
/// it wrong and `deflateParams` flushes where C does not, or fails to where C does, and either
/// changes the emitted bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OracleCompressFunc {
    /// `deflate_stored` -- store only, level 0.
    Stored,
    /// `deflate_fast` -- no lazy matches, levels 1 to 3.
    Fast,
    /// `deflate_slow` -- lazy matching, levels 4 to 9.
    Slow,
    /// `deflate_rle` -- reachable through `Z_RLE`, never through a table row.
    Rle,
    /// `deflate_huff` -- reachable through `Z_HUFFMAN_ONLY`, never through a table row.
    Huff,
}

impl OracleCompressFunc {
    /// Reads a [`c_oracle_config_func`] discriminator.
    ///
    /// `None` covers both negative answers the C side can give: -1, meaning the stored address
    /// matched none of the five compressors, which can only happen if `deflate.c` gained a sixth;
    /// and -2, meaning the level was out of range, which [`oracle_config`] rules out before asking.
    /// Anything else unrecognised is also `None` rather than a guess.
    fn from_discriminator(discriminator: c_int) -> Option<Self> {
        match discriminator {
            0 => Some(Self::Stored),
            1 => Some(Self::Fast),
            2 => Some(Self::Slow),
            3 => Some(Self::Rle),
            4 => Some(Self::Huff),
            _ => None,
        }
    }
}

/// One row of the reference's `configuration_table` (`deflate.c` L112-L124), read from the C object.
///
/// The field names are C's own, and the four numeric fields are widened to [`c_uint`] by the
/// generated accessors rather than narrowed to `ush`: C declares them as `ush` (L99-L102), the
/// widening is lossless for every value in the table, and it means the accessor cannot truncate if
/// the reference ever declared them wider. [`oracle_config_field_size`] reports the width C
/// actually used, so the port's own choice of a 16-bit field is checkable rather than assumed.
///
/// # Comparing this against the port
///
/// `zlib_rs::CONFIGURATION_TABLE` is the port's transcription of the same ten rows, and its
/// `Config` rows carry `u16` fields, so each comparison widens the port's value:
///
/// ```text
/// let rows = oracle_config_table_len();
/// assert_eq!(rows as usize, zlib_rs::CONFIGURATION_TABLE.len());
/// assert_eq!(oracle_config_field_size() as usize, size_of::<u16>());
///
/// for level in 0..rows {
///     let c = oracle_config(level).expect("level is below the reported row count");
///     let rust = zlib_rs::CONFIGURATION_TABLE[level as usize];
///     assert_eq!(c.good_length, c_uint::from(rust.good_length));
///     assert_eq!(c.max_lazy,    c_uint::from(rust.max_lazy));
///     assert_eq!(c.nice_length, c_uint::from(rust.nice_length));
///     assert_eq!(c.max_chain,   c_uint::from(rust.max_chain));
/// }
/// ```
///
/// The compressor is compared as the grouping rather than field-to-field, because the port's
/// `Config::func` is deliberately crate-private -- it holds a `CompressFunc` discriminant, a type
/// `zlib.map` does not export -- so the port's table is not asked to hand it out. What both sides
/// must agree on is the grouping itself: [`OracleCompressFunc::Stored`] at level 0,
/// [`OracleCompressFunc::Fast`] at levels 1 to 3 and [`OracleCompressFunc::Slow`] at 4 to 9, which
/// is what `deflateParams`' identity test at `deflate.c` L791 observes.
///
/// The example is `text` rather than a doctest on purpose: `zlib_rs` is a dev-dependency of this
/// crate, so it does not resolve from this module. That comparison belongs in
/// `crates/zlib-rs-differential/tests/`, which is where the dev-dependencies exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OracleConfig {
    /// `good_length` (L99) -- above this previous-match length the lazy search is cut short.
    pub good_length: c_uint,
    /// `max_lazy` (L100) -- the match length at or above which no lazy search is attempted.
    pub max_lazy: c_uint,
    /// `nice_length` (L101) -- the match length at which the chain walk quits early.
    pub nice_length: c_uint,
    /// `max_chain` (L102) -- the hard limit on how far a hash chain is walked.
    pub max_chain: c_uint,
    /// `func` (L103) -- which compressor the row names, or `None` if the C side did not recognise
    /// the stored address as one of the five.
    pub func: Option<OracleCompressFunc>,
}

/// The number of rows in the reference's `configuration_table`.
///
/// Reported by the C side, never hardcoded: `deflate.c` declares a ten-row table by default and a
/// two-row one under `FASTEST` (L106-L124), and the row count is how a caller tells which encoder
/// it is talking to. The oracle is built without `FASTEST`, so this is ten.
#[must_use]
pub fn oracle_config_table_len() -> c_uint {
    // SAFETY: a niladic accessor returning `unsigned`; no pointers and no preconditions.
    unsafe { c_oracle_config_table_len() }
}

/// `sizeof` one numeric field of a `configuration_table` row, as the C compiler laid it out.
///
/// The four fields are `ush` in C (`deflate.c` L99-L102), so this reports 2 on every supported
/// target. It exists for the same reason [`oracle_element_sizes`] does: the port transcribes those
/// fields at a chosen width, and this is the C compiler's own answer to compare that against
/// instead of assuming the transcription was faithful.
#[must_use]
pub fn oracle_config_field_size() -> c_uint {
    // SAFETY: a niladic accessor returning `unsigned`; no pointers and no preconditions.
    unsafe { c_oracle_config_field_size() }
}

/// The reference's `configuration_table[level]`, or `None` when `level` is not a row of it.
///
/// `None` means exactly "the C table has no such row", tested against
/// [`oracle_config_table_len`] before any accessor is called, so a caller iterating
/// `0..oracle_config_table_len()` never sees it. The C accessors range-check independently as
/// well -- they answer 0 and -2 for an out-of-range level rather than reading past the end -- so
/// the bound is enforced on both sides of the boundary.
#[must_use]
pub fn oracle_config(level: c_uint) -> Option<OracleConfig> {
    if level >= oracle_config_table_len() {
        return None;
    }

    // SAFETY: five accessors taking one `unsigned` by value and returning `unsigned` or `int`. No
    // pointer is involved, so nothing here can be a memory error. `level` is below the row count
    // the C side itself reported immediately above, so each one indexes a row that exists; the C
    // side range-checks it a second time regardless.
    let (good_length, max_lazy, nice_length, max_chain, func) = unsafe {
        (
            c_oracle_config_good_length(level),
            c_oracle_config_max_lazy(level),
            c_oracle_config_nice_length(level),
            c_oracle_config_max_chain(level),
            c_oracle_config_func(level),
        )
    };

    Some(OracleConfig {
        good_length,
        max_lazy,
        nice_length,
        max_chain,
        func: OracleCompressFunc::from_discriminator(func),
    })
}

// =============================================================================
//  Smoke tests
// =============================================================================
//
//  THIS MODULE IS MINIMAL BY DESIGN AND MUST NOT GROW INTO A DIFFERENTIAL SUITE.
//
//  It answers exactly one question: did the oracle archive link, and did the `c_` renaming actually
//  produce callable functions?  That question cannot be answered by declarations alone -- an
//  `extern` block that nothing calls emits no relocation, so the archive is never consulted and a
//  wrong symbol name would go unnoticed until the first real test.  These calls are what force the
//  linker to resolve the names, which is why the smoke module lives here rather than under
//  `tests/`.
//
//  It is also, today, the only thing this crate runs.  Nothing compares the two implementations:
//  `crates/zlib-rs-differential/tests/` does not exist, so there is no byte-identity suite, no
//  stream-interoperability suite and no ported-table comparison anywhere in the tree.  That work is
//  where such assertions belong, and the rule of thumb for the boundary still holds -- a test in
//  this module may reference the oracle and nothing else; the moment it needs `zlib_rs` or
//  `libz_rs_sys` it is a differential test and belongs under `tests/`, which is also the only place
//  those dev-dependencies resolve.
//
//  The expected values below are properties of the reference implementation, taken from the headers
//  and from `test/example.c` rather than from a previous run of this code.
#[cfg(test)]
mod tests {
    use super::*;
    use core::ffi::CStr;

    /// `ZLIB_VERSION` from `zlib.h`, which the oracle must report verbatim.
    ///
    /// Not "1.3.2": `deflateInit_` and `inflateInit_` compare the caller's compile-time version
    /// against the library's and return `Z_VERSION_ERROR` on a major mismatch, so the exact string
    /// is part of the ABI.
    const ZLIB_VERSION: &str = "1.3.2.1-motley";

    /// `Z_OK` from `zlib.h`.
    const Z_OK: c_int = 0;

    /// The payload from `test/example.c`, whose repeated "hello" stresses match finding. The C
    /// declaration is `static z_const char hello[] = "hello, hello!"`, and `sizeof hello` is 14
    /// because it includes the terminating NUL -- which is what the existing suite compresses, so it
    /// is what is compressed here.
    const HELLO: &[u8] = b"hello, hello!\0";

    /// The oracle links, its entry points are callable, and `zlibVersion` reports the version the
    /// header declares.
    ///
    /// This is the assertion the whole harness rests on. If the `c_` renaming had failed, this test
    /// would not link; if it had bound to the system zlib instead, the version would differ.
    #[test]
    fn links_and_reports_the_header_version() {
        // SAFETY: `c_zlibVersion` takes no arguments and returns a pointer to a `'static` string
        // literal compiled into the oracle, so there is nothing for the caller to get wrong.
        let raw = unsafe { c_zlibVersion() };
        assert!(!raw.is_null(), "zlibVersion() must never return NULL");

        // SAFETY: the returned pointer addresses `ZLIB_VERSION`, a NUL-terminated string literal with
        // static storage duration in `zutil.c`, so it is valid for reads through its terminator and
        // for the whole program.
        let version = unsafe { CStr::from_ptr(raw) };
        assert_eq!(version.to_str(), Ok(ZLIB_VERSION));
    }

    /// `zlibCompileFlags` describes the build, so its exact value is target-dependent and only the
    /// bits that are actually pinned by this build are asserted.
    ///
    /// The reference value on `x86_64` LP64 is `0xa9`, but hardcoding it would be wrong anywhere
    /// else, so the fields are decoded instead: bits 0-1 `sizeof(uInt)`, 2-3 `sizeof(uLong)`, 4-5
    /// `sizeof(voidpf)` and 6-7 `sizeof(z_off_t)`, each encoded 2 -> 0, 4 -> 1, 8 -> 2, else 3.
    ///
    /// Bit 13 (`DYNAMIC_CRC_TABLE`) is expected to be **clear**, and the reason is worth recording
    /// because it is easy to assume the opposite. `crc32.c` has two table strategies: under
    /// `DYNAMIC_CRC_TABLE` it computes them at run time behind a `z_once_t`, and otherwise it
    /// `#include`s the committed `crc32.h` (L153). `build.rs` defines only `_LARGEFILE64_SOURCE=1`,
    /// `HAVE_HIDDEN` and the rename macros, so this oracle takes the committed-table branch and
    /// `zutil.c` L73-75 never adds `1 << 13`. That is self-consistent with the reference value
    /// `0xa9`, which has no bit 13 set. Measured, not assumed: the run-time value is asserted below.
    ///
    /// So on this build both implementations happen to agree on bit 13 -- the oracle because its
    /// tables are committed, the port because its tables are const-evaluated. The two flag words are
    /// nevertheless not *required* to be equal in general, and a differential test must not assert
    /// that they are: an oracle built with `-DDYNAMIC_CRC_TABLE` would set the bit while the port
    /// still could not.
    #[test]
    fn compile_flags_describe_this_build() {
        /// The 2-bit width encoding from `zutil.c`: 2 -> 0, 4 -> 1, 8 -> 2, anything else -> 3.
        fn width_code(size: usize) -> c_ulong {
            match size {
                2 => 0,
                4 => 1,
                8 => 2,
                _ => 3,
            }
        }

        // SAFETY: niladic, returns a `uLong` by value.
        let flags = unsafe { c_zlibCompileFlags() };

        assert_eq!(flags & 0x3, width_code(size_of::<uInt>()), "bits 0-1: uInt");
        assert_eq!(
            (flags >> 2) & 0x3,
            width_code(size_of::<uLong>()),
            "bits 2-3: uLong"
        );
        assert_eq!(
            (flags >> 4) & 0x3,
            width_code(size_of::<voidpf>()),
            "bits 4-5: voidpf"
        );
        assert_eq!(
            (flags >> 6) & 0x3,
            width_code(size_of::<z_off_t>()),
            "bits 6-7: z_off_t"
        );

        // `build.rs` does not define `DYNAMIC_CRC_TABLE`, so `crc32.c` uses the committed `crc32.h`
        // tables and `zutil.c` leaves bit 13 clear. This assertion is what pins that build choice:
        // if the oracle were ever switched to run-time table generation, this fails and the
        // table-equality suite's premise -- that it is comparing against the committed tables --
        // would need revisiting rather than silently changing meaning.
        assert_eq!((flags >> 13) & 1, 0, "bit 13: DYNAMIC_CRC_TABLE");

        // On the target the reference numbers were measured on, the whole word is 0xa9.
        if IS_LP64 {
            assert_eq!(flags, 0xa9);
        }
    }

    /// A one-shot round trip through the oracle: `compressBound`, `compress`, then `uncompress`.
    ///
    /// This exercises a real stream end to end, so it proves rather more than linkage: that
    /// `deflateInit2_`, `deflate` and `deflateEnd` inside `compress` all resolved to the renamed
    /// symbols and cooperate.
    #[test]
    fn compress_bound_and_round_trip() {
        // Every width conversion here goes through `try_from` rather than `as`. That is not lint
        // appeasement: `uLong` is `unsigned long`, so it is wider than `usize` on some targets and
        // narrower on others, and a silent truncation in either direction would corrupt a length
        // handed to C. A checked conversion turns that into a visible test failure instead.
        let source_len = uLong::try_from(HELLO.len())
            .expect("the 14-byte payload fits in a uLong on any target");

        // SAFETY: `c_compressBound` is a pure arithmetic function of its argument.
        let bound = unsafe { c_compressBound(source_len) };
        assert_eq!(bound, 27, "compressBound(14) is 27 for this configuration");

        let capacity = usize::try_from(bound).expect("a 27-byte bound fits in a usize");
        let mut compressed = vec![0u8; capacity];
        let mut compressed_len: uLongf = bound;
        // SAFETY: `compressed` is a live allocation of `bound` bytes and `compressed_len` is
        // initialised to that same capacity, so the destination is valid for the writes C may make.
        // `HELLO` is a `'static` slice of `source_len` readable bytes. Both length pointers address
        // live locals that outlive the call. `compress` does not retain any of these pointers past
        // the call, and the two buffers do not overlap.
        let err = unsafe {
            c_compress(
                compressed.as_mut_ptr(),
                &mut compressed_len,
                HELLO.as_ptr(),
                source_len,
            )
        };
        assert_eq!(err, Z_OK, "compress");
        assert_eq!(
            compressed_len, 19,
            "level-6 zlib output for this payload is 19 bytes"
        );
        assert!(compressed_len <= bound, "output must fit within the bound");

        let mut restored = vec![0u8; HELLO.len()];
        let mut restored_len: uLongf =
            uLong::try_from(restored.len()).expect("the 14-byte buffer fits in a uLong");
        // SAFETY: `restored` is a live allocation of `restored_len` bytes, and the source is the
        // prefix of `compressed` that the call above actually filled, so `compressed_len` bytes are
        // readable from its pointer. Both length pointers address live locals that outlive the call,
        // and the two buffers are distinct allocations.
        let err = unsafe {
            c_uncompress(
                restored.as_mut_ptr(),
                &mut restored_len,
                compressed.as_ptr(),
                compressed_len,
            )
        };
        assert_eq!(err, Z_OK, "uncompress");

        let restored_len = usize::try_from(restored_len).expect("a 14-byte result fits in a usize");
        assert_eq!(restored_len, HELLO.len());
        assert_eq!(
            restored.get(..restored_len),
            Some(HELLO),
            "the round trip must reproduce the input byte for byte"
        );
    }

    /// The two checksum families over a known input, and the first `get_crc_table` entry.
    ///
    /// These are the cheapest possible confirmation that `adler32.c` and `crc32.c` are the objects
    /// that got linked: both values are fixed properties of the algorithms, not of the build.
    #[test]
    fn checksums_over_a_known_input() {
        // The dictionary literal from `test/example.c` L40, without its NUL: the existing suite takes
        // this checksum with `adler32(0, dictionary, sizeof dictionary - 1)`.
        let hello = b"hello";
        let len = uInt::try_from(hello.len()).expect("5 fits in a uInt");

        // SAFETY: `hello` is a `'static` slice of 5 readable bytes and `len` is exactly that length,
        // so the whole range C reads is in bounds. Neither function retains the pointer.
        let adler = unsafe { c_adler32(1, hello.as_ptr(), len) };
        assert_eq!(adler, 0x062c_0215);

        // SAFETY: `hello` is a `'static` slice of 5 readable bytes, `u8` needs no alignment
        // beyond 1, and `len` is exactly that length, so the whole range C reads is in bounds.
        // Nothing is written through the pointer and `c_crc32` does not retain it.
        let crc = unsafe { c_crc32(0, hello.as_ptr(), len) };
        assert_eq!(crc, 0x3610_a686);

        // SAFETY: `c_get_crc_table` is niladic and returns a pointer to a table with static storage
        // duration and at least 256 entries; the two reads below are well within that.
        let table = unsafe { c_get_crc_table() };
        assert!(!table.is_null());
        // SAFETY: `table` is non-null by the assertion above and points at `crc_table`, which has 256
        // `z_crc_t` entries, so offsets 0 and 1 are in bounds.
        unsafe {
            assert_eq!(*table, 0x0000_0000);
            assert_eq!(*table.add(1), 0x7707_3096);
        }
    }

    /// The generated-table accessors are wired up and the safe wrappers bound them correctly.
    ///
    /// This checks *shape* only -- lengths, geometry and element sizes. Nothing compares the
    /// contents against the port's `const` arrays: that belongs in a table-equality suite under
    /// `tests/`, which has not been written, and reproducing it here is exactly the growth this
    /// module must not undergo.
    #[test]
    fn table_accessors_report_the_expected_shapes() {
        assert_eq!(oracle_crc_table().len(), 256, "crc_table");
        assert_eq!(oracle_x2n_table().len(), 32, "x2n_table");
        assert_eq!(
            oracle_static_ltree().len(),
            (L_CODES + 2) as usize,
            "static_ltree"
        );
        assert_eq!(
            oracle_static_dtree().len(),
            D_CODES as usize,
            "static_dtree"
        );
        assert_eq!(
            oracle_base_length().len(),
            LENGTH_CODES as usize,
            "base_length"
        );
        assert_eq!(oracle_base_dist().len(), D_CODES as usize, "base_dist");
        assert_eq!(oracle_dist_code().len(), 512, "_dist_code");
        assert_eq!(oracle_length_code().len(), 256, "_length_code");
        assert_eq!(oracle_lenfix().len(), 512, "lenfix");
        assert_eq!(oracle_distfix().len(), 32, "distfix");

        // The first entries of the three generated headers, which is where a transcription error in
        // the port would show up first. `get` rather than `[]` so that a shape regression reports as
        // a clear inequality instead of a panic inside the assertion.
        assert_eq!(oracle_crc_table().get(1), Some(&0x7707_3096));
        assert_eq!(
            oracle_static_ltree().first(),
            Some(&ct_data { fc: 12, dl: 8 })
        );
        assert_eq!(
            oracle_lenfix().first(),
            Some(&code {
                op: 96,
                bits: 7,
                val: 0
            })
        );

        // The C compiler's own element sizes must match the Rust mirrors' -- the transcription is
        // checked, not assumed.
        let (ct_data_size, code_size) = oracle_element_sizes();
        assert_eq!(ct_data_size as usize, size_of::<ct_data>());
        assert_eq!(code_size as usize, size_of::<code>());

        // The braided geometry is reported, never hardcoded. Either the path is compiled out and both
        // dimensions are zero, or the table is exactly `rows * 256` entries with `rows` being
        // `crc32.c`'s `W`.
        let (rows, cols) = oracle_crc_braid_dimensions();
        let braid = oracle_crc_braid_table();
        if rows == 0 {
            assert_eq!(cols, 0, "a zero row count implies no braided table");
            assert!(braid.is_empty());
        } else {
            assert_eq!(cols, 256, "each braid row is indexed by one byte");
            assert!(rows == 4 || rows == 8, "crc32.c defines W as 4 or 8");
            assert_eq!(braid.len(), (rows * cols) as usize);
            // SAFETY-free cross-check: the shim reports `sizeof(z_word_t)`, which must equal W.
            // SAFETY: niladic accessor returning `unsigned`.
            assert_eq!(unsafe { c_oracle_word_size() }, rows);
        }
    }

    /// The configuration-table accessors are wired up and read the reference's own ten rows.
    ///
    /// This is the counterpart of the test above for the second generated file, and it is here for
    /// the same reason: an `extern` block nothing calls emits no relocation, so a wrong symbol name
    /// would go unnoticed until the first real test. Calling all seven forces the linker to resolve
    /// them.
    ///
    /// The forty numbers below are `deflate.c` L112-L124 restated, which makes this a property of
    /// the *reference* -- the same kind of assertion as `crc_table[1] == 0x77073096` above, and the
    /// same kind of check that the accessors read the table they claim to. Comparing them against
    /// `zlib_rs::CONFIGURATION_TABLE` is a differential assertion and belongs in
    /// `crates/zlib-rs-differential/tests/`, not here.
    #[test]
    fn config_table_accessors_report_the_reference_rows() {
        /// L112-L124 in C's column order: good, lazy, nice, chain, compressor.
        ///
        /// Ten rows, because the oracle is built without `FASTEST`; under `FASTEST` `deflate.c`
        /// declares a two-row table instead and this test would fail, which is the point.
        const EXPECTED: [(c_uint, c_uint, c_uint, c_uint, OracleCompressFunc); 10] = [
            (0, 0, 0, 0, OracleCompressFunc::Stored),
            (4, 4, 8, 4, OracleCompressFunc::Fast),
            (4, 5, 16, 8, OracleCompressFunc::Fast),
            (4, 6, 32, 32, OracleCompressFunc::Fast),
            (4, 4, 16, 16, OracleCompressFunc::Slow),
            (8, 16, 32, 32, OracleCompressFunc::Slow),
            (8, 16, 128, 128, OracleCompressFunc::Slow),
            (8, 32, 128, 256, OracleCompressFunc::Slow),
            (32, 128, 258, 1024, OracleCompressFunc::Slow),
            (32, 258, 258, 4096, OracleCompressFunc::Slow),
        ];

        let rows = oracle_config_table_len();
        assert_eq!(
            rows as usize,
            EXPECTED.len(),
            "the default configuration_table has ten rows; two would mean a FASTEST build"
        );

        // `ush` is `unsigned short`, so the C fields are two bytes wide. This is what makes the
        // port's 16-bit transcription of them a checked fact rather than an assumption.
        assert_eq!(
            oracle_config_field_size() as usize,
            size_of::<u16>(),
            "struct config_s declares all four numeric fields as ush"
        );

        for (level, expected) in (0..rows).zip(EXPECTED) {
            let (good_length, max_lazy, nice_length, max_chain, func) = expected;
            assert_eq!(
                oracle_config(level),
                Some(OracleConfig {
                    good_length,
                    max_lazy,
                    nice_length,
                    max_chain,
                    func: Some(func),
                }),
                "configuration_table[{level}]"
            );
        }

        // A level the table does not have is reported as absent rather than read out of bounds.
        assert_eq!(oracle_config(rows), None);
    }

    /// `zError` maps a status code to the reference's own message string.
    ///
    /// The port has to return these strings verbatim, so having the oracle's copy callable is what
    /// makes that checkable.
    #[test]
    fn zerror_returns_the_reference_message() {
        // SAFETY: `c_zError` takes a status code by value and returns a pointer to a `'static`
        // string literal from `zutil.c`'s `z_errmsg` table.
        let raw = unsafe { c_zError(Z_OK) };
        assert!(!raw.is_null());
        // SAFETY: the returned pointer addresses a NUL-terminated string literal with static storage
        // duration, so it is valid for reads through its terminator.
        let message = unsafe { CStr::from_ptr(raw) };
        assert_eq!(
            message.to_str(),
            Ok(""),
            "Z_OK's message is the empty string"
        );
    }
}
