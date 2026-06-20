#![cfg(feature = "capi")]
//! # C-ABI shim — the `#[no_mangle] extern "C"` drop-in for `libz`
//!
//! This module is the prompt-mandated **C-compatible FFI layer**: a thin set of
//! `#[unsafe(no_mangle)] pub unsafe extern "C"` functions whose names and
//! signatures match the canonical zlib C API (`zlib.h` / `zlib.map`) *byte for
//! byte*, operating over `#[repr(C)]` structs whose layout mirrors
//! `z_stream_s` / `gz_header_s` exactly. Every entry point performs the minimal
//! pointer marshalling required and then delegates all real work to the
//! safe-Rust engines (`crate::deflate`, `crate::inflate`, `crate::checksum`,
//! `crate::gz`, `crate::util`), converting the engines' `Result`/`Option`
//! values into the frozen C `int` return codes (`Z_OK` … `Z_VERSION_ERROR`).
//!
//! Building the crate with the `capi` feature produces `cdylib`/`staticlib`
//! artifacts that export the canonical symbol set (`deflate`, `inflate`,
//! `crc32`, `adler32`, `compress2`, `uncompress2`, `gzopen`, `inflateBack`,
//! `zlibVersion`, …), so the result is a drop-in replacement for the system
//! `libz` at the binary level. `cbindgen` (driven by the crate's `build.rs` and
//! `cbindgen.toml`) parses *this* file to regenerate the matching C header.
//!
//! ## The designated `unsafe` boundary
//!
//! Together with the inflate fast path (`src/inflate/fast.rs`), this module is
//! one of the crate's only two `unsafe` zones (AAP §0.6.2). All raw-pointer
//! dereferences, `Box::from_raw`/`Box::into_raw` round-trips, C-string handling
//! and C-callback invocations live here. The crate sets
//! `#![forbid(unsafe_op_in_unsafe_fn)]`, so even inside an `unsafe fn` every
//! unsafe operation is wrapped in an explicit `unsafe { … }` block carrying a
//! `// SAFETY:` comment that documents the caller-upheld C contract.
//!
//! ## Allocator semantics (`zalloc` / `zfree` / `opaque`)
//!
//! The `zalloc`/`zfree`/`opaque` fields are preserved in the [`z_stream`]
//! layout for ABI fidelity. The safe-Rust engines own and free all of their
//! working memory through RAII (`Vec`/`Box`, reclaimed on `Drop`; AAP §0.6.3),
//! so the default contract — *when `zalloc`/`zfree` are `Z_NULL`, the library
//! uses its own heap* — is honoured exactly. A C caller that installs custom
//! allocator hooks will still get correct compression/decompression; the
//! engines simply source their internal buffers from the Rust global allocator
//! rather than routing each block through the hooks. This is the documented
//! consequence of replacing manual `ZALLOC`/`ZFREE` with ownership.
//!
//! ## Symbol gating
//!
//! The entire module is gated behind the non-default `capi` feature (both via
//! the `#![cfg(feature = "capi")]` inner attribute here and the
//! `#[cfg(feature = "capi")] pub mod ffi;` declaration in `lib.rs`). This keeps
//! the canonical `#[no_mangle]` symbol names out of `cargo test`, where they
//! would otherwise clash at link time with the bundled C `zlib` pulled in by
//! the `flate2` dev-dependency oracle.

// The exported items are C identifiers, so they intentionally violate Rust's
// naming conventions (`z_stream`, `deflateInit_`, `gzFile`, …). The C ABI
// contract for each function is the zlib API documented in `zlib.h`; a Rust
// `# Safety` section on all ~90 shims would be pure duplication, so the
// `missing_safety_doc` lint is allowed module-wide with that rationale.
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]
#![allow(clippy::missing_safety_doc)]
// At the C ABI boundary the integer widths of the zlib type aliases are
// platform-dependent: `uLong` is `c_ulong` (64-bit on most Unix LP64 targets
// but 32-bit on Windows/ILP32), and `z_off_t` is `c_long` (likewise). The
// shims therefore normalise these to the engines' fixed-width Rust types with
// explicit `as` casts that are *necessary on at least one target*. On a target
// where the alias already matches the engine type the cast is a no-op, which
// clippy would flag as `unnecessary_cast` — but rewriting it as `From`/`into`
// would instead trip `useless_conversion` on that same target, so neither form
// is clean everywhere. Allowing the lint module-wide keeps the casts correct
// across platforms; this module is essentially nothing *but* deliberate ABI
// width normalisation, so the lint carries little signal here.
#![allow(clippy::unnecessary_cast)]

use core::ffi::{c_char, c_int, c_long, c_uchar, c_uint, c_ulong, c_void};

use alloc::boxed::Box;
use alloc::vec::Vec;

// Re-export every public `Z_*` constant (return codes, flush/level/strategy/
// method, window sizes) plus `MAX_*`/`DEF_*`/`SEEK_*` from the canonical
// `constants` module. The glob both brings them into scope for the shims below
// (keeping the many constant references terse) and makes them part of this
// module's public surface, so cbindgen emits them as C `#define`/`const`
// declarations for downstream C consumers (AAP §0.6.2, Phase E). The
// `wildcard_imports` clippy lint is pedantic (non-default), so the glob does
// not trip `-D warnings`.
pub use crate::constants::*;
use crate::error::result_to_code;

// Engine modules are aliased so the FFI can delegate to functions that share a
// name with the exported C symbol (e.g. the shim `deflate` calls the engine
// `deng::deflate`) without a name collision in this module's scope.
use crate::checksum as cksum;
use crate::deflate as deng;
use crate::inflate as ieng;
use crate::util as ueng;

// The owned Rust gzip-header type marshalled to/from the C `gz_header` by
// `deflateSetHeader` / `inflateGetHeader`. Only referenced under `gzip`.
#[cfg(feature = "gzip")]
use crate::gz_header::GzHeader;

// The gzip file-I/O engine and the `std` glue (paths, descriptors, files) the
// C `gz*` entry points marshal into. All gated behind `gz-io`, which implies
// `std`, so these `std` imports are always available when this code compiles.
#[cfg(feature = "gz-io")]
use crate::gz as geng;
#[cfg(feature = "gz-io")]
use crate::gz::state::GzState;
#[cfg(feature = "gz-io")]
use std::ffi::{CStr, OsStr};
#[cfg(feature = "gz-io")]
use std::fs::File;
#[cfg(feature = "gz-io")]
use std::os::fd::FromRawFd;
#[cfg(feature = "gz-io")]
use std::os::unix::ffi::OsStrExt;
#[cfg(feature = "gz-io")]
use std::path::Path;

// ===========================================================================
// Phase A — `#[repr(C)]` type layout (mirrors zlib.h / zconf.h byte-for-byte)
// ===========================================================================

/// A single byte of (de)compressed data — C `Bytef` (`zconf.h`).
pub type Bytef = u8;
/// C `uInt` — `unsigned int`.
pub type uInt = c_uint;
/// C `uLong` — `unsigned long` (32-bit on Windows, 64-bit on LP64 Unix).
pub type uLong = c_ulong;
/// C `uLongf` — a far `uLong`; identical to [`uLong`] on flat-memory targets.
pub type uLongf = c_ulong;
/// C `voidpf` — a far `void *`.
pub type voidpf = *mut c_void;
/// C `voidpc` — a far `const void *`.
pub type voidpc = *const c_void;
/// C `voidp` — a plain `void *`.
pub type voidp = *mut c_void;
/// C `z_size_t` — the platform `size_t` (`zconf.h`).
pub type z_size_t = usize;
/// C `z_crc_t` — the 32-bit CRC table element type (`zconf.h`).
pub type z_crc_t = u32;
/// C `z_off_t` — the (possibly 32-bit) file-offset type, a `long`.
pub type z_off_t = c_long;
/// C `z_off64_t` — the always-64-bit file-offset type.
pub type z_off64_t = i64;

/// C `alloc_func` — `voidpf (*)(voidpf opaque, uInt items, uInt size)`.
///
/// Wrapped in `Option` so the C `Z_NULL` (a null function pointer) is
/// representable as `None`.
pub type alloc_func =
    Option<unsafe extern "C" fn(opaque: voidpf, items: uInt, size: uInt) -> voidpf>;
/// C `free_func` — `void (*)(voidpf opaque, voidpf address)`.
pub type free_func = Option<unsafe extern "C" fn(opaque: voidpf, address: voidpf)>;

/// C `in_func` — the `inflateBack` input callback
/// `unsigned (*)(void *, z_const unsigned char **)`.
pub type in_func =
    Option<unsafe extern "C" fn(desc: *mut c_void, buf: *mut *const c_uchar) -> c_uint>;
/// C `out_func` — the `inflateBack` output callback
/// `int (*)(void *, unsigned char *, unsigned)`.
pub type out_func =
    Option<unsafe extern "C" fn(desc: *mut c_void, buf: *mut c_uchar, len: c_uint) -> c_int>;

/// The streaming (de)compression control block — C `z_stream` (`zlib.h`).
///
/// All 14 fields appear in the exact C declaration order so the
/// `#[repr(C)]` layout (and `size_of`) match the system `z_stream` precisely,
/// which is what makes the crate a binary drop-in for `libz`.
#[repr(C)]
pub struct z_stream {
    /// Next input byte (`next_in`).
    pub next_in: *const Bytef,
    /// Number of bytes available at `next_in` (`avail_in`).
    pub avail_in: uInt,
    /// Total number of input bytes read so far (`total_in`).
    pub total_in: uLong,
    /// Next output byte will be written here (`next_out`).
    pub next_out: *mut Bytef,
    /// Remaining free space at `next_out` (`avail_out`).
    pub avail_out: uInt,
    /// Total number of bytes output so far (`total_out`).
    pub total_out: uLong,
    /// Last error message, or `NULL` if none (`msg`).
    pub msg: *const c_char,
    /// Internal engine state, opaque to applications (`state`).
    pub state: *mut c_void,
    /// Custom allocation hook, or `Z_NULL` for the default (`zalloc`).
    pub zalloc: alloc_func,
    /// Custom deallocation hook, or `Z_NULL` for the default (`zfree`).
    pub zfree: free_func,
    /// Private data passed to `zalloc`/`zfree` (`opaque`).
    pub opaque: voidpf,
    /// Best guess about the data type for deflate, or decode state for inflate
    /// (`data_type`).
    pub data_type: c_int,
    /// Adler-32 or CRC-32 value of the uncompressed data (`adler`).
    pub adler: uLong,
    /// Reserved for future use (`reserved`).
    pub reserved: uLong,
}

/// C `z_streamp` — a pointer to a [`z_stream`].
pub type z_streamp = *mut z_stream;

/// gzip header information passed to/from the library — C `gz_header`
/// (`zlib.h`, RFC 1952). All 13 fields in exact C order.
#[repr(C)]
pub struct gz_header {
    /// True if the compressed data is believed to be text (`text`).
    pub text: c_int,
    /// Modification time (`time`).
    pub time: uLong,
    /// Extra flags, not used when writing (`xflags`).
    pub xflags: c_int,
    /// Operating system (`os`).
    pub os: c_int,
    /// Pointer to the extra field, or `Z_NULL` (`extra`).
    pub extra: *mut Bytef,
    /// Extra-field length, valid if `extra != Z_NULL` (`extra_len`).
    pub extra_len: uInt,
    /// Space at `extra`, only when reading (`extra_max`).
    pub extra_max: uInt,
    /// Pointer to the zero-terminated file name, or `Z_NULL` (`name`).
    pub name: *mut Bytef,
    /// Space at `name`, only when reading (`name_max`).
    pub name_max: uInt,
    /// Pointer to the zero-terminated comment, or `Z_NULL` (`comment`).
    pub comment: *mut Bytef,
    /// Space at `comment`, only when reading (`comm_max`).
    pub comm_max: uInt,
    /// True if there was or will be a header CRC (`hcrc`).
    pub hcrc: c_int,
    /// True when done reading the gzip header, not used when writing (`done`).
    pub done: c_int,
}

/// C `gz_headerp` — a pointer to a [`gz_header`].
pub type gz_headerp = *mut gz_header;

/// C `gzFile` — an opaque handle to an open gzip file. Internally a
/// `*mut GzState`; applications must treat it as opaque.
pub type gzFile = *mut c_void;

// The decode-table entry (`#[repr(C)] struct code { op, bits, val }`). The
// authoritative definition lives in `crate::inflate::tables`; it is re-exported
// here so it appears at the FFI boundary (and in the generated C header) as a
// single canonical type rather than a duplicate.
pub use crate::inflate::Code;

// ===========================================================================
// Phase B — internal-state marshalling (the unsafe core of this file)
// ===========================================================================

/// The runtime version string, NUL-terminated for C (`zlibVersion`).
const ZLIB_VERSION_CSTR: &[u8] = b"1.3.2.1-motley\0";

