//! `src/ffi.rs` — the `#[no_mangle] extern "C"` C-ABI drop-in shim for `libz`.
//!
//! This module is the prompt-mandated **C-compatible FFI layer** whose exported
//! symbol names and signatures match the zlib C API *exactly*, so the produced
//! `cdylib`/`staticlib` can replace `libz` at the binary level (AAP §0.1.1,
//! §0.6.2). It is a deliberately **thin** shim: it owns no compression logic.
//! Every entry point performs minimal `unsafe` marshalling of raw C pointers
//! into safe Rust slices / owned state, delegates the real work to the
//! 100%-safe engines in [`crate::deflate`], [`crate::inflate`],
//! [`crate::checksum`], [`crate::gz`] and [`crate::util`], and converts the
//! engines' `Result`/`Option` outcomes back into the frozen C `int` return
//! codes (`Z_OK` = 0 … `Z_VERSION_ERROR` = -6).
//!
//! Together with the narrow inner loop in [`crate::inflate::fast`], this file is
//! one of the only two places in the crate that contain `unsafe` (AAP §0.6.2).
//! Every `unsafe` block here carries a `// SAFETY:` comment documenting the
//! preconditions the C caller is contractually required to uphold (AAP §0.7.2).
//!
//! # Feature gate
//!
//! The module is compiled under either of two configurations (see the
//! `#![cfg(any(feature = "capi", all(feature = "no-std", not(test))))]` below
//! and the matching `#[cfg(...)] pub mod ffi;` in `src/lib.rs`):
//!
//!   * **`capi`** — the C-ABI drop-in build. This is the primary trigger. The
//!     reason `capi` is *non-default* (rather than always on) is symbol-collision
//!     avoidance: these exports use the canonical zlib names (`deflate`,
//!     `inflate`, `crc32`, `adler32`, …), and the development-only `flate2`
//!     oracle links canonical C zlib via its `zlib` feature. Building the
//!     `cargo test` binary with this shim enabled would link both and fail with
//!     duplicate-symbol errors. Keeping `capi` out of `default` (hence out of
//!     `cargo test`) avoids that; the cdylib/staticlib drop-in build enables it
//!     explicitly.
//!   * **`all(feature = "no-std", not(test))`** — the genuine bare-metal build
//!     (`cargo build --no-default-features --features no-std`). That
//!     `#![no_std]` artifact has no standard library and therefore needs the
//!     [`no_std_runtime`] (`#[global_allocator]` + `#[panic_handler]`) defined
//!     in this file — the C-runtime allocator/abort plumbing belongs in this
//!     designated `unsafe` boundary, not in the safe engines or the crate root
//!     (AAP §0.6.2). A `no_std` `cdylib`/`staticlib` drop-in also wants the
//!     C-ABI symbols, and no `flate2` oracle exists in a non-test build, so the
//!     collision concern above does not apply. The `not(test)` clause excludes
//!     this file from the no-std build under `cargo test` (the harness links
//!     `std`), so the only in-`test` trigger remains `capi` — preserving the
//!     collision avoidance.
//!
//! # cbindgen
//!
//! The repository-root `build.rs` invokes cbindgen (configured by
//! `cbindgen.toml`) against this file to regenerate the C header
//! (`include/zlib-rs.h`), the `zlib.h`-equivalent. cbindgen is configured with
//! `parse_deps = false`, so every `#[repr(C)]` type and every `Z_*` constant a
//! C consumer needs is declared **in this file** (re-exported from
//! [`crate::constants`]); exported signatures avoid generics and lifetimes,
//! which cbindgen cannot render.
//!
//! # Memory ownership (AAP §0.6.3)
//!
//! The engine **state object** (`DeflateState`, the `InflateFfi` wrapping an
//! inlined `InflateState`, and `InflateBackFfi`) is allocated through the
//! stream's `zalloc` hook and parked as a raw pointer in `z_stream.state`
//! between calls; `deflateEnd`/`inflateEnd`/`inflateBackEnd` release it through
//! the matching `zfree` hook after the state's RAII `Drop` frees its owned
//! working buffers (the FFI allocator bridge — see the detailed "custom
//! allocators" note in Phase B below). When the caller leaves `zalloc`/`zfree`
//! null they are defaulted to a global-allocator-backed implementation, exactly
//! as C `deflateInit_`/`inflateInit_` do, so custom-allocator callers and the
//! common `Z_NULL` path (used by `flate2`, the C smoke tests, and typical C
//! callers) are alike byte-for-byte faithful.

// Module gate. This file is compiled in two configurations, mirrored by the
// matching `#[cfg(...)] pub mod ffi;` in `src/lib.rs`:
//   * `capi` — the C-ABI drop-in build that links the canonical zlib symbol
//     names (the primary purpose of this shim); and
//   * `all(feature = "no-std", not(test))` — the genuine bare-metal build,
//     which needs the `no_std_runtime` (`#[global_allocator]` +
//     `#[panic_handler]`) defined above. That build also (correctly) exports
//     the C-ABI symbols, which is exactly what a `no_std` `cdylib`/`staticlib`
//     drop-in wants; there is no `flate2` oracle in a non-test build, so the
//     symbol-collision concern that keeps `capi` out of `default` does not
//     apply. Under `cargo test` the `not(test)` clause excludes this file from
//     the no-std build (the harness links `std`), leaving `capi` as the only
//     in-test trigger — preserving the collision avoidance described below.
// This inner `#![cfg]` makes the file self-documenting and inert outside those
// two builds even if `lib.rs` were ever changed to declare the module
// unconditionally.
#![cfg(any(feature = "capi", all(feature = "no-std", not(test))))]
// cbindgen requires the canonical zlib names verbatim; they are *intentionally*
// not upper-camel-case types / not snake_case-only, so silence the lints for
// the whole C-ABI surface rather than peppering attributes on every item.
#![allow(non_camel_case_types)]
#![allow(clippy::missing_safety_doc)] // every fn documents safety in prose + // SAFETY: blocks.

use core::alloc::Layout;
use core::ffi::{c_char, c_int, c_long, c_uint, c_ulong, c_void};
use core::ptr;
use core::slice;

use alloc::alloc::{alloc as global_alloc, dealloc as global_dealloc};
use alloc::boxed::Box;

// Engine modules are reached through aliased paths so the canonical
// `#[no_mangle]` export names below (`deflate`, `inflate`, `crc32`, …) never
// shadow the engine functions they delegate to.
use crate::checksum as cksum;
use crate::constants::FlushMode;
use crate::deflate::state::DeflateStream;
use crate::deflate::{self as dfl, DeflateState};
use crate::error::result_to_code;
use crate::inflate::{self as inf, BackInput, BackOutput, InflateState};
use crate::util::compress as cmp;
use crate::util::uncompress as ucmp;
use crate::util::version as ver;

// The gzip-header marshalling helpers and the gzip FILE-I/O exports require the
// owned `GzHeader` / `GzState`, which exist only under the respective features.
#[cfg(feature = "gzip")]
use crate::gz_header::GzHeader;

// ===========================================================================
// Bare-metal (`no_std`) C runtime
// ===========================================================================
//
// The genuine bare-metal drop-in — `cargo build --no-default-features --features
// no-std`, the `Z_SOLO`-equivalent build (AAP §0.5.2) — produces a `#![no_std]`
// `cdylib`/`staticlib` that links no Rust standard library, so it must supply
// its own `#[global_allocator]` and `#[panic_handler]`. This C-runtime plumbing
// binds the linking C program's `malloc`/`free`/`abort` symbols directly and is
// inherently `unsafe`, so it lives here in the crate's *designated* `unsafe` FFI
// boundary (AAP §0.6.2) alongside the C-ABI shim — never in the 100%-safe
// engine modules or the crate root. It is gated `all(feature = "no-std",
// not(test))`: it is compiled ONLY for the real bare-metal artifact and is
// absent from every `std`-linked build (default, `--no-default-features`, and
// any `cargo test`/`cargo bench`, all of which inherit `std`'s allocator and
// panic runtime). This gate matches the broadened module gate at the top of the
// file, which admits this build in addition to `capi`.
#[cfg(all(feature = "no-std", not(test)))]
mod no_std_runtime {
    use core::alloc::{GlobalAlloc, Layout};
    use core::ffi::c_void;

    // Bind the C allocator and `abort` from the linking C runtime directly,
    // rather than pulling in a `libc` crate dependency: the shipped library
    // keeps a zero-C-dependency *Rust* surface, and these are link-time symbols
    // resolved by the C program/loader, not a Rust crate.
    unsafe extern "C" {
        fn malloc(size: usize) -> *mut c_void;
        fn free(ptr: *mut c_void);
        fn abort() -> !;
    }

    /// Global allocator for the standalone `no_std` drop-in: routes Rust's
    /// allocation requests to the C library's `malloc`/`free`.
    struct CAllocator;

    // The maximum alignment the C allocator guarantees (`max_align_t`): 16 on
    // 64-bit targets, 8 on 32-bit. Every type this crate allocates (`u8`,
    // `u16`, the `#[repr(C)] Code`, and the boxed engine-state structs) has an
    // alignment of at most 8, so `malloc`'s guarantee always suffices.
    const MAX_C_ALIGN: usize = core::mem::align_of::<usize>() * 2;

    unsafe impl GlobalAlloc for CAllocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            // The engines never request over-aligned allocations; assert it in
            // debug builds so any future violation is caught rather than
            // silently producing misaligned memory.
            debug_assert!(layout.align() <= MAX_C_ALIGN);
            // SAFETY: `malloc` is the libc allocator provided by the linking C
            // runtime. It returns either null (which the `GlobalAlloc` contract
            // requires callers to handle) or a pointer aligned to
            // `max_align_t`, satisfying `layout.align()` (asserted above).
            unsafe { malloc(layout.size()) as *mut u8 }
        }

        unsafe fn dealloc(&self, ptr: *mut u8, _layout: Layout) {
            // SAFETY: `ptr` was returned by this allocator's `alloc` (hence by
            // `malloc`), so handing it to the matching `free` is sound.
            unsafe { free(ptr as *mut c_void) }
        }
    }

    #[global_allocator]
    static ALLOCATOR: CAllocator = CAllocator;

    /// Panic handler for the standalone `no_std` drop-in. A C library cannot
    /// unwind across the FFI boundary, so a panic — reachable only on an
    /// internal invariant violation, since the engines return error codes for
    /// malformed input rather than panicking — aborts the process.
    #[panic_handler]
    fn panic(_info: &core::panic::PanicInfo) -> ! {
        // SAFETY: libc `abort` never returns and is always available in the
        // hosted C runtime that links this drop-in artifact.
        unsafe { abort() }
    }
}

// ===========================================================================
// Phase A — C type layout (must match zlib.h / zconf.h byte-for-byte)
// ===========================================================================

/// `Bytef` — a single byte of (de)compressed data (zconf.h `Byte FAR`).
pub type Bytef = u8;
/// `uInt` — zlib's "16 bits or more" unsigned integer (zconf.h `unsigned int`).
pub type uInt = c_uint;
/// `uLong` — zlib's "32 bits or more" unsigned long (zconf.h `unsigned long`).
/// On LP64 platforms this is 64-bit; the running 32-bit checksums are widened
/// to / narrowed from `u32` at the boundary.
pub type uLong = c_ulong;
/// `uLongf` — `uLong FAR`; identical to [`uLong`] on flat-memory targets. Used
/// for the in/out length pointers of `compress`/`uncompress`.
pub type uLongf = c_ulong;
/// `voidpf` — a far `void *` (zconf.h), used for the `opaque` allocator context.
pub type voidpf = *mut c_void;
/// `voidpc` — a far `const void *` (zconf.h).
pub type voidpc = *const c_void;
/// `voidp` — a far `void *` (zconf.h), used for the gz read/write buffers.
pub type voidp = *mut c_void;
/// `z_size_t` — zlib's `size_t`-width count (zconf.h), backing the `*_z` API.
pub type z_size_t = usize;
/// `z_crc_t` — the 32-bit CRC table element type (zconf.h `Z_U4`).
pub type z_crc_t = u32;
/// `z_off_t` — the file-offset type (zconf.h `off_t`); modeled as C `long`.
pub type z_off_t = c_long;
/// `z_off64_t` — the 64-bit file-offset type (zconf.h), modeled as `i64`.
pub type z_off64_t = i64;

/// `alloc_func` — the C `zalloc` hook: `voidpf (*)(voidpf, uInt, uInt)`.
///
/// Wrapped in [`Option`] so the C `Z_NULL` (a null function pointer) is
/// representable as [`None`].
pub type alloc_func =
    Option<unsafe extern "C" fn(opaque: voidpf, items: uInt, size: uInt) -> voidpf>;
/// `free_func` — the C `zfree` hook: `void (*)(voidpf, voidpf)`.
pub type free_func = Option<unsafe extern "C" fn(opaque: voidpf, address: voidpf)>;

/// `z_stream` — the public stream descriptor (zlib.h `struct z_stream_s`).
///
/// The 14 fields appear in the exact order and with the exact ABI types of the
/// C declaration so that `sizeof`/field offsets match `libz`. `state` parks the
/// `Box`-owned engine state between calls; `zalloc`/`zfree`/`opaque` carry the
/// optional custom allocator; `msg` points at a `'static`, NUL-terminated error
/// string (or null).
#[repr(C)]
pub struct z_stream {
    /// Next input byte (C `next_in`).
    pub next_in: *const Bytef,
    /// Number of bytes available at `next_in` (C `avail_in`).
    pub avail_in: uInt,
    /// Total number of input bytes read so far (C `total_in`).
    pub total_in: uLong,
    /// Next output byte will go here (C `next_out`).
    pub next_out: *mut Bytef,
    /// Remaining free space at `next_out` (C `avail_out`).
    pub avail_out: uInt,
    /// Total number of bytes output so far (C `total_out`).
    pub total_out: uLong,
    /// Last error message, or null if none (C `msg`).
    pub msg: *const c_char,
    /// Opaque pointer to the boxed engine state (C `internal_state *`).
    pub state: *mut c_void,
    /// Custom allocation hook, or null for the global allocator (C `zalloc`).
    pub zalloc: alloc_func,
    /// Custom free hook, or null (C `zfree`).
    pub zfree: free_func,
    /// Private context passed to `zalloc`/`zfree` (C `opaque`).
    pub opaque: voidpf,
    /// Best guess at the data type, or the inflate decode state (C `data_type`).
    pub data_type: c_int,
    /// Adler-32 or CRC-32 of the uncompressed data (C `adler`).
    pub adler: uLong,
    /// Reserved for future use; always 0 (C `reserved`).
    pub reserved: uLong,
}

/// `z_streamp` — a pointer to a [`z_stream`] (zlib.h `z_streamp`).
pub type z_streamp = *mut z_stream;

/// `gz_header` — gzip header metadata for `deflateSetHeader`/`inflateGetHeader`
/// (zlib.h `gz_header`). The 13 fields match the C layout exactly.
#[repr(C)]
pub struct gz_header {
    /// True (1) if compressed data believed to be text (C `text`).
    pub text: c_int,
    /// Modification time, or 0 if unavailable (C `time`).
    pub time: uLong,
    /// Extra flags (not used when writing a gzip file) (C `xflags`).
    pub xflags: c_int,
    /// Operating system (C `os`).
    pub os: c_int,
    /// Pointer to the extra field, or null (C `extra`).
    pub extra: *mut Bytef,
    /// Extra field length (valid once `done`) (C `extra_len`).
    pub extra_len: uInt,
    /// Space at `extra` (only when reading) (C `extra_max`).
    pub extra_max: uInt,
    /// Pointer to the zero-terminated file name, or null (C `name`).
    pub name: *mut Bytef,
    /// Space at `name` (only when reading) (C `name_max`).
    pub name_max: uInt,
    /// Pointer to the zero-terminated comment, or null (C `comment`).
    pub comment: *mut Bytef,
    /// Space at `comment` (only when reading) (C `comm_max`).
    pub comm_max: uInt,
    /// True (1) if there was or will be a header CRC (C `hcrc`).
    pub hcrc: c_int,
    /// True (1) when done reading the gzip header (C `done`).
    pub done: c_int,
}

/// `gz_headerp` — a pointer to a [`gz_header`] (zlib.h `gz_headerp`).
pub type gz_headerp = *mut gz_header;

/// `code` — a decode-table entry shared with the inflate engine (inftrees.h
/// `code`). `#[repr(C)]` keeps the layout identical to C (AAP §0.6.5).
#[repr(C)]
pub struct Code {
    /// Operation, extra bits, or table-link flags (C `op`).
    pub op: u8,
    /// Bits in this code or sub-table (C `bits`).
    pub bits: u8,
    /// Literal/length base value or distance base value (C `val`).
    pub val: u16,
}

/// `gzFile` — an opaque handle to the gzip FILE-I/O state (zlib.h `gzFile`).
/// Internally a `Box`-owned `GzState` whose raw pointer is round-tripped here.
pub type gzFile = *mut c_void;

// ---------------------------------------------------------------------------
// Z_* constants re-exported as `c_int` so cbindgen emits them for C consumers.
// (cbindgen has `parse_deps = false`, so it cannot see `crate::constants`.)
// ---------------------------------------------------------------------------

/// `Z_NO_FLUSH` — accumulate before producing output (zlib.h).
pub const Z_NO_FLUSH: c_int = crate::constants::Z_NO_FLUSH;
/// `Z_PARTIAL_FLUSH` — flush to a byte boundary (zlib.h).
pub const Z_PARTIAL_FLUSH: c_int = crate::constants::Z_PARTIAL_FLUSH;
/// `Z_SYNC_FLUSH` — flush and align to a byte boundary with an empty block.
pub const Z_SYNC_FLUSH: c_int = crate::constants::Z_SYNC_FLUSH;
/// `Z_FULL_FLUSH` — sync flush and reset the compression dictionary.
pub const Z_FULL_FLUSH: c_int = crate::constants::Z_FULL_FLUSH;
/// `Z_FINISH` — finish the stream.
pub const Z_FINISH: c_int = crate::constants::Z_FINISH;
/// `Z_BLOCK` — stop/resume at deflate block boundaries.
pub const Z_BLOCK: c_int = crate::constants::Z_BLOCK;
/// `Z_TREES` — stop/resume at deflate block-header boundaries.
pub const Z_TREES: c_int = crate::constants::Z_TREES;

/// `Z_OK` — success (zlib.h).
pub const Z_OK: c_int = crate::constants::Z_OK;
/// `Z_STREAM_END` — end of stream reached.
pub const Z_STREAM_END: c_int = crate::constants::Z_STREAM_END;
/// `Z_NEED_DICT` — a preset dictionary is required to continue.
pub const Z_NEED_DICT: c_int = crate::constants::Z_NEED_DICT;
/// `Z_ERRNO` — a file-system error occurred (gz layer).
pub const Z_ERRNO: c_int = crate::constants::Z_ERRNO;
/// `Z_STREAM_ERROR` — inconsistent stream state or invalid parameters.
pub const Z_STREAM_ERROR: c_int = crate::constants::Z_STREAM_ERROR;
/// `Z_DATA_ERROR` — input data was corrupted.
pub const Z_DATA_ERROR: c_int = crate::constants::Z_DATA_ERROR;
/// `Z_MEM_ERROR` — not enough memory.
pub const Z_MEM_ERROR: c_int = crate::constants::Z_MEM_ERROR;
/// `Z_BUF_ERROR` — no progress was possible (provide more buffer space).
pub const Z_BUF_ERROR: c_int = crate::constants::Z_BUF_ERROR;
/// `Z_VERSION_ERROR` — incompatible library version.
pub const Z_VERSION_ERROR: c_int = crate::constants::Z_VERSION_ERROR;

