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
//! layout for ABI fidelity, and the FFI honours them through an allocator
//! bridge. When a C caller installs both hooks, the **FFI-created state
//! object** — the `DeflateHandle`/`InflateHandle`/`InflateBackHandle` that
//! `z_stream.state` points at, the direct analogue of C zlib's
//! `ZALLOC(strm, 1, sizeof(internal_state))` — is allocated through
//! `zalloc(opaque, 1, size)` and released through `zfree(opaque, ptr)`, each
//! handle recording its own provenance so `*End` frees it symmetrically (see
//! the [`alloc_handle`] / [`reclaim_handle`] bridge). When the hooks are
//! `Z_NULL` the handle falls back to a global `Box`. A caller's counting or
//! pool allocator therefore observes the state-object alloc/free pair balanced
//! through its own hooks.
//!
//! The engines' large *internal* working buffers (the deflate window,
//! `pending_buf`, `head`/`prev`, and the inflate code tables) remain owned
//! `Vec`/`Box` reclaimed on `Drop`. The AAP fixes this boundary deliberately:
//! the core is 100% safe Rust with `unsafe` confined to two zones (§0.6.2) and
//! all working buffers are owned `Vec`/`Box` (§0.6.3), while stable Rust has no
//! stable custom-allocator `Vec`/`Box` (the `allocator_api` is unstable) — so
//! routing those buffers through the hooks is neither expressible on the stable
//! MSRV nor compatible with the safe-core mandate. Hooks are thus *preserved at
//! the FFI layer* exactly as §0.6.3 prescribes, and pure-Rust callers always
//! use the Rust global allocator.
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

/// C `struct gzFile_s` — the **macro-visible prefix** of a gzip file handle,
/// laid out to match `zlib.h` byte-for-byte:
///
/// ```c
/// struct gzFile_s {
///     unsigned have;
///     unsigned char *next;
///     z_off64_t pos;
/// };
/// ```
///
/// The canonical `zlib.h` defines `gzgetc` as a macro that reads and mutates
/// these three fields directly:
///
/// ```c
/// #define gzgetc(g) \
///     ((g)->have ? ((g)->have--, (g)->pos++, *((g)->next)++) : (gzgetc)(g))
/// ```
///
/// A C caller compiled against the canonical header therefore dereferences this
/// exact layout. To remain a binary/header-compatible `libz` drop-in, the FFI
/// handle ([`GzHandle`]) places a `gzFile_s` at offset 0 (`#[repr(C)]`) and
/// keeps `have`/`next`/`pos` synchronized with the engine's read buffer around
/// every gz entry point (see [`GzHandle::sync_in`] / [`GzHandle::sync_out`]),
/// so the `gzgetc` macro fast-path and the `gzgetc_`/`gzread` functions observe
/// one consistent stream position.
#[repr(C)]
pub struct gzFile_s {
    /// Bytes immediately available in `next` (the macro decrements this).
    pub have: c_uint,
    /// Cursor into the engine's output buffer (the macro post-increments this).
    pub next: *mut c_uchar,
    /// Uncompressed stream position (the macro post-increments this).
    pub pos: z_off64_t,
}

// Only the gzip FILE-I/O layer (`gz-io`) constructs handles, so `zeroed()` is
// exercised exclusively under that feature; gate the impl to match and avoid a
// dead-code warning in the (unusual) `capi`-without-`gz-io` configuration.
#[cfg(feature = "gz-io")]
impl gzFile_s {
    /// The initial (pre-first-read) state: empty buffer, null cursor, position
    /// zero — matching a freshly opened handle before any data is buffered.
    #[inline]
    const fn zeroed() -> Self {
        gzFile_s {
            have: 0,
            next: core::ptr::null_mut(),
            pos: 0,
        }
    }
}

/// C `gzFile` — a semi-opaque handle to an open gzip file: a pointer to a
/// [`gzFile_s`] whose macro-visible prefix the FFI keeps in sync with the
/// engine. Applications treat the pointee as opaque beyond the three exposed
/// fields the `gzgetc` macro touches.
pub type gzFile = *mut gzFile_s;

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
    /// How this handle's backing block was obtained (caller `zalloc` hook vs.
    /// the global allocator), so `deflateEnd` frees it symmetrically.
    alloc: HandleAlloc,
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
    /// How this handle's backing block was obtained (caller `zalloc` hook vs.
    /// the global allocator), so `inflateEnd` frees it symmetrically.
    alloc: HandleAlloc,
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
    /// How this handle's backing block was obtained (caller `zalloc` hook vs.
    /// the global allocator), so `inflateBackEnd` frees it symmetrically.
    alloc: HandleAlloc,
}

// ===========================================================================
// Allocator-hook bridge (AAP §0.6.2 / §0.6.3)
// ===========================================================================
//
// zlib lets a C caller install custom `zalloc`/`zfree`/`opaque` hooks on the
// `z_stream` so the library draws its heap from a caller-controlled pool. The
// safe-Rust engines own their large working buffers (the deflate window,
// `pending_buf`, `head`/`prev`, and the inflate code tables) as `Vec`/`Box`
// reclaimed by `Drop`: the AAP mandates a 100% safe core with `unsafe` confined
// to `inflate/fast.rs` and this module (§0.6.2), and stable Rust has no stable
// custom-allocator `Vec`/`Box` (the `allocator_api` is unstable), so those
// engine buffers necessarily use the Rust global allocator — exactly the
// boundary §0.6.3 draws ("hooks preserved at the FFI layer … pure-Rust callers
// use the global allocator").
//
// What this layer CAN and does route through the caller hooks is the
// FFI-created *state object* itself — the `DeflateHandle` / `InflateHandle` /
// `InflateBackHandle` that `z_stream.state` points at. That allocation is the
// direct analogue of C zlib's `ZALLOC(strm, 1, sizeof(internal_state))`: when
// the caller installs both hooks, the handle's backing block comes from
// `zalloc(opaque, 1, size_of::<Handle>())` and is released through
// `zfree(opaque, ptr)`; when either hook is null the handle uses an ordinary
// `Box`. Each handle records its own provenance so `*End` frees it through the
// same allocator that created it, independent of any later mutation of the
// stream's hook fields. A caller's counting/pool allocator therefore observes
// the state-object alloc/free pair balanced through its own hooks.