/// Boxed compression state attached to `z_stream.state`.
///
/// Owns the engine [`DeflateState`](crate::deflate::DeflateState) plus a small
/// NUL-terminated buffer that backs `z_stream.msg`: the engine reports messages
/// as `&'static str` (no terminator), so the shim copies the current message
/// here and points `msg` at it. The buffer lives as long as the stream, so the
/// pointer stays valid until the next call overwrites it — exactly zlib's
/// `msg` contract.
struct DeflateHandle {
    state: Box<deng::DeflateState>,
    msg: Vec<u8>,
}

/// Boxed decompression state attached to `z_stream.state`.
///
/// In addition to the engine state and the `msg` cache (see [`DeflateHandle`]),
/// this records the caller's `gz_header` pointer registered by
/// [`inflateGetHeader`]; after each [`inflate`] call the captured header fields
/// are synced back into that C struct.
struct InflateHandle {
    state: Box<ieng::InflateState>,
    /// The `gz_header` registered by `inflateGetHeader`, or null.
    head: *mut gz_header,
    msg: Vec<u8>,
}

/// Boxed `inflateBack` state attached to `z_stream.state`.
///
/// `inflateBack` decompresses with a caller-supplied window buffer; the raw
/// pointer and size are stored here at init and rebuilt into a slice for each
/// `inflateBack` call.
struct InflateBackHandle {
    state: Box<ieng::InflateState>,
    /// Caller-supplied sliding-window buffer (`1 << windowBits` bytes).
    window: *mut Bytef,
    /// Length of `window` in bytes.
    wsize: usize,
}

/// Reconstruct the input slice from `next_in` / `avail_in`.
///
/// A null `next_in` (legal when `avail_in == 0`) yields an empty slice.
///
/// # Safety
///
/// `next_in` must be valid for reads of `avail_in` bytes, per the C contract.
#[inline]
unsafe fn input_slice<'a>(next_in: *const Bytef, avail_in: uInt) -> &'a [u8] {
    if next_in.is_null() || avail_in == 0 {
        &[]
    } else {
        // SAFETY: the caller guarantees `next_in` points to `avail_in` readable
        // bytes for the duration of the call (the zlib `next_in`/`avail_in`
        // contract); the borrow does not outlive this call.
        unsafe { core::slice::from_raw_parts(next_in, avail_in as usize) }
    }
}

/// Reconstruct the output slice from `next_out` / `avail_out`.
///
/// A null `next_out` (legal when `avail_out == 0`) yields an empty slice.
///
/// # Safety
///
/// `next_out` must be valid for writes of `avail_out` bytes, per the C
/// contract, and not alias the input slice.
#[inline]
unsafe fn output_slice<'a>(next_out: *mut Bytef, avail_out: uInt) -> &'a mut [u8] {
    if next_out.is_null() || avail_out == 0 {
        &mut []
    } else {
        // SAFETY: the caller guarantees `next_out` points to `avail_out`
        // writable bytes for the duration of the call (the zlib
        // `next_out`/`avail_out` contract); the borrow does not outlive it.
        unsafe { core::slice::from_raw_parts_mut(next_out, avail_out as usize) }
    }
}

/// Reconstruct a `&[u8]` from a C buffer pointer and a `usize` length (the
/// one-shot helpers carry `uLong`/`z_size_t` lengths, which can exceed `uInt`).
/// A null pointer or zero length yields an empty slice.
///
/// # Safety
///
/// When non-null, `ptr` must point to at least `len` readable bytes that remain
/// valid for the duration of the call.
#[inline]
unsafe fn const_buf<'a>(ptr: *const Bytef, len: usize) -> &'a [u8] {
    if ptr.is_null() || len == 0 {
        &[]
    } else {
        // SAFETY: caller upholds the validity-for-`len`-bytes precondition.
        unsafe { core::slice::from_raw_parts(ptr, len) }
    }
}

/// Reconstruct a `&mut [u8]` from a C buffer pointer and a `usize` length.
/// A null pointer or zero length yields an empty slice.
///
/// # Safety
///
/// When non-null, `ptr` must point to at least `len` writable bytes that remain
/// valid for the duration of the call and are not aliased elsewhere.
#[inline]
unsafe fn mut_buf<'a>(ptr: *mut Bytef, len: usize) -> &'a mut [u8] {
    if ptr.is_null() || len == 0 {
        &mut []
    } else {
        // SAFETY: caller upholds the validity-for-`len`-bytes precondition.
        unsafe { core::slice::from_raw_parts_mut(ptr, len) }
    }
}

/// Copy an engine message into `cache` (NUL-terminated) and point
/// `strm.msg` at it, or clear `strm.msg` when there is no message.
#[inline]
fn store_msg(strm: &mut z_stream, cache: &mut Vec<u8>, msg: Option<&'static str>) {
    match msg {
        Some(text) => {
            cache.clear();
            cache.extend_from_slice(text.as_bytes());
            cache.push(0);
            strm.msg = cache.as_ptr() as *const c_char;
        }
        None => strm.msg = core::ptr::null(),
    }
}

/// Validate the `version` / `stream_size` arguments passed to the `*Init_`
/// entry points, mirroring the C check: the major version digit must match and
/// the caller's `sizeof(z_stream)` must equal ours.
///
/// # Safety
///
/// `version`, when non-null, must point to a NUL-terminated C string.
#[inline]
unsafe fn version_compatible(version: *const c_char, stream_size: c_int) -> bool {
    if version.is_null() {
        return false;
    }
    // SAFETY: per the C contract `version` is a valid NUL-terminated string, so
    // its first byte is readable. We only inspect that first byte (the major
    // version digit), exactly as the C `deflateInit_`/`inflateInit_` do.
    let first = unsafe { *version };
    first == ZLIB_VERSION_CSTR[0] as c_char && stream_size as usize == size_of::<z_stream>()
}

/// Convert a C `int` flush code into the typed [`FlushMode`](crate::constants::FlushMode),
/// returning `None` (→ `Z_STREAM_ERROR` at the call site) for an invalid value.
#[inline]
fn flush_from_c(flush: c_int) -> Option<crate::constants::FlushMode> {
    crate::constants::FlushMode::try_from(flush).ok()
}

/// Run a delegate that may theoretically panic, mapping any unwinding to a C
/// return code instead of letting the panic cross the FFI boundary (which is
/// undefined behaviour). Only the `std` build can catch unwinds; under
/// `no_std` the delegate is run directly (the engines are written not to
/// panic on the supported paths).
#[inline]
fn guard_call<F: FnOnce() -> c_int>(f: F) -> c_int {
    #[cfg(feature = "std")]
    {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
            Ok(code) => code,
            Err(_) => Z_STREAM_ERROR,
        }
    }
    #[cfg(not(feature = "std"))]
    {
        f()
    }
}

/// Box a freshly initialised [`DeflateState`](crate::deflate::DeflateState),
/// attach it to `strm.state`, and seed the public `z_stream` fields from it.
fn attach_deflate(strm: &mut z_stream, state: Box<deng::DeflateState>) {
    let handle = Box::new(DeflateHandle {
        state,
        msg: Vec::new(),
    });
    strm.total_in = handle.state.total_in as uLong;
    strm.total_out = handle.state.total_out as uLong;
    strm.data_type = handle.state.data_type;
    strm.adler = handle.state.adler as uLong;
    strm.msg = core::ptr::null();
    // Hand ownership of the heap state to the C `z_stream`; reclaimed by
    // `deflateEnd`.
    strm.state = Box::into_raw(handle) as *mut c_void;
}

/// Borrow the [`DeflateHandle`] attached to `strm`, or `None` if `strm` or its
/// `state` is null.
///
/// # Safety
///
/// `strm`, when non-null, must point to a valid [`z_stream`] whose `state` (if
/// non-null) was produced by an earlier `deflate*Init*` call in this module.
#[inline]
unsafe fn deflate_handle<'a>(strm: z_streamp) -> Option<(&'a mut z_stream, &'a mut DeflateHandle)> {
    if strm.is_null() {
        return None;
    }
    // SAFETY: caller guarantees `strm` points to a valid `z_stream`.
    let strm = unsafe { &mut *strm };
    if strm.state.is_null() {
        return None;
    }
    // SAFETY: a non-null `state` was set by `attach_deflate` to a
    // `Box::into_raw(DeflateHandle)`; no other deflate stream aliases it.
    let handle = unsafe { &mut *(strm.state as *mut DeflateHandle) };
    Some((strm, handle))
}

// ---------------------------------------------------------------------------
// Phase C — deflate (deflate.c public API)
// ---------------------------------------------------------------------------

/// C `deflateInit_` — initialise a deflate stream at `level` with the default
/// method/window/memory/strategy (zlib wrapper). The header-only C
/// `deflateInit` macro forwards here with `ZLIB_VERSION` and
/// `sizeof(z_stream)`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn deflateInit_(
    strm: z_streamp,
    level: c_int,
    version: *const c_char,
    stream_size: c_int,
) -> c_int {
    // SAFETY: `version` is a NUL-terminated C string per the C contract.
    if !unsafe { version_compatible(version, stream_size) } {
        return Z_VERSION_ERROR;
    }
    if strm.is_null() {
        return Z_STREAM_ERROR;
    }
    // SAFETY: caller guarantees `strm` points to a valid `z_stream`.
    let strm = unsafe { &mut *strm };
    match deng::deflate_init(level) {
        Ok(state) => {
            attach_deflate(strm, state);
            Z_OK
        }
        Err(e) => e.as_c_int(),
    }
}

/// C `deflateInit2_` — initialise a deflate stream with full control over
/// `level`, `method`, `windowBits`, `memLevel`, and `strategy`. The header-only
/// C `deflateInit2` macro forwards here.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn deflateInit2_(
    strm: z_streamp,
    level: c_int,
    method: c_int,
    windowBits: c_int,
    memLevel: c_int,
    strategy: c_int,
    version: *const c_char,
    stream_size: c_int,
) -> c_int {
    // SAFETY: `version` is a NUL-terminated C string per the C contract.
    if !unsafe { version_compatible(version, stream_size) } {
        return Z_VERSION_ERROR;
    }
    if strm.is_null() {
        return Z_STREAM_ERROR;
    }
    // SAFETY: caller guarantees `strm` points to a valid `z_stream`.
    let strm = unsafe { &mut *strm };
    match deng::deflate_init2(level, method, windowBits, memLevel, strategy) {
        Ok(state) => {
            attach_deflate(strm, state);
            Z_OK
        }
        Err(e) => e.as_c_int(),
    }
}

/// C `deflate` — compress `avail_in` bytes at `next_in` into `avail_out` bytes
/// at `next_out`, applying `flush`. Advances the cursors and updates
/// `total_in`/`total_out`/`adler`/`data_type`/`msg`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn deflate(strm: z_streamp, flush: c_int) -> c_int {
    // Guard the engine call: a panic must never unwind across the C ABI (under
    // the abort-on-unwind rules it would otherwise terminate the host process).
    // `guard_call` maps any panic to `Z_STREAM_ERROR`.
    guard_call(move || {
        // SAFETY: delegated; `deflate_handle` performs the null/validity checks.
        let Some((strm, handle)) = (unsafe { deflate_handle(strm) }) else {
            return Z_STREAM_ERROR;
        };
        let Some(flush_mode) = flush_from_c(flush) else {
            return Z_STREAM_ERROR;
        };

        // SAFETY: `next_in`/`avail_in` and `next_out`/`avail_out` describe the
        // caller's buffers per the zlib contract.
        let input = unsafe { input_slice(strm.next_in, strm.avail_in) };
        let output = unsafe { output_slice(strm.next_out, strm.avail_out) };

        let mut stream = deng::state::DeflateStream {
            state: &mut handle.state,
            input,
            in_next: 0,
            output,
            out_next: 0,
        };
        let result = deng::deflate(&mut stream, flush_mode);
        let consumed = stream.in_next as uInt;
        let produced = stream.out_next as uInt;

        // Advance the public cursors and refresh the reported counters from the
        // engine state (which maintains `total_in`/`total_out` itself).
        // SAFETY: `consumed`/`produced` never exceed the slice lengths the
        // engine was given, so the pointer offsets stay within the buffers.
        strm.next_in = unsafe { strm.next_in.add(consumed as usize) };
        strm.avail_in -= consumed;
        strm.next_out = unsafe { strm.next_out.add(produced as usize) };
        strm.avail_out -= produced;
        strm.total_in = handle.state.total_in as uLong;
        strm.total_out = handle.state.total_out as uLong;
        strm.adler = handle.state.adler as uLong;
        strm.data_type = handle.state.data_type;
        let msg = handle.state.msg;
        store_msg(strm, &mut handle.msg, msg);

        result_to_code(result)
    })
}