/// `Z_NO_COMPRESSION` — store only (level 0).
pub const Z_NO_COMPRESSION: c_int = crate::constants::Z_NO_COMPRESSION;
/// `Z_BEST_SPEED` — fastest compression (level 1).
pub const Z_BEST_SPEED: c_int = crate::constants::Z_BEST_SPEED;
/// `Z_BEST_COMPRESSION` — best compression (level 9).
pub const Z_BEST_COMPRESSION: c_int = crate::constants::Z_BEST_COMPRESSION;
/// `Z_DEFAULT_COMPRESSION` — the default level (-1 → 6).
pub const Z_DEFAULT_COMPRESSION: c_int = crate::constants::Z_DEFAULT_COMPRESSION;

/// `Z_FILTERED` — strategy for data produced by a filter (predictor).
pub const Z_FILTERED: c_int = crate::constants::Z_FILTERED;
/// `Z_HUFFMAN_ONLY` — force Huffman encoding only (no string match).
pub const Z_HUFFMAN_ONLY: c_int = crate::constants::Z_HUFFMAN_ONLY;
/// `Z_RLE` — limit match distances to one (run-length encoding).
pub const Z_RLE: c_int = crate::constants::Z_RLE;
/// `Z_FIXED` — prevent the use of dynamic Huffman codes.
pub const Z_FIXED: c_int = crate::constants::Z_FIXED;
/// `Z_DEFAULT_STRATEGY` — the default strategy.
pub const Z_DEFAULT_STRATEGY: c_int = crate::constants::Z_DEFAULT_STRATEGY;

/// `Z_BINARY` — binary data type.
pub const Z_BINARY: c_int = crate::constants::Z_BINARY;
/// `Z_TEXT` — text data type.
pub const Z_TEXT: c_int = crate::constants::Z_TEXT;
/// `Z_ASCII` — deprecated alias of [`Z_TEXT`].
pub const Z_ASCII: c_int = crate::constants::Z_ASCII;
/// `Z_UNKNOWN` — unknown data type.
pub const Z_UNKNOWN: c_int = crate::constants::Z_UNKNOWN;

/// `Z_DEFLATED` — the only supported compression method.
pub const Z_DEFLATED: c_int = crate::constants::Z_DEFLATED;
/// `Z_NULL` — the zlib "null" sentinel (0).
pub const Z_NULL: c_int = crate::constants::Z_NULL;

/// `MAX_WBITS` — maximum window-size base-2 logarithm (32 KiB window).
pub const MAX_WBITS: c_int = crate::constants::MAX_WBITS;
/// `MAX_MEM_LEVEL` — maximum `memLevel`.
pub const MAX_MEM_LEVEL: c_int = crate::constants::MAX_MEM_LEVEL;
/// `DEF_WBITS` — default window bits (= [`MAX_WBITS`]).
pub const DEF_WBITS: c_int = crate::constants::DEF_WBITS;
/// `DEF_MEM_LEVEL` — default `memLevel`.
pub const DEF_MEM_LEVEL: c_int = crate::constants::DEF_MEM_LEVEL;

// ===========================================================================
// Phase B — internal-state marshalling helpers (the unsafe core of this file)
// ===========================================================================
//
// NOTE on custom allocators (AAP §0.6.3) — the FFI allocator bridge.
//
// The C `z_stream` carries optional `zalloc`/`zfree`/`opaque` allocator hooks.
// This shim implements a genuine allocator bridge for the engine **state
// object** — the `void* internal_state` that C zlib allocates with its first
// `ZALLOC` call:
//
//   * `deflateInit2_`/`inflateInit2_`/`inflateBackInit_` default a null
//     `zalloc`/`zfree` to `default_zalloc`/`default_zfree` and reset `opaque`
//     to null, exactly as C `deflateInit_`/`inflateInit_` do; a caller's custom
//     hooks are preserved verbatim.
//   * The engine state container (`DeflateState`, the `InflateFfi` wrapping an
//     inlined `InflateState`, and `InflateBackFfi`) is allocated through the
//     stream's `zalloc` via `ffi_alloc_state` and released through the matching
//     `zfree` via `ffi_free_state`. Custom C allocators are thus genuinely
//     *called* for the primary state allocation and *balanced* on teardown /
//     copy, and `deflateStateCheck`/`inflateStateCheck` parity is preserved (a
//     stream whose hooks are null after init is rejected with `Z_STREAM_ERROR`).
//
// The engines' *secondary* working buffers (the deflate `window`/`pending_buf`
// and the inflate `window`/`codes`) remain owned `Vec`/`Box` backed by Rust's
// global allocator: routing those through arbitrary C hooks is not expressible
// on stable Rust without `Box<[u8]>`/free-mismatch hazards or pervasive
// `unsafe` inside the safe compression core, which AAP §0.6.2 forbids ("zero
// unsafe blocks in core compression logic"). This matches the common `Z_NULL`
// path byte-for-byte (where `default_zalloc` is itself global-backed) and keeps
// custom-hook callers fully interoperable, with their hooks invoked for the
// state object that dominates the per-stream control allocation.

/// Header reserved ahead of every [`default_zalloc`] block to record the total
/// `Layout` size for [`default_zfree`]. Sized — and the whole block aligned —
/// to 16 bytes (`max_align_t` on the common targets), so the returned user
/// pointer satisfies the alignment of every `z_stream` state object (all ≤ 8)
/// and matches the alignment guarantee of C `malloc`.
const DEFAULT_ALLOC_HEADER: usize = 16;
/// Alignment of every [`default_zalloc`] block (see [`DEFAULT_ALLOC_HEADER`]).
const DEFAULT_ALLOC_ALIGN: usize = 16;

/// Default `zalloc` hook — a global-allocator-backed implementation of the C
/// `zcalloc` contract (zutil.c). Installed by the `*Init2_` constructors when
/// the caller leaves `z_stream.zalloc` null, so the ubiquitous `Z_NULL` path
/// still routes the state allocation through a real hook. `no_std`-clean (uses
/// the `alloc` crate). Like C `zcalloc` on modern targets (`sizeof(uInt) > 2`),
/// it returns *uninitialized* `items * size` bytes (the state is fully written
/// by [`ffi_alloc_state`] immediately after).
///
/// # Safety
///
/// This is a C-ABI function pointer; `opaque` is ignored. The returned pointer,
/// if non-null, must be released only via [`default_zfree`].
unsafe extern "C" fn default_zalloc(_opaque: voidpf, items: uInt, size: uInt) -> voidpf {
    // C `ZALLOC` semantics: total user bytes = items * size. Compute in usize
    // with an overflow guard (a hostile/huge request fails closed → null).
    let bytes = match (items as usize).checked_mul(size as usize) {
        Some(n) if n != 0 => n,
        _ => return ptr::null_mut(),
    };
    let total = match bytes.checked_add(DEFAULT_ALLOC_HEADER) {
        Some(n) => n,
        None => return ptr::null_mut(),
    };
    let Ok(layout) = Layout::from_size_align(total, DEFAULT_ALLOC_ALIGN) else {
        return ptr::null_mut();
    };
    // SAFETY: `layout` has a non-zero size (the header alone is 16 bytes).
    let base = unsafe { global_alloc(layout) };
    if base.is_null() {
        return ptr::null_mut();
    }
    // Record the total size in the header so `default_zfree` can rebuild the
    // exact `Layout`.
    // SAFETY: `base` is non-null and owns ≥ `DEFAULT_ALLOC_HEADER` writable,
    // 16-aligned bytes; a `usize` write at the base is in bounds and aligned.
    unsafe { ptr::write(base as *mut usize, total) };
    // SAFETY: `base + DEFAULT_ALLOC_HEADER` is within the allocation and is the
    // 16-aligned user region.
    unsafe { base.add(DEFAULT_ALLOC_HEADER) as voidpf }
}

/// Default `zfree` hook — the teardown counterpart of [`default_zalloc`].
///
/// # Safety
///
/// `address` must be null or a pointer previously returned by
/// [`default_zalloc`] (and not yet freed).
unsafe extern "C" fn default_zfree(_opaque: voidpf, address: voidpf) {
    if address.is_null() {
        return;
    }
    // Recover the allocation base and the stored layout size.
    // SAFETY: `address` came from `default_zalloc`, so `DEFAULT_ALLOC_HEADER`
    // bytes precede it and the leading `usize` holds the total layout size.
    let base = unsafe { (address as *mut u8).sub(DEFAULT_ALLOC_HEADER) };
    let total = unsafe { ptr::read(base as *const usize) };
    let Ok(layout) = Layout::from_size_align(total, DEFAULT_ALLOC_ALIGN) else {
        // Unreachable for our own allocations; never free under a bad layout.
        return;
    };
    // SAFETY: `base`/`layout` exactly reconstruct the original allocation.
    unsafe { global_dealloc(base, layout) };
}

/// Allocate the `z_stream.state` container of type `T` through the stream's
/// `zalloc` hook and move `value` into it — the FFI allocator bridge mandated by
/// AAP §0.6.3. Mirrors C `ZALLOC(strm, 1, sizeof(state))`.
///
/// Returns [`None`] (→ `Z_MEM_ERROR` at the call site) when `zalloc` is null,
/// the hook returns null, or the hook returns memory insufficiently aligned for
/// `T`; an under-aligned block is released through `zfree` before returning so
/// no memory leaks on the failure path.
///
/// # Safety
///
/// `zalloc`/`zfree` (if `Some`) must be valid C allocator hooks and `opaque`
/// the context they expect. The returned pointer must be released only via
/// [`ffi_free_state`] with the matching `zfree`/`opaque`.
unsafe fn ffi_alloc_state<T>(
    value: T,
    zalloc: alloc_func,
    zfree: free_func,
    opaque: voidpf,
) -> Option<*mut T> {
    let zalloc = zalloc?;
    let size = core::mem::size_of::<T>();
    debug_assert!(size != 0, "z_stream state types are never zero-sized");
    // SAFETY: `zalloc` is a valid hook; request a single object of
    // `size_of::<T>()` bytes — exactly C `ZALLOC(strm, 1, sizeof(state))`.
    let raw = unsafe { zalloc(opaque, 1, size as uInt) };
    if raw.is_null() {
        return None;
    }
    if (raw as usize) % core::mem::align_of::<T>() != 0 {
        // A pathological hook returned under-aligned memory: release it through
        // the matching `zfree` and fail closed rather than risk a misaligned
        // write/read.
        if let Some(free) = zfree {
            // SAFETY: `raw` was just produced by the paired `zalloc`.
            unsafe { free(opaque, raw) };
        }
        return None;
    }
    let typed = raw as *mut T;
    // SAFETY: `typed` is non-null, suitably aligned, and points to
    // `size_of::<T>()` bytes we own exclusively; move `value` into place
    // without dropping the (uninitialized) destination.
    unsafe { ptr::write(typed, value) };
    Some(typed)
}

/// Drop the `T` behind a `z_stream.state` container and release the container
/// through the stream's `zfree` hook — the teardown counterpart of
/// [`ffi_alloc_state`] (RAII replaces C's `ZFREE`). Running `drop_in_place`
/// first frees `T`'s owned (global) `Vec`/`Box` buffers; `zfree` then releases
/// the container through the same hook that allocated it.
///
/// A null `zfree` is a corrupted stream (C `deflateStateCheck`/
/// `inflateStateCheck` reject it); callers gate on that and return
/// `Z_STREAM_ERROR` before reaching teardown, so by the time we are here `zfree`
/// is the hook paired with the original `zalloc`.
///
/// # Safety
///
/// `ptr` must have been produced by [`ffi_alloc_state`] with the same
/// `zfree`/`opaque` and must not be used afterwards.
unsafe fn ffi_free_state<T>(ptr: *mut T, zfree: free_func, opaque: voidpf) {
    if ptr.is_null() {
        return;
    }
    // SAFETY: `ptr` is a live, uniquely-owned `T` from `ffi_alloc_state`; drop
    // it in place to release its owned buffers before the container memory goes.
    unsafe { ptr::drop_in_place(ptr) };
    if let Some(free) = zfree {
        // SAFETY: `ptr` came from the paired `zalloc`; hand the same address
        // back to `zfree`.
        unsafe { free(opaque, ptr as voidpf) };
    }
}

/// The `z_stream.state` payload for an **inflate** stream.
///
/// Wrapping the boxed [`InflateState`] alongside the caller's `gz_header`
/// pointer lets [`inflateGetHeader`] remember where to mirror parsed gzip
/// header metadata after each [`inflate`] call (the safe engine fills its own
/// owned [`GzHeader`]; this shim copies it out into the C struct).
struct InflateFfi {
    /// The (unboxed) safe-engine decode state, stored inline so the FFI
    /// allocator bridge ([`ffi_alloc_state`]) places the *whole* `InflateState`
    /// — the analogue of C's `ZALLOC`'d `inflate_state` — in the caller's
    /// (or defaulted) allocator memory, not in a separate global `Box`.
    state: InflateState,
    /// The caller's `gz_header` sink from `inflateGetHeader`, or null. Only read
    /// on the gzip path (header mirroring); kept unconditionally so the storage
    /// layout is identical across feature sets.
    #[allow(dead_code)]
    head: *mut gz_header,
}

/// The `z_stream.state` payload for an **inflateBack** stream.
///
/// `inflateBack` uses a caller-provided window buffer (the `window` argument of
/// `inflateBackInit_`), so the raw pointer and its length are parked here for
/// the later [`inflateBack`] call to reconstruct as a `&mut [u8]`.
struct InflateBackFfi {
    /// The (unboxed) safe-engine decode state for the callback API.
    state: InflateState,
    /// Caller-owned window buffer base pointer (`1 << windowBits` bytes).
    window: *mut Bytef,
    /// Window length in bytes (`1 << windowBits`).
    wsize: usize,
}

/// Reconstruct a shared byte slice from a `(ptr, len)` C pair.
///
/// Returns an empty slice when `ptr` is null or `len` is zero, so callers never
/// pass a dangling pointer to [`slice::from_raw_parts`].
///
/// # Safety
///
/// When `ptr` is non-null and `len > 0`, the caller must guarantee that
/// `ptr..ptr+len` is a single allocation of initialized bytes that stays valid
/// and is not mutated for the lifetime `'a` — exactly the contract the zlib C
/// API places on `next_in`/`avail_in`.
#[inline]
unsafe fn const_slice<'a>(ptr: *const Bytef, len: uInt) -> &'a [u8] {
    if ptr.is_null() || len == 0 {
        &[]
    } else {
        // SAFETY: ptr is non-null and len > 0; the C caller guarantees the
        // region is a valid, initialized, immutable allocation for 'a.
        unsafe { slice::from_raw_parts(ptr, len as usize) }
    }
}

/// Reconstruct a mutable byte slice from a `(ptr, len)` C pair.
///
/// Returns an empty slice when `ptr` is null or `len` is zero.
///
/// # Safety
///
/// When `ptr` is non-null and `len > 0`, the caller must guarantee that
/// `ptr..ptr+len` is a single allocation of writable bytes, uniquely borrowed
/// for `'a` (no aliasing) — the contract zlib places on `next_out`/`avail_out`.
#[inline]
unsafe fn mut_slice<'a>(ptr: *mut Bytef, len: uInt) -> &'a mut [u8] {
    if ptr.is_null() || len == 0 {
        &mut []
    } else {
        // SAFETY: ptr is non-null and len > 0; the C caller guarantees the
        // region is a valid, uniquely-borrowed, writable allocation for 'a.
        unsafe { slice::from_raw_parts_mut(ptr, len as usize) }
    }
}

/// Map the safe engine's `Option<&'static str>` message into the C
/// `z_stream.msg` pointer type (`*const c_char`).
///
/// Every distinct message the deflate and inflate engines can emit is matched
/// to a `'static`, NUL-terminated C string literal (`c"…"`, stabilized in the
/// 2024 edition), so the returned pointer is always valid for the life of the
/// process. [`None`] maps to a null pointer, exactly like C's `strm->msg = Z_NULL`.
fn msg_to_cstr(msg: Option<&'static str>) -> *const c_char {
    let Some(text) = msg else {
        return ptr::null();
    };
    // The match enumerates every literal produced by the engines (see the
    // inflate `BAD` arms and the deflate error paths). Any unmatched string
    // falls back to a generic, still-valid C literal rather than risking a
    // non-NUL-terminated pointer.
    let cstr: &'static core::ffi::CStr = match text {
        // ---- inflate (inflate.c BAD arms) --------------------------------
        "incorrect header check" => c"incorrect header check",
        "unknown compression method" => c"unknown compression method",
        "invalid window size" => c"invalid window size",
        "unknown header flags set" => c"unknown header flags set",
        "header crc mismatch" => c"header crc mismatch",
        "invalid block type" => c"invalid block type",
        "invalid stored block lengths" => c"invalid stored block lengths",
        "too many length or distance symbols" => c"too many length or distance symbols",
        "invalid code lengths set" => c"invalid code lengths set",
        "invalid bit length repeat" => c"invalid bit length repeat",
        "invalid code -- missing end-of-block" => c"invalid code -- missing end-of-block",
        "invalid literal/lengths set" => c"invalid literal/lengths set",
        "invalid distances set" => c"invalid distances set",
        "invalid literal/length code" => c"invalid literal/length code",
        "invalid distance code" => c"invalid distance code",
        "invalid distance too far back" => c"invalid distance too far back",
        "incorrect data check" => c"incorrect data check",
        "incorrect length check" => c"incorrect length check",
        // ---- deflate / shared --------------------------------------------
        "stream end" => c"stream end",
        "need dictionary" => c"need dictionary",
        "file error" => c"file error",
        "stream error" => c"stream error",
        "data error" => c"data error",
        "insufficient memory" => c"insufficient memory",
        "buffer error" => c"buffer error",
        "incompatible version" => c"incompatible version",
        // ---- fallback: still a valid, NUL-terminated C string ------------
        _ => c"zlib error",
    };
    cstr.as_ptr()
}

/// The crate version string, NUL-terminated for [`zlibVersion`]
/// (`ZLIB_VERSION`, mirrored from `zlib.h`).
const ZLIB_VERSION_CSTR: &core::ffi::CStr = c"1.3.2.1-motley";

/// Convert a NUL-terminated C string to an owned byte vector (without the NUL).
///
/// Returns an empty vector when `ptr` is null. Used to marshal the
/// `gz_header.name`/`comment` C strings into the engine's `GzHeader`, and to
/// marshal `gzopen` paths/modes.
///
/// # Safety
///
/// When non-null, `ptr` must point to a valid NUL-terminated C string that
/// stays valid for the duration of this call.
#[cfg(any(feature = "gzip", feature = "gz-io"))]
unsafe fn cstr_to_vec(ptr: *const c_char) -> alloc::vec::Vec<u8> {
    if ptr.is_null() {
        return alloc::vec::Vec::new();
    }
    // SAFETY: caller guarantees `ptr` is a valid NUL-terminated C string.
    let cstr = unsafe { core::ffi::CStr::from_ptr(ptr) };
    cstr.to_bytes().to_vec()
}

