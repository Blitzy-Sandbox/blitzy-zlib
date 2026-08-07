//! Bindings to the C reference implementation, compiled as a test-only oracle.
//!
//! Everything in this module is a *declaration*. There is no logic here, and there are no safe
//! wrappers: the point is to make the C implementation callable from Rust so that a test can run
//! both implementations against the same input, in the same process, and compare the bytes.
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
//! * `gzvprintf` is the one entry point in `zlib.h` that cannot be declared on stable Rust: its
//!   `va_list` parameter has no portable spelling (`core::ffi::VaList` is unstable, and the type is
//!   a pointer on one Tier-1 target and a by-value struct on another). Declaring it with a
//!   stand-in type would be an ABI hazard rather than a convenience, so it is deliberately omitted.
//!   The symbol is present in the archive, and [`c_gzprintf`] -- which *is* declarable, because
//!   variadic `extern "C"` declarations are stable -- exercises the same code path, since C's
//!   `gzprintf` is a one-line forward to `gzvprintf`.
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
const _: () = assert!(size_of::<alloc_func>() == size_of::<voidpf>());
const _: () = assert!(size_of::<free_func>() == size_of::<voidpf>());
const _: () = assert!(size_of::<in_func>() == size_of::<voidpf>());
const _: () = assert!(size_of::<out_func>() == size_of::<voidpf>());

/// True when the target uses the integer model the reference numbers were measured under: 64-bit
/// pointers with a 64-bit `unsigned long`, i.e. LP64.
const IS_LP64: bool = size_of::<*const c_void>() == 8 && size_of::<c_ulong>() == 8;

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
const _: () = assert!(!IS_LP64 || size_of::<gz_header>() == 80);
const _: () = assert!(!IS_LP64 || size_of::<gzFile_s>() == 24);
const _: () = assert!(!IS_LP64 || offset_of!(gzFile_s, next) == 8);
const _: () = assert!(!IS_LP64 || offset_of!(gzFile_s, pos) == 16);

// =============================================================================
//  The public zlib.h surface
// =============================================================================
//
//  Every `ZEXTERN` declaration in `zlib.h` that corresponds to a real symbol: 96 prototypes, less
//  `gzvprintf`, whose `va_list` parameter has no portable stable-Rust spelling (see the module
//  docs).  Each doc line is the C prototype verbatim, so this block can be diffed against the
//  header by eye.
//
//  Two entries deserve a note.  `gzopen_w` is declared only under `_WIN32` in `zlib.h` and is
//  therefore compiled into the oracle only there, so its declaration is gated the same way.
//  `gzgetc` reaches the archive through the `objcopy` rename rather than the `-D` rename, because
//  `gzread.c` undefines the macro before defining the function and no command-line define can
//  survive that; the resulting symbol is `c_gzgetc` all the same.

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

    /// `int gzprintf(gzFile file, const char *format, ...);`
    pub fn c_gzprintf(file: gzFile, format: *const c_char, ...) -> c_int;

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