/// C `deflateEnd` — free the deflate stream's state. Returns `Z_DATA_ERROR` if
/// the stream was freed mid-block (data discarded), else `Z_OK`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn deflateEnd(strm: z_streamp) -> c_int {
    // SAFETY: `deflate_handle` performs the null/validity checks.
    let Some((strm, _handle)) = (unsafe { deflate_handle(strm) }) else {
        return Z_STREAM_ERROR;
    };
    // SAFETY: `state` is a live `Box::into_raw(DeflateHandle)`; reclaim it.
    let handle = unsafe { Box::from_raw(strm.state as *mut DeflateHandle) };
    let status = deng::deflate_end(&handle.state);
    strm.state = core::ptr::null_mut();
    drop(handle);
    result_to_code(status)
}

/// C `deflateReset` — reset the stream to start a fresh deflate, keeping the
/// allocated buffers and configuration.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn deflateReset(strm: z_streamp) -> c_int {
    // SAFETY: `deflate_handle` performs the null/validity checks.
    let Some((strm, handle)) = (unsafe { deflate_handle(strm) }) else {
        return Z_STREAM_ERROR;
    };
    let status = deng::deflate_reset(&mut handle.state);
    strm.total_in = 0;
    strm.total_out = 0;
    strm.data_type = handle.state.data_type;
    strm.adler = handle.state.adler as uLong;
    strm.msg = core::ptr::null();
    result_to_code(status)
}

/// C `deflateResetKeep` — partial reset that keeps the LZ match state (used
/// when only the output framing is restarted).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn deflateResetKeep(strm: z_streamp) -> c_int {
    // SAFETY: `deflate_handle` performs the null/validity checks.
    let Some((strm, handle)) = (unsafe { deflate_handle(strm) }) else {
        return Z_STREAM_ERROR;
    };
    deng::deflate_reset_keep(&mut handle.state);
    strm.total_in = 0;
    strm.total_out = 0;
    strm.data_type = handle.state.data_type;
    strm.adler = handle.state.adler as uLong;
    strm.msg = core::ptr::null();
    Z_OK
}

/// C `deflateParams` — dynamically change `level`/`strategy`, flushing any
/// pending block into `next_out` first if required.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn deflateParams(strm: z_streamp, level: c_int, strategy: c_int) -> c_int {
    // SAFETY: `deflate_handle` performs the null/validity checks.
    let Some((strm, handle)) = (unsafe { deflate_handle(strm) }) else {
        return Z_STREAM_ERROR;
    };
    // SAFETY: the buffers are described by the zlib contract.
    let input = unsafe { input_slice(strm.next_in, strm.avail_in) };
    let output = unsafe { output_slice(strm.next_out, strm.avail_out) };
    let mut stream = deng::state::DeflateStream {
        state: &mut handle.state,
        input,
        in_next: 0,
        output,
        out_next: 0,
    };
    let result = deng::deflate_params(&mut stream, level, strategy);
    let consumed = stream.in_next as uInt;
    let produced = stream.out_next as uInt;
    // SAFETY: counts stay within the caller's buffers (see `deflate`).
    strm.next_in = unsafe { strm.next_in.add(consumed as usize) };
    strm.avail_in -= consumed;
    strm.next_out = unsafe { strm.next_out.add(produced as usize) };
    strm.avail_out -= produced;
    strm.total_in = handle.state.total_in as uLong;
    strm.total_out = handle.state.total_out as uLong;
    strm.adler = handle.state.adler as uLong;
    strm.data_type = handle.state.data_type;
    result_to_code(result)
}

/// C `deflateSetDictionary` — install a preset compression dictionary before
/// the first `deflate` call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn deflateSetDictionary(
    strm: z_streamp,
    dictionary: *const Bytef,
    dictLength: uInt,
) -> c_int {
    // SAFETY: `deflate_handle` performs the null/validity checks.
    let Some((strm, handle)) = (unsafe { deflate_handle(strm) }) else {
        return Z_STREAM_ERROR;
    };
    if dictionary.is_null() {
        return Z_STREAM_ERROR;
    }
    // SAFETY: `dictionary` is valid for `dictLength` bytes per the contract.
    let dict = unsafe { core::slice::from_raw_parts(dictionary, dictLength as usize) };
    let result = deng::deflate_set_dictionary(&mut handle.state, dict);
    strm.adler = handle.state.adler as uLong;
    result_to_code(result)
}

/// C `deflateGetDictionary` — copy the current sliding-window history into
/// `dictionary` (when non-null) and report its length in `*dictLength`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn deflateGetDictionary(
    strm: z_streamp,
    dictionary: *mut Bytef,
    dictLength: *mut uInt,
) -> c_int {
    // SAFETY: `deflate_handle` performs the null/validity checks.
    let Some((_strm, handle)) = (unsafe { deflate_handle(strm) }) else {
        return Z_STREAM_ERROR;
    };
    // First obtain the length without copying.
    let (_rc, len) = deng::deflate_get_dictionary(&handle.state, None);
    if !dictionary.is_null() {
        // SAFETY: the caller guarantees at least `len` bytes at `dictionary`
        // (zlib documents a 32768-byte buffer); the engine copies exactly `len`.
        let out = unsafe { core::slice::from_raw_parts_mut(dictionary, len) };
        deng::deflate_get_dictionary(&handle.state, Some(out));
    }
    if !dictLength.is_null() {
        // SAFETY: `dictLength` is a valid writable `uInt` per the contract.
        unsafe { *dictLength = len as uInt };
    }
    Z_OK
}

/// C `deflateCopy` — duplicate `source`'s complete state into `dest`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn deflateCopy(dest: z_streamp, source: z_streamp) -> c_int {
    if dest.is_null() {
        return Z_STREAM_ERROR;
    }
    // SAFETY: `deflate_handle` performs the source null/validity checks.
    let Some((src_strm, src_handle)) = (unsafe { deflate_handle(source) }) else {
        return Z_STREAM_ERROR;
    };
    // SAFETY: caller guarantees `dest` points to a valid `z_stream`.
    let dest_strm = unsafe { &mut *dest };
    let cloned = deng::deflate_copy(&src_handle.state);
    let handle = Box::new(DeflateHandle {
        state: cloned,
        msg: Vec::new(),
    });
    // Mirror the public scalar fields from the source onto the destination.
    dest_strm.total_in = src_strm.total_in;
    dest_strm.total_out = src_strm.total_out;
    dest_strm.adler = src_strm.adler;
    dest_strm.data_type = src_strm.data_type;
    dest_strm.msg = core::ptr::null();
    dest_strm.zalloc = src_strm.zalloc;
    dest_strm.zfree = src_strm.zfree;
    dest_strm.opaque = src_strm.opaque;
    dest_strm.state = Box::into_raw(handle) as *mut c_void;
    Z_OK
}

/// C `deflateBound` — upper bound on the compressed size of `sourceLen` bytes
/// for this stream's configuration. Works even on an uninitialised stream.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn deflateBound(strm: z_streamp, sourceLen: uLong) -> uLong {
    // SAFETY: `deflate_handle` tolerates a null/uninitialised stream.
    let bound = match unsafe { deflate_handle(strm) } {
        Some((_strm, handle)) => deng::deflate_bound(Some(&handle.state), sourceLen as u64),
        None => deng::deflate_bound(None, sourceLen as u64),
    };
    bound as uLong
}

/// C `deflateBound_z` — `size_t`-typed [`deflateBound`] (zlib 1.3.2 ABI).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn deflateBound_z(strm: z_streamp, sourceLen: z_size_t) -> z_size_t {
    // SAFETY: `deflate_handle` tolerates a null/uninitialised stream.
    let bound = match unsafe { deflate_handle(strm) } {
        Some((_strm, handle)) => deng::deflate_bound(Some(&handle.state), sourceLen as u64),
        None => deng::deflate_bound(None, sourceLen as u64),
    };
    bound as z_size_t
}

/// C `deflatePending` — report bytes/bits generated but not yet flushed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn deflatePending(
    strm: z_streamp,
    pending: *mut c_uint,
    bits: *mut c_int,
) -> c_int {
    // SAFETY: `deflate_handle` performs the null/validity checks.
    let Some((_strm, handle)) = (unsafe { deflate_handle(strm) }) else {
        return Z_STREAM_ERROR;
    };
    let (p, b) = deng::deflate_pending(&handle.state);
    if !pending.is_null() {
        // SAFETY: `pending` is a valid writable `unsigned` per the contract.
        unsafe { *pending = p as c_uint };
    }
    if !bits.is_null() {
        // SAFETY: `bits` is a valid writable `int` per the contract.
        unsafe { *bits = b };
    }
    Z_OK
}

/// C `deflateUsed` — report the bit count used in the last byte at the most
/// recent byte-boundary flush (`1..=8`, or `0` if none yet).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn deflateUsed(strm: z_streamp, bits: *mut c_int) -> c_int {
    // SAFETY: `deflate_handle` performs the null/validity checks.
    let Some((_strm, handle)) = (unsafe { deflate_handle(strm) }) else {
        return Z_STREAM_ERROR;
    };
    if bits.is_null() {
        return Z_STREAM_ERROR;
    }
    // SAFETY: `bits` is a valid writable `int` per the contract.
    unsafe { *bits = deng::deflate_used(&handle.state) };
    Z_OK
}

/// C `deflatePrime` — insert `bits` low-order bits of `value` into the output.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn deflatePrime(strm: z_streamp, bits: c_int, value: c_int) -> c_int {
    // SAFETY: `deflate_handle` performs the null/validity checks.
    let Some((_strm, handle)) = (unsafe { deflate_handle(strm) }) else {
        return Z_STREAM_ERROR;
    };
    result_to_code(deng::deflate_prime(&mut handle.state, bits, value))
}

/// C `deflateTune` — fine-tune the internal LZ match-search parameters.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn deflateTune(
    strm: z_streamp,
    good_length: c_int,
    max_lazy: c_int,
    nice_length: c_int,
    max_chain: c_int,
) -> c_int {
    // SAFETY: `deflate_handle` performs the null/validity checks.
    let Some((_strm, handle)) = (unsafe { deflate_handle(strm) }) else {
        return Z_STREAM_ERROR;
    };
    result_to_code(deng::deflate_tune(
        &mut handle.state,
        good_length,
        max_lazy,
        nice_length,
        max_chain,
    ))
}

/// Read a NUL-terminated C byte string into an owned `Vec<u8>` (excluding the
/// terminator), or `None` if `ptr` is null.
///
/// # Safety
///
/// `ptr`, when non-null, must point to a NUL-terminated buffer that remains
/// valid for the duration of the scan (the zlib `deflateSetHeader` contract,
/// which documents `name`/`comment` as C strings).
#[cfg(feature = "gzip")]
unsafe fn cstr_to_vec(ptr: *const Bytef) -> Option<Vec<u8>> {
    if ptr.is_null() {
        return None;
    }
    let mut len = 0usize;
    // SAFETY: the caller guarantees a NUL terminator is reachable from `ptr`,
    // so every `ptr.add(len)` read up to and including the terminator is in
    // bounds of the same allocation.
    while unsafe { *ptr.add(len) } != 0 {
        len += 1;
    }
    // SAFETY: `ptr` is valid for the `len` non-NUL bytes just scanned.
    let slice = unsafe { core::slice::from_raw_parts(ptr, len) };
    Some(slice.to_vec())
}

/// Convert a C `gz_header` into an owned Rust [`GzHeader`] for the write side
/// (`deflateSetHeader`). The `extra` field is captured as `extra_len` raw
/// bytes; `name`/`comment` are captured as NUL-terminated C strings, matching
/// the C `deflateSetHeader` contract.
///
/// # Safety
///
/// `head`'s `extra` pointer (when non-null) must be valid for `extra_len`
/// bytes, and its `name`/`comment` pointers (when non-null) must reference
/// NUL-terminated C strings.
#[cfg(feature = "gzip")]
unsafe fn gz_header_to_rust(head: &gz_header) -> GzHeader {
    let extra = if head.extra.is_null() {
        None
    } else {
        // SAFETY: `extra` is valid for `extra_len` bytes per the C contract.
        let slice = unsafe { core::slice::from_raw_parts(head.extra, head.extra_len as usize) };
        Some(slice.to_vec())
    };
    // SAFETY: `name`/`comment` are NUL-terminated C strings when non-null.
    let name = unsafe { cstr_to_vec(head.name) };
    // SAFETY: see above.
    let comment = unsafe { cstr_to_vec(head.comment) };
    GzHeader {
        text: head.text != 0,
        time: head.time as u32,
        xflags: head.xflags,
        os: head.os,
        extra,
        name,
        comment,
        hcrc: head.hcrc != 0,
        done: head.done != 0,
        extra_max: head.extra_max as usize,
        name_max: head.name_max as usize,
        comm_max: head.comm_max as usize,
    }
}

/// C `deflateSetHeader` — supply gzip header metadata for a gzip-wrapped
/// stream, before the first `deflate`. Only meaningful with the `gzip` feature.
#[cfg(feature = "gzip")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn deflateSetHeader(strm: z_streamp, head: gz_headerp) -> c_int {
    // SAFETY: `deflate_handle` performs the null/validity checks.
    let Some((_strm, handle)) = (unsafe { deflate_handle(strm) }) else {
        return Z_STREAM_ERROR;
    };
    if head.is_null() {
        return Z_STREAM_ERROR;
    }
    // SAFETY: `head` points to a valid `gz_header` per the contract.
    let rust_head = unsafe { gz_header_to_rust(&*head) };
    result_to_code(deng::deflate_set_header(&mut handle.state, rust_head))
}