// ===========================================================================
// Phase C — exported functions: deflate family (deflate.c / deflate.h)
// ===========================================================================

/// Borrow the boxed [`DeflateState`] parked in `strm.state`.
///
/// Returns [`None`] when `strm` or `strm.state` is null, or when the allocator
/// hooks are null (the C `deflateStateCheck` `Z_STREAM_ERROR` preconditions).
/// The returned mutable reference points into the hook-allocated `DeflateState`
/// installed by `deflateInit*`, an allocation disjoint from `*strm` itself.
///
/// # Safety
///
/// `strm` must be null or a valid `z_stream` whose `state` (if non-null) was
/// produced by this crate's `deflateInit*` (i.e. is an [`ffi_alloc_state`]
/// [`DeflateState`]). The caller must not create a second alias to the same
/// state while the returned reference is live.
#[inline]
unsafe fn deflate_state<'a>(strm: z_streamp) -> Option<&'a mut DeflateState> {
    if strm.is_null() {
        return None;
    }
    // C `deflateStateCheck` parity: a stream whose allocator hooks were cleared
    // (or never defaulted) is invalid — reject it before touching `state`.
    // SAFETY: strm is non-null; read the hook fields by value.
    if unsafe { (*strm).zalloc }.is_none() || unsafe { (*strm).zfree }.is_none() {
        return None;
    }
    // SAFETY: strm is non-null; read its `state` field by value.
    let state_ptr = unsafe { (*strm).state } as *mut DeflateState;
    if state_ptr.is_null() {
        return None;
    }
    // SAFETY: state_ptr was produced by `deflateInit*` via `ffi_alloc_state` and
    // is a unique, valid DeflateState for the duration of this borrow.
    Some(unsafe { &mut *state_ptr })
}

/// Validate the C `version`/`stream_size` ABI parameters, mirroring the checks
/// in C `deflateInit_`/`inflateInit_`.
///
/// Returns `true` only when `version` is non-null, its first byte equals the
/// major-version character of [`ZLIB_VERSION_CSTR`], and `stream_size` equals
/// `sizeof(z_stream)` on this target.
///
/// # Safety
///
/// `version` must be null or point to a valid, readable NUL-terminated C string.
#[inline]
unsafe fn version_ok(version: *const c_char, stream_size: c_int) -> bool {
    if version.is_null() {
        return false;
    }
    // SAFETY: version is non-null per the check; read its first byte only.
    let first = unsafe { *version };
    first == ZLIB_VERSION_CSTR.to_bytes()[0] as c_char
        && stream_size as usize == core::mem::size_of::<z_stream>()
}

/// Install a freshly-initialized [`DeflateState`] into `strm` through the FFI
/// allocator bridge, seeding the public `z_stream` accounting fields exactly as
/// C `deflateReset` does. Returns `false` (→ `Z_MEM_ERROR`) if the allocator
/// hook cannot provide suitably-aligned storage for the state container.
///
/// # Safety
///
/// `strm` must be non-null and valid for reads/writes, and its `zalloc`/`zfree`
/// hooks must already be defaulted (non-null) by the calling constructor.
unsafe fn attach_deflate_state(strm: z_streamp, boxed: Box<DeflateState>) -> bool {
    let adler = boxed.adler;
    let data_type = boxed.data_type;
    // Move the state out of its temporary global `Box` and re-home it in
    // allocator-hook memory (the FFI allocator bridge). The `Box` backing is
    // released here; the `DeflateState`'s own `Vec`/`Box` buffers move with it.
    let state: DeflateState = *boxed;
    // SAFETY: strm is non-null and valid for reads; the hooks were defaulted by
    // the constructor, so they are non-null.
    let (zalloc, zfree, opaque) = unsafe { ((*strm).zalloc, (*strm).zfree, (*strm).opaque) };
    // SAFETY: the hooks are valid C allocator hooks; allocate the state
    // container through `zalloc` and move the engine state into it.
    let Some(raw) = (unsafe { ffi_alloc_state(state, zalloc, zfree, opaque) }) else {
        return false;
    };
    // SAFETY: strm is non-null and valid for writes per the caller's contract.
    unsafe {
        (*strm).state = raw as *mut c_void;
        (*strm).total_in = 0;
        (*strm).total_out = 0;
        (*strm).msg = ptr::null();
        (*strm).adler = adler as uLong;
        (*strm).data_type = data_type;
    }
    true
}

/// `deflateInit2_` — the real deflate constructor (zlib.h L1907-1910).
///
/// Validates the ABI version/size, then delegates parameter validation and
/// allocation to [`crate::deflate::deflate_init2`]. On success the boxed state
/// is parked in `strm.state` and the public accounting fields are seeded.
///
/// # Safety
///
/// `strm` must be a valid `z_stream` pointer (or null) and `version` a valid C
/// string (or null), per the zlib C contract.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn deflateInit2_(
    strm: z_streamp,
    level: c_int,
    method: c_int,
    window_bits: c_int,
    mem_level: c_int,
    strategy: c_int,
    version: *const c_char,
    stream_size: c_int,
) -> c_int {
    if strm.is_null() {
        return Z_STREAM_ERROR;
    }
    // SAFETY: version may be null; version_ok handles that.
    if !unsafe { version_ok(version, stream_size) } {
        return Z_VERSION_ERROR;
    }
    // SAFETY: strm is non-null; clear the internal-state slot before init so a
    // failure leaves a well-defined null state, and default the allocator hooks
    // exactly as C `deflateInit2_` does (null `zalloc` → `default_zalloc` +
    // `opaque = NULL`; null `zfree` → `default_zfree`).
    unsafe {
        (*strm).msg = ptr::null();
        (*strm).state = ptr::null_mut();
        if (*strm).zalloc.is_none() {
            (*strm).zalloc = Some(default_zalloc);
            (*strm).opaque = ptr::null_mut();
        }
        if (*strm).zfree.is_none() {
            (*strm).zfree = Some(default_zfree);
        }
    }
    match dfl::deflate_init2(level, method, window_bits, mem_level, strategy) {
        Ok(boxed) => {
            // SAFETY: strm is non-null and valid for writes; the hooks were just
            // defaulted, so the state container can be hook-allocated.
            if unsafe { attach_deflate_state(strm, boxed) } {
                Z_OK
            } else {
                // The allocator hook could not provide aligned state storage.
                Z_MEM_ERROR
            }
        }
        Err(e) => {
            // SAFETY: strm non-null; report the failure like C's ERR_MSG.
            unsafe { (*strm).msg = msg_to_cstr(Some("stream error")) };
            let _ = e;
            Z_STREAM_ERROR
        }
    }
}

/// `deflateInit_` — default-parameter deflate constructor (zlib.h L1903-1904).
///
/// Equivalent to `deflateInit2_` with `method = Z_DEFLATED`,
/// `windowBits = MAX_WBITS`, `memLevel = DEF_MEM_LEVEL`, and the default
/// strategy.
///
/// # Safety
///
/// As [`deflateInit2_`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn deflateInit_(
    strm: z_streamp,
    level: c_int,
    version: *const c_char,
    stream_size: c_int,
) -> c_int {
    // SAFETY: forwards the same raw pointers under the same contract.
    unsafe {
        deflateInit2_(
            strm,
            level,
            Z_DEFLATED,
            MAX_WBITS,
            DEF_MEM_LEVEL,
            Z_DEFAULT_STRATEGY,
            version,
            stream_size,
        )
    }
}

/// `deflate` — compress as much input as possible (zlib.h L254).
///
/// Reconstructs the input/output slices from the `next_*`/`avail_*` fields,
/// drives [`crate::deflate::deflate`], advances the cursors, and mirrors the
/// running totals/checksum/data-type/message back into `*strm`.
///
/// # Safety
///
/// `strm` must be a valid initialized deflate stream; `next_in`/`next_out` must
/// describe valid buffers of `avail_in`/`avail_out` bytes (or be null with the
/// corresponding length 0).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn deflate(strm: z_streamp, flush: c_int) -> c_int {
    if strm.is_null() {
        return Z_STREAM_ERROR;
    }
    // SAFETY: strm non-null; read the state pointer.
    let state_ptr = unsafe { (*strm).state } as *mut DeflateState;
    if state_ptr.is_null() {
        return Z_STREAM_ERROR;
    }
    // C rejects `flush < 0 || flush > Z_BLOCK` (Z_TREES is inflate-only).
    if !(0..=Z_BLOCK).contains(&flush) {
        // SAFETY: strm non-null.
        unsafe { (*strm).msg = msg_to_cstr(Some("stream error")) };
        return Z_STREAM_ERROR;
    }
    let Ok(flush_mode) = FlushMode::try_from(flush) else {
        return Z_STREAM_ERROR;
    };

    // SAFETY: strm non-null; read the I/O descriptor fields by value.
    let (in_ptr, in_len, out_ptr, out_len) = unsafe {
        (
            (*strm).next_in,
            (*strm).avail_in,
            (*strm).next_out,
            (*strm).avail_out,
        )
    };
    // C `deflate` pointer/length contract (deflate.c): a null `next_out`, or a
    // null `next_in` paired with a nonzero `avail_in`, is `Z_STREAM_ERROR` — not
    // a silently-empty buffer. Validate BEFORE constructing the slices so a
    // `(NULL, nonzero)` pair never becomes an empty slice (the silent
    // data-loss/wrong-result hazard). A null pointer with a zero length stays
    // permitted, exactly as C allows.
    if out_ptr.is_null() || (in_len != 0 && in_ptr.is_null()) {
        // SAFETY: strm non-null.
        unsafe { (*strm).msg = msg_to_cstr(Some("stream error")) };
        return Z_STREAM_ERROR;
    }
    // SAFETY: the (ptr, len) pairs were just validated against the C contract.
    let input = unsafe { const_slice(in_ptr, in_len) };
    let output = unsafe { mut_slice(out_ptr, out_len) };
    // SAFETY: state_ptr is a valid, uniquely-borrowed DeflateState.
    let state = unsafe { &mut *state_ptr };

    let mut stream = DeflateStream::new(state, input, output);
    let result = dfl::deflate(&mut stream, flush_mode);

    // Capture the post-call accounting before releasing the borrows.
    let consumed = stream.in_next;
    let produced = stream.out_next;
    let total_in = stream.state.total_in;
    let total_out = stream.state.total_out;
    let adler = stream.state.adler;
    let data_type = stream.state.data_type;
    let msg = stream.state.msg;
    // `stream` (holding the `&mut DeflateState`) is last used above; NLL ends
    // that borrow here, before the raw-pointer write-back to `*strm` below.

    // SAFETY: consumed <= in_len and produced <= out_len (engine invariant);
    // strm is non-null and valid for writes.
    unsafe {
        if !in_ptr.is_null() {
            (*strm).next_in = in_ptr.add(consumed);
        }
        (*strm).avail_in = in_len - consumed as uInt;
        if !out_ptr.is_null() {
            (*strm).next_out = out_ptr.add(produced);
        }
        (*strm).avail_out = out_len - produced as uInt;
        (*strm).total_in = total_in as uLong;
        (*strm).total_out = total_out as uLong;
        (*strm).adler = adler as uLong;
        (*strm).data_type = data_type;
        (*strm).msg = msg_to_cstr(msg);
    }

    result_to_code(result)
}

/// `deflateEnd` — finalize and release a deflate stream (zlib.h L367).
///
/// Reports C's "interrupted mid-compression?" status via
/// [`crate::deflate::deflate_end`], then frees the state container through the
/// stream's `zfree` hook (RAII drop of the state runs first, replacing C's
/// `ZFREE` of the working buffers) and nulls `strm.state`.
///
/// # Safety
///
/// `strm` must be null or a valid deflate stream initialized by `deflateInit*`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn deflateEnd(strm: z_streamp) -> c_int {
    if strm.is_null() {
        return Z_STREAM_ERROR;
    }
    // C `deflateStateCheck` parity: null allocator hooks make the stream invalid
    // (and we need a valid `zfree` to release the hook-allocated container).
    // SAFETY: strm non-null; read the hook fields by value.
    if unsafe { (*strm).zalloc }.is_none() || unsafe { (*strm).zfree }.is_none() {
        return Z_STREAM_ERROR;
    }
    // SAFETY: strm non-null; read the state pointer.
    let state_ptr = unsafe { (*strm).state } as *mut DeflateState;
    if state_ptr.is_null() {
        return Z_STREAM_ERROR;
    }
    // SAFETY: state_ptr is the live, uniquely-owned DeflateState produced by
    // `attach_deflate_state`; borrow it to compute the C return status.
    let status = dfl::deflate_end(unsafe { &*state_ptr });
    // Free the state container through the matching `zfree` hook.
    // SAFETY: state_ptr came from `ffi_alloc_state` with this stream's hooks.
    let (zfree, opaque) = unsafe { ((*strm).zfree, (*strm).opaque) };
    unsafe { ffi_free_state(state_ptr, zfree, opaque) };
    // SAFETY: strm non-null; clear the now-dangling state pointer.
    unsafe { (*strm).state = ptr::null_mut() };
    result_to_code(status)
}

/// `deflateReset` — reset a stream to its post-`deflateInit` state without
/// reallocating (zlib.h L702).
///
/// # Safety
///
/// `strm` must be a valid initialized deflate stream.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn deflateReset(strm: z_streamp) -> c_int {
    // SAFETY: validate and borrow the boxed state.
    let Some(state) = (unsafe { deflate_state(strm) }) else {
        return Z_STREAM_ERROR;
    };
    let ret = dfl::deflate_reset(state);
    let adler = state.adler;
    let data_type = state.data_type;
    // SAFETY: strm is non-null (deflate_state validated it); reseed accounting.
    unsafe {
        (*strm).total_in = 0;
        (*strm).total_out = 0;
        (*strm).msg = ptr::null();
        (*strm).adler = adler as uLong;
        (*strm).data_type = data_type;
    }
    result_to_code(ret)
}

/// `deflateResetKeep` — reset accounting and header state while keeping the
/// allocated buffers and (unlike [`deflateReset`]) without re-running
/// `lm_init` (zlib.h L2040).
///
/// This crate's engine exposes a single [`DeflateState::reset`] that performs
/// the full C `deflateReset` (a superset of `deflateResetKeep`). Mapping
/// `deflateResetKeep` onto it is behaviorally safe — the extra match-table
/// re-initialization is idempotent for a freshly-reset stream — and keeps the
/// public symbol available for binary compatibility. (Documented superset
/// behavior, AAP open risk #3.)
///
/// # Safety
///
/// As [`deflateReset`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn deflateResetKeep(strm: z_streamp) -> c_int {
    // SAFETY: forwards the same pointer under the same contract.
    unsafe { deflateReset(strm) }
}

/// `deflateParams` — change the compression level/strategy mid-stream
/// (zlib.h L713-715).
///
/// Like [`deflate`], this can produce output (it pre-flushes the current block
/// on a boundary), so it operates on a [`DeflateStream`] and mirrors the I/O
/// accounting back into `*strm`.
///
/// # Safety
///
/// As [`deflate`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn deflateParams(strm: z_streamp, level: c_int, strategy: c_int) -> c_int {
    if strm.is_null() {
        return Z_STREAM_ERROR;
    }
    // SAFETY: strm non-null; read the state pointer.
    let state_ptr = unsafe { (*strm).state } as *mut DeflateState;
    if state_ptr.is_null() {
        return Z_STREAM_ERROR;
    }
    // SAFETY: strm non-null; read the I/O descriptor fields by value.
    let (in_ptr, in_len, out_ptr, out_len) = unsafe {
        (
            (*strm).next_in,
            (*strm).avail_in,
            (*strm).next_out,
            (*strm).avail_out,
        )
    };
    // SAFETY: C contract guarantees the (ptr, len) buffers are valid.
    let input = unsafe { const_slice(in_ptr, in_len) };
    let output = unsafe { mut_slice(out_ptr, out_len) };
    // SAFETY: state_ptr is a valid, uniquely-borrowed DeflateState.
    let state = unsafe { &mut *state_ptr };

    let mut stream = DeflateStream::new(state, input, output);
    let result = dfl::deflate_params(&mut stream, level, strategy);

    let consumed = stream.in_next;
    let produced = stream.out_next;
    let total_in = stream.state.total_in;
    let total_out = stream.state.total_out;
    let adler = stream.state.adler;
    let data_type = stream.state.data_type;
    let msg = stream.state.msg;
    // `stream` (holding the `&mut DeflateState`) is last used above; NLL ends
    // that borrow here, before the raw-pointer write-back to `*strm` below.

    // SAFETY: consumed <= in_len, produced <= out_len; strm valid for writes.
    unsafe {
        if !in_ptr.is_null() {
            (*strm).next_in = in_ptr.add(consumed);
        }
        (*strm).avail_in = in_len - consumed as uInt;
        if !out_ptr.is_null() {
            (*strm).next_out = out_ptr.add(produced);
        }
        (*strm).avail_out = out_len - produced as uInt;
        (*strm).total_in = total_in as uLong;
        (*strm).total_out = total_out as uLong;
        (*strm).adler = adler as uLong;
        (*strm).data_type = data_type;
        (*strm).msg = msg_to_cstr(msg);
    }

    result_to_code(result)
}

/// `deflateSetDictionary` — initialize the compression dictionary
/// (zlib.h L618-620). On success the stream Adler-32 is mirrored into
/// `strm.adler`.
///
/// # Safety
///
/// `strm` must be a valid initialized deflate stream; `dictionary` must point
/// to `dict_length` readable bytes (or be null with `dict_length == 0`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn deflateSetDictionary(
    strm: z_streamp,
    dictionary: *const Bytef,
    dict_length: uInt,
) -> c_int {
    // SAFETY: validate and borrow the boxed state.
    let Some(state) = (unsafe { deflate_state(strm) }) else {
        return Z_STREAM_ERROR;
    };
    if dictionary.is_null() {
        return Z_STREAM_ERROR;
    }
    // SAFETY: dictionary non-null with dict_length valid bytes per contract.
    let dict = unsafe { const_slice(dictionary, dict_length) };
    let ret = dfl::deflate_set_dictionary(state, dict);
    let adler = state.adler;
    // SAFETY: strm non-null (deflate_state validated); mirror the new Adler-32.
    unsafe { (*strm).adler = adler as uLong };
    result_to_code(ret)
}

/// `deflateGetDictionary` — copy the sliding-window history out of the stream
/// (zlib.h L662-664).
///
/// When `dictionary` is null only the length is reported (in `*dict_length`).
/// The length is queried first so the destination slice is sized exactly to the
/// available history before the copy.
///
/// # Safety
///
/// `strm` must be a valid deflate stream; if `dictionary` is non-null it must
/// have room for the returned length (up to the window size, ≤ 32 KiB);
/// `dict_length` must be null or a valid `*mut uInt`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn deflateGetDictionary(
    strm: z_streamp,
    dictionary: *mut Bytef,
    dict_length: *mut uInt,
) -> c_int {
    // SAFETY: validate and borrow the boxed state.
    let Some(state) = (unsafe { deflate_state(strm) }) else {
        return Z_STREAM_ERROR;
    };
    // Query the available history length without copying.
    let (_, len) = dfl::deflate_get_dictionary(state, None);
    if !dictionary.is_null() {
        // SAFETY: caller guarantees `dictionary` has room for `len` bytes.
        let dict = unsafe { mut_slice(dictionary, len as uInt) };
        dfl::deflate_get_dictionary(state, Some(dict));
    }
    if !dict_length.is_null() {
        // SAFETY: dict_length non-null and valid for writes per contract.
        unsafe { *dict_length = len as uInt };
    }
    Z_OK
}