/// How an FFI handle's backing allocation was obtained, recorded inside the
/// handle so the matching deallocator can be chosen at `*End`.
#[derive(Clone, Copy)]
enum HandleAlloc {
    /// Allocated with a Rust `Box` from the global allocator.
    Global,
    /// Allocated through the caller's `zalloc`; release via `zfree(opaque, …)`.
    Hook {
        zfree: unsafe extern "C" fn(voidpf, voidpf),
        opaque: voidpf,
    },
}

/// Lets the generic [`reclaim_handle`] read a handle's allocation provenance.
trait FfiHandle {
    fn alloc_kind(&self) -> HandleAlloc;
}

impl FfiHandle for DeflateHandle {
    #[inline]
    fn alloc_kind(&self) -> HandleAlloc {
        self.alloc
    }
}

impl FfiHandle for InflateHandle {
    #[inline]
    fn alloc_kind(&self) -> HandleAlloc {
        self.alloc
    }
}

impl FfiHandle for InflateBackHandle {
    #[inline]
    fn alloc_kind(&self) -> HandleAlloc {
        self.alloc
    }
}

/// Return the caller's allocator hooks iff BOTH `zalloc` and `zfree` are
/// installed (non-null). zlib treats a half-installed pair as the default
/// allocator, so the hooks are used only when the pair is complete; `opaque`
/// is forwarded verbatim as the hooks' first argument.
#[inline]
fn caller_hooks(
    strm: &z_stream,
) -> Option<(
    unsafe extern "C" fn(voidpf, uInt, uInt) -> voidpf,
    unsafe extern "C" fn(voidpf, voidpf),
    voidpf,
)> {
    match (strm.zalloc, strm.zfree) {
        (Some(zalloc), Some(zfree)) => Some((zalloc, zfree, strm.opaque)),
        _ => None,
    }
}

/// Allocate and initialise an FFI handle `T`, routing the allocation through
/// the caller's `zalloc` hook when installed (otherwise a global `Box`). The
/// `make` closure receives the chosen [`HandleAlloc`] so the constructed value
/// records its own provenance for symmetric release in [`reclaim_handle`].
///
/// Returns `None` (→ `Z_MEM_ERROR`) when the hook returns null or — defensively
/// — yields a block too misaligned to hold `T` (zlib requires malloc-grade
/// alignment, so a conforming hook never trips that check).
fn alloc_handle<T: FfiHandle>(
    strm: &z_stream,
    make: impl FnOnce(HandleAlloc) -> T,
) -> Option<*mut T> {
    match caller_hooks(strm) {
        None => Some(Box::into_raw(Box::new(make(HandleAlloc::Global)))),
        Some((zalloc, zfree, opaque)) => {
            let size = core::mem::size_of::<T>();
            let align = core::mem::align_of::<T>();
            // SAFETY: `zalloc` is a non-null C `alloc_func` (verified by
            // `caller_hooks`); invoking it as `(opaque, 1, size)` matches the
            // C contract `ZALLOC(strm, 1, sizeof(state))`.
            let raw = unsafe { zalloc(opaque, 1, size as uInt) } as *mut T;
            if raw.is_null() {
                return None;
            }
            // `align` is a power of two (`align_of` guarantee), so `align - 1`
            // is its alignment mask. A conforming malloc-grade hook satisfies
            // this; if not, free the block and fail rather than risk UB.
            if (raw as usize) & (align - 1) != 0 {
                // SAFETY: `raw` came from `zalloc(opaque, …)`, so
                // `zfree(opaque, raw)` is the matching deallocation.
                unsafe { zfree(opaque, raw as voidpf) };
                return None;
            }
            // SAFETY: `raw` is non-null and aligned for `T` and addresses
            // `size_of::<T>()` writable bytes; `write` initialises them without
            // reading/dropping the prior uninitialised contents.
            unsafe { raw.write(make(HandleAlloc::Hook { zfree, opaque })) };
            Some(raw)
        }
    }
}

/// Reclaim an FFI handle created by [`alloc_handle`]: move the `T` value out of
/// its backing block, release that block through the allocator recorded inside
/// the handle, and return the owned value so the caller can hand its engine
/// `state` to the relevant `*_end`.
///
/// # Safety
///
/// `ptr` must be a live, uniquely-owned handle previously produced by
/// [`alloc_handle`] (i.e. the current `z_stream.state`) that has not yet been
/// reclaimed.
unsafe fn reclaim_handle<T: FfiHandle>(ptr: *mut T) -> T {
    // SAFETY: `ptr` is a live handle (caller precondition); read its provenance
    // before disturbing the allocation.
    let kind = unsafe { (*ptr).alloc_kind() };
    match kind {
        // The `Box` owns the global block; `*boxed` moves `T` out and frees it.
        // SAFETY: in the `Global` arm `ptr` came from `Box::into_raw`.
        HandleAlloc::Global => *unsafe { Box::from_raw(ptr) },
        HandleAlloc::Hook { zfree, opaque } => {
            // SAFETY: `ptr` is valid for reads of `T`; move the value out
            // bitwise. The shell is freed below WITHOUT running `T`'s drop, so
            // the inner `Box`/`Vec` fields end up owned solely by `value`.
            let value = unsafe { core::ptr::read(ptr) };
            // SAFETY: in the `Hook` arm `ptr` came from `zalloc(opaque, …)`, so
            // `zfree(opaque, ptr)` is the matching deallocation; afterwards the
            // shell is gone but `value` still owns the inner allocations.
            unsafe { zfree(opaque, ptr as voidpf) };
            value
        }
    }
}