// ---------------------------------------------------------------------------
// Phase C — inflate (inflate.c public API)
// ---------------------------------------------------------------------------

/// Box a freshly initialised [`InflateState`](crate::inflate::InflateState),
/// attach it to `strm.state`, and seed the public `z_stream` fields.
fn attach_inflate(strm: &mut z_stream, state: Box<ieng::InflateState>) {
    let handle = Box::new(InflateHandle {
        state,
        head: core::ptr::null_mut(),
        msg: Vec::new(),
    });
    strm.total_in = 0;
    strm.total_out = 0;
    strm.msg = core::ptr::null();
    strm.adler = 0;
    // Hand ownership of the heap state to the C `z_stream`; reclaimed by
    // `inflateEnd`.
    strm.state = Box::into_raw(handle) as *mut c_void;
}

/// Borrow the [`InflateHandle`] attached to `strm`, or `None` if `strm` or its
/// `state` is null.
///
/// # Safety
///
/// `strm`, when non-null, must point to a valid [`z_stream`] whose `state` (if
/// non-null) was produced by an earlier `inflate*Init*` call in this module.
#[inline]
unsafe fn inflate_handle<'a>(strm: z_streamp) -> Option<(&'a mut z_stream, &'a mut InflateHandle)> {
    if strm.is_null() {
        return None;
    }
    // SAFETY: caller guarantees `strm` points to a valid `z_stream`.
    let strm = unsafe { &mut *strm };
    if strm.state.is_null() {
        return None;
    }
    // SAFETY: a non-null `state` was set by `attach_inflate` to a
    // `Box::into_raw(InflateHandle)`; no other inflate stream aliases it.
    let handle = unsafe { &mut *(strm.state as *mut InflateHandle) };
    Some((strm, handle))
}

/// C `inflateInit_` — initialise an inflate stream with the default window
/// (auto-detecting zlib/gzip wrappers). The header-only C `inflateInit` macro
/// forwards here.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn inflateInit_(
    strm: z_streamp,
    version: *const c_char,
    stream_size: c_int,
) -> c_int {
    // SAFETY: `version` is a NUL-terminated C string per the C contract.
    if !unsafe { version_compatible(version, stream_size) } {
        return Z_VERSION_ERROR;
    }
    if strm.is_null() {
        return Z_STREAM_ERROR;
    }
    // SAFETY: caller guarantees `strm` points to a valid `z_stream`.
    let strm = unsafe { &mut *strm };
    match ieng::inflate_init() {
        Ok(state) => {
            attach_inflate(strm, state);
            Z_OK
        }
        Err(code) => code,
    }
}

/// C `inflateInit2_` — initialise an inflate stream selecting the wrapper and
/// window size via `windowBits` (8–15 zlib, −8…−15 raw, 24–31 gzip, 40–47
/// auto-detect). The header-only C `inflateInit2` macro forwards here.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn inflateInit2_(
    strm: z_streamp,
    windowBits: c_int,
    version: *const c_char,
    stream_size: c_int,
) -> c_int {
    // SAFETY: `version` is a NUL-terminated C string per the C contract.
    if !unsafe { version_compatible(version, stream_size) } {
        return Z_VERSION_ERROR;
    }
    if strm.is_null() {
        return Z_STREAM_ERROR;
    }
    // SAFETY: caller guarantees `strm` points to a valid `z_stream`.
    let strm = unsafe { &mut *strm };
    match ieng::inflate_init2(windowBits) {
        Ok(state) => {
            attach_inflate(strm, state);
            Z_OK
        }
        Err(code) => code,
    }
}

/// C `inflate` — decompress `avail_in` bytes at `next_in` into `avail_out`
/// bytes at `next_out`, applying `flush`. Advances the cursors and updates the
/// running counters/check value, mirroring any requested gzip header.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn inflate(strm: z_streamp, flush: c_int) -> c_int {
    // Guard the engine call: a panic must never unwind across the C ABI.
    // `guard_call` maps any panic to `Z_STREAM_ERROR`.
    guard_call(move || {
        // SAFETY: `inflate_handle` performs the null/validity checks.
        let Some((strm, handle)) = (unsafe { inflate_handle(strm) }) else {
            return Z_STREAM_ERROR;
        };

        // SAFETY: the buffers are described by `next_in`/`avail_in` and
        // `next_out`/`avail_out` per the zlib contract.
        let input = unsafe { input_slice(strm.next_in, strm.avail_in) };
        let output = unsafe { output_slice(strm.next_out, strm.avail_out) };

        let outcome = ieng::inflate(&mut handle.state, input, output, flush);

        let consumed = outcome.in_consumed as uInt;
        let produced = outcome.out_produced as uInt;
        // SAFETY: `consumed`/`produced` never exceed the slice lengths
        // supplied, so the pointer offsets remain within the caller's buffers.
        strm.next_in = unsafe { strm.next_in.add(consumed as usize) };
        strm.avail_in -= consumed;
        strm.next_out = unsafe { strm.next_out.add(produced as usize) };
        strm.avail_out -= produced;
        strm.total_in = strm.total_in.wrapping_add(outcome.total_in_delta as uLong);
        strm.total_out = strm
            .total_out
            .wrapping_add(outcome.total_out_delta as uLong);
        strm.adler = outcome.adler as uLong;
        strm.data_type = outcome.data_type;
        store_msg(strm, &mut handle.msg, outcome.msg);

        // Mirror any captured gzip header into the caller's registered struct.
        #[cfg(feature = "gzip")]
        if !handle.head.is_null() {
            // SAFETY: `handle.head` was validated non-null and stored by
            // `inflateGetHeader`; the caller guarantees it stays valid.
            unsafe { sync_inflate_header(handle) };
        }

        outcome.ret
    })
}

/// C `inflateEnd` — free the inflate stream's state.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn inflateEnd(strm: z_streamp) -> c_int {
    // SAFETY: `inflate_handle` performs the null/validity checks.
    let Some((strm, _handle)) = (unsafe { inflate_handle(strm) }) else {
        return Z_STREAM_ERROR;
    };
    // SAFETY: `state` is a live `Box::into_raw(InflateHandle)`; reclaim it and
    // move the engine state out so `inflate_end` can consume it.
    let handle = *unsafe { Box::from_raw(strm.state as *mut InflateHandle) };
    let ret = ieng::inflate_end(handle.state);
    strm.state = core::ptr::null_mut();
    ret
}

/// C `inflateReset` — reset the stream to start a fresh inflate, keeping the
/// allocated window and configuration.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn inflateReset(strm: z_streamp) -> c_int {
    // SAFETY: `inflate_handle` performs the null/validity checks.
    let Some((strm, handle)) = (unsafe { inflate_handle(strm) }) else {
        return Z_STREAM_ERROR;
    };
    let mut adler = strm.adler as u32;
    let ret = ieng::inflate_reset(&mut handle.state, &mut adler);
    strm.adler = adler as uLong;
    strm.total_in = 0;
    strm.total_out = 0;
    strm.msg = core::ptr::null();
    ret
}

/// C `inflateReset2` — like [`inflateReset`] but also reselects the wrapper and
/// window size via `windowBits`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn inflateReset2(strm: z_streamp, windowBits: c_int) -> c_int {
    // SAFETY: `inflate_handle` performs the null/validity checks.
    let Some((strm, handle)) = (unsafe { inflate_handle(strm) }) else {
        return Z_STREAM_ERROR;
    };
    let mut adler = strm.adler as u32;
    let ret = ieng::inflate_reset2(&mut handle.state, windowBits, &mut adler);
    strm.adler = adler as uLong;
    if ret == Z_OK {
        strm.total_in = 0;
        strm.total_out = 0;
        strm.msg = core::ptr::null();
    }
    ret
}

/// C `inflateResetKeep` — partial reset that retains the decode history.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn inflateResetKeep(strm: z_streamp) -> c_int {
    // SAFETY: `inflate_handle` performs the null/validity checks.
    let Some((strm, handle)) = (unsafe { inflate_handle(strm) }) else {
        return Z_STREAM_ERROR;
    };
    let mut adler = strm.adler as u32;
    let ret = ieng::inflate_reset_keep(&mut handle.state, &mut adler);
    strm.adler = adler as uLong;
    ret
}

/// C `inflatePrime` — insert `bits` low-order bits of `value` into the input
/// bit accumulator (used to resume from a mid-byte position).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn inflatePrime(strm: z_streamp, bits: c_int, value: c_int) -> c_int {
    // SAFETY: `inflate_handle` performs the null/validity checks.
    let Some((_strm, handle)) = (unsafe { inflate_handle(strm) }) else {
        return Z_STREAM_ERROR;
    };
    ieng::inflate_prime(&mut handle.state, bits, value)
}

/// C `inflateSetDictionary` — install a preset decompression dictionary
/// (during the `Z_NEED_DICT` handshake, or at any point for a raw stream).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn inflateSetDictionary(
    strm: z_streamp,
    dictionary: *const Bytef,
    dictLength: uInt,
) -> c_int {
    // SAFETY: `inflate_handle` performs the null/validity checks.
    let Some((_strm, handle)) = (unsafe { inflate_handle(strm) }) else {
        return Z_STREAM_ERROR;
    };
    if dictionary.is_null() {
        return Z_STREAM_ERROR;
    }
    // SAFETY: `dictionary` is valid for `dictLength` bytes per the contract.
    let dict = unsafe { core::slice::from_raw_parts(dictionary, dictLength as usize) };
    ieng::inflate_set_dictionary(&mut handle.state, dict)
}

/// C `inflateGetDictionary` — copy the current sliding-window history into
/// `dictionary` (when non-null) and report its length in `*dictLength`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn inflateGetDictionary(
    strm: z_streamp,
    dictionary: *mut Bytef,
    dictLength: *mut uInt,
) -> c_int {
    // SAFETY: `inflate_handle` performs the null/validity checks.
    let Some((_strm, handle)) = (unsafe { inflate_handle(strm) }) else {
        return Z_STREAM_ERROR;
    };
    // The engine returns the available history length (`whave`) and only copies
    // when the destination is large enough; an empty slice yields the length.
    let whave = ieng::inflate_get_dictionary(&handle.state, &mut []);
    if !dictionary.is_null() {
        // SAFETY: the caller guarantees at least `whave` bytes at `dictionary`
        // (zlib documents a 32768-byte buffer); the engine copies exactly that.
        let out = unsafe { core::slice::from_raw_parts_mut(dictionary, whave) };
        ieng::inflate_get_dictionary(&handle.state, out);
    }
    if !dictLength.is_null() {
        // SAFETY: `dictLength` is a valid writable `uInt` per the contract.
        unsafe { *dictLength = whave as uInt };
    }
    Z_OK
}

/// Mirror the engine-captured gzip header (`state.head`) into the C
/// `gz_header` registered by [`inflateGetHeader`].
///
/// # Safety
///
/// `handle.head` must be non-null and point to a valid `gz_header` whose
/// `extra`/`name`/`comment` buffers (when non-null) hold at least
/// `extra_max`/`name_max`/`comm_max` bytes (the `inflateGetHeader` contract).
#[cfg(feature = "gzip")]
unsafe fn sync_inflate_header(handle: &mut InflateHandle) {
    // SAFETY: `handle.head` is non-null (checked by the caller) and points to a
    // valid `gz_header` per the `inflateGetHeader` contract.
    let ch = unsafe { &mut *handle.head };

    // A zlib stream detected while a gzip header was requested: C sets
    // `head->done = -1`. The engine flags this with `state.flags == 0` (a real
    // gzip header always carries the method byte in its low 8 bits, so it is
    // never 0); see `inflate.c` / the engine HEAD handling.
    if handle.state.flags == 0 {
        ch.done = -1;
        return;
    }

    let Some(rh) = handle.state.head.as_ref() else {
        return;
    };

    ch.text = c_int::from(rh.text);
    ch.time = rh.time as uLong;
    ch.xflags = rh.xflags;
    ch.os = rh.os;
    ch.hcrc = c_int::from(rh.hcrc);

    if let Some(extra) = rh.extra.as_ref() {
        ch.extra_len = extra.len() as uInt;
        if !ch.extra.is_null() && ch.extra_max > 0 {
            let n = core::cmp::min(extra.len(), ch.extra_max as usize);
            // SAFETY: `ch.extra` is valid for `ch.extra_max >= n` bytes.
            unsafe { core::ptr::copy_nonoverlapping(extra.as_ptr(), ch.extra, n) };
        }
    }

    if let Some(name) = rh.name.as_ref() {
        if !ch.name.is_null() && ch.name_max > 0 {
            let n = core::cmp::min(name.len(), (ch.name_max as usize).saturating_sub(1));
            // SAFETY: `ch.name` is valid for `ch.name_max` bytes; we write `n`
            // bytes plus a terminating NUL, and `n + 1 <= name_max`.
            unsafe {
                core::ptr::copy_nonoverlapping(name.as_ptr(), ch.name, n);
                *ch.name.add(n) = 0;
            }
        }
    }

    if let Some(comment) = rh.comment.as_ref() {
        if !ch.comment.is_null() && ch.comm_max > 0 {
            let n = core::cmp::min(comment.len(), (ch.comm_max as usize).saturating_sub(1));
            // SAFETY: `ch.comment` is valid for `ch.comm_max` bytes; we write
            // `n` bytes plus a terminating NUL, and `n + 1 <= comm_max`.
            unsafe {
                core::ptr::copy_nonoverlapping(comment.as_ptr(), ch.comment, n);
                *ch.comment.add(n) = 0;
            }
        }
    }

    ch.done = c_int::from(rh.done);
}