/// `deflateCopy` — duplicate a deflate stream, history and all (zlib.h L684-685).
///
/// Deep-clones the source state into a fresh box parked in `dest.state` and
/// copies the public `z_stream` accounting fields, so the two streams are
/// thereafter independent.
///
/// # Safety
///
/// `dest` and `source` must both be valid `z_stream` pointers; `source` must be
/// an initialized deflate stream.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn deflateCopy(dest: z_streamp, source: z_streamp) -> c_int {
    if dest.is_null() {
        return Z_STREAM_ERROR;
    }
    // SAFETY: validate and borrow the source's boxed state.
    let Some(src_state) = (unsafe { deflate_state(source) }) else {
        return Z_STREAM_ERROR;
    };
    let cloned = dfl::deflate_copy(src_state);
    // Move the clone out of its temporary global `Box` so it can be re-homed in
    // dest's allocator-hook memory below.
    let state: DeflateState = *cloned;
    // SAFETY: source non-null (validated); copy the public accounting fields,
    // including the allocator hooks — `dest` inherits source's `zalloc`/`zfree`/
    // `opaque`, mirroring C `deflateCopy`'s `zmemcpy` of the whole `z_stream`
    // followed by a `ZALLOC` on `dest` with those (inherited) hooks.
    unsafe {
        (*dest).next_in = (*source).next_in;
        (*dest).avail_in = (*source).avail_in;
        (*dest).total_in = (*source).total_in;
        (*dest).next_out = (*source).next_out;
        (*dest).avail_out = (*source).avail_out;
        (*dest).total_out = (*source).total_out;
        (*dest).adler = (*source).adler;
        (*dest).data_type = (*source).data_type;
        (*dest).msg = ptr::null();
        (*dest).zalloc = (*source).zalloc;
        (*dest).zfree = (*source).zfree;
        (*dest).opaque = (*source).opaque;
        (*dest).reserved = 0;
    }
    // Allocate dest's state container through dest's (now-inherited) hooks.
    // SAFETY: dest non-null; the hooks were just copied from the valid source.
    let (zalloc, zfree, opaque) = unsafe { ((*dest).zalloc, (*dest).zfree, (*dest).opaque) };
    let Some(raw) = (unsafe { ffi_alloc_state(state, zalloc, zfree, opaque) }) else {
        // Allocation failure: leave dest with no installed state, matching C
        // `deflateCopy` returning `Z_MEM_ERROR`.
        // SAFETY: dest non-null and valid for writes.
        unsafe { (*dest).state = ptr::null_mut() };
        return Z_MEM_ERROR;
    };
    // SAFETY: dest non-null; install the cloned state container.
    unsafe { (*dest).state = raw as *mut c_void };
    Z_OK
}

/// `deflateBound` — an upper bound on the compressed size of `source_len` input
/// bytes for this stream's configuration (zlib.h L768).
///
/// # Safety
///
/// `strm` must be null or a valid `z_stream` (an uninitialized/null state
/// yields the conservative bound, matching C).
#[unsafe(no_mangle)]
// `uLong`/`u64` casts are no-ops on LP64 but real conversions on LLP64 (Windows
// `unsigned long` is 32-bit) — kept for C-ABI portability.
#[allow(clippy::unnecessary_cast)]
pub unsafe extern "C" fn deflateBound(strm: z_streamp, source_len: uLong) -> uLong {
    // SAFETY: strm may be null; deflate_state handles that.
    let state = unsafe { deflate_state(strm) };
    dfl::deflate_bound(state.as_deref(), source_len as u64) as uLong
}

/// `deflatePending` — report bytes/bits generated but not yet flushed
/// (zlib.h L786-788).
///
/// # Safety
///
/// `strm` must be a valid deflate stream; `pending`/`bits` must be null or
/// valid for writes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn deflatePending(
    strm: z_streamp,
    pending: *mut c_uint,
    bits: *mut c_int,
) -> c_int {
    // SAFETY: validate and borrow the boxed state.
    let Some(state) = (unsafe { deflate_state(strm) }) else {
        return Z_STREAM_ERROR;
    };
    let (p, b) = dfl::deflate_pending(state);
    if !pending.is_null() {
        // SAFETY: pending non-null and valid for writes per contract.
        unsafe { *pending = p as c_uint };
    }
    if !bits.is_null() {
        // SAFETY: bits non-null and valid for writes per contract.
        unsafe { *bits = b as c_int };
    }
    Z_OK
}

/// `deflateUsed` — report the number of deflate bits used in the last byte at
/// the most recent flush, in `1..=8` (or 0 before any flush) (zlib.h L804-805).
///
/// # Safety
///
/// `strm` must be a valid deflate stream; `bits` must be null or valid for
/// writes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn deflateUsed(strm: z_streamp, bits: *mut c_int) -> c_int {
    // SAFETY: validate and borrow the boxed state.
    let Some(state) = (unsafe { deflate_state(strm) }) else {
        return Z_STREAM_ERROR;
    };
    if !bits.is_null() {
        // SAFETY: bits non-null and valid for writes per contract.
        unsafe { *bits = state.bi_used as c_int };
    }
    Z_OK
}

/// `deflatePrime` — insert `bits` low-order bits of `value` into the output
/// ahead of the compressed data (zlib.h L816-818).
///
/// # Safety
///
/// `strm` must be a valid deflate stream.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn deflatePrime(strm: z_streamp, bits: c_int, value: c_int) -> c_int {
    // SAFETY: validate and borrow the boxed state.
    let Some(state) = (unsafe { deflate_state(strm) }) else {
        return Z_STREAM_ERROR;
    };
    result_to_code(dfl::deflate_prime(state, bits, value))
}

/// `deflateTune` — override the internal LZ77 match parameters (zlib.h L751-755).
///
/// # Safety
///
/// `strm` must be a valid deflate stream.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn deflateTune(
    strm: z_streamp,
    good_length: c_int,
    max_lazy: c_int,
    nice_length: c_int,
    max_chain: c_int,
) -> c_int {
    // SAFETY: validate and borrow the boxed state.
    let Some(state) = (unsafe { deflate_state(strm) }) else {
        return Z_STREAM_ERROR;
    };
    result_to_code(dfl::deflate_tune(
        state,
        good_length,
        max_lazy,
        nice_length,
        max_chain,
    ))
}

/// `deflateSetHeader` — supply a gzip header for a gzip-wrapped stream
/// (zlib.h L833-834).
///
/// The C [`gz_header`] is marshalled into an owned [`GzHeader`] (deep-copying
/// the extra/name/comment fields) and handed to the engine. Only valid on a
/// stream created with `windowBits` in `24..=31` (`wrap == 2`).
///
/// # Safety
///
/// `strm` must be a valid deflate stream; `head` must be null or a valid
/// `gz_header` whose `extra`/`name`/`comment` pointers (if set) are valid.
#[cfg(feature = "gzip")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn deflateSetHeader(strm: z_streamp, head: gz_headerp) -> c_int {
    // SAFETY: validate and borrow the boxed state.
    let Some(state) = (unsafe { deflate_state(strm) }) else {
        return Z_STREAM_ERROR;
    };
    if head.is_null() {
        return Z_STREAM_ERROR;
    }
    // SAFETY: head is non-null and valid per the C contract.
    let rust_head = unsafe { c_head_to_rust(head) };
    result_to_code(dfl::deflate_set_header(state, rust_head))
}

/// Marshal a C [`gz_header`] into the engine's owned [`GzHeader`]
/// (deep-copying the optional extra/name/comment byte fields).
///
/// # Safety
///
/// `head` must be a valid `gz_header`; when its `extra`/`name`/`comment`
/// pointers are non-null they must reference valid memory (`extra_len` bytes
/// for `extra`; NUL-terminated C strings for `name`/`comment`).
#[cfg(feature = "gzip")]
unsafe fn c_head_to_rust(head: gz_headerp) -> GzHeader {
    // SAFETY: head is non-null and valid per the caller's contract.
    let h = unsafe { &*head };
    let mut g = GzHeader::new();
    g.text = h.text != 0;
    g.time = h.time as u32;
    g.xflags = h.xflags;
    g.os = h.os;
    g.hcrc = h.hcrc != 0;
    if !h.extra.is_null() {
        // SAFETY: extra points to extra_len readable bytes per the contract.
        let extra = unsafe { const_slice(h.extra, h.extra_len) };
        g.extra = Some(extra.to_vec());
    }
    if !h.name.is_null() {
        // SAFETY: name is a valid NUL-terminated C string per the contract.
        g.name = Some(unsafe { cstr_to_vec(h.name as *const c_char) });
    }
    if !h.comment.is_null() {
        // SAFETY: comment is a valid NUL-terminated C string per the contract.
        g.comment = Some(unsafe { cstr_to_vec(h.comment as *const c_char) });
    }
    g
}

// ===========================================================================
// Phase C — exported functions: inflate family (inflate.c / inflate.h)
// ===========================================================================

/// Borrow the boxed [`InflateFfi`] parked in `strm.state`.
///
/// Returns [`None`] when `strm` or `strm.state` is null.
///
/// # Safety
///
/// `strm` must be null or a valid `z_stream` whose `state` (if non-null) was
/// produced by this crate's `inflateInit*` (i.e. is an [`ffi_alloc_state`]
/// [`InflateFfi`]).
#[inline]
unsafe fn inflate_ffi<'a>(strm: z_streamp) -> Option<&'a mut InflateFfi> {
    if strm.is_null() {
        return None;
    }
    // C `inflateStateCheck` parity: a stream whose allocator hooks were cleared
    // (or never defaulted) is invalid — reject it before touching `state`.
    // SAFETY: strm is non-null; read the hook fields by value.
    if unsafe { (*strm).zalloc }.is_none() || unsafe { (*strm).zfree }.is_none() {
        return None;
    }
    // SAFETY: strm is non-null; read its `state` field by value.
    let ffi_ptr = unsafe { (*strm).state } as *mut InflateFfi;
    if ffi_ptr.is_null() {
        return None;
    }
    // SAFETY: ffi_ptr was produced by `inflateInit*` via ffi_alloc_state.
    Some(unsafe { &mut *ffi_ptr })
}

/// Mirror the engine's parsed [`GzHeader`] into the caller's C [`gz_header`]
/// after an [`inflate`] call, respecting the caller's `*_max` capacities and
/// NUL-terminating the name/comment fields (the safe analogue of C inflate's
/// in-place header fill).
///
/// # Safety
///
/// `c_head` must be null or a valid `gz_header`; when its `extra`/`name`/
/// `comment` pointers are non-null they must have room for `extra_max`/
/// `name_max`/`comm_max` bytes respectively.
#[cfg(feature = "gzip")]
unsafe fn sync_gz_header_out(rust_head: Option<&GzHeader>, c_head: *mut gz_header) {
    if c_head.is_null() {
        return;
    }
    let Some(h) = rust_head else {
        return;
    };
    // SAFETY: c_head is non-null and valid per the inflateGetHeader contract.
    let dst = unsafe { &mut *c_head };
    dst.text = c_int::from(h.text);
    dst.time = h.time as uLong;
    dst.xflags = h.xflags;
    dst.os = h.os;
    dst.hcrc = c_int::from(h.hcrc);
    dst.done = c_int::from(h.done);
    if let Some(extra) = h.extra.as_ref() {
        dst.extra_len = extra.len() as uInt;
        if !dst.extra.is_null() && dst.extra_max > 0 {
            let n = core::cmp::min(extra.len(), dst.extra_max as usize);
            // SAFETY: dst.extra has room for extra_max >= n bytes per contract.
            unsafe { ptr::copy_nonoverlapping(extra.as_ptr(), dst.extra, n) };
        }
    }
    if let Some(name) = h.name.as_ref() {
        if !dst.name.is_null() && dst.name_max > 0 {
            let n = core::cmp::min(name.len(), dst.name_max.saturating_sub(1) as usize);
            // SAFETY: dst.name has room for name_max > n bytes; the +n write is
            // the NUL terminator, in bounds because n <= name_max - 1.
            unsafe {
                ptr::copy_nonoverlapping(name.as_ptr(), dst.name, n);
                *dst.name.add(n) = 0;
            }
        }
    }
    if let Some(comment) = h.comment.as_ref() {
        if !dst.comment.is_null() && dst.comm_max > 0 {
            let n = core::cmp::min(comment.len(), dst.comm_max.saturating_sub(1) as usize);
            // SAFETY: dst.comment has room for comm_max > n bytes; the +n write
            // is the NUL terminator, in bounds because n <= comm_max - 1.
            unsafe {
                ptr::copy_nonoverlapping(comment.as_ptr(), dst.comment, n);
                *dst.comment.add(n) = 0;
            }
        }
    }
}

/// `inflateInit2_` — the real inflate constructor (zlib.h L1911-1912).
///
/// # Safety
///
/// `strm` must be a valid `z_stream` (or null) and `version` a valid C string
/// (or null).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn inflateInit2_(
    strm: z_streamp,
    window_bits: c_int,
    version: *const c_char,
    stream_size: c_int,
) -> c_int {
    if strm.is_null() {
        return Z_STREAM_ERROR;
    }
    // SAFETY: version may be null; version_ok handles that.
    if !unsafe { version_ok(version, stream_size) } {
        return Z_VERSION_ERROR;
    }
    // SAFETY: strm non-null; clear the state slot before init, and default the
    // allocator hooks exactly as C `inflateInit2_` does (null `zalloc` →
    // `default_zalloc` + `opaque = NULL`; null `zfree` → `default_zfree`).
    unsafe {
        (*strm).msg = ptr::null();
        (*strm).state = ptr::null_mut();
        if (*strm).zalloc.is_none() {
            (*strm).zalloc = Some(default_zalloc);
            (*strm).opaque = ptr::null_mut();
        }
        if (*strm).zfree.is_none() {
            (*strm).zfree = Some(default_zfree);
        }
    }
    match inf::inflate_init2(window_bits) {
        Ok(boxed_state) => {
            // The initial public Adler is `wrap & 1` (1 for zlib, 0 for gzip/raw).
            let adler = (boxed_state.wrap & 1) as uLong;
            // Move the state out of its temporary global `Box`; it is re-homed
            // inline (alongside the `head` sink) in allocator-hook memory below.
            let ffi = InflateFfi {
                state: *boxed_state,
                head: ptr::null_mut(),
            };
            // SAFETY: strm non-null; the hooks were just defaulted (non-null).
            let (zalloc, zfree, opaque) =
                unsafe { ((*strm).zalloc, (*strm).zfree, (*strm).opaque) };
            // SAFETY: valid hooks; allocate the `InflateFfi` container through
            // them (the FFI allocator bridge).
            let Some(raw) = (unsafe { ffi_alloc_state(ffi, zalloc, zfree, opaque) }) else {
                // SAFETY: strm non-null; report OOM like C's ERR_MSG.
                unsafe { (*strm).msg = msg_to_cstr(Some("insufficient memory")) };
                return Z_MEM_ERROR;
            };
            // SAFETY: strm non-null and valid for writes.
            unsafe {
                (*strm).state = raw as *mut c_void;
                (*strm).total_in = 0;
                (*strm).total_out = 0;
                (*strm).msg = ptr::null();
                (*strm).adler = adler;
                (*strm).data_type = 0;
            }
            Z_OK
        }
        Err(code) => {
            // SAFETY: strm non-null; report like C's ERR_MSG.
            unsafe { (*strm).msg = msg_to_cstr(Some("stream error")) };
            code
        }
    }
}

/// `inflateInit_` — default-window inflate constructor (zlib.h L1905-1906).
///
/// # Safety
///
/// As [`inflateInit2_`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn inflateInit_(
    strm: z_streamp,
    version: *const c_char,
    stream_size: c_int,
) -> c_int {
    // SAFETY: forwards the same raw pointers under the same contract.
    unsafe { inflateInit2_(strm, DEF_WBITS, version, stream_size) }
}

/// `inflate` — decompress as much as possible (zlib.h L405).
///
/// # Safety
///
/// `strm` must be a valid initialized inflate stream; `next_in`/`next_out` must
/// describe valid buffers of `avail_in`/`avail_out` bytes (or be null/0).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn inflate(strm: z_streamp, flush: c_int) -> c_int {
    if strm.is_null() {
        return Z_STREAM_ERROR;
    }
    // SAFETY: strm non-null; read the state pointer.
    let ffi_ptr = unsafe { (*strm).state } as *mut InflateFfi;
    if ffi_ptr.is_null() {
        return Z_STREAM_ERROR;
    }
    // SAFETY: strm non-null; read the I/O descriptor fields by value.
    let (in_ptr, in_len, out_ptr, out_len) = unsafe {
        (
            (*strm).next_in,
            (*strm).avail_in,
            (*strm).next_out,
            (*strm).avail_out,
        )
    };
    // C `inflate` pointer/length contract (inflate.c): a null `next_out`, or a
    // null `next_in` paired with a nonzero `avail_in`, is `Z_STREAM_ERROR` — not
    // a silently-empty buffer. Validate BEFORE constructing the slices so a
    // `(NULL, nonzero)` pair never becomes an empty slice. A null pointer with a
    // zero length stays permitted, exactly as C allows.
    if out_ptr.is_null() || (in_len != 0 && in_ptr.is_null()) {
        return Z_STREAM_ERROR;
    }
    // SAFETY: the (ptr, len) pairs were just validated against the C contract.
    let input = unsafe { const_slice(in_ptr, in_len) };
    let output = unsafe { mut_slice(out_ptr, out_len) };
    // SAFETY: ffi_ptr is a valid, uniquely-borrowed InflateFfi.
    let ffi = unsafe { &mut *ffi_ptr };

    let outcome = inf::inflate(&mut ffi.state, input, output, flush);

    // Mirror any parsed gzip header into the caller's sink.
    #[cfg(feature = "gzip")]
    {
        // SAFETY: ffi.head is the caller's gz_header pointer (or null).
        unsafe { sync_gz_header_out(ffi.state.head.as_ref(), ffi.head) };
    }

    let consumed = outcome.in_consumed;
    let produced = outcome.out_produced;
    // SAFETY: consumed <= in_len, produced <= out_len; strm valid for writes.
    unsafe {
        if !in_ptr.is_null() {
            (*strm).next_in = in_ptr.add(consumed);
        }
        (*strm).avail_in = in_len - consumed as uInt;
        if !out_ptr.is_null() {
            (*strm).next_out = out_ptr.add(produced);
        }
        (*strm).avail_out = out_len - produced as uInt;
        (*strm).total_in = (*strm)
            .total_in
            .wrapping_add(outcome.total_in_delta as uLong);
        (*strm).total_out = (*strm)
            .total_out
            .wrapping_add(outcome.total_out_delta as uLong);
        (*strm).adler = outcome.adler as uLong;
        (*strm).data_type = outcome.data_type;
        (*strm).msg = msg_to_cstr(outcome.msg);
    }

    outcome.ret
}