/// Reconstruct the input slice from `next_in` / `avail_in`.
///
/// A null `next_in` (legal when `avail_in == 0`) yields an empty slice.
///
/// # Safety
///
/// `next_in` must be valid for reads of `avail_in` bytes, per the C contract.
///
/// Returns `None` for the invalid `(next_in == NULL && avail_in != 0)`
/// combination so the caller can reject it with `Z_STREAM_ERROR`, mirroring C
/// zlib's defensive `next_in == Z_NULL` checks. A null pointer with a zero
/// length is legal and yields `Some(&[])`.
#[inline]
unsafe fn input_slice<'a>(next_in: *const Bytef, avail_in: uInt) -> Option<&'a [u8]> {
    if next_in.is_null() {
        // A null pointer is only valid when the advertised length is zero;
        // a non-zero length with a null pointer is a caller contract violation.
        if avail_in != 0 { None } else { Some(&[]) }
    } else {
        // SAFETY: the caller guarantees `next_in` points to `avail_in` readable
        // bytes for the duration of the call (the zlib `next_in`/`avail_in`
        // contract); the borrow does not outlive this call. A non-null pointer
        // with `avail_in == 0` is valid for `from_raw_parts` (empty slice).
        Some(unsafe { core::slice::from_raw_parts(next_in, avail_in as usize) })
    }
}

/// Reconstruct the output slice from `next_out` / `avail_out`.
///
/// Returns `None` for the invalid `(next_out == NULL && avail_out != 0)`
/// combination so the caller can reject it with `Z_STREAM_ERROR`. A null
/// pointer with a zero length is legal and yields `Some(&mut [])`.
///
/// # Safety
///
/// `next_out` must be valid for writes of `avail_out` bytes, per the C
/// contract, and not alias the input slice.
#[inline]
unsafe fn output_slice<'a>(next_out: *mut Bytef, avail_out: uInt) -> Option<&'a mut [u8]> {
    if next_out.is_null() {
        // A null pointer is only valid when the advertised length is zero.
        if avail_out != 0 { None } else { Some(&mut []) }
    } else {
        // SAFETY: the caller guarantees `next_out` points to `avail_out`
        // writable bytes for the duration of the call (the zlib
        // `next_out`/`avail_out` contract); the borrow does not outlive it. A
        // non-null pointer with `avail_out == 0` is valid (empty slice).
        Some(unsafe { core::slice::from_raw_parts_mut(next_out, avail_out as usize) })
    }
}

/// Reconstruct a `&[u8]` from a C buffer pointer and a `usize` length (the
/// one-shot helpers carry `uLong`/`z_size_t` lengths, which can exceed `uInt`).
///
/// Returns `None` for the invalid `(ptr == NULL && len != 0)` combination so the
/// caller can reject it; a null pointer with a zero length yields `Some(&[])`.
///
/// # Safety
///
/// When non-null, `ptr` must point to at least `len` readable bytes that remain
/// valid for the duration of the call.
#[inline]
unsafe fn const_buf<'a>(ptr: *const Bytef, len: usize) -> Option<&'a [u8]> {
    if ptr.is_null() {
        // A null pointer is only valid when the advertised length is zero.
        if len != 0 { None } else { Some(&[]) }
    } else {
        // SAFETY: caller upholds the validity-for-`len`-bytes precondition.
        Some(unsafe { core::slice::from_raw_parts(ptr, len) })
    }
}