/// C `inflateGetHeader` — request that the next gzip header be captured into
/// the caller-supplied `head` (only valid when the stream may be gzip-wrapped).
#[cfg(feature = "gzip")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn inflateGetHeader(strm: z_streamp, head: gz_headerp) -> c_int {
    // SAFETY: `inflate_handle` performs the null/validity checks.
    let Some((_strm, handle)) = (unsafe { inflate_handle(strm) }) else {
        return Z_STREAM_ERROR;
    };
    if head.is_null() {
        return Z_STREAM_ERROR;
    }
    let ret = ieng::inflate_get_header(&mut handle.state);
    if ret != Z_OK {
        return ret;
    }
    // Adopt the caller's capture caps and reset the completion flag, mirroring
    // C `inflateGetHeader` (`head->done = 0`).
    // SAFETY: `head` is a valid `gz_header` per the contract.
    let c_head = unsafe { &mut *head };
    if let Some(rh) = handle.state.head.as_mut() {
        rh.extra_max = c_head.extra_max as usize;
        rh.name_max = c_head.name_max as usize;
        rh.comm_max = c_head.comm_max as usize;
    }
    c_head.done = 0;
    handle.head = head;
    Z_OK
}

/// C `inflateSync` — skip invalid compressed data, searching for the next
/// full-flush point (`00 00 FF FF`) to resume decoding after corruption.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn inflateSync(strm: z_streamp) -> c_int {
    // SAFETY: `inflate_handle` performs the null/validity checks.
    let Some((strm, handle)) = (unsafe { inflate_handle(strm) }) else {
        return Z_STREAM_ERROR;
    };
    // SAFETY: the input buffer is described by `next_in`/`avail_in`.
    let input = unsafe { input_slice(strm.next_in, strm.avail_in) };
    let mut adler = strm.adler as u32;
    let (ret, consumed) = ieng::inflate_sync(&mut handle.state, input, &mut adler);
    strm.adler = adler as uLong;
    // SAFETY: `consumed <= input.len() == avail_in`, so the cursor stays in
    // bounds of the caller's buffer.
    strm.next_in = unsafe { strm.next_in.add(consumed) };
    strm.avail_in -= consumed as uInt;
    strm.total_in = strm.total_in.wrapping_add(consumed as uLong);
    ret
}

/// C `inflateSyncPoint` — non-zero if the stream is currently positioned at a
/// byte-aligned full-flush point.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn inflateSyncPoint(strm: z_streamp) -> c_int {
    // SAFETY: `inflate_handle` performs the null/validity checks.
    let Some((_strm, handle)) = (unsafe { inflate_handle(strm) }) else {
        return Z_STREAM_ERROR;
    };
    ieng::inflate_sync_point(&handle.state)
}

/// C `inflateCopy` — duplicate `source`'s complete state into `dest`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn inflateCopy(dest: z_streamp, source: z_streamp) -> c_int {
    if dest.is_null() {
        return Z_STREAM_ERROR;
    }
    // SAFETY: `inflate_handle` performs the source null/validity checks.
    let Some((src_strm, src_handle)) = (unsafe { inflate_handle(source) }) else {
        return Z_STREAM_ERROR;
    };
    // SAFETY: caller guarantees `dest` points to a valid `z_stream`.
    let dest_strm = unsafe { &mut *dest };
    let cloned = Box::new(ieng::inflate_copy(&src_handle.state));
    let handle = Box::new(InflateHandle {
        state: cloned,
        // Mirror C, which copies the registered header pointer verbatim.
        head: src_handle.head,
        msg: Vec::new(),
    });
    dest_strm.total_in = src_strm.total_in;
    dest_strm.total_out = src_strm.total_out;
    dest_strm.adler = src_strm.adler;
    dest_strm.data_type = src_strm.data_type;
    dest_strm.msg = core::ptr::null();
    dest_strm.zalloc = src_strm.zalloc;
    dest_strm.zfree = src_strm.zfree;
    dest_strm.opaque = src_strm.opaque;
    dest_strm.state = Box::into_raw(handle) as *mut c_void;
    Z_OK
}

/// C `inflateMark` — encode decode progress (`back` bits in the high word, copy
/// length remaining in the low word) for random-access tooling.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn inflateMark(strm: z_streamp) -> c_long {
    // SAFETY: `inflate_handle` performs the null/validity checks.
    let Some((_strm, handle)) = (unsafe { inflate_handle(strm) }) else {
        // C returns `-(1L << 16)` for an invalid stream.
        return -(1 << 16);
    };
    ieng::inflate_mark(&handle.state) as c_long
}

/// C `inflateValidate` — enable (`check != 0`) or disable running check-value
/// validation for a wrapped stream.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn inflateValidate(strm: z_streamp, check: c_int) -> c_int {
    // SAFETY: `inflate_handle` performs the null/validity checks.
    let Some((_strm, handle)) = (unsafe { inflate_handle(strm) }) else {
        return Z_STREAM_ERROR;
    };
    ieng::inflate_validate(&mut handle.state, check)
}

/// C `inflateCodesUsed` — number of Huffman code-table entries consumed so far.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn inflateCodesUsed(strm: z_streamp) -> c_ulong {
    // SAFETY: `inflate_handle` performs the null/validity checks.
    let Some((_strm, handle)) = (unsafe { inflate_handle(strm) }) else {
        return c_ulong::MAX;
    };
    ieng::inflate_codes_used(&handle.state) as c_ulong
}

/// C `inflateUndermine` — (debug build hook) request tolerance of invalid
/// distances; in a normal build this returns `Z_DATA_ERROR` like C.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn inflateUndermine(strm: z_streamp, subvert: c_int) -> c_int {
    // SAFETY: `inflate_handle` performs the null/validity checks.
    let Some((_strm, handle)) = (unsafe { inflate_handle(strm) }) else {
        return Z_STREAM_ERROR;
    };
    ieng::inflate_undermine(&mut handle.state, subvert)
}

// ---------------------------------------------------------------------------
// Phase C — inflateBack (infback.c callback-driven API)
// ---------------------------------------------------------------------------

/// Adapter bridging the C `in_func` input callback to the engine's
/// [`BackInput`](crate::inflate::BackInput) trait.
///
/// `inflateBack` uses the stream's `next_in`/`avail_in` as the *first* input
/// chunk and the `in_func` callback for everything after, exactly like C. The
/// initial chunk is handed out once (`fill` returns it, then flips
/// `used_initial`); subsequent `fill` calls invoke the C callback.
struct CBackInput {
    func: in_func,
    desc: *mut c_void,
    initial: *const c_uchar,
    initial_len: usize,
    used_initial: bool,
}

impl ieng::BackInput for CBackInput {
    fn fill(&mut self) -> &[u8] {
        if !self.used_initial {
            self.used_initial = true;
            if self.initial_len != 0 && !self.initial.is_null() {
                // SAFETY: `initial`/`initial_len` come from the stream's
                // `next_in`/`avail_in`, which the caller guarantees describe a
                // valid buffer that outlives this `inflateBack` call.
                return unsafe { core::slice::from_raw_parts(self.initial, self.initial_len) };
            }
        }
        let Some(f) = self.func else {
            return &[];
        };
        let mut buf: *const c_uchar = core::ptr::null();
        // SAFETY: invoking the caller-supplied C input callback per the zlib
        // `in_func` contract; it sets `buf` and returns the available length.
        let len = unsafe { f(self.desc, &mut buf as *mut *const c_uchar) };
        if len == 0 || buf.is_null() {
            return &[];
        }
        // SAFETY: the callback guarantees `buf` points to `len` valid bytes that
        // remain valid until the next callback invocation (the `in_func`
        // contract); the engine consumes the slice before its next `fill`.
        unsafe { core::slice::from_raw_parts(buf, len as usize) }
    }
}

/// Adapter bridging the C `out_func` output callback to the engine's
/// [`BackOutput`](crate::inflate::BackOutput) trait. Returns `true` (failure)
/// when the callback reports a non-zero status or is null.
struct CBackOutput {
    func: out_func,
    desc: *mut c_void,
}

impl ieng::BackOutput for CBackOutput {
    fn write(&mut self, buf: &[u8]) -> bool {
        let Some(f) = self.func else {
            return true;
        };
        // SAFETY: invoking the caller-supplied C output callback per the zlib
        // `out_func` contract. The callback treats the buffer as read-only (the
        // `unsigned char *` is non-`const` only for historical reasons), so
        // casting away `const` on our shared slice is sound.
        let ret = unsafe { f(self.desc, buf.as_ptr() as *mut c_uchar, buf.len() as c_uint) };
        ret != 0
    }
}

/// C `inflateBackInit_` — initialise a stream for callback-driven raw inflate,
/// binding the caller-supplied `window` (`1 << windowBits` bytes). The
/// header-only C `inflateBackInit` macro forwards here.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn inflateBackInit_(
    strm: z_streamp,
    windowBits: c_int,
    window: *mut c_uchar,
    version: *const c_char,
    stream_size: c_int,
) -> c_int {
    // SAFETY: `version` is a NUL-terminated C string per the C contract.
    if !unsafe { version_compatible(version, stream_size) } {
        return Z_VERSION_ERROR;
    }
    if strm.is_null() || window.is_null() {
        return Z_STREAM_ERROR;
    }
    // SAFETY: caller guarantees `strm` points to a valid `z_stream`.
    let strm = unsafe { &mut *strm };
    match ieng::inflate_back_init(windowBits) {
        Ok(state) => {
            let wsize = state.wsize as usize;
            let handle = Box::new(InflateBackHandle {
                state: Box::new(state),
                window: window as *mut Bytef,
                wsize,
            });
            strm.total_in = 0;
            strm.total_out = 0;
            strm.msg = core::ptr::null();
            strm.adler = 0;
            strm.state = Box::into_raw(handle) as *mut c_void;
            Z_OK
        }
        Err(code) => code,
    }
}

/// C `inflateBack` — perform a complete raw inflate in a single call, pulling
/// compressed input via `in`/`in_desc` and pushing decompressed output via
/// `out`/`out_desc`.
///
/// On entry the stream's `next_in`/`avail_in` provide the first input chunk
/// (matching C). On return, `next_in`/`avail_in` are updated to reflect that
/// the supplied input was consumed: the engine drives input purely through the
/// callback model and does not surface a partial-chunk cursor, so any bytes the
/// caller pre-loaded in `next_in` are reported as fully consumed
/// (`avail_in == 0`). Decoded output and the return code are byte-identical to C.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn inflateBack(
    strm: z_streamp,
    in_: in_func,
    in_desc: *mut c_void,
    out: out_func,
    out_desc: *mut c_void,
) -> c_int {
    // Guard the engine call: a panic (including one raised inside the C
    // `in_func`/`out_func` callbacks if they themselves unwind) must never
    // cross the C ABI. `guard_call` maps any panic to `Z_STREAM_ERROR`.
    guard_call(move || {
        if strm.is_null() {
            return Z_STREAM_ERROR;
        }
        // SAFETY: caller guarantees `strm` points to a valid `z_stream`.
        let strm = unsafe { &mut *strm };
        if strm.state.is_null() {
            return Z_STREAM_ERROR;
        }
        // SAFETY: a non-null `state` here was produced by `inflateBackInit_`,
        // which stores a `Box::into_raw(InflateBackHandle)`; nothing else
        // aliases it.
        let handle = unsafe { &mut *(strm.state as *mut InflateBackHandle) };
        if handle.window.is_null() || handle.wsize == 0 {
            return Z_STREAM_ERROR;
        }

        // SAFETY: `window`/`wsize` were captured from a valid
        // `inflateBackInit_` call; the buffer is exactly `wsize` bytes and
        // outlives this call.
        let window = unsafe { core::slice::from_raw_parts_mut(handle.window, handle.wsize) };

        let initial = strm.next_in;
        let initial_len = strm.avail_in as usize;
        let mut cin = CBackInput {
            func: in_,
            desc: in_desc,
            initial,
            initial_len,
            used_initial: false,
        };
        let mut cout = CBackOutput {
            func: out,
            desc: out_desc,
        };

        let ret = ieng::inflate_back(&mut handle.state, window, &mut cin, &mut cout);

        // Bookkeeping write-back (the engine delegates this to the FFI). The
        // pre-loaded input is reported as consumed; see the function docs.
        if initial_len != 0 && !initial.is_null() {
            // SAFETY: advancing `next_in` by `initial_len` keeps it within (or
            // one past the end of) the caller's original input buffer.
            strm.next_in = unsafe { strm.next_in.add(initial_len) };
            strm.total_in = strm.total_in.wrapping_add(initial_len as uLong);
            strm.avail_in = 0;
        }

        ret
    })
}