/// `inflateEnd` — finalize and release an inflate stream (zlib.h L525).
///
/// # Safety
///
/// `strm` must be null or a valid inflate stream initialized by `inflateInit*`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn inflateEnd(strm: z_streamp) -> c_int {
    if strm.is_null() {
        return Z_STREAM_ERROR;
    }
    // C `inflateStateCheck` parity: null allocator hooks make the stream invalid
    // (and we need a valid `zfree` to release the hook-allocated container).
    // SAFETY: strm non-null; read the hook fields by value.
    if unsafe { (*strm).zalloc }.is_none() || unsafe { (*strm).zfree }.is_none() {
        return Z_STREAM_ERROR;
    }
    // SAFETY: strm non-null; read the state pointer.
    let ffi_ptr = unsafe { (*strm).state } as *mut InflateFfi;
    if ffi_ptr.is_null() {
        return Z_STREAM_ERROR;
    }
    // Free the `InflateFfi` container through the matching `zfree` hook; the
    // RAII drop of the inlined `InflateState` (deterministic teardown — the
    // `inflateEnd` analogue) runs first and releases its owned buffers.
    // SAFETY: ffi_ptr came from `ffi_alloc_state` with this stream's hooks.
    let (zfree, opaque) = unsafe { ((*strm).zfree, (*strm).opaque) };
    unsafe { ffi_free_state(ffi_ptr, zfree, opaque) };
    // SAFETY: strm non-null; clear the now-dangling state pointer.
    unsafe { (*strm).state = ptr::null_mut() };
    Z_OK
}

/// `inflateReset` — reset to the post-`inflateInit` state, clearing the window
/// (zlib.h L986).
///
/// # Safety
///
/// `strm` must be a valid initialized inflate stream.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn inflateReset(strm: z_streamp) -> c_int {
    // SAFETY: validate and borrow the boxed state.
    let Some(ffi) = (unsafe { inflate_ffi(strm) }) else {
        return Z_STREAM_ERROR;
    };
    let mut adler = 0u32;
    inf::inflate_reset(&mut ffi.state, &mut adler);
    // SAFETY: strm non-null (inflate_ffi validated it); reseed accounting.
    unsafe {
        (*strm).total_in = 0;
        (*strm).total_out = 0;
        (*strm).msg = ptr::null();
        (*strm).adler = adler as uLong;
    }
    Z_OK
}

/// `inflateResetKeep` — reset without clearing the sliding window (zlib.h L2039).
///
/// # Safety
///
/// As [`inflateReset`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn inflateResetKeep(strm: z_streamp) -> c_int {
    // SAFETY: validate and borrow the boxed state.
    let Some(ffi) = (unsafe { inflate_ffi(strm) }) else {
        return Z_STREAM_ERROR;
    };
    let mut adler = 0u32;
    inf::inflate_reset_keep(&mut ffi.state, &mut adler);
    // SAFETY: strm non-null; reseed accounting.
    unsafe {
        (*strm).total_in = 0;
        (*strm).total_out = 0;
        (*strm).msg = ptr::null();
        (*strm).adler = adler as uLong;
    }
    Z_OK
}

/// `inflateReset2` — reset and re-apply the `windowBits` framing convention
/// (zlib.h L997-998).
///
/// # Safety
///
/// As [`inflateReset`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn inflateReset2(strm: z_streamp, window_bits: c_int) -> c_int {
    // SAFETY: validate and borrow the boxed state.
    let Some(ffi) = (unsafe { inflate_ffi(strm) }) else {
        return Z_STREAM_ERROR;
    };
    let mut adler = 0u32;
    let ret = inf::inflate_reset2(&mut ffi.state, window_bits, &mut adler);
    if ret == Z_OK {
        // SAFETY: strm non-null; reseed accounting on success.
        unsafe {
            (*strm).total_in = 0;
            (*strm).total_out = 0;
            (*strm).msg = ptr::null();
            (*strm).adler = adler as uLong;
        }
    }
    ret
}

/// `inflateSetDictionary` — install a preset dictionary (zlib.h L913-915).
///
/// # Safety
///
/// `strm` must be a valid inflate stream; `dictionary` must point to
/// `dict_length` readable bytes (or be null with `dict_length == 0`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn inflateSetDictionary(
    strm: z_streamp,
    dictionary: *const Bytef,
    dict_length: uInt,
) -> c_int {
    // SAFETY: validate and borrow the boxed state.
    let Some(ffi) = (unsafe { inflate_ffi(strm) }) else {
        return Z_STREAM_ERROR;
    };
    if dictionary.is_null() {
        return Z_STREAM_ERROR;
    }
    // SAFETY: dictionary non-null with dict_length valid bytes per contract.
    let dict = unsafe { const_slice(dictionary, dict_length) };
    inf::inflate_set_dictionary(&mut ffi.state, dict)
}

/// `inflateGetDictionary` — copy the sliding-window history out (zlib.h L936-938).
///
/// # Safety
///
/// `strm` must be a valid inflate stream; `dictionary` must be null or have room
/// for the returned length (≤ 32 KiB); `dict_length` must be null or valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn inflateGetDictionary(
    strm: z_streamp,
    dictionary: *mut Bytef,
    dict_length: *mut uInt,
) -> c_int {
    // SAFETY: validate and borrow the boxed state.
    let Some(ffi) = (unsafe { inflate_ffi(strm) }) else {
        return Z_STREAM_ERROR;
    };
    // Query the window occupancy without copying (empty destination slice).
    let whave = inf::inflate_get_dictionary(&ffi.state, &mut []);
    if !dictionary.is_null() {
        // SAFETY: caller guarantees `dictionary` has room for `whave` bytes.
        let dict = unsafe { mut_slice(dictionary, whave as uInt) };
        inf::inflate_get_dictionary(&ffi.state, dict);
    }
    if !dict_length.is_null() {
        // SAFETY: dict_length non-null and valid for writes per contract.
        unsafe { *dict_length = whave as uInt };
    }
    Z_OK
}

/// `inflateGetHeader` — request capture of the gzip header into `head`
/// (zlib.h L1070-1071). The pointer is remembered and mirrored after each
/// [`inflate`] call.
///
/// # Safety
///
/// `strm` must be a valid inflate stream; `head` must be null or a valid
/// `gz_header` that stays valid until decoding completes.
#[cfg(feature = "gzip")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn inflateGetHeader(strm: z_streamp, head: gz_headerp) -> c_int {
    // SAFETY: validate and borrow the boxed state.
    let Some(ffi) = (unsafe { inflate_ffi(strm) }) else {
        return Z_STREAM_ERROR;
    };
    let ret = inf::inflate_get_header(&mut ffi.state);
    if ret == Z_OK {
        ffi.head = head;
    }
    ret
}

/// `inflateSync` — skip corrupted input to the next flush point (zlib.h L951).
///
/// # Safety
///
/// `strm` must be a valid inflate stream; `next_in`/`avail_in` must describe a
/// valid buffer (or be null/0).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn inflateSync(strm: z_streamp) -> c_int {
    if strm.is_null() {
        return Z_STREAM_ERROR;
    }
    // SAFETY: strm non-null; read the state pointer.
    let ffi_ptr = unsafe { (*strm).state } as *mut InflateFfi;
    if ffi_ptr.is_null() {
        return Z_STREAM_ERROR;
    }
    // SAFETY: strm non-null; read the input descriptor by value.
    let (in_ptr, in_len) = unsafe { ((*strm).next_in, (*strm).avail_in) };
    // SAFETY: C contract guarantees the (ptr, len) input buffer is valid.
    let input = unsafe { const_slice(in_ptr, in_len) };
    // SAFETY: ffi_ptr is a valid, uniquely-borrowed InflateFfi.
    let ffi = unsafe { &mut *ffi_ptr };

    let outcome = inf::inflate_sync(&mut ffi.state, input);
    let consumed = outcome.in_consumed;
    // SAFETY: consumed <= in_len; strm valid for writes.
    unsafe {
        if !in_ptr.is_null() {
            (*strm).next_in = in_ptr.add(consumed);
        }
        (*strm).avail_in = in_len - consumed as uInt;
        (*strm).total_in = (*strm).total_in.wrapping_add(consumed as uLong);
        (*strm).adler = outcome.adler as uLong;
    }
    outcome.ret
}

/// `inflateSyncPoint` — report whether the stream is stopped on a flush point
/// (zlib.h L2034).
///
/// # Safety
///
/// `strm` must be a valid inflate stream.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn inflateSyncPoint(strm: z_streamp) -> c_int {
    // SAFETY: validate and borrow the boxed state.
    let Some(ffi) = (unsafe { inflate_ffi(strm) }) else {
        return Z_STREAM_ERROR;
    };
    inf::inflate_sync_point(&ffi.state)
}

/// `inflateCopy` — duplicate a mid-stream inflate state (zlib.h L970-971).
///
/// # Safety
///
/// `dest` and `source` must be valid `z_stream` pointers; `source` must be an
/// initialized inflate stream.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn inflateCopy(dest: z_streamp, source: z_streamp) -> c_int {
    if dest.is_null() {
        return Z_STREAM_ERROR;
    }
    // SAFETY: validate and borrow the source's boxed state.
    let Some(src_ffi) = (unsafe { inflate_ffi(source) }) else {
        return Z_STREAM_ERROR;
    };
    let cloned = inf::inflate_copy(&src_ffi.state);
    // Build the dest `InflateFfi` value (inlined state + null header sink); it
    // is re-homed in dest's allocator-hook memory once the hooks are inherited.
    let new_ffi = InflateFfi {
        state: cloned,
        head: ptr::null_mut(),
    };
    // SAFETY: source non-null (validated); copy the public accounting fields,
    // including the allocator hooks — dest inherits source's `zalloc`/`zfree`/
    // `opaque` (mirroring C's whole-`z_stream` copy followed by a `ZALLOC` on
    // dest with those inherited hooks).
    unsafe {
        (*dest).next_in = (*source).next_in;
        (*dest).avail_in = (*source).avail_in;
        (*dest).total_in = (*source).total_in;
        (*dest).next_out = (*source).next_out;
        (*dest).avail_out = (*source).avail_out;
        (*dest).total_out = (*source).total_out;
        (*dest).adler = (*source).adler;
        (*dest).data_type = (*source).data_type;
        (*dest).msg = ptr::null();
        (*dest).zalloc = (*source).zalloc;
        (*dest).zfree = (*source).zfree;
        (*dest).opaque = (*source).opaque;
        (*dest).reserved = 0;
    }
    // Allocate dest's container through dest's (now-inherited) hooks.
    // SAFETY: dest non-null; the hooks were just copied from the valid source.
    let (zalloc, zfree, opaque) = unsafe { ((*dest).zalloc, (*dest).zfree, (*dest).opaque) };
    let Some(raw) = (unsafe { ffi_alloc_state(new_ffi, zalloc, zfree, opaque) }) else {
        // Allocation failure: leave dest with no installed state, matching C
        // returning `Z_MEM_ERROR`.
        // SAFETY: dest non-null and valid for writes.
        unsafe { (*dest).state = ptr::null_mut() };
        return Z_MEM_ERROR;
    };
    // SAFETY: dest non-null; install the cloned state container.
    unsafe { (*dest).state = raw as *mut c_void };
    Z_OK
}

/// `inflateMark` — encode decode-position diagnostics for random access
/// (zlib.h L1042). Returns `-(1 << 16)` for an inconsistent state, mirroring C.
///
/// # Safety
///
/// `strm` must be a valid inflate stream.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn inflateMark(strm: z_streamp) -> c_long {
    // SAFETY: validate and borrow the boxed state.
    let Some(ffi) = (unsafe { inflate_ffi(strm) }) else {
        return -(1 << 16);
    };
    inf::inflate_mark(&ffi.state) as c_long
}

/// `inflatePrime` — insert bits ahead of the stream, or (for `bits < 0`) flush
/// the bit accumulator (zlib.h L1011-1013).
///
/// # Safety
///
/// `strm` must be a valid inflate stream.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn inflatePrime(strm: z_streamp, bits: c_int, value: c_int) -> c_int {
    // SAFETY: validate and borrow the boxed state.
    let Some(ffi) = (unsafe { inflate_ffi(strm) }) else {
        return Z_STREAM_ERROR;
    };
    inf::inflate_prime(&mut ffi.state, bits, value)
}

/// `inflateValidate` — enable/disable check-value validation at run time
/// (zlib.h L2037).
///
/// # Safety
///
/// `strm` must be a valid inflate stream.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn inflateValidate(strm: z_streamp, check: c_int) -> c_int {
    // SAFETY: validate and borrow the boxed state.
    let Some(ffi) = (unsafe { inflate_ffi(strm) }) else {
        return Z_STREAM_ERROR;
    };
    inf::inflate_validate(&mut ffi.state, check)
}

/// `inflateCodesUsed` — number of inflate code-table entries used so far
/// (zlib.h L2038).
///
/// # Safety
///
/// `strm` must be a valid inflate stream. Returns `(unsigned long)-1` for an
/// invalid handle, mirroring C.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn inflateCodesUsed(strm: z_streamp) -> c_ulong {
    // SAFETY: validate and borrow the boxed state.
    let Some(ffi) = (unsafe { inflate_ffi(strm) }) else {
        return c_ulong::MAX;
    };
    inf::inflate_codes_used(&ffi.state) as c_ulong
}

/// `inflateUndermine` — request tolerance of invalid distances (zlib.h L2036).
/// With the repo's default configuration this forces `sane` and reports
/// `Z_DATA_ERROR`, exactly like C's `#else` branch.
///
/// # Safety
///
/// `strm` must be a valid inflate stream.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn inflateUndermine(strm: z_streamp, subvert: c_int) -> c_int {
    // SAFETY: validate and borrow the boxed state.
    let Some(ffi) = (unsafe { inflate_ffi(strm) }) else {
        return Z_STREAM_ERROR;
    };
    inf::inflate_undermine(&mut ffi.state, subvert)
}

// ===========================================================================
// Phase C — exported functions: inflateBack family (infback.c)
// ===========================================================================

/// `in_func` — the C input callback for [`inflateBack`] (zlib.h L1134-1135):
/// `unsigned (*)(void *, const unsigned char **)`. Returns the number of bytes
/// available and writes a pointer to them through the out-parameter.
pub type in_func = Option<unsafe extern "C" fn(in_desc: voidpf, buf: *mut *const Bytef) -> c_uint>;
/// `out_func` — the C output callback for [`inflateBack`] (zlib.h L1136):
/// `int (*)(void *, unsigned char *, unsigned)`. Returns non-zero on failure.
pub type out_func =
    Option<unsafe extern "C" fn(out_desc: voidpf, buf: *mut Bytef, len: c_uint) -> c_int>;

/// Adapter bridging the C `in_func` callback to the safe [`BackInput`] trait.
///
/// The first [`fill`](BackInput::fill) returns the input the C caller seeded via
/// `strm->next_in`/`avail_in` (if any), exactly as C `inflateBack` does; every
/// subsequent call invokes the C `in_func`.
struct CBackInput {
    /// The (non-null) C input callback.
    in_func: unsafe extern "C" fn(voidpf, *mut *const Bytef) -> c_uint,
    /// The opaque descriptor passed to `in_func`.
    in_desc: voidpf,
    /// Seed input pointer from `strm->next_in` (consumed on the first `fill`).
    seed: *const Bytef,
    /// Seed input length from `strm->avail_in`.
    seed_len: uInt,
    /// Whether the seed has already been yielded.
    seed_used: bool,
}

impl<'a> BackInput<'a> for CBackInput {
    fn fill(&mut self) -> &'a [u8] {
        if !self.seed_used {
            self.seed_used = true;
            if !self.seed.is_null() && self.seed_len > 0 {
                // SAFETY: seed/seed_len mirror strm->next_in/avail_in, a valid
                // readable region per the inflateBack contract.
                return unsafe { slice::from_raw_parts(self.seed, self.seed_len as usize) };
            }
        }
        let mut buf: *const Bytef = ptr::null();
        // SAFETY: in_func is a valid C callback (non-null, checked at the entry
        // point); it writes a buffer pointer into `buf` and returns its length.
        let n = unsafe { (self.in_func)(self.in_desc, &mut buf) };
        if n == 0 || buf.is_null() {
            return &[];
        }
        // SAFETY: the C contract guarantees `buf..buf+n` is readable and stays
        // valid until the next `in_func` call (i.e. the next `fill`).
        unsafe { slice::from_raw_parts(buf, n as usize) }
    }
}

/// Adapter bridging the C `out_func` callback to the safe [`BackOutput`] trait.
struct CBackOutput {
    /// The (non-null) C output callback.
    out_func: unsafe extern "C" fn(voidpf, *mut Bytef, c_uint) -> c_int,
    /// The opaque descriptor passed to `out_func`.
    out_desc: voidpf,
}

impl BackOutput for CBackOutput {
    fn write(&mut self, buf: &[u8]) -> bool {
        // SAFETY: out_func is a valid C callback (non-null, checked at the entry
        // point); `buf` is a valid readable region for the duration of the call.
        let r = unsafe {
            (self.out_func)(
                self.out_desc,
                buf.as_ptr() as *mut Bytef,
                buf.len() as c_uint,
            )
        };
        // C convention: a non-zero return signals a write failure.
        r != 0
    }
}

/// `inflateBackInit_` — initialize a callback-based raw-inflate stream over a
/// caller-provided `window` of `1 << windowBits` bytes (zlib.h L1913-1917).
///
/// # Safety
///
/// `strm` must be a valid `z_stream` (or null); `window` must point to at least
/// `1 << windowBits` writable bytes that remain valid until `inflateBackEnd`;
/// `version` must be a valid C string (or null).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn inflateBackInit_(
    strm: z_streamp,
    window_bits: c_int,
    window: *mut Bytef,
    version: *const c_char,
    stream_size: c_int,
) -> c_int {
    if strm.is_null() || window.is_null() {
        return Z_STREAM_ERROR;
    }
    // SAFETY: version may be null; version_ok handles that.
    if !unsafe { version_ok(version, stream_size) } {
        return Z_VERSION_ERROR;
    }
    // SAFETY: strm non-null; clear the state slot before init, and default the
    // allocator hooks exactly as C `inflateBackInit_` does.
    unsafe {
        (*strm).msg = ptr::null();
        (*strm).state = ptr::null_mut();
        if (*strm).zalloc.is_none() {
            (*strm).zalloc = Some(default_zalloc);
            (*strm).opaque = ptr::null_mut();
        }
        if (*strm).zfree.is_none() {
            (*strm).zfree = Some(default_zfree);
        }
    }
    match inf::inflate_back_init(window_bits) {
        Ok(state) => {
            let wsize = state.wsize as usize;
            let ffi = InflateBackFfi {
                state,
                window,
                wsize,
            };
            // SAFETY: strm non-null; the hooks were just defaulted (non-null).
            let (zalloc, zfree, opaque) =
                unsafe { ((*strm).zalloc, (*strm).zfree, (*strm).opaque) };
            // SAFETY: valid hooks; allocate the `InflateBackFfi` container
            // through them (the FFI allocator bridge).
            let Some(raw) = (unsafe { ffi_alloc_state(ffi, zalloc, zfree, opaque) }) else {
                // SAFETY: strm non-null; report OOM like C's ERR_MSG.
                unsafe { (*strm).msg = msg_to_cstr(Some("insufficient memory")) };
                return Z_MEM_ERROR;
            };
            // SAFETY: strm non-null and valid for writes.
            unsafe { (*strm).state = raw as *mut c_void };
            Z_OK
        }
        Err(code) => {
            // SAFETY: strm non-null; report like C's ERR_MSG.
            unsafe { (*strm).msg = msg_to_cstr(Some("stream error")) };
            code
        }
    }
}