/// Reconstruct a `&mut [u8]` from a C buffer pointer and a `usize` length.
///
/// Returns `None` for the invalid `(ptr == NULL && len != 0)` combination so the
/// caller can reject it; a null pointer with a zero length yields `Some(&mut [])`.
///
/// # Safety
///
/// When non-null, `ptr` must point to at least `len` writable bytes that remain
/// valid for the duration of the call and are not aliased elsewhere.
#[inline]
unsafe fn mut_buf<'a>(ptr: *mut Bytef, len: usize) -> Option<&'a mut [u8]> {
    if ptr.is_null() {
        // A null pointer is only valid when the advertised length is zero.
        if len != 0 { None } else { Some(&mut []) }
    } else {
        // SAFETY: caller upholds the validity-for-`len`-bytes precondition.
        Some(unsafe { core::slice::from_raw_parts_mut(ptr, len) })
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
fn attach_deflate(strm: &mut z_stream, state: Box<deng::DeflateState>) -> c_int {
    // Seed the public scalar fields from the engine state while it is still
    // owned here (before it is moved into the handle below).
    strm.total_in = state.total_in as uLong;
    strm.total_out = state.total_out as uLong;
    strm.data_type = state.data_type;
    strm.adler = state.adler as uLong;
    strm.msg = core::ptr::null();
    // Allocate the handle through the caller's `zalloc` hook when installed
    // (else a global `Box`), recording the provenance so `deflateEnd` frees it
    // through the same allocator. Hand ownership to the C `z_stream`.
    match alloc_handle(strm, move |alloc| DeflateHandle {
        state,
        msg: Vec::new(),
        alloc,
    }) {
        Some(handle) => {
            strm.state = handle as *mut c_void;
            Z_OK
        }
        None => Z_MEM_ERROR,
    }
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
        Ok(state) => attach_deflate(strm, state),
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
        Ok(state) => attach_deflate(strm, state),
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
        // caller's buffers per the zlib contract. A null pointer paired with a
        // non-zero length is an invalid argument and is rejected with
        // `Z_STREAM_ERROR`, mirroring C zlib's defensive null checks.
        let (Some(input), Some(output)) = (
            unsafe { input_slice(strm.next_in, strm.avail_in) },
            unsafe { output_slice(strm.next_out, strm.avail_out) },
        ) else {
            return Z_STREAM_ERROR;
        };

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
    // SAFETY: `state` is a live handle from `alloc_handle`; reclaim it through
    // the same allocator (caller hook or global) that created it.
    let handle = unsafe { reclaim_handle(strm.state as *mut DeflateHandle) };
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
    // SAFETY: the buffers are described by the zlib contract. A null pointer
    // paired with a non-zero length is rejected with `Z_STREAM_ERROR`.
    let (Some(input), Some(output)) = (
        unsafe { input_slice(strm.next_in, strm.avail_in) },
        unsafe { output_slice(strm.next_out, strm.avail_out) },
    ) else {
        return Z_STREAM_ERROR;
    };
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
    // Mirror the public scalar fields AND the allocator hooks from the source
    // onto the destination before allocating, so the clone's handle is drawn
    // from — and later freed through — the same allocator as the source.
    dest_strm.total_in = src_strm.total_in;
    dest_strm.total_out = src_strm.total_out;
    dest_strm.adler = src_strm.adler;
    dest_strm.data_type = src_strm.data_type;
    dest_strm.msg = core::ptr::null();
    dest_strm.zalloc = src_strm.zalloc;
    dest_strm.zfree = src_strm.zfree;
    dest_strm.opaque = src_strm.opaque;
    match alloc_handle(dest_strm, move |alloc| DeflateHandle {
        state: cloned,
        msg: Vec::new(),
        alloc,
    }) {
        Some(handle) => {
            dest_strm.state = handle as *mut c_void;
            Z_OK
        }
        None => Z_MEM_ERROR,
    }
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
fn attach_inflate(strm: &mut z_stream, state: Box<ieng::InflateState>) -> c_int {
    strm.total_in = 0;
    strm.total_out = 0;
    strm.msg = core::ptr::null();
    strm.adler = 0;
    // Allocate the handle through the caller's `zalloc` hook when installed
    // (else a global `Box`), recording the provenance so `inflateEnd` frees it
    // through the same allocator. Hand ownership to the C `z_stream`.
    match alloc_handle(strm, move |alloc| InflateHandle {
        state,
        head: core::ptr::null_mut(),
        msg: Vec::new(),
        alloc,
    }) {
        Some(handle) => {
            strm.state = handle as *mut c_void;
            Z_OK
        }
        None => Z_MEM_ERROR,
    }
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
        Ok(state) => attach_inflate(strm, state),
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
        Ok(state) => attach_inflate(strm, state),
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
        // `next_out`/`avail_out` per the zlib contract. A null pointer paired
        // with a non-zero length is rejected with `Z_STREAM_ERROR`.
        let (Some(input), Some(output)) = (
            unsafe { input_slice(strm.next_in, strm.avail_in) },
            unsafe { output_slice(strm.next_out, strm.avail_out) },
        ) else {
            return Z_STREAM_ERROR;
        };

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
    // SAFETY: `state` is a live handle from `alloc_handle`; reclaim it through
    // the same allocator (caller hook or global) that created it, moving the
    // engine state out so `inflate_end` can consume it.
    let handle = unsafe { reclaim_handle(strm.state as *mut InflateHandle) };
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
    // SAFETY: the input buffer is described by `next_in`/`avail_in`. A null
    // pointer paired with a non-zero length is rejected with `Z_STREAM_ERROR`.
    let Some(input) = (unsafe { input_slice(strm.next_in, strm.avail_in) }) else {
        return Z_STREAM_ERROR;
    };
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
    // Mirror C, which copies the registered header pointer verbatim.
    let head = src_handle.head;
    // Mirror the public scalar fields AND the allocator hooks from the source
    // onto the destination before allocating, so the clone's handle is drawn
    // from — and later freed through — the same allocator as the source.
    dest_strm.total_in = src_strm.total_in;
    dest_strm.total_out = src_strm.total_out;
    dest_strm.adler = src_strm.adler;
    dest_strm.data_type = src_strm.data_type;
    dest_strm.msg = core::ptr::null();
    dest_strm.zalloc = src_strm.zalloc;
    dest_strm.zfree = src_strm.zfree;
    dest_strm.opaque = src_strm.opaque;
    match alloc_handle(dest_strm, move |alloc| InflateHandle {
        state: cloned,
        head,
        msg: Vec::new(),
        alloc,
    }) {
        Some(handle) => {
            dest_strm.state = handle as *mut c_void;
            Z_OK
        }
        None => Z_MEM_ERROR,
    }
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
            let state = Box::new(state);
            let win = window as *mut Bytef;
            strm.total_in = 0;
            strm.total_out = 0;
            strm.msg = core::ptr::null();
            strm.adler = 0;
            // Allocate the handle through the caller's `zalloc` hook when
            // installed (else a global `Box`), recording the provenance so
            // `inflateBackEnd` frees it through the same allocator.
            match alloc_handle(strm, move |alloc| InflateBackHandle {
                state,
                window: win,
                wsize,
                alloc,
            }) {
                Some(handle) => {
                    strm.state = handle as *mut c_void;
                    Z_OK
                }
                None => Z_MEM_ERROR,
            }
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
    // SAFETY: `state` is a live handle from `alloc_handle`; reclaim it through
    // the same allocator (caller hook or global) that created it, moving the
    // engine state out so `inflate_back_end` can consume it.
    let handle = unsafe { reclaim_handle(strm.state as *mut InflateBackHandle) };
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
        // bytes per the contract. A null pointer paired with a non-zero length
        // is rejected with `Z_STREAM_ERROR`, matching C zlib's argument checks.
        let (Some(dest_slice), Some(src_slice)) = (unsafe { mut_buf(dest, cap) }, unsafe {
            const_buf(source, sourceLen as usize)
        }) else {
            return Z_STREAM_ERROR;
        };
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
        // SAFETY: see `compress2`; null + non-zero length is rejected.
        let (Some(dest_slice), Some(src_slice)) = (unsafe { mut_buf(dest, cap) }, unsafe {
            const_buf(source, sourceLen as usize)
        }) else {
            return Z_STREAM_ERROR;
        };
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
        // SAFETY: see `compress2`; null + non-zero length is rejected.
        let (Some(dest_slice), Some(src_slice)) = (unsafe { mut_buf(dest, cap) }, unsafe {
            const_buf(source, sourceLen as usize)
        }) else {
            return Z_STREAM_ERROR;
        };
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
        // SAFETY: see `compress2`; null + non-zero length is rejected.
        let (Some(dest_slice), Some(src_slice)) = (unsafe { mut_buf(dest, cap) }, unsafe {
            const_buf(source, src_len)
        }) else {
            return Z_STREAM_ERROR;
        };
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

/// FFI-side owner of a gzip file. Its **first field** is a [`gzFile_s`] laid out
/// exactly as the public `zlib.h` struct so the C `gzgetc` macro can read/mutate
/// `have`/`next`/`pos` directly at offset 0; behind it sit the engine state and
/// a scratch buffer that backs the `*const c_char` returned by [`gzerror`].
///
/// `#[repr(C)]` pins the field order so `x` is guaranteed at offset 0. The
/// struct never crosses the FFI boundary by value — only as `*mut gzFile_s`
/// ([`gzFile`]) — so the `Box<GzState>` / `Vec<u8>` tail fields are immaterial to
/// the C ABI and raise no `improper_ctypes` concern.
#[cfg(feature = "gz-io")]
#[repr(C)]
struct GzHandle {
    /// Macro-visible `gzFile_s` prefix — MUST remain the first field (offset 0).
    x: gzFile_s,
    /// The engine's gzip state (owned; freed on `gzclose`).
    state: Box<GzState>,
    /// NUL-terminated scratch backing the string returned by [`gzerror`].
    err_cache: Vec<u8>,
}

#[cfg(feature = "gz-io")]
impl GzHandle {
    /// Reconcile bytes the C `gzgetc` macro consumed **directly** from the
    /// exposed `gzFile_s` buffer back into the engine state, before any FFI
    /// function operates on the stream (`x` → engine; read mode only).
    ///
    /// The macro `((g)->have--, (g)->pos++, *((g)->next)++)` decrements `x.have`
    /// (and advances `x.next`/`x.pos`) for every byte it reads without calling
    /// into the library. Since [`sync_out`](Self::sync_out) last published
    /// `x.have == state.have`, the gap `state.have - x.have` is exactly the
    /// number of bytes consumed by the macro; replaying that consumption
    /// (`next += consumed; have = x.have; pos = x.pos`) leaves the engine in the
    /// identical state it would hold had `gzread`/`gzgetc_` consumed those bytes
    /// itself — so a subsequent refill picks up from the correct position.
    #[inline]
    fn sync_in(&mut self) {
        if self.state.is_reading() {
            let engine_have = self.state.have;
            let macro_have = self.x.have as usize;
            // `macro_have <= engine_have` always holds in correct usage (the
            // macro only ever consumes from what we published); guard against a
            // malformed prefix to keep the engine state authoritative.
            if macro_have <= engine_have {
                let consumed = engine_have - macro_have;
                self.state.next += consumed;
                self.state.have = macro_have;
                self.state.pos = self.x.pos;
            }
        }
    }

    /// Publish the engine's current read buffer into the exposed `gzFile_s`
    /// prefix so the C `gzgetc` macro can consume bytes directly (engine → `x`).
    ///
    /// Runs after every FFI entry point via [`GzAccess`]'s `Drop`. When reading
    /// with buffered data, it points `x.next` at `out_buf[next]` and sets
    /// `x.have` to the available count; otherwise it clears the prefix so the
    /// macro falls back to the `(gzgetc)(g)` function call. `x.pos` always
    /// tracks the engine position so `gztell`/the macro stay consistent.
    #[inline]
    fn sync_out(&mut self) {
        self.x.pos = self.state.pos;
        if self.state.is_reading() && self.state.have > 0 {
            // Read the scalar cursors first, then borrow `out_buf` mutably for
            // its base pointer (avoids overlapping borrows of `self.state`).
            let have = self.state.have;
            let next_idx = self.state.next;
            let base = self.state.out_buf.as_mut_ptr();
            self.x.have = have as c_uint;
            // `next_idx <= out_buf.len()` by the engine's window invariant
            // (`out_buf[next..next+have]` is the live span), so this is an
            // in-bounds (or one-past) pointer that the macro reads `have` times.
            self.x.next = base.wrapping_add(next_idx);
        } else {
            self.x.have = 0;
            self.x.next = core::ptr::null_mut();
        }
    }
}

/// RAII access guard returned by [`gz_handle`]. On creation it has already
/// reconciled any `gzgetc`-macro consumption into the engine
/// ([`GzHandle::sync_in`]); on drop it republishes the engine's buffer into the
/// macro-visible prefix ([`GzHandle::sync_out`]). It derefs to [`GzHandle`] so
/// existing `handle.state` / `handle.err_cache` access sites are unchanged.
#[cfg(feature = "gz-io")]
struct GzAccess<'a> {
    handle: &'a mut GzHandle,
}

#[cfg(feature = "gz-io")]
impl core::ops::Deref for GzAccess<'_> {
    type Target = GzHandle;
    #[inline]
    fn deref(&self) -> &GzHandle {
        self.handle
    }
}

#[cfg(feature = "gz-io")]
impl core::ops::DerefMut for GzAccess<'_> {
    #[inline]
    fn deref_mut(&mut self) -> &mut GzHandle {
        self.handle
    }
}

#[cfg(feature = "gz-io")]
impl Drop for GzAccess<'_> {
    #[inline]
    fn drop(&mut self) {
        self.handle.sync_out();
    }
}