/// C `inflateBackEnd` — release the state allocated by [`inflateBackInit_`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn inflateBackEnd(strm: z_streamp) -> c_int {
    if strm.is_null() {
        return Z_STREAM_ERROR;
    }
    // SAFETY: caller guarantees `strm` points to a valid `z_stream`.
    let strm = unsafe { &mut *strm };
    if strm.state.is_null() {
        return Z_STREAM_ERROR;
    }
    // SAFETY: `state` is a live `Box::into_raw(InflateBackHandle)`; reclaim it
    // and move the engine state out so `inflate_back_end` can consume it.
    let handle = *unsafe { Box::from_raw(strm.state as *mut InflateBackHandle) };
    let ret = ieng::inflate_back_end(*handle.state);
    strm.state = core::ptr::null_mut();
    ret
}

// ---------------------------------------------------------------------------
// Phase C — checksums (adler32.c / crc32.c)
// ---------------------------------------------------------------------------

/// C `adler32` — update a running Adler-32 with `len` bytes at `buf`. A null
/// `buf` returns the initial value `1` (the zlib `adler32(0, Z_NULL, 0)` idiom).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn adler32(adler: uLong, buf: *const Bytef, len: uInt) -> uLong {
    if buf.is_null() {
        return 1;
    }
    // SAFETY: `buf` is valid for `len` bytes per the C contract.
    let data = unsafe { core::slice::from_raw_parts(buf, len as usize) };
    cksum::adler32(adler as u32, data) as uLong
}

/// C `adler32_z` — [`adler32`] with a `z_size_t` length (zlib 1.2.9+).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn adler32_z(adler: uLong, buf: *const Bytef, len: z_size_t) -> uLong {
    if buf.is_null() {
        return 1;
    }
    // SAFETY: `buf` is valid for `len` bytes per the C contract.
    let data = unsafe { core::slice::from_raw_parts(buf, len) };
    cksum::adler32_z(adler as u32, data) as uLong
}

/// C `adler32_combine` — combine the Adler-32 of two sequences into the
/// checksum of their concatenation, given the second sequence's length.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn adler32_combine(adler1: uLong, adler2: uLong, len2: z_off_t) -> uLong {
    cksum::adler32_combine(adler1 as u32, adler2 as u32, len2 as i64) as uLong
}

/// C `adler32_combine64` — the 64-bit-length variant of [`adler32_combine`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn adler32_combine64(adler1: uLong, adler2: uLong, len2: z_off64_t) -> uLong {
    cksum::adler32_combine64(adler1 as u32, adler2 as u32, len2 as i64) as uLong
}

/// C `crc32` — update a running CRC-32 with `len` bytes at `buf`. A null `buf`
/// returns the initial value `0` (the zlib `crc32(0, Z_NULL, 0)` idiom).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn crc32(crc: uLong, buf: *const Bytef, len: uInt) -> uLong {
    if buf.is_null() {
        return 0;
    }
    // SAFETY: `buf` is valid for `len` bytes per the C contract.
    let data = unsafe { core::slice::from_raw_parts(buf, len as usize) };
    cksum::crc32(crc as u32, data) as uLong
}

/// C `crc32_z` — [`crc32`] with a `z_size_t` length (zlib 1.2.9+).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn crc32_z(crc: uLong, buf: *const Bytef, len: z_size_t) -> uLong {
    if buf.is_null() {
        return 0;
    }
    // SAFETY: `buf` is valid for `len` bytes per the C contract.
    let data = unsafe { core::slice::from_raw_parts(buf, len) };
    cksum::crc32_z(crc as u32, data) as uLong
}

/// C `crc32_combine` — combine two CRC-32 values into the CRC-32 of their
/// concatenation, given the second sequence's length.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn crc32_combine(crc1: uLong, crc2: uLong, len2: z_off_t) -> uLong {
    cksum::crc32_combine(crc1 as u32, crc2 as u32, len2 as i64) as uLong
}

/// C `crc32_combine64` — the 64-bit-length variant of [`crc32_combine`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn crc32_combine64(crc1: uLong, crc2: uLong, len2: z_off64_t) -> uLong {
    cksum::crc32_combine64(crc1 as u32, crc2 as u32, len2 as i64) as uLong
}

/// C `crc32_combine_gen` — precompute the combine *operator* for a given second
/// length, amortizing repeated [`crc32_combine_op`] calls at that length.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn crc32_combine_gen(len2: z_off_t) -> uLong {
    cksum::crc32_combine_gen(len2 as i64) as uLong
}

/// C `crc32_combine_gen64` — the 64-bit-length variant of
/// [`crc32_combine_gen`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn crc32_combine_gen64(len2: z_off64_t) -> uLong {
    cksum::crc32_combine_gen64(len2 as i64) as uLong
}

/// C `crc32_combine_op` — combine two CRC-32 values using a precomputed
/// operator from [`crc32_combine_gen`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn crc32_combine_op(crc1: uLong, crc2: uLong, op: uLong) -> uLong {
    cksum::crc32_combine_op(crc1 as u32, crc2 as u32, op as u32) as uLong
}

/// C `get_crc_table` — return a pointer to the 256-entry CRC-32 lookup table
/// (`const z_crc_t *`), primarily for C-API parity.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn get_crc_table() -> *const z_crc_t {
    cksum::get_crc_table().as_ptr()
}

// ---------------------------------------------------------------------------
// Phase C — one-shot helpers (compress.c / uncompr.c)
// ---------------------------------------------------------------------------

/// C `compress2` — compress `source` into `dest` at `level` in a single call.
/// On entry `*destLen` is the destination capacity; on success it is updated to
/// the compressed length.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn compress2(
    dest: *mut Bytef,
    destLen: *mut uLongf,
    source: *const Bytef,
    sourceLen: uLong,
    level: c_int,
) -> c_int {
    // Guard the engine call: a panic must never unwind across the C ABI.
    guard_call(move || {
        if destLen.is_null() {
            return Z_STREAM_ERROR;
        }
        // SAFETY: `destLen` is a valid in/out `uLong` per the contract.
        let cap = unsafe { *destLen } as usize;
        // SAFETY: `dest` is valid for `cap` bytes and `source` for `sourceLen`
        // bytes per the contract; null buffers degrade to empty slices.
        let dest_slice = unsafe { mut_buf(dest, cap) };
        let src_slice = unsafe { const_buf(source, sourceLen as usize) };
        match ueng::compress2_to_buf(dest_slice, src_slice, level) {
            Ok(produced) => {
                // SAFETY: `destLen` validated non-null above.
                unsafe { *destLen = produced as uLongf };
                Z_OK
            }
            Err(e) => e.as_c_int(),
        }
    })
}

/// C `compress` — [`compress2`] at `Z_DEFAULT_COMPRESSION`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn compress(
    dest: *mut Bytef,
    destLen: *mut uLongf,
    source: *const Bytef,
    sourceLen: uLong,
) -> c_int {
    // Guard the engine call: a panic must never unwind across the C ABI.
    guard_call(move || {
        if destLen.is_null() {
            return Z_STREAM_ERROR;
        }
        // SAFETY: `destLen` is a valid in/out `uLong` per the contract.
        let cap = unsafe { *destLen } as usize;
        // SAFETY: see `compress2`.
        let dest_slice = unsafe { mut_buf(dest, cap) };
        let src_slice = unsafe { const_buf(source, sourceLen as usize) };
        match ueng::compress_to_buf(dest_slice, src_slice) {
            Ok(produced) => {
                // SAFETY: `destLen` validated non-null above.
                unsafe { *destLen = produced as uLongf };
                Z_OK
            }
            Err(e) => e.as_c_int(),
        }
    })
}

/// C `compressBound` — upper bound on the compressed size of `sourceLen` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn compressBound(sourceLen: uLong) -> uLong {
    ueng::compress_bound(sourceLen as usize) as uLong
}

/// C `uncompress` — decompress `source` into `dest` in a single call. On entry
/// `*destLen` is the destination capacity; on success it is updated to the
/// decompressed length.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uncompress(
    dest: *mut Bytef,
    destLen: *mut uLongf,
    source: *const Bytef,
    sourceLen: uLong,
) -> c_int {
    // Guard the engine call: a panic must never unwind across the C ABI.
    guard_call(move || {
        if destLen.is_null() {
            return Z_STREAM_ERROR;
        }
        // SAFETY: `destLen` is a valid in/out `uLong` per the contract.
        let cap = unsafe { *destLen } as usize;
        // SAFETY: see `compress2`.
        let dest_slice = unsafe { mut_buf(dest, cap) };
        let src_slice = unsafe { const_buf(source, sourceLen as usize) };
        match ueng::uncompress_to_buf(dest_slice, src_slice) {
            Ok((produced, _consumed)) => {
                // SAFETY: `destLen` validated non-null above.
                unsafe { *destLen = produced as uLongf };
                Z_OK
            }
            Err(e) => e.as_c_int(),
        }
    })
}

/// C `uncompress2` — like [`uncompress`] but `sourceLen` is an in/out pointer;
/// on return `*sourceLen` is the number of source bytes consumed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uncompress2(
    dest: *mut Bytef,
    destLen: *mut uLongf,
    source: *const Bytef,
    sourceLen: *mut uLong,
) -> c_int {
    // Guard the engine call: a panic must never unwind across the C ABI.
    guard_call(move || {
        if destLen.is_null() || sourceLen.is_null() {
            return Z_STREAM_ERROR;
        }
        // SAFETY: `destLen`/`sourceLen` are valid in/out `uLong`s per the
        // contract.
        let cap = unsafe { *destLen } as usize;
        let src_len = unsafe { *sourceLen } as usize;
        // SAFETY: see `compress2`.
        let dest_slice = unsafe { mut_buf(dest, cap) };
        let src_slice = unsafe { const_buf(source, src_len) };
        match ueng::uncompress2_to_buf(dest_slice, src_slice) {
            Ok((produced, consumed)) => {
                // SAFETY: both pointers validated non-null above.
                unsafe {
                    *destLen = produced as uLongf;
                    *sourceLen = consumed as uLong;
                }
                Z_OK
            }
            Err(e) => e.as_c_int(),
        }
    })
}

// --- `z_size_t`-typed variants (zlib 1.3.2 ABI additions) -------------------

/// C `compress2_z` — `z_size_t`-typed [`compress2`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn compress2_z(
    dest: *mut Bytef,
    destLen: *mut z_size_t,
    source: *const Bytef,
    sourceLen: z_size_t,
    level: c_int,
) -> c_int {
    // Guard the engine call: a panic must never unwind across the C ABI.
    guard_call(move || {
        if destLen.is_null() {
            return Z_STREAM_ERROR;
        }
        // SAFETY: `destLen` is a valid in/out `z_size_t` per the contract.
        let cap = unsafe { *destLen };
        let dest_slice = if dest.is_null() {
            &mut [][..]
        } else {
            // SAFETY: `dest` is valid for `cap` bytes per the C contract; a null
            // pointer is handled above by degrading to an empty slice.
            unsafe { core::slice::from_raw_parts_mut(dest, cap) }
        };
        let src_slice = if source.is_null() {
            &[][..]
        } else {
            // SAFETY: `source` is valid for `sourceLen` bytes per the C
            // contract; a null pointer degrades to an empty slice above.
            unsafe { core::slice::from_raw_parts(source, sourceLen) }
        };
        match ueng::compress2_to_buf(dest_slice, src_slice, level) {
            Ok(produced) => {
                // SAFETY: `destLen` validated non-null above.
                unsafe { *destLen = produced };
                Z_OK
            }
            Err(e) => e.as_c_int(),
        }
    })
}

/// C `compress_z` — `z_size_t`-typed [`compress`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn compress_z(
    dest: *mut Bytef,
    destLen: *mut z_size_t,
    source: *const Bytef,
    sourceLen: z_size_t,
) -> c_int {
    // Guard the engine call: a panic must never unwind across the C ABI.
    guard_call(move || {
        if destLen.is_null() {
            return Z_STREAM_ERROR;
        }
        // SAFETY: `destLen` is a valid in/out `z_size_t` per the contract.
        let cap = unsafe { *destLen };
        let dest_slice = if dest.is_null() {
            &mut [][..]
        } else {
            // SAFETY: `dest` is valid for `cap` bytes per the C contract; a null
            // pointer is handled above by degrading to an empty slice.
            unsafe { core::slice::from_raw_parts_mut(dest, cap) }
        };
        let src_slice = if source.is_null() {
            &[][..]
        } else {
            // SAFETY: `source` is valid for `sourceLen` bytes per the C
            // contract; a null pointer degrades to an empty slice above.
            unsafe { core::slice::from_raw_parts(source, sourceLen) }
        };
        match ueng::compress_to_buf(dest_slice, src_slice) {
            Ok(produced) => {
                // SAFETY: `destLen` validated non-null above.
                unsafe { *destLen = produced };
                Z_OK
            }
            Err(e) => e.as_c_int(),
        }
    })
}