/// `inflateBack` — do a raw inflate using caller-supplied input/output callbacks
/// (zlib.h L1138-1140).
///
/// The C `in`/`out` callbacks are adapted to the safe [`BackInput`]/
/// [`BackOutput`] traits and driven by [`crate::inflate::inflate_back`] over the
/// window registered at init.
///
/// Limitation: unlike C, this does not write the residual `next_in`/`avail_in`
/// back into `*strm` on return (the safe callback API does not surface the
/// partial consumption of the final input chunk). Callers that rely on the
/// callback model — the documented and overwhelmingly common usage — are
/// unaffected.
///
/// # Safety
///
/// `strm` must be a valid inflateBack stream; `in_`/`out` must be valid C
/// callbacks honoring the `in_func`/`out_func` contracts.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn inflateBack(
    strm: z_streamp,
    in_: in_func,
    in_desc: voidpf,
    out: out_func,
    out_desc: voidpf,
) -> c_int {
    if strm.is_null() {
        return Z_STREAM_ERROR;
    }
    // SAFETY: strm non-null; read the state pointer.
    let ffi_ptr = unsafe { (*strm).state } as *mut InflateBackFfi;
    if ffi_ptr.is_null() {
        return Z_STREAM_ERROR;
    }
    // Both callbacks must be present (C requires non-null function pointers).
    let (Some(in_fn), Some(out_fn)) = (in_, out) else {
        return Z_STREAM_ERROR;
    };
    // SAFETY: strm non-null; read the seed input descriptor by value.
    let (seed, seed_len) = unsafe { ((*strm).next_in, (*strm).avail_in) };
    // SAFETY: ffi_ptr is a valid, uniquely-borrowed InflateBackFfi.
    let ffi = unsafe { &mut *ffi_ptr };
    // SAFETY: window/wsize were validated at init and remain caller-owned and
    // valid for `wsize` writable bytes per the inflateBack contract.
    let window = unsafe { slice::from_raw_parts_mut(ffi.window, ffi.wsize) };

    let mut input = CBackInput {
        in_func: in_fn,
        in_desc,
        seed,
        seed_len,
        seed_used: false,
    };
    let mut output = CBackOutput {
        out_func: out_fn,
        out_desc,
    };

    inf::inflate_back(&mut ffi.state, window, &mut input, &mut output)
}

/// `inflateBackEnd` — release a callback-based inflate stream (zlib.h L1208).
///
/// The caller-provided window buffer is **not** freed here (it is owned by the
/// caller, matching C); only this crate's boxed state is reclaimed.
///
/// # Safety
///
/// `strm` must be null or a valid inflateBack stream.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn inflateBackEnd(strm: z_streamp) -> c_int {
    if strm.is_null() {
        return Z_STREAM_ERROR;
    }
    // C `inflateBackEnd` parity: it requires a valid `zfree` hook (and we need
    // it to release the hook-allocated container).
    // SAFETY: strm non-null; read the hook field by value.
    if unsafe { (*strm).zfree }.is_none() {
        return Z_STREAM_ERROR;
    }
    // SAFETY: strm non-null; read the state pointer.
    let ffi_ptr = unsafe { (*strm).state } as *mut InflateBackFfi;
    if ffi_ptr.is_null() {
        return Z_STREAM_ERROR;
    }
    // Free the `InflateBackFfi` container through the matching `zfree` hook; the
    // RAII drop of the inlined `InflateState` (the `inflateBackEnd` teardown)
    // runs first and releases its owned buffers. The caller-owned `window` is a
    // borrowed raw pointer and is intentionally not freed here.
    // SAFETY: ffi_ptr came from `ffi_alloc_state` with this stream's hooks.
    let (zfree, opaque) = unsafe { ((*strm).zfree, (*strm).opaque) };
    unsafe { ffi_free_state(ffi_ptr, zfree, opaque) };
    // SAFETY: strm non-null; clear the now-dangling state pointer.
    unsafe { (*strm).state = ptr::null_mut() };
    Z_OK
}

// ===========================================================================
// Phase C — exported functions: checksums (adler32.c / crc32.c)
// ===========================================================================
//
// The engines operate on `u32`; zlib's `uLong` may be 64-bit (LP64), so each
// running value is narrowed to `u32` on the way in and widened on the way out.
// The C `buf == Z_NULL` "initialize" convention is honored exactly: `adler32`
// returns 1 and `crc32` returns 0 for a null buffer (the documented init values).

/// `adler32` — update a running Adler-32 with `len` bytes at `buf` (zlib.h L1809).
///
/// # Safety
///
/// `buf` must be null, or point to `len` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn adler32(adler: uLong, buf: *const Bytef, len: uInt) -> uLong {
    if buf.is_null() {
        return 1;
    }
    // SAFETY: buf non-null with `len` readable bytes per the contract.
    let data = unsafe { const_slice(buf, len) };
    cksum::adler32(adler as u32, data) as uLong
}

/// `adler32_z` — `size_t`-length variant of [`adler32`] (zlib.h L1829-1830).
///
/// # Safety
///
/// `buf` must be null, or point to `len` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn adler32_z(adler: uLong, buf: *const Bytef, len: z_size_t) -> uLong {
    if buf.is_null() {
        return 1;
    }
    // `len == 0` yields an empty slice, avoiding a `from_raw_parts` call on a
    // non-null-but-possibly-dangling pointer with zero length.
    let data = if len == 0 {
        &[][..]
    } else {
        // SAFETY: `buf` is non-null (checked above) and points to `len`
        // readable bytes per the C contract.
        unsafe { slice::from_raw_parts(buf, len) }
    };
    cksum::adler32_z(adler as u32, data) as uLong
}

/// `adler32_combine` — combine two Adler-32 values (zlib.h L1837-1838).
#[unsafe(no_mangle)]
// `z_off_t`->`i64` is a no-op on LP64 but a real widening on ILP32/LLP64
// (C `long` is 32-bit there) — kept for C-ABI portability.
#[allow(clippy::unnecessary_cast)]
pub extern "C" fn adler32_combine(adler1: uLong, adler2: uLong, len2: z_off_t) -> uLong {
    cksum::adler32_combine(adler1 as u32, adler2 as u32, len2 as i64) as uLong
}

/// `adler32_combine64` — 64-bit-length variant of [`adler32_combine`]
/// (zlib.h L1982).
#[unsafe(no_mangle)]
pub extern "C" fn adler32_combine64(adler1: uLong, adler2: uLong, len2: z_off64_t) -> uLong {
    cksum::adler32_combine64(adler1 as u32, adler2 as u32, len2) as uLong
}

/// `crc32` — update a running CRC-32 with `len` bytes at `buf` (zlib.h L1848).
///
/// # Safety
///
/// `buf` must be null, or point to `len` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn crc32(crc: uLong, buf: *const Bytef, len: uInt) -> uLong {
    if buf.is_null() {
        return 0;
    }
    // SAFETY: buf non-null with `len` readable bytes per the contract.
    let data = unsafe { const_slice(buf, len) };
    cksum::crc32(crc as u32, data) as uLong
}

/// `crc32_z` — `size_t`-length variant of [`crc32`] (zlib.h L1866-1867).
///
/// # Safety
///
/// `buf` must be null, or point to `len` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn crc32_z(crc: uLong, buf: *const Bytef, len: z_size_t) -> uLong {
    if buf.is_null() {
        return 0;
    }
    // `len == 0` yields an empty slice, avoiding a `from_raw_parts` call on a
    // non-null-but-possibly-dangling pointer with zero length.
    let data = if len == 0 {
        &[][..]
    } else {
        // SAFETY: `buf` is non-null (checked above) and points to `len`
        // readable bytes per the C contract.
        unsafe { slice::from_raw_parts(buf, len) }
    };
    cksum::crc32_z(crc as u32, data) as uLong
}

/// `crc32_combine` — combine two CRC-32 values (zlib.h L1874).
#[unsafe(no_mangle)]
// `z_off_t`->`i64` is a no-op on LP64 but a real widening on ILP32/LLP64 — kept
// for C-ABI portability.
#[allow(clippy::unnecessary_cast)]
pub extern "C" fn crc32_combine(crc1: uLong, crc2: uLong, len2: z_off_t) -> uLong {
    cksum::crc32_combine(crc1 as u32, crc2 as u32, len2 as i64) as uLong
}

/// `crc32_combine64` — 64-bit-length variant of [`crc32_combine`] (zlib.h L1983).
#[unsafe(no_mangle)]
pub extern "C" fn crc32_combine64(crc1: uLong, crc2: uLong, len2: z_off64_t) -> uLong {
    cksum::crc32_combine64(crc1 as u32, crc2 as u32, len2) as uLong
}

/// `crc32_combine_gen` — generate the combine operator for length `len2`
/// (zlib.h L1884).
#[unsafe(no_mangle)]
// `z_off_t`->`i64` is a no-op on LP64 but a real widening on ILP32/LLP64 — kept
// for C-ABI portability.
#[allow(clippy::unnecessary_cast)]
pub extern "C" fn crc32_combine_gen(len2: z_off_t) -> uLong {
    cksum::crc32_combine_gen(len2 as i64) as uLong
}

/// `crc32_combine_gen64` — 64-bit-length variant of [`crc32_combine_gen`]
/// (zlib.h L1984).
#[unsafe(no_mangle)]
pub extern "C" fn crc32_combine_gen64(len2: z_off64_t) -> uLong {
    cksum::crc32_combine_gen64(len2) as uLong
}

/// `crc32_combine_op` — combine using a precomputed operator (zlib.h L1890).
#[unsafe(no_mangle)]
pub extern "C" fn crc32_combine_op(crc1: uLong, crc2: uLong, op: uLong) -> uLong {
    cksum::crc32_combine_op(crc1 as u32, crc2 as u32, op as u32) as uLong
}

/// `get_crc_table` — pointer to the 256-entry CRC-32 lookup table
/// (zlib.h L2035). The table is `'static`, so the returned pointer is valid for
/// the life of the process.
#[unsafe(no_mangle)]
pub extern "C" fn get_crc_table() -> *const z_crc_t {
    cksum::get_crc_table().as_ptr()
}

// ===========================================================================
// Phase C — exported functions: one-shot helpers (compress.c / uncompr.c)
// ===========================================================================

/// Reconstruct a shared byte slice from a `(ptr, usize-len)` C pair.
///
/// # Safety
///
/// As [`const_slice`], but with a `usize` length (for the `z_size_t` API).
#[inline]
unsafe fn const_slice_sz<'a>(ptr: *const Bytef, len: usize) -> &'a [u8] {
    if ptr.is_null() || len == 0 {
        &[]
    } else {
        // SAFETY: ptr non-null with `len` readable bytes per the caller.
        unsafe { slice::from_raw_parts(ptr, len) }
    }
}

/// Reconstruct a mutable byte slice from a `(ptr, usize-len)` C pair.
///
/// # Safety
///
/// As [`mut_slice`], but with a `usize` length (for the `z_size_t` API).
#[inline]
unsafe fn mut_slice_sz<'a>(ptr: *mut Bytef, len: usize) -> &'a mut [u8] {
    if ptr.is_null() || len == 0 {
        &mut []
    } else {
        // SAFETY: ptr non-null with `len` writable bytes per the caller.
        unsafe { slice::from_raw_parts_mut(ptr, len) }
    }
}

/// `compress2` — one-shot compression at the given `level` (zlib.h L1288-1290).
///
/// `*dest_len` is the destination capacity on entry and the compressed length
/// on exit.
///
/// # Safety
///
/// `dest`/`dest_len`/`source` must be valid; `dest` must have room for
/// `*dest_len` bytes and `source` for `source_len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn compress2(
    dest: *mut Bytef,
    dest_len: *mut uLongf,
    source: *const Bytef,
    source_len: uLong,
    level: c_int,
) -> c_int {
    if dest.is_null() || dest_len.is_null() {
        return Z_STREAM_ERROR;
    }
    // C one-shot contract: a null `source` paired with a nonzero `source_len`
    // is invalid (inside C it becomes a null `next_in` with nonzero `avail_in`,
    // which `deflate` rejects with `Z_STREAM_ERROR`). Reject it here rather than
    // treating the buffer as silently empty.
    if source.is_null() && source_len != 0 {
        return Z_STREAM_ERROR;
    }
    // SAFETY: dest_len non-null; read the destination capacity.
    let cap = unsafe { *dest_len } as usize;
    // SAFETY: validated (ptr, len) pairs per the contract.
    let dst = unsafe { mut_slice_sz(dest, cap) };
    let src = unsafe { const_slice_sz(source, source_len as usize) };
    match cmp::compress2_to_buf(dst, src, level) {
        Ok(written) => {
            // SAFETY: dest_len non-null and valid for writes.
            unsafe { *dest_len = written as uLongf };
            Z_OK
        }
        Err(e) => e.as_c_int(),
    }
}

/// `compress` — one-shot compression at the default level (zlib.h L1271-1272).
///
/// # Safety
///
/// As [`compress2`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn compress(
    dest: *mut Bytef,
    dest_len: *mut uLongf,
    source: *const Bytef,
    source_len: uLong,
) -> c_int {
    if dest.is_null() || dest_len.is_null() {
        return Z_STREAM_ERROR;
    }
    // C one-shot contract: a null `source` paired with a nonzero `source_len`
    // is invalid (it would be a null `next_in` with nonzero `avail_in` inside
    // `deflate`). Reject it rather than treating the buffer as silently empty.
    if source.is_null() && source_len != 0 {
        return Z_STREAM_ERROR;
    }
    // SAFETY: dest_len non-null; read the destination capacity.
    let cap = unsafe { *dest_len } as usize;
    // SAFETY: validated (ptr, len) pairs per the contract.
    let dst = unsafe { mut_slice_sz(dest, cap) };
    let src = unsafe { const_slice_sz(source, source_len as usize) };
    match cmp::compress_to_buf(dst, src) {
        Ok(written) => {
            // SAFETY: dest_len non-null and valid for writes.
            unsafe { *dest_len = written as uLongf };
            Z_OK
        }
        Err(e) => e.as_c_int(),
    }
}

/// `compressBound` — upper bound on the compressed size of `source_len` bytes
/// (zlib.h L1307).
#[unsafe(no_mangle)]
pub extern "C" fn compressBound(source_len: uLong) -> uLong {
    cmp::compress_bound(source_len as usize) as uLong
}

/// `uncompress2` — one-shot decompression, reporting consumed input via
/// `*source_len` (zlib.h L1335-1336).
///
/// # Safety
///
/// All four pointers must be valid; `dest` must have room for `*dest_len` bytes
/// and `source` for `*source_len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uncompress2(
    dest: *mut Bytef,
    dest_len: *mut uLongf,
    source: *const Bytef,
    source_len: *mut uLong,
) -> c_int {
    if dest.is_null() || dest_len.is_null() || source_len.is_null() {
        return Z_STREAM_ERROR;
    }
    // SAFETY: the out-length pointers are non-null; read the capacities.
    let cap = unsafe { *dest_len } as usize;
    let src_len = unsafe { *source_len } as usize;
    // C one-shot contract: a null `source` paired with a nonzero `*source_len`
    // is invalid (it would be a null `next_in` with nonzero `avail_in` inside
    // `inflate`). Reject it rather than treating the buffer as silently empty.
    if source.is_null() && src_len != 0 {
        return Z_STREAM_ERROR;
    }
    // SAFETY: validated (ptr, len) pairs per the contract.
    let dst = unsafe { mut_slice_sz(dest, cap) };
    let src = unsafe { const_slice_sz(source, src_len) };
    match ucmp::uncompress2_to_buf(dst, src) {
        Ok((dused, sused)) => {
            // SAFETY: both out pointers are non-null and valid for writes.
            unsafe {
                *dest_len = dused as uLongf;
                *source_len = sused as uLong;
            }
            Z_OK
        }
        Err(e) => e.as_c_int(),
    }
}

/// `uncompress` — one-shot decompression (zlib.h L1315-1316).
///
/// # Safety
///
/// `dest`/`dest_len`/`source` must be valid; `dest` must have room for
/// `*dest_len` bytes and `source` for `source_len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uncompress(
    dest: *mut Bytef,
    dest_len: *mut uLongf,
    source: *const Bytef,
    source_len: uLong,
) -> c_int {
    if dest.is_null() || dest_len.is_null() {
        return Z_STREAM_ERROR;
    }
    // C one-shot contract: a null `source` paired with a nonzero `source_len`
    // is invalid (it would be a null `next_in` with nonzero `avail_in` inside
    // `inflate`). Reject it rather than treating the buffer as silently empty.
    if source.is_null() && source_len != 0 {
        return Z_STREAM_ERROR;
    }
    // SAFETY: dest_len non-null; read the destination capacity.
    let cap = unsafe { *dest_len } as usize;
    // SAFETY: validated (ptr, len) pairs per the contract.
    let dst = unsafe { mut_slice_sz(dest, cap) };
    let src = unsafe { const_slice_sz(source, source_len as usize) };
    match ucmp::uncompress(dst, src) {
        Ok(written) => {
            // SAFETY: dest_len non-null and valid for writes.
            unsafe { *dest_len = written as uLongf };
            Z_OK
        }
        Err(e) => e.as_c_int(),
    }
}

// --- `_z` ABI variants (zlib 1.3.2; `z_size_t`-based lengths) ---------------

/// `compress2_z` — `size_t`-length variant of [`compress2`] (zlib.h L1291-1293).
///
/// # Safety
///
/// As [`compress2`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn compress2_z(
    dest: *mut Bytef,
    dest_len: *mut z_size_t,
    source: *const Bytef,
    source_len: z_size_t,
    level: c_int,
) -> c_int {
    // C zlib rejects a NULL source paired with a nonzero length rather than
    // silently treating it as empty; mirror that exact contract here.
    if dest.is_null() || dest_len.is_null() || (source.is_null() && source_len != 0) {
        return Z_STREAM_ERROR;
    }
    // SAFETY: dest_len non-null; read the destination capacity.
    let cap = unsafe { *dest_len };
    // SAFETY: validated (ptr, len) pairs per the contract.
    let dst = unsafe { mut_slice_sz(dest, cap) };
    let src = unsafe { const_slice_sz(source, source_len) };
    match cmp::compress2_to_buf(dst, src, level) {
        Ok(written) => {
            // SAFETY: dest_len non-null and valid for writes.
            unsafe { *dest_len = written };
            Z_OK
        }
        Err(e) => e.as_c_int(),
    }
}

/// `compress_z` — `size_t`-length variant of [`compress`] (zlib.h L1273-1274).
///
/// # Safety
///
/// As [`compress`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn compress_z(
    dest: *mut Bytef,
    dest_len: *mut z_size_t,
    source: *const Bytef,
    source_len: z_size_t,
) -> c_int {
    // C zlib rejects a NULL source paired with a nonzero length rather than
    // silently treating it as empty; mirror that exact contract here.
    if dest.is_null() || dest_len.is_null() || (source.is_null() && source_len != 0) {
        return Z_STREAM_ERROR;
    }
    // SAFETY: dest_len non-null; read the destination capacity.
    let cap = unsafe { *dest_len };
    // SAFETY: validated (ptr, len) pairs per the contract.
    let dst = unsafe { mut_slice_sz(dest, cap) };
    let src = unsafe { const_slice_sz(source, source_len) };
    match cmp::compress_to_buf(dst, src) {
        Ok(written) => {
            // SAFETY: dest_len non-null and valid for writes.
            unsafe { *dest_len = written };
            Z_OK
        }
        Err(e) => e.as_c_int(),
    }
}

