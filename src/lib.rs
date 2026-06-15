//! `zlib-rs` — a memory-safe Rust rewrite of the zlib 1.3.2.1 general-purpose
//! lossless data-compression library.
//!
//! This crate ports zlib's DEFLATE/INFLATE engines, the Adler-32 and CRC-32
//! checksum engines, the gzip file-I/O layer, and the shared utilities into
//! idiomatic, memory-safe Rust while preserving zlib's externally observable
//! behavior: it produces output that is **byte-identical** to C zlib for the
//! same input, level, strategy, and window configuration, and it implements
//! RFC 1950 (zlib), RFC 1951 (DEFLATE), and RFC 1952 (gzip).
//!
//! Manual memory management is replaced by Rust ownership — working buffers are
//! owned `Vec`/`Box`, engine teardown is handled by `Drop` (no `deflateEnd`/
//! `inflateEnd` leaks), and the LZ77/Huffman core is 100% safe Rust.
//!
//! # Checksums
//!
//! The checksum engines are bit-for-bit compatible with C zlib:
//!
//! ```
//! use zlib_rs::{adler32, crc32};
//!
//! // Adler-32 is seeded with 1; CRC-32 is seeded with 0.
//! assert_eq!(adler32(1, b"123456789"), 0x091e_01de);
//! assert_eq!(crc32(0, b"123456789"), 0xcbf4_3926);
//! ```
//!
//! # Cargo features
//!
//! Cargo features replace zlib's C preprocessor conditionals:
//!
//! | Feature  | Default | Replaces (C)            |
//! |----------|:-------:|-------------------------|
//! | `std`    | yes     | the default C build     |
//! | `gzip`   | yes     | `#ifdef GZIP`           |
//! | `gz-io`  | yes     | `#ifndef NO_GZCOMPRESS` |
//! | `no-std` | no      | `Z_SOLO`                |
//! | `simd`   | yes     | hardware CRC-32 paths   |
//! | `capi`   | no      | the C-ABI drop-in build |
//!
//! `std` and `no-std` are mutually exclusive (a `compile_error!` guard below
//! enforces this). The non-default `capi` feature gates the `#[no_mangle]
//! extern "C"` C-ABI exports so they do not collide with the development-only
//! `flate2` C-zlib oracle during `cargo test`.

// Under any build that does not enable `std`, the crate is `no_std`: it relies
// only on `core` and `alloc`. The default build keeps `std` for `std::io`
// integration and the gzip file-I/O layer.
#![cfg_attr(not(feature = "std"), no_std)]

// The compression engines are heap-backed (owned `Vec`/`Box` working buffers)
// even in the `no_std` configuration, so the `alloc` crate is always required.
// Bringing it into scope here is what lets the engine modules write
// `use alloc::{vec::Vec, boxed::Box};` under `#[cfg(not(feature = "std"))]`.
extern crate alloc;

// ---------------------------------------------------------------------------
// Feature mutual-exclusivity guards (replace impossible C #ifdef combinations)
// ---------------------------------------------------------------------------
// `std` and `no-std` describe opposite build profiles. Cargo features are
// additive and cannot be hard-declared mutually exclusive, so enabling both is
// caught here with a clear message (AAP folder spec / Cargo.toml note). The
// `--no-default-features` build (neither `std` nor `no-std`) is intentionally
// allowed: absence of `std` simply selects the bare-metal `core`/`alloc` path.
#[cfg(all(feature = "std", feature = "no-std"))]
compile_error!(
    "features `std` and `no-std` are mutually exclusive — enable exactly one \
     (use `--no-default-features --features no-std` for the bare-metal build)"
);

// ---------------------------------------------------------------------------
// no_std standalone-artifact runtime (global allocator + panic handler)
// ---------------------------------------------------------------------------
// The crate is built as `crate-type = ["lib", "cdylib", "staticlib"]` — the
// C-linkable drop-in for `libz` (AAP §0.3.1 / §0.5.1). Under a `no_std` profile
// the `cdylib`/`staticlib` are *final* link products that, unlike the `rlib`,
// must carry a global allocator and a panic handler. `std` builds inherit both
// from the standard library; the bare-metal profile has no such defaults, so
// the standalone drop-in supplies them here, routed to the hosted C runtime
// that links the artifact.
//
// These items are confined to the `not(feature = "std")` build and are NOT
// compiled for `std` builds or under `cfg(test)`, where the standard library /
// test harness already provide them. They are runtime plumbing for the C
// drop-in — NOT part of the compression engines — so the "zero unsafe in core
// compression logic" constraint (AAP §0.6.2) is preserved; every `unsafe` here
// carries a `// SAFETY:` justification. (A future `capi`/`c-runtime` feature
// could gate these so bare-metal embedders of the `rlib` may supply their own.)
#[cfg(all(not(feature = "std"), not(test)))]
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

// ---------------------------------------------------------------------------
// Module tree (the six-layer architecture, AAP §0.3.1)
// ---------------------------------------------------------------------------

pub mod constants;
pub mod error;
pub mod stream;

// Gzip header parsing/marshalling is only meaningful when the gzip wrapper
// format is enabled.
#[cfg(feature = "gzip")]
pub mod gz_header;

pub mod checksum;
pub mod inflate;
pub mod util;

// The deflate engine: the `state`/`trees`/`strategy` foundation, the five
// per-level strategy modules, and the public `deflate*` orchestration plus the
// idiomatic `Deflate` wrapper (re-exported below). A few C-API entry points
// (`deflate_init`, `deflate_copy`, `deflate_end`) are consumed only by the
// (not-yet-present) `extern "C"` FFI shim rather than the idiomatic wrapper, so
// the scoped `#[allow(dead_code)]` keeps them from tripping the strict
// `-D warnings` policy until that shim lands. The attribute on the module
// declaration propagates to the whole `deflate` subtree.
#[allow(dead_code)]
pub mod deflate;

// ---------------------------------------------------------------------------
// Public re-exports — the flat API surface (mirrors zlib.h's namespace)
// ---------------------------------------------------------------------------

// All `Z_*` constants (flush/level/strategy/return-code/data-type) plus the
// version constants and the idiomatic `FlushMode`/`Strategy`/`DataType` enum
// bridges.
pub use constants::*;

// Error handling: the idiomatic error enum, the non-negative return-code enum,
// and the crate-wide `Result` alias (`Result<T> = core::result::Result<T,
// ZlibError>`).
pub use error::{Result, ReturnCode, ZlibError};

// The owning stream handle and the allocator abstraction that preserves
// zlib's `zalloc`/`zfree` hooks at the FFI boundary.
pub use stream::{Allocator, GlobalAlloc, ZStream};

// The owned, safe gzip header representation.
#[cfg(feature = "gzip")]
pub use gz_header::GzHeader;

// Checksums (Adler-32 / CRC-32), including the slice-length and combine
// variants that back the stream trailers.
pub use checksum::{
    adler32, adler32_combine, adler32_combine64, adler32_z, crc32, crc32_combine, crc32_combine64,
    crc32_z,
};

// The idiomatic streaming compressor and decompressor.
pub use deflate::{Deflate, DeflateOutcome};
pub use inflate::{Inflate, InflateOutcome};

// Version/diagnostic helpers.
pub use util::version::{zlib_version, zlib_version_num};