/// C `compressBound_z` — `z_size_t`-typed [`compressBound`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn compressBound_z(sourceLen: z_size_t) -> z_size_t {
    ueng::compress_bound(sourceLen)
}

/// C `uncompress_z` — `z_size_t`-typed [`uncompress`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uncompress_z(
    dest: *mut Bytef,
    destLen: *mut z_size_t,
    source: *const Bytef,
    sourceLen: z_size_t,
) -> c_int {
    // Guard the engine call: a panic must never unwind across the C ABI.
    guard_call(move || {
        if destLen.is_null() {
            return Z_STREAM_ERROR;
        }
        // SAFETY: `destLen` is a valid in/out `z_size_t` per the contract.
        let cap = unsafe { *destLen };
        let dest_slice = if dest.is_null() {
            &mut [][..]
        } else {
            // SAFETY: `dest` is valid for `cap` bytes per the C contract; a null
            // pointer is handled above by degrading to an empty slice.
            unsafe { core::slice::from_raw_parts_mut(dest, cap) }
        };
        let src_slice = if source.is_null() {
            &[][..]
        } else {
            // SAFETY: `source` is valid for `sourceLen` bytes per the C
            // contract; a null pointer degrades to an empty slice above.
            unsafe { core::slice::from_raw_parts(source, sourceLen) }
        };
        match ueng::uncompress_to_buf(dest_slice, src_slice) {
            Ok((produced, _consumed)) => {
                // SAFETY: `destLen` validated non-null above.
                unsafe { *destLen = produced };
                Z_OK
            }
            Err(e) => e.as_c_int(),
        }
    })
}

/// C `uncompress2_z` — `z_size_t`-typed [`uncompress2`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uncompress2_z(
    dest: *mut Bytef,
    destLen: *mut z_size_t,
    source: *const Bytef,
    sourceLen: *mut z_size_t,
) -> c_int {
    // Guard the engine call: a panic must never unwind across the C ABI.
    guard_call(move || {
        if destLen.is_null() || sourceLen.is_null() {
            return Z_STREAM_ERROR;
        }
        // SAFETY: `destLen`/`sourceLen` are valid in/out `z_size_t`s per the
        // contract.
        let cap = unsafe { *destLen };
        let src_len = unsafe { *sourceLen };
        let dest_slice = if dest.is_null() {
            &mut [][..]
        } else {
            // SAFETY: `dest` is valid for `cap` bytes per the C contract; a null
            // pointer is handled above by degrading to an empty slice.
            unsafe { core::slice::from_raw_parts_mut(dest, cap) }
        };
        let src_slice = if source.is_null() {
            &[][..]
        } else {
            // SAFETY: `source` is valid for `src_len` bytes per the C contract;
            // a null pointer degrades to an empty slice above.
            unsafe { core::slice::from_raw_parts(source, src_len) }
        };
        match ueng::uncompress2_to_buf(dest_slice, src_slice) {
            Ok((produced, consumed)) => {
                // SAFETY: both pointers validated non-null above.
                unsafe {
                    *destLen = produced;
                    *sourceLen = consumed;
                }
                Z_OK
            }
            Err(e) => e.as_c_int(),
        }
    })
}

// ---------------------------------------------------------------------------
// Phase C — gzip file I/O (gzlib.c / gzread.c / gzwrite.c / gzclose.c)
//
// Gated behind `gz-io` (which implies `std` + `gzip`). The opaque C `gzFile`
// handle is a `Box::into_raw(GzHandle)`, where [`GzHandle`] wraps the engine's
// `Box<GzState>` plus a NUL-terminated cache backing the string returned by
// `gzerror`.
// ---------------------------------------------------------------------------

/// FFI-side owner of a gzip file: the engine state plus a scratch buffer that
/// backs the `*const c_char` returned by [`gzerror`] (kept alive until the next
/// `gzerror` call on the same handle).
#[cfg(feature = "gz-io")]
struct GzHandle {
    state: Box<GzState>,
    err_cache: Vec<u8>,
}

/// Borrow the [`GzHandle`] behind a `gzFile`, or `None` if the handle is null.
///
/// # Safety
///
/// A non-null `file` must be a handle returned by [`gzopen`]/[`gzopen64`]/
/// [`gzdopen`] and not yet closed.
#[cfg(feature = "gz-io")]
#[inline]
unsafe fn gz_handle<'a>(file: gzFile) -> Option<&'a mut GzHandle> {
    if file.is_null() {
        None
    } else {
        // SAFETY: a non-null `gzFile` was produced as `Box::into_raw(GzHandle)`
        // by an open call; nothing else aliases it for the call's duration.
        Some(unsafe { &mut *(file as *mut GzHandle) })
    }
}

/// Shared `gzopen`/`gzopen64` body: marshal the C path/mode strings and wrap
/// the resulting engine state in a `gzFile`.
///
/// # Safety
///
/// `path`/`mode` must be NUL-terminated C strings (or null).
#[cfg(feature = "gz-io")]
unsafe fn gz_open_path(path: *const c_char, mode: *const c_char, large: bool) -> gzFile {
    if path.is_null() || mode.is_null() {
        return core::ptr::null_mut();
    }
    // SAFETY: `path`/`mode` are NUL-terminated C strings per the contract.
    let path_bytes = unsafe { CStr::from_ptr(path) }.to_bytes();
    let p = Path::new(OsStr::from_bytes(path_bytes));
    // SAFETY: see above.
    let Ok(m) = (unsafe { CStr::from_ptr(mode) }).to_str() else {
        return core::ptr::null_mut();
    };
    let opened = if large {
        geng::gzopen64(p, m)
    } else {
        geng::gzopen(p, m)
    };
    match opened {
        Some(state) => Box::into_raw(Box::new(GzHandle {
            state,
            err_cache: Vec::new(),
        })) as gzFile,
        None => core::ptr::null_mut(),
    }
}

/// C `gzopen` — open the named file for gzip reading/writing per `mode`
/// (e.g. `"rb"`, `"wb9"`). Returns `NULL` on failure.
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzopen(path: *const c_char, mode: *const c_char) -> gzFile {
    // SAFETY: `path`/`mode` are NUL-terminated C strings per the contract.
    unsafe { gz_open_path(path, mode, false) }
}

/// C `gzopen64` — `gzopen` with 64-bit file offsets.
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzopen64(path: *const c_char, mode: *const c_char) -> gzFile {
    // SAFETY: `path`/`mode` are NUL-terminated C strings per the contract.
    unsafe { gz_open_path(path, mode, true) }
}

/// C `gzdopen` — associate a gzip stream with an already-open file descriptor.
/// The descriptor is adopted and closed when the stream is closed.
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzdopen(fd: c_int, mode: *const c_char) -> gzFile {
    if mode.is_null() {
        return core::ptr::null_mut();
    }
    // SAFETY: `mode` is a NUL-terminated C string per the contract.
    let Ok(m) = (unsafe { CStr::from_ptr(mode) }).to_str() else {
        return core::ptr::null_mut();
    };
    // SAFETY: the caller transfers ownership of `fd` to the new stream (the C
    // `gzdopen` contract); `File` adopts it and closes it on teardown.
    let file = unsafe { File::from_raw_fd(fd) };
    match geng::gzdopen(file, m) {
        Some(state) => Box::into_raw(Box::new(GzHandle {
            state,
            err_cache: Vec::new(),
        })) as gzFile,
        None => core::ptr::null_mut(),
    }
}

/// C `gzbuffer` — set the internal buffer size (before the first read/write).
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzbuffer(file: gzFile, size: c_uint) -> c_int {
    // SAFETY: `gz_handle` performs the null check.
    let Some(handle) = (unsafe { gz_handle(file) }) else {
        return -1;
    };
    geng::gzbuffer(&mut handle.state, size as u32)
}

/// C `gzsetparams` — change the compression `level`/`strategy` mid-stream.
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzsetparams(file: gzFile, level: c_int, strategy: c_int) -> c_int {
    // SAFETY: `gz_handle` performs the null check.
    let Some(handle) = (unsafe { gz_handle(file) }) else {
        return Z_STREAM_ERROR;
    };
    geng::gzsetparams(&mut handle.state, level, strategy)
}

/// C `gzrewind` — rewind a read-mode stream to the start of the file.
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzrewind(file: gzFile) -> c_int {
    // SAFETY: `gz_handle` performs the null check.
    let Some(handle) = (unsafe { gz_handle(file) }) else {
        return -1;
    };
    geng::gzrewind(&mut handle.state)
}

/// C `gzseek` — reposition the stream; `whence` is `SEEK_SET`/`SEEK_CUR`.
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzseek(file: gzFile, offset: z_off_t, whence: c_int) -> z_off_t {
    // SAFETY: `gz_handle` performs the null check.
    let Some(handle) = (unsafe { gz_handle(file) }) else {
        return -1;
    };
    geng::gzseek(&mut handle.state, offset as i64, whence) as z_off_t
}

/// C `gzseek64` — [`gzseek`] with a 64-bit offset.
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzseek64(file: gzFile, offset: z_off64_t, whence: c_int) -> z_off64_t {
    // SAFETY: `gz_handle` performs the null check.
    let Some(handle) = (unsafe { gz_handle(file) }) else {
        return -1;
    };
    geng::gzseek64(&mut handle.state, offset, whence) as z_off64_t
}

/// C `gztell` — current (uncompressed) stream position.
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gztell(file: gzFile) -> z_off_t {
    // SAFETY: `gz_handle` performs the null check.
    let Some(handle) = (unsafe { gz_handle(file) }) else {
        return -1;
    };
    geng::gztell(&handle.state) as z_off_t
}

/// C `gztell64` — [`gztell`] as a 64-bit offset.
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gztell64(file: gzFile) -> z_off64_t {
    // SAFETY: `gz_handle` performs the null check.
    let Some(handle) = (unsafe { gz_handle(file) }) else {
        return -1;
    };
    geng::gztell64(&handle.state) as z_off64_t
}

/// C `gzoffset` — current offset into the underlying compressed file.
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzoffset(file: gzFile) -> z_off_t {
    // SAFETY: `gz_handle` performs the null check.
    let Some(handle) = (unsafe { gz_handle(file) }) else {
        return -1;
    };
    geng::gzoffset(&mut handle.state) as z_off_t
}

/// C `gzoffset64` — [`gzoffset`] as a 64-bit offset.
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzoffset64(file: gzFile) -> z_off64_t {
    // SAFETY: `gz_handle` performs the null check.
    let Some(handle) = (unsafe { gz_handle(file) }) else {
        return -1;
    };
    geng::gzoffset64(&mut handle.state) as z_off64_t
}

/// C `gzeof` — non-zero once a read reached end-of-file.
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzeof(file: gzFile) -> c_int {
    // SAFETY: `gz_handle` performs the null check.
    let Some(handle) = (unsafe { gz_handle(file) }) else {
        return 0;
    };
    c_int::from(geng::gzeof(&handle.state))
}

/// C `gzdirect` — non-zero if the stream is being read as raw (non-gzip) data.
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzdirect(file: gzFile) -> c_int {
    // SAFETY: `gz_handle` performs the null check.
    let Some(handle) = (unsafe { gz_handle(file) }) else {
        return 0;
    };
    geng::gzdirect(&mut handle.state)
}

/// C `gzerror` — return the most recent error message for `file` and write its
/// code to `*errnum`. The returned pointer is valid until the next `gzerror`
/// call on the same handle.
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzerror(file: gzFile, errnum: *mut c_int) -> *const c_char {
    // SAFETY: `gz_handle` performs the null check.
    let Some(handle) = (unsafe { gz_handle(file) }) else {
        return core::ptr::null();
    };
    let (code, msg) = geng::gzerror(&handle.state);
    // Copy out of the borrowed state before mutating the cache field.
    let bytes = msg.map(|m| m.as_bytes().to_vec());
    if !errnum.is_null() {
        // SAFETY: `errnum` is a valid writable `int` per the contract.
        unsafe { *errnum = code };
    }
    handle.err_cache.clear();
    if let Some(b) = bytes {
        handle.err_cache.extend_from_slice(&b);
    }
    handle.err_cache.push(0);
    handle.err_cache.as_ptr() as *const c_char
}

/// C `gzclearerr` — clear the latched error/EOF state of `file`.
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzclearerr(file: gzFile) {
    // SAFETY: `gz_handle` performs the null check.
    if let Some(handle) = unsafe { gz_handle(file) } {
        geng::gzclearerr(&mut handle.state);
    }
}