/// `compressBound_z` — `size_t` variant of [`compressBound`] (zlib.h L1308).
#[unsafe(no_mangle)]
pub extern "C" fn compressBound_z(source_len: z_size_t) -> z_size_t {
    cmp::compress_bound(source_len)
}

/// `deflateBound_z` — `size_t` variant of [`deflateBound`] (zlib.h L769).
///
/// # Safety
///
/// `strm` must be null or a valid `z_stream`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn deflateBound_z(strm: z_streamp, source_len: z_size_t) -> z_size_t {
    // SAFETY: strm may be null; deflate_state handles that.
    let state = unsafe { deflate_state(strm) };
    dfl::deflate_bound(state.as_deref(), source_len as u64) as z_size_t
}

/// `uncompress2_z` — `size_t`-length variant of [`uncompress2`]
/// (zlib.h L1337-1339).
///
/// # Safety
///
/// As [`uncompress2`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uncompress2_z(
    dest: *mut Bytef,
    dest_len: *mut z_size_t,
    source: *const Bytef,
    source_len: *mut z_size_t,
) -> c_int {
    if dest.is_null() || dest_len.is_null() || source_len.is_null() {
        return Z_STREAM_ERROR;
    }
    // SAFETY: the out-length pointers are non-null; read the capacities.
    let cap = unsafe { *dest_len };
    let src_len = unsafe { *source_len };
    // C zlib rejects a NULL source paired with a nonzero length rather than
    // silently treating it as empty; mirror that exact contract here.
    if source.is_null() && src_len != 0 {
        return Z_STREAM_ERROR;
    }
    // SAFETY: validated (ptr, len) pairs per the contract.
    let dst = unsafe { mut_slice_sz(dest, cap) };
    let src = unsafe { const_slice_sz(source, src_len) };
    match ucmp::uncompress2_to_buf(dst, src) {
        Ok((dused, sused)) => {
            // SAFETY: both out pointers are non-null and valid for writes.
            unsafe {
                *dest_len = dused;
                *source_len = sused;
            }
            Z_OK
        }
        Err(e) => e.as_c_int(),
    }
}

/// `uncompress_z` — `size_t`-length variant of [`uncompress`] (zlib.h L1317-1318).
///
/// # Safety
///
/// As [`uncompress`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uncompress_z(
    dest: *mut Bytef,
    dest_len: *mut z_size_t,
    source: *const Bytef,
    source_len: z_size_t,
) -> c_int {
    // C zlib rejects a NULL source paired with a nonzero length rather than
    // silently treating it as empty; mirror that exact contract here.
    if dest.is_null() || dest_len.is_null() || (source.is_null() && source_len != 0) {
        return Z_STREAM_ERROR;
    }
    // SAFETY: dest_len non-null; read the destination capacity.
    let cap = unsafe { *dest_len };
    // SAFETY: validated (ptr, len) pairs per the contract.
    let dst = unsafe { mut_slice_sz(dest, cap) };
    let src = unsafe { const_slice_sz(source, source_len) };
    match ucmp::uncompress(dst, src) {
        Ok(written) => {
            // SAFETY: dest_len non-null and valid for writes.
            unsafe { *dest_len = written };
            Z_OK
        }
        Err(e) => e.as_c_int(),
    }
}

// ===========================================================================
// Phase C — exported functions: version / flags (zutil.c)
// ===========================================================================

/// `zlibVersion` — the library version string (zlib.h L224).
///
/// Returns a pointer to a `'static` NUL-terminated string (`ZLIB_VERSION`),
/// valid for the life of the process.
#[unsafe(no_mangle)]
pub extern "C" fn zlibVersion() -> *const c_char {
    ZLIB_VERSION_CSTR.as_ptr()
}

/// `zlibCompileFlags` — the compile-time configuration bitset (zlib.h L1216).
#[unsafe(no_mangle)]
pub extern "C" fn zlibCompileFlags() -> uLong {
    ver::zlib_compile_flags() as uLong
}

/// `zError` — map a return code to its static error string (zlib.h L2033).
///
/// Reproduces C's `z_errmsg` table (indexed by `Z_NEED_DICT - err`, i.e.
/// `2 - err`), returning a `'static` NUL-terminated C string. `Z_OK` and
/// out-of-range codes map to the empty string, matching C.
#[unsafe(no_mangle)]
pub extern "C" fn zError(err: c_int) -> *const c_char {
    let cstr: &core::ffi::CStr = match err {
        2 => c"need dictionary",       // Z_NEED_DICT
        1 => c"stream end",            // Z_STREAM_END
        0 => c"",                      // Z_OK
        -1 => c"file error",           // Z_ERRNO
        -2 => c"stream error",         // Z_STREAM_ERROR
        -3 => c"data error",           // Z_DATA_ERROR
        -4 => c"insufficient memory",  // Z_MEM_ERROR
        -5 => c"buffer error",         // Z_BUF_ERROR
        -6 => c"incompatible version", // Z_VERSION_ERROR
        _ => c"",
    };
    cstr.as_ptr()
}

// ============================================================================
// Phase C — gzip file I/O FFI  (`gzlib.c` / `gzread.c` / `gzwrite.c` /
// `gzclose.c`).  Gated behind `feature = "gz-io"` (which implies `std + gzip`);
// the whole module is already `capi`-gated at the file head.
// ----------------------------------------------------------------------------
// `gzFile` (an opaque C `void *`) is this crate's `Box<GzState>` erased to a raw
// pointer with `Box::into_raw`.  Every entry point reclaims a `&mut GzState`
// borrow for the duration of the call; `gzclose`/`gzclose_r`/`gzclose_w` reclaim
// the owning `Box` and drop it, the RAII replacement for C's `free`.
//
// NULL-handle behavior is matched byte-for-byte to the C baseline (verified
// against `gz*.c`): functions return -1, 0, `Z_STREAM_ERROR`, or NULL exactly
// as their C counterparts do when handed a NULL `gzFile`.
//
// Three deliberate, documented divergences from C (each unavoidable on the safe
// Rust/`#[no_mangle]` boundary; all preserve the integer/return contract):
//
//   * `gzerror` — the engine stores its message as a dynamically-formatted
//     `"<path>: <msg>"` Rust `String`, which is NOT NUL-terminated, so a pointer
//     into it cannot be handed to C soundly.  We write `*errnum` EXACTLY and
//     return the canonical `'static`, NUL-terminated text for the code via
//     [`gz_strerror`] (mirroring C's wording, e.g. `Z_MEM_ERROR` → "out of
//     memory").  Callers branch on `*errnum`, which is bit-identical to C.
//
//   * `gzprintf` — C's variadic `gzprintf(file, format, ...)` cannot be DEFINED
//     in stable Rust (`c_variadic` is unstable) and a `cc` trampoline is barred
//     by the "zero C dependency in the shipping crate" constraint (§0.7.2), so
//     the C symbol is intentionally NOT exported. It is absent from `zlib.map`'s
//     `global:` nodes (only `gzvprintf` is listed, in `ZLIB_1.2.7.1`), so this
//     does not breach the zlib.map-grounded FFI contract (§0.6.2). `gzvprintf`
//     IS exported with full, byte-identical `printf` formatting via the platform
//     `vsnprintf`; C callers needing `gzprintf` add the canonical three-line
//     `va_start` → `gzvprintf` forwarder, and Rust callers use the safe
//     `zlib_rs::gz::gzprintf(format_args!(...))`. See the detailed note at the
//     `gzvprintf` definition.
//
//   * `gzdopen` — adopting a C file descriptor requires `File::from_raw_fd`
//     (unix only).  On the rare engine-open failure the `File` is dropped,
//     closing the fd (Rust ownership); C would leave it open.  The success path
//     — where the returned `gzFile` owns the fd and `gzclose` closes it — is
//     identical to C.
// ============================================================================

#[cfg(feature = "gz-io")]
use crate::gz::{self, GzState};
#[cfg(feature = "gz-io")]
use std::fs::File;
#[cfg(feature = "gz-io")]
use std::path::Path;

/// Reclaim a `&mut GzState` borrow from a C `gzFile` handle.
///
/// Returns [`None`] for a NULL handle (the C NULL-pointer case), letting each
/// exported shim emit its specific C NULL-return value.
///
/// # Safety
///
/// `file` must be either NULL or a pointer previously returned by one of the
/// `gz*open*` exports and not yet closed.  The borrow must not outlive the call,
/// and no other borrow of the same handle may be live concurrently.
#[cfg(feature = "gz-io")]
#[inline]
unsafe fn gz_state<'a>(file: gzFile) -> Option<&'a mut GzState> {
    if file.is_null() {
        None
    } else {
        // SAFETY: a non-NULL `file` is a live `Box<GzState>` pointer per the
        // C `gzFile` contract; we hand out a single `&mut` for the call only.
        Some(unsafe { &mut *(file as *mut GzState) })
    }
}

/// Borrow a C path string as a [`Path`] (zero-copy, tied to the C buffer).
///
/// Returns [`None`] for a NULL pointer.  On unix the raw bytes map directly to
/// an [`OsStr`](std::ffi::OsStr) (preserving non-UTF-8 paths exactly, like C);
/// on other targets a non-UTF-8 path yields [`None`].
///
/// # Safety
///
/// `ptr` must be NULL or a valid NUL-terminated C string that stays alive and
/// unmodified for the returned borrow's lifetime.
#[cfg(feature = "gz-io")]
#[inline]
unsafe fn cstr_to_path<'a>(ptr: *const c_char) -> Option<&'a Path> {
    if ptr.is_null() {
        return None;
    }
    // SAFETY: caller guarantees `ptr` is a valid NUL-terminated C string.
    let bytes = unsafe { core::ffi::CStr::from_ptr(ptr) }.to_bytes();
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        Some(Path::new(std::ffi::OsStr::from_bytes(bytes)))
    }
    #[cfg(not(unix))]
    {
        core::str::from_utf8(bytes).ok().map(Path::new)
    }
}

/// Borrow a C string as a `&str` (UTF-8 validated; used for `gz*open` modes).
///
/// Returns [`None`] for a NULL pointer or non-UTF-8 contents.  zlib mode strings
/// (`"rb"`, `"wb9"`, …) are ASCII, so this is faithful in practice.
///
/// # Safety
///
/// As [`cstr_to_path`].
#[cfg(feature = "gz-io")]
#[inline]
unsafe fn cstr_to_str<'a>(ptr: *const c_char) -> Option<&'a str> {
    if ptr.is_null() {
        return None;
    }
    // SAFETY: caller guarantees `ptr` is a valid NUL-terminated C string.
    core::str::from_utf8(unsafe { core::ffi::CStr::from_ptr(ptr) }.to_bytes()).ok()
}

/// Narrow an engine `i64` file offset to the platform `z_off_t` (C `long`).
///
/// On LP64 targets (e.g. x86-64 Linux) `z_off_t == i64`, so this is the
/// identity; on ILP32 / LLP64 targets it truncates exactly as the C `(z_off_t)`
/// cast does.  The lone `as` cast is intentional and portable.
#[cfg(feature = "gz-io")]
#[inline]
#[allow(clippy::unnecessary_cast)] // z_off_t is i64 on LP64 but i32 on ILP32/LLP64
fn off_t(v: i64) -> z_off_t {
    v as z_off_t
}

/// Widen a platform `z_off_t` (C `long`) to the engine's `i64` offset.
///
/// The identity on LP64 targets; a lossless widening on ILP32/LLP64 (where C
/// `long` is 32-bit).  This is the inverse of [`off_t`] for input offsets.
#[cfg(feature = "gz-io")]
#[inline]
#[allow(clippy::useless_conversion)] // identity on LP64 where z_off_t == i64
fn off_in(v: z_off_t) -> i64 {
    i64::from(v)
}

/// Map a zlib return/error code to its canonical `'static`, NUL-terminated C
/// message, for [`gzerror`].
///
/// Mirrors the wording C's `gzerror` reports (notably `Z_MEM_ERROR` →
/// "out of memory").  See the module-level note on the `gzerror` divergence.
#[cfg(feature = "gz-io")]
#[inline]
fn gz_strerror(err: c_int) -> *const c_char {
    let cstr: &core::ffi::CStr = match err {
        2 => c"need dictionary",       // Z_NEED_DICT
        1 => c"stream end",            // Z_STREAM_END
        0 => c"",                      // Z_OK (no error)
        -1 => c"file error",           // Z_ERRNO
        -2 => c"stream error",         // Z_STREAM_ERROR
        -3 => c"data error",           // Z_DATA_ERROR
        -4 => c"out of memory",        // Z_MEM_ERROR (C `gzerror` wording)
        -5 => c"buffer error",         // Z_BUF_ERROR
        -6 => c"incompatible version", // Z_VERSION_ERROR
        _ => c"",
    };
    cstr.as_ptr()
}

/// `gzopen` — open a gzip file by path for reading or writing (zlib.h L1357).
///
/// Returns a `gzFile` handle, or NULL if `path`/`mode` is NULL or the open
/// fails.
///
/// # Safety
///
/// `path` and `mode` must be NULL or valid NUL-terminated C strings.
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzopen(path: *const c_char, mode: *const c_char) -> gzFile {
    // SAFETY: `path`/`mode` are valid NUL-terminated C strings or NULL per the
    // caller; the borrows live only until `gz::gzopen` returns.
    let (Some(p), Some(m)) = (unsafe { cstr_to_path(path) }, unsafe { cstr_to_str(mode) }) else {
        return ptr::null_mut();
    };
    match gz::gzopen(p, m) {
        Some(state) => Box::into_raw(state) as gzFile,
        None => ptr::null_mut(),
    }
}

/// `gzopen64` — `gzopen` with an explicit 64-bit-offset entry point
/// (zlib.h L1978).  Behaviorally identical to [`gzopen`] in this port.
///
/// # Safety
///
/// As [`gzopen`].
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzopen64(path: *const c_char, mode: *const c_char) -> gzFile {
    // SAFETY: `path`/`mode` are valid NUL-terminated C strings or NULL per the
    // caller; the borrows live only until `gz::gzopen64` returns.
    let (Some(p), Some(m)) = (unsafe { cstr_to_path(path) }, unsafe { cstr_to_str(mode) }) else {
        return ptr::null_mut();
    };
    match gz::gzopen64(p, m) {
        Some(state) => Box::into_raw(state) as gzFile,
        None => ptr::null_mut(),
    }
}

/// `gzdopen` — associate a `gzFile` with an already-open file descriptor
/// (zlib.h L1404).  Unix targets only; see the module-level divergence note.
///
/// # Safety
///
/// `fd` must be a valid, open descriptor whose ownership transfers to the
/// returned handle (C contract).  `mode` must be NULL or a valid C string.
#[cfg(all(feature = "gz-io", unix))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzdopen(fd: c_int, mode: *const c_char) -> gzFile {
    use std::os::fd::FromRawFd;
    // SAFETY: `mode` is a valid NUL-terminated C string or NULL per the caller.
    let Some(m) = (unsafe { cstr_to_str(mode) }) else {
        return ptr::null_mut();
    };
    // SAFETY: per the C `gzdopen` contract the caller passes an open fd whose
    // ownership transfers to the returned handle. We adopt it as an owned
    // `File`; on success `GzState` owns it (closed by `gzclose`), matching C.
    let file = unsafe { File::from_raw_fd(fd) };
    match gz::gzdopen(file, m) {
        Some(state) => Box::into_raw(state) as gzFile,
        None => ptr::null_mut(),
    }
}

/// `gzdopen` — non-unix fallback. The fd→`File` mapping is implemented only for
/// unix targets; other platforms return NULL (open-by-path via [`gzopen`] is
/// fully supported everywhere).
///
/// # Safety
///
/// Trivially safe: ignores its arguments and returns NULL.
#[cfg(all(feature = "gz-io", not(unix)))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzdopen(_fd: c_int, _mode: *const c_char) -> gzFile {
    ptr::null_mut()
}

/// `gzclose` — flush, finalize, and free a `gzFile`, dispatching by mode
/// (zlib.h L1750).  Returns `Z_STREAM_ERROR` for a NULL handle.
///
/// # Safety
///
/// `file` must be NULL or a live handle from a `gz*open*` export; it is
/// consumed and must not be used afterward.
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzclose(file: gzFile) -> c_int {
    let state = if file.is_null() {
        None
    } else {
        // SAFETY: reclaim ownership of the `Box<GzState>` created at open time;
        // `gz::gzclose` consumes and drops it (deterministic teardown).
        Some(unsafe { Box::from_raw(file as *mut GzState) })
    };
    // `gz::gzclose(None)` returns `Z_STREAM_ERROR`, matching C's NULL check.
    gz::gzclose(state)
}

/// `gzclose_r` — read-side close/free of a `gzFile` (zlib.h L1763).
///
/// Returns `Z_STREAM_ERROR` for a NULL handle.  The owning `Box` is always
/// reclaimed and dropped, so the handle is freed even if the engine reports a
/// mode mismatch (leak-free; C would leak on mismatch).
///
/// # Safety
///
/// As [`gzclose`].
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzclose_r(file: gzFile) -> c_int {
    if file.is_null() {
        return Z_STREAM_ERROR;
    }
    // SAFETY: reclaim the owning `Box<GzState>` created at open time.
    let mut state = unsafe { Box::from_raw(file as *mut GzState) };
    let ret = crate::gz::read::gzclose_r(&mut state);
    drop(state); // free the handle (RAII teardown via `Drop`)
    ret
}

/// `gzclose_w` — write-side close/free of a `gzFile` (zlib.h L1764).
///
/// Returns `Z_STREAM_ERROR` for a NULL handle.  See [`gzclose_r`] regarding
/// always-free semantics.
///
/// # Safety
///
/// As [`gzclose`].
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzclose_w(file: gzFile) -> c_int {
    if file.is_null() {
        return Z_STREAM_ERROR;
    }
    // SAFETY: reclaim the owning `Box<GzState>` created at open time.
    let mut state = unsafe { Box::from_raw(file as *mut GzState) };
    let ret = gz::gzclose_w(&mut state);
    drop(state); // free the handle (RAII teardown via `Drop`)
    ret
}

/// `gzbuffer` — set the internal buffer size for the handle (zlib.h L1429).
/// Returns -1 on a NULL handle or if called after I/O has begun.
///
/// # Safety
///
/// `file` must be NULL or a live `gzFile`.
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzbuffer(file: gzFile, size: c_uint) -> c_int {
    // SAFETY: `file` is NULL or a live handle per the caller.
    match unsafe { gz_state(file) } {
        Some(state) => gz::gzbuffer(state, size),
        None => -1,
    }
}

/// `gzsetparams` — change compression level/strategy mid-stream (zlib.h L1445).
/// Returns `Z_STREAM_ERROR` on a NULL handle.
///
/// # Safety
///
/// `file` must be NULL or a live `gzFile`.
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzsetparams(file: gzFile, level: c_int, strategy: c_int) -> c_int {
    // SAFETY: `file` is NULL or a live handle per the caller.
    match unsafe { gz_state(file) } {
        Some(state) => gz::gzsetparams(state, level, strategy),
        None => Z_STREAM_ERROR,
    }
}