/// Borrow the [`GzHandle`] behind a `gzFile` as a [`GzAccess`] guard, or `None`
/// if the handle is null.
///
/// The returned guard reconciles any `gzgetc`-macro consumption into the engine
/// up front ([`GzHandle::sync_in`]) and republishes the engine buffer to the
/// macro-visible prefix when it drops ([`GzHandle::sync_out`]). It derefs to
/// [`GzHandle`], so callers keep using `handle.state` / `handle.err_cache`.
///
/// # Safety
///
/// A non-null `file` must be a handle returned by [`gzopen`]/[`gzopen64`]/
/// [`gzdopen`] and not yet closed.
#[cfg(feature = "gz-io")]
#[inline]
unsafe fn gz_handle<'a>(file: gzFile) -> Option<GzAccess<'a>> {
    if file.is_null() {
        None
    } else {
        // SAFETY: a non-null `gzFile` was produced as `Box::into_raw(GzHandle)`
        // by an open call; nothing else aliases it for the call's duration.
        let handle = unsafe { &mut *(file as *mut GzHandle) };
        // Reconcile any bytes the C `gzgetc` macro consumed directly from the
        // exposed prefix before the engine operates on the stream.
        handle.sync_in();
        Some(GzAccess { handle })
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
            // Fresh handle: the macro-visible prefix starts empty (no buffered
            // data) and is populated lazily by `sync_out` after the first read.
            x: gzFile_s::zeroed(),
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
    // C `gzdopen` rejects `fd == -1` up front and, on any failure, returns NULL
    // WITHOUT adopting or closing the descriptor. We reject every negative `fd`
    // because `File::from_raw_fd` carries a safety precondition that the
    // descriptor be valid (non-negative, owned); calling it with `-1` would be
    // undefined behavior. This check must precede `from_raw_fd` so an invalid
    // descriptor is never wrapped, and it preserves zlib's no-adopt/no-close
    // contract for invalid input (we never construct a `File`, so nothing is
    // closed on drop).
    if fd < 0 {
        return core::ptr::null_mut();
    }
    if mode.is_null() {
        return core::ptr::null_mut();
    }
    // SAFETY: `mode` is a NUL-terminated C string per the contract.
    let Ok(m) = (unsafe { CStr::from_ptr(mode) }).to_str() else {
        return core::ptr::null_mut();
    };
    // Validate the mode string BEFORE adopting `fd`. C `gz_open` never closes a
    // caller-supplied descriptor on a mode-parse failure (it only `free`s the
    // partial state), so we must not wrap `fd` in an owning `File` — whose
    // `Drop` would close it — until the mode is known to be valid. Rejecting an
    // invalid mode here leaves `fd` entirely untouched, preserving zlib's
    // no-adopt/no-close failure-path contract.
    if !geng::mode_is_valid(m) {
        return core::ptr::null_mut();
    }
    // SAFETY: `fd` was validated as non-negative above, satisfying the
    // `from_raw_fd` precondition. The caller transfers ownership of `fd` to the
    // new stream (the C `gzdopen` contract); `File` adopts it and closes it on
    // teardown.
    let file = unsafe { File::from_raw_fd(fd) };
    match geng::gzdopen(file, m) {
        Some(state) => Box::into_raw(Box::new(GzHandle {
            // Fresh handle: the macro-visible prefix starts empty (see above).
            x: gzFile_s::zeroed(),
            state,
            err_cache: Vec::new(),
        })) as gzFile,
        // The mode was validated above, so for an adopted descriptor `gzdopen`
        // does not fail here; this arm is defensive. (Were it reachable, the
        // `File` would drop and close `fd` — acceptable only because a valid
        // mode guarantees `Some`.)
        None => core::ptr::null_mut(),
    }
}

/// C `gzbuffer` — set the internal buffer size (before the first read/write).
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzbuffer(file: gzFile, size: c_uint) -> c_int {
    // SAFETY: `gz_handle` performs the null check.
    let Some(mut handle) = (unsafe { gz_handle(file) }) else {
        return -1;
    };
    geng::gzbuffer(&mut handle.state, size as u32)
}

/// C `gzsetparams` — change the compression `level`/`strategy` mid-stream.
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzsetparams(file: gzFile, level: c_int, strategy: c_int) -> c_int {
    // SAFETY: `gz_handle` performs the null check.
    let Some(mut handle) = (unsafe { gz_handle(file) }) else {
        return Z_STREAM_ERROR;
    };
    geng::gzsetparams(&mut handle.state, level, strategy)
}

/// C `gzrewind` — rewind a read-mode stream to the start of the file.
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzrewind(file: gzFile) -> c_int {
    // SAFETY: `gz_handle` performs the null check.
    let Some(mut handle) = (unsafe { gz_handle(file) }) else {
        return -1;
    };
    geng::gzrewind(&mut handle.state)
}

/// C `gzseek` — reposition the stream; `whence` is `SEEK_SET`/`SEEK_CUR`.
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzseek(file: gzFile, offset: z_off_t, whence: c_int) -> z_off_t {
    // SAFETY: `gz_handle` performs the null check.
    let Some(mut handle) = (unsafe { gz_handle(file) }) else {
        return -1;
    };
    geng::gzseek(&mut handle.state, offset as i64, whence) as z_off_t
}

/// C `gzseek64` — [`gzseek`] with a 64-bit offset.
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzseek64(file: gzFile, offset: z_off64_t, whence: c_int) -> z_off64_t {
    // SAFETY: `gz_handle` performs the null check.
    let Some(mut handle) = (unsafe { gz_handle(file) }) else {
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
    let Some(mut handle) = (unsafe { gz_handle(file) }) else {
        return -1;
    };
    geng::gzoffset(&mut handle.state) as z_off_t
}

/// C `gzoffset64` — [`gzoffset`] as a 64-bit offset.
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzoffset64(file: gzFile) -> z_off64_t {
    // SAFETY: `gz_handle` performs the null check.
    let Some(mut handle) = (unsafe { gz_handle(file) }) else {
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
    let Some(mut handle) = (unsafe { gz_handle(file) }) else {
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
    let Some(mut handle) = (unsafe { gz_handle(file) }) else {
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
    if let Some(mut handle) = unsafe { gz_handle(file) } {
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
    let Some(mut handle) = (unsafe { gz_handle(file) }) else {
        return -1;
    };
    // SAFETY: `buf` is valid for `len` bytes per the contract. A null `buf`
    // paired with a non-zero `len` is an invalid argument; C `gzread` returns
    // `-1` for such errors.
    let Some(data) = (unsafe { mut_buf(buf as *mut Bytef, len as usize) }) else {
        return -1;
    };
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
    let Some(mut handle) = (unsafe { gz_handle(file) }) else {
        return 0;
    };
    let total = size.saturating_mul(nitems);
    // SAFETY: `buf` is valid for `size * nitems` bytes per the contract. A null
    // `buf` paired with a non-zero total is rejected (C `gzfread` returns `0`).
    let Some(data) = (unsafe { mut_buf(buf as *mut Bytef, total) }) else {
        return 0;
    };
    geng::gzfread(&mut handle.state, size, nitems, data)
}

/// C `gzgetc` — read a single byte, or `-1` at EOF/on error.
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzgetc(file: gzFile) -> c_int {
    // SAFETY: `gz_handle` performs the null check.
    let Some(mut handle) = (unsafe { gz_handle(file) }) else {
        return -1;
    };
    geng::gzgetc(&mut handle.state)
}

/// C `gzgetc_` — the function form of the `gzgetc` macro (identical behavior).
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzgetc_(file: gzFile) -> c_int {
    // SAFETY: `gz_handle` performs the null check.
    let Some(mut handle) = (unsafe { gz_handle(file) }) else {
        return -1;
    };
    geng::gzgetc_(&mut handle.state)
}

/// C `gzungetc` — push `c` back so the next read returns it.
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzungetc(c: c_int, file: gzFile) -> c_int {
    // SAFETY: `gz_handle` performs the null check.
    let Some(mut handle) = (unsafe { gz_handle(file) }) else {
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
    let Some(mut handle) = (unsafe { gz_handle(file) }) else {
        return core::ptr::null_mut();
    };
    // Reserve one byte for the terminating NUL (the engine writes raw bytes).
    let cap = (len - 1) as usize;
    // SAFETY: `buf` is valid for `len` bytes per the contract; we expose `cap`
    // of them to the engine and reserve the last for the NUL. `buf` was already
    // null-checked above, so this is always `Some`, but we reject defensively to
    // match the validating helper contract.
    let Some(data) = (unsafe { mut_buf(buf as *mut Bytef, cap) }) else {
        return core::ptr::null_mut();
    };
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
    let Some(mut handle) = (unsafe { gz_handle(file) }) else {
        return 0;
    };
    // SAFETY: `buf` is valid for `len` bytes per the contract. A null `buf`
    // paired with a non-zero `len` is invalid; C `gzwrite` returns `0` on error.
    let Some(data) = (unsafe { const_buf(buf as *const Bytef, len as usize) }) else {
        return 0;
    };
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
    let Some(mut handle) = (unsafe { gz_handle(file) }) else {
        return 0;
    };
    let total = size.saturating_mul(nitems);
    // SAFETY: `buf` is valid for `size * nitems` bytes per the contract. A null
    // `buf` paired with a non-zero total is rejected (C `gzfwrite` returns `0`).
    let Some(data) = (unsafe { const_buf(buf as *const Bytef, total) }) else {
        return 0;
    };
    geng::gzfwrite(&mut handle.state, size, nitems, data)
}

/// C `gzputc` — compress and write a single byte. Returns the byte, or `-1` on
/// error.
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzputc(file: gzFile, c: c_int) -> c_int {
    // SAFETY: `gz_handle` performs the null check.
    let Some(mut handle) = (unsafe { gz_handle(file) }) else {
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
    let Some(mut handle) = (unsafe { gz_handle(file) }) else {
        return -1;
    };
    if s.is_null() {
        return -1;
    }
    // SAFETY: `s` is a NUL-terminated C string per the contract.
    let bytes = unsafe { CStr::from_ptr(s) }.to_bytes();
    geng::gzputs(&mut handle.state, bytes)
}

/// Internal write callback for the C `gzprintf`/`gzvprintf` shim
/// (`csrc/gzprintf.c`).
///
/// C's variadic `...` / `va_list` cannot be read from stable Rust
/// (`c_variadic` is unstable and would break the edition-2024 stable MSRV), so
/// a tiny self-authored C shim owns the canonical `gzprintf`/`gzvprintf`
/// symbols: it runs `vsnprintf` into a bounded buffer and then calls this
/// function to perform the actual gzip write through the safe `gz` engine via
/// [`gzprintf_bytes`](crate::gz::gzprintf_bytes) — the byte-oriented core of
/// zlib's formatted write (`gzwrite.c`), which enforces the `want`-size limit
/// and updates the checksum/position exactly as C does.
///
/// `buf` points at `len` already-formatted bytes. Returns the number of
/// uncompressed bytes written (matching zlib's `gzprintf` return), `0` when the
/// render is empty or would not fit in the stream's buffer (`want`), or a
/// negative `Z_*` code (e.g. `Z_STREAM_ERROR` for a null/invalid handle). The
/// `zlibrs_` prefix keeps this internal glue out of the canonical zlib symbol
/// namespace; it is not part of the public zlib API and is excluded from the
/// generated C header.
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn zlibrs_gzprintf_write(
    file: gzFile,
    buf: *const c_uchar,
    len: c_int,
) -> c_int {
    // SAFETY: `gz_handle` performs the null/validity check on the handle.
    let Some(mut handle) = (unsafe { gz_handle(file) }) else {
        return Z_STREAM_ERROR;
    };
    if len < 0 {
        return Z_STREAM_ERROR;
    }
    // SAFETY: the C shim guarantees `buf` is valid for `len` bytes (it points
    // into the shim's formatting buffer); reject an invalid (null, non-zero)
    // pair just like the crate's other FFI buffer entry points.
    let Some(data) = (unsafe { const_buf(buf as *const Bytef, len as usize) }) else {
        return Z_STREAM_ERROR;
    };
    geng::gzprintf_bytes(&mut handle.state, data)
}

// ---------------------------------------------------------------------------
// Variadic `gzprintf` / `gzvprintf` — exported trampolines into the C shim.
//
// These two canonical zlib symbols are VARIADIC (`...` / `va_list`), which
// stable Rust cannot express (`c_variadic` is unstable). The actual variadic
// capture + `vsnprintf` formatting therefore lives in the C shim
// `csrc/gzprintf.c` as `zlibrs_gzprintf_impl` / `zlibrs_gzvprintf_impl`.
//
// A C function cannot simply be named `gzprintf` and be done with it: rustc
// builds the `cdylib`'s linker version script from the crate's Rust
// `#[no_mangle]` items and localizes everything else (`local: *;`), so a
// C-defined `gzprintf` would be present in the archive yet ABSENT from the
// shared object's dynamic symbol table — not callable by a `dlopen`/link-time
// C consumer. To make the canonical symbol a first-class export of BOTH the
// `cdylib` and the `staticlib`, the public `gzprintf` / `gzvprintf` are defined
// HERE as Rust `#[no_mangle]` symbols, so rustc adds them to the export set.
//
// Each is a `#[unsafe(naked)]` function whose entire body is a single tail
// `jmp` to the C implementation. A naked tail-jump emits NO prologue/epilogue
// and touches NO registers, so every argument register (the integer registers
// `rdi, rsi, rdx, rcx, r8, r9`, the vector registers, the variadic count in
// `al`, and any stack arguments) is forwarded to the C implementation exactly
// as the caller set it — i.e. the variadic arguments pass through untouched —
// and the C implementation's `ret` returns straight to the original caller.
// The result is a zero-overhead, ABI-exact bridge: a true variadic `gzprintf`
// that is also an exported symbol of the drop-in library.
//
// MSRV NOTE: naked functions (`#[unsafe(naked)]` / `core::arch::naked_asm!`)
// were stabilized in Rust 1.88. They are used ONLY under the optional,
// non-default `capi` drop-in feature; the default pure-Rust library keeps the
// crate's declared 1.85 MSRV. The `capi` feature therefore requires Rust
// >= 1.88 (and a C compiler for the shim) — see `Cargo.toml`.
// ---------------------------------------------------------------------------

// The C-shim implementations. Only their *addresses* are needed (taken via the
// `sym` operand below), so they are declared as bare externs; the real C
// signatures are variadic and live in `csrc/gzprintf.c`.
#[cfg(feature = "gz-io")]
unsafe extern "C" {
    fn zlibrs_gzprintf_impl();
    fn zlibrs_gzvprintf_impl();
}

/// C `gzprintf` — convert, format, compress, and write the variadic arguments
/// under control of `format`, as in `fprintf` (zlib `gzprintf`). Returns the
/// number of uncompressed bytes written, `0` if nothing fit in the stream
/// buffer, or a negative `Z_*` code on error.
///
/// This is a naked tail-call trampoline into the C shim
/// (`zlibrs_gzprintf_impl`) that performs the variadic formatting; see the
/// module-level note above for why the canonical symbol is defined in Rust.
#[cfg(feature = "gz-io")]
#[unsafe(naked)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzprintf(_file: gzFile, _format: *const c_char) -> c_int {
    // SAFETY: a bare tail `jmp` to the C implementation. No registers are
    // touched, so all (including variadic) arguments are forwarded verbatim and
    // the callee's `ret` returns to our caller.
    core::arch::naked_asm!("jmp {tgt}", tgt = sym zlibrs_gzprintf_impl)
}

/// C `gzvprintf` — the `va_list` form of [`gzprintf`] (zlib `gzvprintf`). Same
/// return contract as `gzprintf`.
///
/// Naked tail-call trampoline into the C shim (`zlibrs_gzvprintf_impl`); the
/// `va_list` and all other arguments are forwarded verbatim.
#[cfg(feature = "gz-io")]
#[unsafe(naked)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzvprintf(
    _file: gzFile,
    _format: *const c_char,
    _va: *mut c_void,
) -> c_int {
    // SAFETY: a bare tail `jmp` to the C implementation; arguments (including
    // the `va_list`) are forwarded verbatim and the callee's `ret` returns to
    // our caller.
    core::arch::naked_asm!("jmp {tgt}", tgt = sym zlibrs_gzvprintf_impl)
}

/// C `gzflush` — flush pending output with the given `flush` mode.
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzflush(file: gzFile, flush: c_int) -> c_int {
    // SAFETY: `gz_handle` performs the null check.
    let Some(mut handle) = (unsafe { gz_handle(file) }) else {
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