// --- read path (gzread.c) ---------------------------------------------------

/// C `gzread` — read and decompress up to `len` bytes into `buf`. Returns the
/// number of bytes read, `0` at EOF, or `-1` on error.
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzread(file: gzFile, buf: voidp, len: c_uint) -> c_int {
    // SAFETY: `gz_handle` performs the null check.
    let Some(handle) = (unsafe { gz_handle(file) }) else {
        return -1;
    };
    // SAFETY: `buf` is valid for `len` bytes per the contract.
    let data = unsafe { mut_buf(buf as *mut Bytef, len as usize) };
    geng::gzread(&mut handle.state, data)
}

/// C `gzfread` — read `size * nitems` bytes into `buf`; returns the number of
/// full items read. Note the C parameter order places `file` last.
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzfread(
    buf: voidp,
    size: z_size_t,
    nitems: z_size_t,
    file: gzFile,
) -> z_size_t {
    // SAFETY: `gz_handle` performs the null check.
    let Some(handle) = (unsafe { gz_handle(file) }) else {
        return 0;
    };
    let total = size.saturating_mul(nitems);
    // SAFETY: `buf` is valid for `size * nitems` bytes per the contract.
    let data = unsafe { mut_buf(buf as *mut Bytef, total) };
    geng::gzfread(&mut handle.state, size, nitems, data)
}

/// C `gzgetc` — read a single byte, or `-1` at EOF/on error.
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzgetc(file: gzFile) -> c_int {
    // SAFETY: `gz_handle` performs the null check.
    let Some(handle) = (unsafe { gz_handle(file) }) else {
        return -1;
    };
    geng::gzgetc(&mut handle.state)
}

/// C `gzgetc_` — the function form of the `gzgetc` macro (identical behavior).
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzgetc_(file: gzFile) -> c_int {
    // SAFETY: `gz_handle` performs the null check.
    let Some(handle) = (unsafe { gz_handle(file) }) else {
        return -1;
    };
    geng::gzgetc_(&mut handle.state)
}

/// C `gzungetc` — push `c` back so the next read returns it.
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzungetc(c: c_int, file: gzFile) -> c_int {
    // SAFETY: `gz_handle` performs the null check.
    let Some(handle) = (unsafe { gz_handle(file) }) else {
        return -1;
    };
    geng::gzungetc(c, &mut handle.state)
}

/// C `gzgets` — read a NUL-terminated line of at most `len - 1` bytes into
/// `buf`, stopping after a newline. Returns `buf`, or `NULL` at EOF/on error.
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzgets(file: gzFile, buf: *mut c_char, len: c_int) -> *mut c_char {
    if buf.is_null() || len <= 0 {
        return core::ptr::null_mut();
    }
    // SAFETY: `gz_handle` performs the null check.
    let Some(handle) = (unsafe { gz_handle(file) }) else {
        return core::ptr::null_mut();
    };
    // Reserve one byte for the terminating NUL (the engine writes raw bytes).
    let cap = (len - 1) as usize;
    // SAFETY: `buf` is valid for `len` bytes per the contract; we expose `cap`
    // of them to the engine and reserve the last for the NUL.
    let data = unsafe { mut_buf(buf as *mut Bytef, cap) };
    let n = geng::gzgets(&mut handle.state, data);
    if n == 0 {
        return core::ptr::null_mut();
    }
    // SAFETY: `n <= cap`, so index `n` is within the caller's `len`-byte buffer.
    unsafe { *(buf as *mut u8).add(n) = 0 };
    buf
}

// --- write path (gzwrite.c) -------------------------------------------------

/// C `gzwrite` — compress and write `len` bytes from `buf`. Returns the number
/// of uncompressed bytes written, or `0` on error.
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzwrite(file: gzFile, buf: voidpc, len: c_uint) -> c_int {
    // SAFETY: `gz_handle` performs the null check.
    let Some(handle) = (unsafe { gz_handle(file) }) else {
        return 0;
    };
    // SAFETY: `buf` is valid for `len` bytes per the contract.
    let data = unsafe { const_buf(buf as *const Bytef, len as usize) };
    geng::gzwrite(&mut handle.state, data)
}

/// C `gzfwrite` — compress and write `size * nitems` bytes from `buf`; returns
/// the number of full items written. Note the C parameter order places `file`
/// last.
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzfwrite(
    buf: voidpc,
    size: z_size_t,
    nitems: z_size_t,
    file: gzFile,
) -> z_size_t {
    // SAFETY: `gz_handle` performs the null check.
    let Some(handle) = (unsafe { gz_handle(file) }) else {
        return 0;
    };
    let total = size.saturating_mul(nitems);
    // SAFETY: `buf` is valid for `size * nitems` bytes per the contract.
    let data = unsafe { const_buf(buf as *const Bytef, total) };
    geng::gzfwrite(&mut handle.state, size, nitems, data)
}

/// C `gzputc` — compress and write a single byte. Returns the byte, or `-1` on
/// error.
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzputc(file: gzFile, c: c_int) -> c_int {
    // SAFETY: `gz_handle` performs the null check.
    let Some(handle) = (unsafe { gz_handle(file) }) else {
        return -1;
    };
    geng::gzputc(&mut handle.state, c)
}

/// C `gzputs` — compress and write the NUL-terminated string `s`. Returns the
/// number of bytes written, or `-1` on error.
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzputs(file: gzFile, s: *const c_char) -> c_int {
    // SAFETY: `gz_handle` performs the null check.
    let Some(handle) = (unsafe { gz_handle(file) }) else {
        return -1;
    };
    if s.is_null() {
        return -1;
    }
    // SAFETY: `s` is a NUL-terminated C string per the contract.
    let bytes = unsafe { CStr::from_ptr(s) }.to_bytes();
    geng::gzputs(&mut handle.state, bytes)
}

/// C `gzprintf` — formatted write. C's variadic `...` is not expressible as a
/// safe stable-Rust `extern "C"` signature, so this best-effort shim writes the
/// `format` string verbatim (no `%`-substitution); the exported symbol and the
/// fixed `(gzFile, const char *)` prefix match zlib so the drop-in links and
/// the common no-argument case behaves identically.
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzprintf(file: gzFile, format: *const c_char) -> c_int {
    // SAFETY: `gz_handle` performs the null check.
    let Some(handle) = (unsafe { gz_handle(file) }) else {
        return -1;
    };
    if format.is_null() {
        return 0;
    }
    // SAFETY: `format` is a NUL-terminated C string per the contract.
    let s = unsafe { CStr::from_ptr(format) }.to_string_lossy();
    geng::gzprintf(&mut handle.state, format_args!("{s}"))
}

/// C `gzvprintf` — the `va_list` form of [`gzprintf`]. The same stable-Rust
/// variadic limitation applies: the `va_list` argument (typed as an opaque
/// pointer) is ignored and `format` is written verbatim.
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzvprintf(file: gzFile, format: *const c_char, _va: *mut c_void) -> c_int {
    // SAFETY: `gz_handle` performs the null check.
    let Some(handle) = (unsafe { gz_handle(file) }) else {
        return -1;
    };
    if format.is_null() {
        return 0;
    }
    // SAFETY: `format` is a NUL-terminated C string per the contract.
    let s = unsafe { CStr::from_ptr(format) }.to_string_lossy();
    geng::gzprintf(&mut handle.state, format_args!("{s}"))
}

/// C `gzflush` — flush pending output with the given `flush` mode.
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzflush(file: gzFile, flush: c_int) -> c_int {
    // SAFETY: `gz_handle` performs the null check.
    let Some(handle) = (unsafe { gz_handle(file) }) else {
        return Z_STREAM_ERROR;
    };
    geng::gzflush(&mut handle.state, flush)
}

// --- close path (gzclose.c) -------------------------------------------------

/// C `gzclose` — flush, finalize, and free `file`, dispatching to the read- or
/// write-side teardown based on the open mode.
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzclose(file: gzFile) -> c_int {
    if file.is_null() {
        return Z_STREAM_ERROR;
    }
    // SAFETY: `file` is a live `Box::into_raw(GzHandle)`; reclaim ownership and
    // hand the engine state to `gzclose`, which finalizes and drops it.
    let handle = *unsafe { Box::from_raw(file as *mut GzHandle) };
    geng::gzclose(Some(handle.state))
}

/// C `gzclose_r` — close a read-mode stream (errors if `file` is write-mode).
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzclose_r(file: gzFile) -> c_int {
    if file.is_null() {
        return Z_STREAM_ERROR;
    }
    // SAFETY: `file` is a live `Box::into_raw(GzHandle)`; reclaim it. The engine
    // close marks the state finalized, so the box's `Drop` is then a no-op.
    let mut handle = unsafe { Box::from_raw(file as *mut GzHandle) };
    crate::gz::read::gzclose_r(&mut handle.state)
}

/// C `gzclose_w` — close a write-mode stream (errors if `file` is read-mode).
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzclose_w(file: gzFile) -> c_int {
    if file.is_null() {
        return Z_STREAM_ERROR;
    }
    // SAFETY: `file` is a live `Box::into_raw(GzHandle)`; reclaim it. The engine
    // close marks the state finalized, so the box's `Drop` is then a no-op.
    let mut handle = unsafe { Box::from_raw(file as *mut GzHandle) };
    geng::gzclose_w(&mut handle.state)
}

// ===========================================================================
// Phase C — version / compile-flags / error-string queries
// ===========================================================================
//
// These three exports take no pointer arguments and perform no unsafe
// operations; they are nonetheless declared `unsafe extern "C"` for uniformity
// with the rest of the C-ABI surface (and because C callers reach them through
// the same `extern "C"` linkage). Each returns a value with a `'static`
// lifetime so the pointer remains valid for the lifetime of the program, just
// as the C originals return pointers into static storage.

/// NUL-terminated mirror of the C `z_errmsg` table from `zutil.c`, indexed
/// identically to [`crate::error::err_msg`]: index `2 - code`, clamped to the
/// empty sentinel at index 9 for codes outside the canonical `-6..=2` range.
///
/// Every entry ends in `\0`, so the pointer handed back by [`zError`] is a
/// valid C string with `'static` lifetime. The visible text is byte-for-byte
/// identical to [`crate::error::err_msg`] (and therefore to canonical zlib);
/// only the trailing NUL — required by the C contract but absent from a Rust
/// `&str` — is added here.
static Z_ERRMSG_CSTR: [&[u8]; 10] = [
    b"need dictionary\0",      // index 0 — Z_NEED_DICT       ( 2)
    b"stream end\0",           // index 1 — Z_STREAM_END      ( 1)
    b"\0",                     // index 2 — Z_OK              ( 0)
    b"file error\0",           // index 3 — Z_ERRNO          (-1)
    b"stream error\0",         // index 4 — Z_STREAM_ERROR   (-2)
    b"data error\0",           // index 5 — Z_DATA_ERROR     (-3)
    b"insufficient memory\0",  // index 6 — Z_MEM_ERROR      (-4)
    b"buffer error\0",         // index 7 — Z_BUF_ERROR      (-5)
    b"incompatible version\0", // index 8 — Z_VERSION_ERROR  (-6)
    b"\0",                     // index 9 — out-of-range sentinel
];

/// C `zlibVersion` — return a pointer to the NUL-terminated library version
/// string (`"1.3.2.1-motley"`), matching `ZLIB_VERSION` from `zlib.h`.
///
/// The returned pointer references static storage and is valid for the whole
/// program; callers must not free it (identical to the C contract).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn zlibVersion() -> *const c_char {
    ZLIB_VERSION_CSTR.as_ptr() as *const c_char
}

/// C `zlibCompileFlags` — return the `uLong` bitset describing the
/// compile-time type sizes and feature switches of this build.
///
/// Delegates to the safe engine ([`crate::util::zlib_compile_flags`]), which
/// reproduces the bit layout of `zutil.c`'s `zlibCompileFlags`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn zlibCompileFlags() -> uLong {
    ueng::zlib_compile_flags() as uLong
}

/// C `zError` — map a zlib return code to its human-readable, NUL-terminated
/// message string, mirroring `ERR_MSG(err)` (`z_errmsg[Z_NEED_DICT - err]`).
///
/// Out-of-range codes resolve to the empty string, matching
/// [`crate::error::err_msg`]'s clamping behaviour (canonical C zlib only ever
/// passes in-range codes). The returned pointer references static storage and
/// is valid for the whole program.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn zError(err: c_int) -> *const c_char {
    // Mirror `err_msg`: clamp out-of-range codes to the empty entry at index 9,
    // otherwise index by `2 - code`. This keeps the text byte-identical to the
    // safe engine while guaranteeing the result is NUL-terminated.
    let index = if !(-6..=2).contains(&err) {
        9
    } else {
        (2 - err) as usize
    };
    Z_ERRMSG_CSTR[index].as_ptr() as *const c_char
}