/// `gzread` — read and decompress up to `len` bytes into `buf` (zlib.h L1456).
/// Returns the number of bytes read, or -1 on error / NULL handle.
///
/// # Safety
///
/// `file` must be NULL or a live `gzFile`; `buf` must point to at least `len`
/// writable bytes (or be NULL with `len == 0`).
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzread(file: gzFile, buf: voidp, len: c_uint) -> c_int {
    // SAFETY: `file` is NULL or a live handle per the caller.
    let Some(state) = (unsafe { gz_state(file) }) else {
        return -1;
    };
    // SAFETY: `buf` has `len` writable bytes per the C contract.
    let slice = unsafe { mut_slice(buf as *mut Bytef, len) };
    gz::gzread(state, slice)
}

/// `gzfread` — read `size * nitems` bytes, `fread`-style (zlib.h L1492).
/// Returns the number of full items read.  Note the C argument order: `buf`
/// FIRST, `file` LAST.  Returns 0 on a NULL handle.
///
/// # Safety
///
/// `file` must be NULL or a live `gzFile`; `buf` must point to at least
/// `size * nitems` writable bytes.
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzfread(
    buf: voidp,
    size: z_size_t,
    nitems: z_size_t,
    file: gzFile,
) -> z_size_t {
    // SAFETY: `file` is NULL or a live handle per the caller.
    let Some(state) = (unsafe { gz_state(file) }) else {
        return 0;
    };
    // On `size * nitems` overflow, hand the engine an empty slice: it re-derives
    // the product, detects the `size_t` overflow, records the error, and returns
    // 0 — so the empty slice is never read (preserving C's error reporting).
    let slice = match size.checked_mul(nitems) {
        // SAFETY: `buf` has `total` writable bytes per the C contract.
        Some(total) => unsafe { mut_slice_sz(buf as *mut Bytef, total) },
        None => &mut [],
    };
    gz::gzfread(state, size, nitems, slice)
}

/// `gzwrite` — compress and write `len` bytes from `buf` (zlib.h L1519).
/// Returns the number of bytes written, or 0 on error / NULL handle.
///
/// # Safety
///
/// `file` must be NULL or a live `gzFile`; `buf` must point to at least `len`
/// readable bytes (or be NULL with `len == 0`).
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzwrite(file: gzFile, buf: voidpc, len: c_uint) -> c_int {
    // SAFETY: `file` is NULL or a live handle per the caller.
    let Some(state) = (unsafe { gz_state(file) }) else {
        return 0;
    };
    // SAFETY: `buf` has `len` readable bytes per the C contract.
    let slice = unsafe { const_slice(buf as *const Bytef, len) };
    gz::gzwrite(state, slice)
}

/// `gzfwrite` — write `size * nitems` bytes, `fwrite`-style (zlib.h L1529).
/// Returns the number of full items written.  Note the C argument order: `buf`
/// FIRST, `file` LAST.  Returns 0 on a NULL handle.
///
/// # Safety
///
/// `file` must be NULL or a live `gzFile`; `buf` must point to at least
/// `size * nitems` readable bytes.
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzfwrite(
    buf: voidpc,
    size: z_size_t,
    nitems: z_size_t,
    file: gzFile,
) -> z_size_t {
    // SAFETY: `file` is NULL or a live handle per the caller.
    let Some(state) = (unsafe { gz_state(file) }) else {
        return 0;
    };
    // See `gzfread`: on overflow the engine receives an empty slice, detects the
    // `size_t` overflow, records the error, and returns 0 (slice never read).
    let slice = match size.checked_mul(nitems) {
        // SAFETY: `buf` has `total` readable bytes per the C contract.
        Some(total) => unsafe { const_slice_sz(buf as *const Bytef, total) },
        None => &[],
    };
    gz::gzfwrite(state, size, nitems, slice)
}

// `gzprintf` — INTENTIONALLY NOT EXPORTED as a C symbol.
//
// C's `gzprintf(gzFile, const char *format, ...)` is variadic, and a variadic
// `extern "C"` function cannot be *defined* in stable Rust (the `c_variadic`
// feature is unstable, and the AAP fixes the MSRV at 1.85.0 stable). The only
// other ways to materialize the symbol — a `cc`-compiled C trampoline — are
// ruled out by the AAP's "Zero C dependency in the shipping crate" constraint
// (§0.7.2). Exporting a *non-variadic* stub that writes the format string
// verbatim (no `%`-expansion) is silent data corruption and was explicitly
// rejected in review, so the broken stub is removed rather than shipped.
//
// Omitting `gzprintf` does NOT violate the FFI symbol contract: the AAP grounds
// the required export set in `zlib.map` (§0.6.2), and `gzprintf` is not listed
// under any `global:` node there — only its `va_list` sibling `gzvprintf` is
// (node `ZLIB_1.2.7.1`), and that IS exported below with full formatting. A C
// consumer needing `gzprintf` writes the canonical three-line forwarder:
//
//     int gzprintf(gzFile f, const char *fmt, ...) {
//         va_list va; va_start(va, fmt);
//         int r = gzvprintf(f, fmt, va); va_end(va); return r;
//     }
//
// Pure-Rust callers use the safe, fully-formatting
// `zlib_rs::gz::gzprintf(file, format_args!(...))` (AAP §0.4.1), which is
// retained unchanged.

/// `gzvprintf` — `va_list` formatted write (zlib.h L2047; exported in `zlib.map`
/// node `ZLIB_1.2.7.1`).
///
/// Performs real C `printf`-style formatting, byte-for-byte compatible with
/// zlib's `gzvprintf` (`gzwrite.c`): the arguments are rendered into a
/// `want`-sized scratch buffer via the platform C runtime's `vsnprintf`, and the
/// formatted bytes are then written through the very same buffered gzip path C
/// uses (`gz_comp` at `Z_NO_FLUSH`), so the compressed gzip output is identical.
/// As in C, a non-writing handle or a pending fatal error returns
/// `Z_STREAM_ERROR`; an empty result, a truncated result (`len >= want`), or a
/// formatting error writes nothing and returns `0`; otherwise the number of
/// formatted bytes is returned.
///
/// On the SysV / AArch64 / Win64 C ABIs a `va_list` argument decays to (is
/// passed as) a single pointer, so forwarding the caller's `va` pointer
/// unchanged to `vsnprintf` is call-compatible — the standard variadic-bridge
/// technique, and the only way to honor the `va_list` on stable Rust. The
/// `vsnprintf` import is declared *inside* this function so it stays a private
/// link-time dependency and is never emitted into the generated C header (it is
/// a symbol this crate consumes, not one it exports).
///
/// # Safety
///
/// `file` must be NULL or a live `gzFile`; `format` must be NULL or a valid
/// NUL-terminated C string whose conversion specifiers match the variadic
/// arguments captured in `va`; `va` must be the caller's live `va_list` (passed
/// as its ABI pointer), or may be NULL only when `format` contains no
/// conversions.
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzvprintf(file: gzFile, format: *const c_char, va: *mut c_void) -> c_int {
    // SAFETY: `file` is NULL or a live handle per the caller.
    let Some(state) = (unsafe { gz_state(file) }) else {
        return Z_STREAM_ERROR;
    };
    if format.is_null() {
        return Z_STREAM_ERROR;
    }
    // C `gzvprintf` rejects a non-writing handle or a pending fatal (non-soft)
    // error up front with `Z_STREAM_ERROR` (gzwrite.c), then clears any soft
    // error before formatting.
    if !state.is_writing() || (state.err != Z_OK && !state.again) {
        return Z_STREAM_ERROR;
    }
    state.clear_error();

    // Render into a `want`-sized scratch buffer, exactly as C formats into its
    // `state->size`-sized input buffer (`state->size == want` after init).
    let cap = state.want.max(1);
    let mut buf: alloc::vec::Vec<u8> = alloc::vec::Vec::new();
    if buf.try_reserve_exact(cap).is_err() {
        return 0;
    }
    buf.resize(cap, 0u8);
    // `vsnprintf` is the platform C runtime's formatter (always linked under
    // `gz-io`, which implies `std`). It is imported HERE, inside the function
    // body, on purpose: a module-level `extern` block would be picked up by
    // cbindgen and re-emitted into the generated header, clashing with
    // `<stdio.h>`'s own declaration. As a local item it stays an internal
    // link-time dependency, invisible to cbindgen. It formats `format` + the
    // varargs referenced by `ap` into `buf` (NUL-terminating when `n > 0`) and
    // returns the byte count (excluding the NUL) the full output would need.
    unsafe extern "C" {
        fn vsnprintf(buf: *mut c_char, n: usize, format: *const c_char, ap: *mut c_void) -> c_int;
    }
    // SAFETY: `buf` has `cap` writable bytes; `format` is a valid C string; `va`
    // is the caller's live `va_list` (an opaque pointer at the ABI level), read
    // by `vsnprintf` only as directed by `format`'s conversion specifiers.
    let len = unsafe { vsnprintf(buf.as_mut_ptr().cast::<c_char>(), cap, format, va) };
    // Match C: empty output, truncation (`len >= want`), or a negative error
    // (which C detects via its unsigned `len >= size` comparison) → write
    // nothing and return 0.
    if len <= 0 || (len as usize) >= cap {
        return 0;
    }
    let len = len as usize;
    // Write exactly the formatted bytes through the same buffered gzip path C
    // uses, so the produced gzip stream is byte-identical to C `gzvprintf`.
    let written = gz::gzwrite(state, &buf[..len]);
    if written < 0 { written } else { len as c_int }
}

/// `gzputs` — write a NUL-terminated string (zlib.h L1575).
/// Returns the number of bytes written, or -1 on error / NULL handle / NULL `s`.
///
/// # Safety
///
/// `file` must be NULL or a live `gzFile`; `s` must be NULL or a valid
/// NUL-terminated C string.
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzputs(file: gzFile, s: *const c_char) -> c_int {
    // SAFETY: `file` is NULL or a live handle per the caller.
    let Some(state) = (unsafe { gz_state(file) }) else {
        return -1;
    };
    if s.is_null() {
        return -1;
    }
    // SAFETY: `s` is a valid NUL-terminated C string per the caller.
    let bytes = unsafe { core::ffi::CStr::from_ptr(s) }.to_bytes();
    match core::str::from_utf8(bytes) {
        Ok(st) => gz::gzputs(state, st),
        // Non-UTF-8: write the raw bytes verbatim (C `gzputs` writes bytes).
        Err(_) => gz::gzwrite(state, bytes),
    }
}

/// `gzputc` — write a single byte (zlib.h L1607).
/// Returns the byte written, or -1 on error / NULL handle.
///
/// # Safety
///
/// `file` must be NULL or a live `gzFile`.
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzputc(file: gzFile, c: c_int) -> c_int {
    // SAFETY: `file` is NULL or a live handle per the caller.
    match unsafe { gz_state(file) } {
        Some(state) => gz::gzputc(state, c),
        None => -1,
    }
}

/// `gzgetc` — read a single byte (zlib.h L1613).
/// Returns the byte (0..=255), or -1 on end-of-file / error / NULL handle.
///
/// # Safety
///
/// `file` must be NULL or a live `gzFile`.
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzgetc(file: gzFile) -> c_int {
    // SAFETY: `file` is NULL or a live handle per the caller.
    match unsafe { gz_state(file) } {
        Some(state) => gz::gzgetc(state),
        None => -1,
    }
}

/// `gzgetc_` — the function form of `gzgetc` (zlib.h L1961); identical behavior.
///
/// # Safety
///
/// `file` must be NULL or a live `gzFile`.
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzgetc_(file: gzFile) -> c_int {
    // SAFETY: `file` is NULL or a live handle per the caller.
    match unsafe { gz_state(file) } {
        Some(state) => gz::gzgetc_(state),
        None => -1,
    }
}

/// `gzungetc` — push one byte back into the read buffer (zlib.h L1630).
/// Returns the byte pushed, or -1 on error / NULL handle.  Note the C argument
/// order: `c` FIRST, `file` LAST.
///
/// # Safety
///
/// `file` must be NULL or a live `gzFile`.
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzungetc(c: c_int, file: gzFile) -> c_int {
    // SAFETY: `file` is NULL or a live handle per the caller.
    match unsafe { gz_state(file) } {
        Some(state) => gz::gzungetc(state, c),
        None => -1,
    }
}

/// `gzgets` — read a line into `buf` (at most `len-1` bytes), NUL-terminated
/// (zlib.h L1588).  Returns `buf` on success, or NULL if `file`/`buf` is NULL,
/// `len < 1`, or nothing was read (end-of-file / error) — matching C exactly.
///
/// # Safety
///
/// `file` must be NULL or a live `gzFile`; `buf` must point to at least `len`
/// writable bytes when non-NULL.
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzgets(file: gzFile, buf: *mut c_char, len: c_int) -> *mut c_char {
    // C: `if (file == NULL || buf == NULL || len < 1) return NULL;`
    if buf.is_null() || len < 1 {
        return ptr::null_mut();
    }
    // SAFETY: `file` is NULL or a live handle per the caller.
    let Some(state) = (unsafe { gz_state(file) }) else {
        return ptr::null_mut();
    };
    // Reserve one byte for the terminator: the engine fills at most `len-1`.
    let cap = (len as usize) - 1;
    // SAFETY: `buf` has `len` writable bytes per the caller; we expose `len-1`
    // and keep `buf[written]` (written <= len-1) for the NUL we write below.
    let slice = unsafe { mut_slice_sz(buf as *mut Bytef, cap) };
    let written = gz::gzgets(state, slice);
    if written == 0 {
        // Nothing copied — C `if (buf == str) return NULL;` (EOF / error).
        return ptr::null_mut();
    }
    // SAFETY: `written <= cap = len-1`, so `buf[written]` is in bounds.
    unsafe {
        *buf.add(written) = 0;
    }
    buf
}

/// `gzflush` — flush pending output (zlib.h L1647).
/// Returns a zlib code, or `Z_STREAM_ERROR` on a NULL handle.
///
/// # Safety
///
/// `file` must be NULL or a live `gzFile`.
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzflush(file: gzFile, flush: c_int) -> c_int {
    // SAFETY: `file` is NULL or a live handle per the caller.
    match unsafe { gz_state(file) } {
        Some(state) => gz::gzflush(state, flush),
        None => Z_STREAM_ERROR,
    }
}

/// `gzseek` — set the read/write position (zlib.h L1663).
/// Returns the resulting uncompressed offset, or -1 on error / NULL handle.
///
/// # Safety
///
/// `file` must be NULL or a live `gzFile`.
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzseek(file: gzFile, offset: z_off_t, whence: c_int) -> z_off_t {
    // SAFETY: `file` is NULL or a live handle per the caller.
    match unsafe { gz_state(file) } {
        Some(state) => off_t(gz::gzseek(state, off_in(offset), whence)),
        None => -1,
    }
}

/// `gzseek64` — 64-bit-offset form of [`gzseek`] (zlib.h L1979).
///
/// # Safety
///
/// `file` must be NULL or a live `gzFile`.
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzseek64(file: gzFile, offset: z_off64_t, whence: c_int) -> z_off64_t {
    // SAFETY: `file` is NULL or a live handle per the caller.
    match unsafe { gz_state(file) } {
        Some(state) => gz::gzseek64(state, offset, whence),
        None => -1,
    }
}

/// `gzrewind` — rewind a file open for reading (zlib.h L1683).
/// Returns 0 on success, or -1 on error / NULL handle.
///
/// # Safety
///
/// `file` must be NULL or a live `gzFile`.
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzrewind(file: gzFile) -> c_int {
    // SAFETY: `file` is NULL or a live handle per the caller.
    match unsafe { gz_state(file) } {
        Some(state) => gz::gzrewind(state),
        None => -1,
    }
}

/// `gztell` — current uncompressed offset (zlib.h L1691).
/// Returns the offset, or -1 on a NULL handle.
///
/// # Safety
///
/// `file` must be NULL or a live `gzFile`.
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gztell(file: gzFile) -> z_off_t {
    // SAFETY: `file` is NULL or a live handle per the caller.
    match unsafe { gz_state(file) } {
        Some(state) => off_t(gz::gztell(state)),
        None => -1,
    }
}

/// `gztell64` — 64-bit-offset form of [`gztell`] (zlib.h L1980).
///
/// # Safety
///
/// `file` must be NULL or a live `gzFile`.
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gztell64(file: gzFile) -> z_off64_t {
    // SAFETY: `file` is NULL or a live handle per the caller.
    match unsafe { gz_state(file) } {
        Some(state) => gz::gztell64(state),
        None => -1,
    }
}

/// `gzoffset` — current offset in the compressed file (zlib.h L1702).
/// Returns the offset, or -1 on a NULL handle.
///
/// # Safety
///
/// `file` must be NULL or a live `gzFile`.
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzoffset(file: gzFile) -> z_off_t {
    // SAFETY: `file` is NULL or a live handle per the caller.
    match unsafe { gz_state(file) } {
        Some(state) => off_t(gz::gzoffset(state)),
        None => -1,
    }
}

/// `gzoffset64` — 64-bit-offset form of [`gzoffset`] (zlib.h L1981).
///
/// # Safety
///
/// `file` must be NULL or a live `gzFile`.
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzoffset64(file: gzFile) -> z_off64_t {
    // SAFETY: `file` is NULL or a live handle per the caller.
    match unsafe { gz_state(file) } {
        Some(state) => gz::gzoffset64(state),
        None => -1,
    }
}

/// `gzeof` — non-zero once the read end-of-file has been reached (zlib.h L1711).
/// Returns 0 on a NULL handle.
///
/// # Safety
///
/// `file` must be NULL or a live `gzFile`.
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzeof(file: gzFile) -> c_int {
    // SAFETY: `file` is NULL or a live handle per the caller.
    match unsafe { gz_state(file) } {
        Some(state) => i32::from(gz::gzeof(state)),
        None => 0,
    }
}

/// `gzdirect` — non-zero if the stream is being read verbatim (not gzip)
/// (zlib.h L1726).  Returns 0 on a NULL handle.
///
/// # Safety
///
/// `file` must be NULL or a live `gzFile`.
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzdirect(file: gzFile) -> c_int {
    // SAFETY: `file` is NULL or a live handle per the caller.
    match unsafe { gz_state(file) } {
        Some(state) => gz::gzdirect(state),
        None => 0,
    }
}

/// `gzerror` — return the last error message and code for the handle
/// (zlib.h L1775).  Writes `*errnum` (exact) when non-NULL and returns the
/// canonical `'static` message text for that code (see the module-level
/// `gzerror` divergence note).  Returns NULL on a NULL handle.
///
/// # Safety
///
/// `file` must be NULL or a live `gzFile`; `errnum` must be NULL or point to a
/// writable `c_int`.
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzerror(file: gzFile, errnum: *mut c_int) -> *const c_char {
    // SAFETY: `file` is NULL or a live handle per the caller.
    let Some(state) = (unsafe { gz_state(file) }) else {
        return ptr::null();
    };
    let (err, _msg) = gz::gzerror(state);
    if !errnum.is_null() {
        // SAFETY: `errnum` is non-NULL and points to a writable `c_int`.
        unsafe {
            *errnum = err;
        }
    }
    gz_strerror(err)
}

/// `gzclearerr` — clear the error and end-of-file state for the handle
/// (zlib.h L1792).  No-op on a NULL handle.
///
/// # Safety
///
/// `file` must be NULL or a live `gzFile`.
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzclearerr(file: gzFile) {
    // SAFETY: `file` is NULL or a live handle per the caller.
    if let Some(state) = unsafe { gz_state(file) } {
        gz::gzclearerr(state);
    }
}
