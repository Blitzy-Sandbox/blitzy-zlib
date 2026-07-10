//! # `zlib-rs` — a safe-Rust reimplementation of zlib
//!
//! `zlib-rs` is an idiomatic, memory-safe Rust port of zlib `1.3.2.1-motley`
//! ([`ZLIB_VERNUM`] `0x1321`). It reproduces the DEFLATE (RFC 1951), zlib
//! (RFC 1950), and gzip (RFC 1952) stream formats with **byte-for-byte** output
//! compatibility with the reference C library, and exposes a C-compatible FFI so
//! the emitted `cdylib`/`staticlib` (`libzlib_rs.so` / `libzlib_rs.a`) can drop
//! in for the system `libz`.
//!
//! ## Safe core, unsafe boundary
//!
//! The crate is organized as a **safe core** wrapped by a thin **unsafe
//! boundary**. Every compression, decompression, checksum, and one-call engine
//! is written in fully safe Rust — the deflate engine ([`deflate`]) contains
//! **zero** `unsafe` — while all C-ABI raw-pointer handling is confined to the
//! [`ffi`] module (and, at most, the inflate fast-path decode loop). The manual
//! `zcalloc`/`zcfree` memory management of the C sources is replaced by Rust
//! ownership: a [`ZStream`] owns its boxed engine state and releases it in
//! [`Drop`], so the "uninitialized-state pointer" hazards of the C API become
//! unrepresentable.
//!
//! ## Architecture
//!
//! The crate mirrors the six functional layers of the C baseline as modules,
//! adds a public-API-types layer, and isolates the C ABI behind [`ffi`]:
//!
//! | Module        | Ported from (C)                           | Responsibility                              |
//! |---------------|-------------------------------------------|---------------------------------------------|
//! | [`error`]     | `zutil.h`                                 | [`ReturnCode`] / [`ZlibError`] error model  |
//! | [`constants`] | `zlib.h`                                  | flush modes, levels, strategies, wrap modes |
//! | [`stream`]    | `zlib.h`                                  | idiomatic [`ZStream`] + [`Allocator`]       |
//! | [`gz_header`] | `zlib.h`                                  | idiomatic [`GzHeader`]                      |
//! | [`checksum`]  | `adler32.c`, `crc32.c`                    | Adler-32 and CRC-32                         |
//! | [`deflate`]   | `deflate.c`, `trees.c`                    | compression engine (zero `unsafe`)          |
//! | [`inflate`]   | `inflate.c`, `inffast.c`, `infback.c`, …  | decompression engine incl. `inflateBack`    |
//! | [`util`]      | `compress.c`, `uncompr.c`, `zutil.c`      | one-call wrappers and version reporting     |
//! | [`gz`]        | `gzlib.c`, `gzread.c`, `gzwrite.c`, …     | gzip file-I/O (Cargo feature `gz-io`)       |
//! | [`ffi`]       | `zlib.h`, `zconf.h`, `zlib.map`           | `extern "C"` drop-in boundary               |
//!
//! ## `no_std`
//!
//! The crate is `#![no_std]` + [`alloc`] whenever the `std` feature is disabled,
//! so the compression core can be embedded in freestanding environments. The
//! `std` feature (enabled by default) links the standard library for the gz
//! file-I/O layer ([`std::fs`]/[`std::io`]) and for `catch_unwind` panic guards
//! at the FFI boundary. Core modules use [`alloc`] types ([`alloc::boxed::Box`],
//! [`alloc::vec::Vec`], [`alloc::string::String`]) rather than their `std`
//! re-exports, so the whole crate participates in `no_std` builds.
//!
//! ## Feature flags
//!
//! | Feature          | Default | Effect                                                       |
//! |------------------|:-------:|--------------------------------------------------------------|
//! | `std`            |   yes   | Standard-library build (file I/O, `catch_unwind` guards).    |
//! | `gzip`           |   yes   | gzip framing within deflate/inflate (mirrors C `#ifdef GZIP`). |
//! | `gz-io`          |   yes   | gzip file-I/O `gz*` API; implies `std` + `gzip`.             |
//! | `simd`           |   yes   | SIMD-accelerated CRC-32 via the `crc32fast` crate.          |
//! | `inflate_strict` |   no    | Stricter inflate distance validation (mirrors C `INFLATE_STRICT`). |
//!
//! Building with `--no-default-features` yields the `no_std` compression and
//! decompression core without the gz file-I/O layer (the [`gz`] module is gated
//! behind `gz-io`, which implies `std`).
//!
//! ## Quick start
//!
//! ```
//! use zlib_rs::{compress2, compress_bound, uncompress, Z_BEST_COMPRESSION};
//!
//! let source = b"the quick brown fox jumps over the lazy dog";
//!
//! // Size the destination with the same bound formula the C library uses.
//! let mut compressed = vec![0u8; compress_bound(source.len())];
//! let produced = compress2(&mut compressed, source, Z_BEST_COMPRESSION).unwrap();
//! compressed.truncate(produced);
//!
//! // Decompress into a buffer sized to the known original length.
//! let mut restored = vec![0u8; source.len()];
//! let written = uncompress(&mut restored, &compressed).unwrap();
//! restored.truncate(written);
//!
//! assert_eq!(&restored[..], &source[..]);
//! ```

// The core engines are `no_std` + `alloc`; the standard library is linked only
// under the `std` feature (the default). Because the FFI layer and several
// engines allocate (`Box`/`Vec`), `alloc` is imported unconditionally below.
//
// `no_std` is applied only for a genuine freestanding build, identified by the
// conjunction of three stable predicates:
//   * `not(feature = "std")` — the default `std` feature is off.
//   * `panic = "abort"`      — the crate is compiled with the abort panic
//     strategy. On the STABLE toolchain (MSRV 1.85.0, no `-Z build-std`) a
//     `#![no_std]` crate CANNOT be codegen'd with `panic = "unwind"`: the
//     compiler rejects it with "unwinding panics are not supported without
//     std". `[profile.dev]`/`[profile.release]` set `panic = "abort"` precisely
//     so the no_std `cdylib`/`staticlib` link (`cargo build
//     --no-default-features`). But `cargo test` forces `panic = "unwind"`
//     (libtest needs `catch_unwind`) AND still builds every `crate-type` of the
//     lib target — including the `cdylib`/`staticlib`. Gating `no_std` on
//     `panic = "abort"` therefore keeps the crate `std`-linked under
//     `cargo test --no-default-features` (so those artifacts and the integration
//     tests / doctests build), while `cargo build --no-default-features` (abort)
//     still yields the real freestanding artifact. The `feature = "std"` code
//     gates stay OFF regardless, so `cargo test --no-default-features` genuinely
//     exercises the `no_std`-configured code paths. `cfg(panic = ...)` is stable
//     since Rust 1.60. See the `no_std_support` gate below (identical
//     predicate) and the `src/util/compress.rs` test module.
//   * `not(test)`            — belt-and-braces exclusion of the unit-test
//     harness (which links `std`); redundant given `panic = "abort"` on stable,
//     but documents intent and guards custom profiles.
#![cfg_attr(all(not(feature = "std"), not(test), panic = "abort"), no_std)]
// Every public item in the crate must be documented (AAP: "document all public
// items"). This is a `warn`, never a `deny`, so it can never break the build.
#![warn(missing_docs)]
// Every `unsafe` block in SHIPPED crate code must carry an immediately-adjacent
// `// SAFETY:` justification (AAP §0.7.2 / user rule R3). Like `missing_docs`
// this is a `warn` (never a `deny`) so it cannot break a plain build, but the
// CI `-D warnings` gate promotes it to an error for the production library
// target, preventing recurrence of the undocumented-`unsafe` finding across the
// FFI boundary. The lint is relaxed to `allow` under `cfg(test)` so it governs
// only the shipped `cdylib`/`staticlib`/`rlib` (whose `unsafe` lives in
// `src/ffi/**`), not the crate's inline `#[cfg(test)]` unit tests.
#![warn(clippy::undocumented_unsafe_blocks)]
#![cfg_attr(test, allow(clippy::undocumented_unsafe_blocks))]

// `Box`, `Vec`, and `String` are provided by `alloc` in both `std` and `no_std`
// builds. Importing the crate here makes those types available crate-wide
// without pulling in the whole standard library.
extern crate alloc;

// ===========================================================================
// `no_std` runtime support — global allocator + panic handler
//
// A `#![no_std]` crate that still allocates (this one uses `Box`/`Vec`/`String`
// via `alloc`) and is emitted as a `cdylib`/`staticlib` must SUPPLY its own
// `#[global_allocator]` and `#[panic_handler]`: the standard library normally
// provides both, and without `std` the final `cdylib`/`staticlib` link step
// fails with "no global memory allocator found" and "`#[panic_handler]`
// function required, but not found" (see QA finding on the `no-std` build).
//
// These items are compiled ONLY for a genuine freestanding library build —
// `#[cfg(all(not(feature = "std"), not(test), panic = "abort"))]`, the SAME
// predicate that applies `#![no_std]` above (see the detailed rationale there):
//   * `not(feature = "std")`  — under the default (`std`) build the standard
//     library already provides the global allocator and panic handler, and
//     redefining them here would be a duplicate-lang-item error. The whole std
//     surface therefore stays byte-for-byte unchanged.
//   * `panic = "abort"`       — under `cargo test` the crate is `std`-linked
//     (panic = "unwind"; see the crate attribute above), so `std` already
//     supplies the allocator and panic handler; defining them here would then
//     collide. This predicate keeps these items in lockstep with `#![no_std]`.
//   * `not(test)`             — the unit-test harness links `libtest`, which
//     pulls in `std`; excluding the items from test builds avoids clashing with
//     the std-provided ones.
//
// The allocator is a thin, faithful port of the standard library's Unix
// `System` allocator: the platform `malloc`/`calloc`/`realloc`/`free` satisfy
// the common (≤ `MIN_ALIGN`) case, and `posix_memalign` covers over-aligned
// requests. Routing through the C runtime's `malloc` family is exactly what a
// C consumer of the emitted `libzlib_rs.{so,a}` expects, and AAP §0.5.2
// explicitly permits the platform `libc` already linked by any hosted artifact
// (it introduces no *additional* C dependency and keeps the shipped Rust graph
// pure). Per-`z_stream` `zalloc`/`zfree` caller hooks remain a separate concern
// handled inside the FFI layer; this global allocator is only the fallback the
// `alloc` crate requires.
//
// SAFETY: this module is crate-root C-ABI plumbing, analogous to the FFI
// boundary carve-out in AAP §0.6.2 — it is NOT part of the compression core
// (`src/deflate/**` remains 100% `unsafe`-free, honoring the "zero unsafe in
// core compression logic" rule). Every `unsafe` operation is justified inline.
#[cfg(all(not(feature = "std"), not(test), panic = "abort"))]
mod no_std_support {
    use core::alloc::{GlobalAlloc, Layout};
    use core::ffi::c_void;

    // Minimum alignment guaranteed by the platform `malloc` family: 16 bytes on
    // 64-bit targets, 8 on 32-bit. This mirrors the constant the standard
    // library's Unix allocator uses.
    #[cfg(target_pointer_width = "64")]
    const MIN_ALIGN: usize = 16;
    #[cfg(not(target_pointer_width = "64"))]
    const MIN_ALIGN: usize = 8;

    // The C runtime allocation primitives. These are resolved against the
    // platform `libc` that every hosted `cdylib`/`staticlib` links against, so
    // declaring them here adds no new dependency to the shipped Rust graph.
    unsafe extern "C" {
        fn malloc(size: usize) -> *mut c_void;
        fn calloc(nmemb: usize, size: usize) -> *mut c_void;
        fn realloc(ptr: *mut c_void, size: usize) -> *mut c_void;
        fn free(ptr: *mut c_void);
        fn posix_memalign(memptr: *mut *mut c_void, align: usize, size: usize) -> i32;
        fn abort() -> !;
    }

    /// A `GlobalAlloc` implementation backed by the platform `libc` allocator.
    ///
    /// This is a faithful port of the standard library's Unix `System`
    /// allocator so that a `no_std` build behaves identically to the default
    /// `std` build with respect to heap allocation.
    struct LibcAllocator;

    /// Allocate an over-aligned block via `posix_memalign`.
    ///
    /// # Safety
    /// The caller must free the returned pointer (when non-null) with `free`,
    /// and must not request a zero-sized layout (the `GlobalAlloc` contract
    /// already guarantees `layout.size() > 0`).
    unsafe fn aligned_malloc(layout: Layout) -> *mut u8 {
        // `posix_memalign` requires the alignment to be a power of two and a
        // multiple of `size_of::<*mut c_void>()`; clamp up to that minimum.
        let align = layout.align().max(core::mem::size_of::<usize>());
        let mut out: *mut c_void = core::ptr::null_mut();
        // SAFETY: `out` is a valid pointer to a `*mut c_void` slot; `align` is a
        // power-of-two multiple of the pointer size as required.
        let ret = unsafe { posix_memalign(&mut out, align, layout.size()) };
        if ret != 0 {
            core::ptr::null_mut()
        } else {
            out as *mut u8
        }
    }

    // SAFETY: `LibcAllocator` forwards every request to the C runtime allocator,
    // which upholds the `GlobalAlloc` contract (returns suitably aligned blocks
    // or null, and `free`/`realloc` operate on pointers it previously handed
    // out). Over-aligned requests are routed through `posix_memalign`, whose
    // allocations are also freeable with `free`.
    unsafe impl GlobalAlloc for LibcAllocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            if layout.align() <= MIN_ALIGN && layout.align() <= layout.size() {
                // SAFETY: `malloc` returns a `MIN_ALIGN`-aligned block that
                // satisfies this layout, or null on failure.
                unsafe { malloc(layout.size()) as *mut u8 }
            } else {
                // SAFETY: over-aligned path; `aligned_malloc` honors the layout.
                unsafe { aligned_malloc(layout) }
            }
        }

        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            if layout.align() <= MIN_ALIGN && layout.align() <= layout.size() {
                // SAFETY: `calloc` returns zeroed, `MIN_ALIGN`-aligned memory.
                unsafe { calloc(layout.size(), 1) as *mut u8 }
            } else {
                // SAFETY: allocate over-aligned, then zero it explicitly.
                let ptr = unsafe { aligned_malloc(layout) };
                if !ptr.is_null() {
                    // SAFETY: `ptr` points to `layout.size()` writable bytes.
                    unsafe { core::ptr::write_bytes(ptr, 0, layout.size()) };
                }
                ptr
            }
        }

        unsafe fn dealloc(&self, ptr: *mut u8, _layout: Layout) {
            // SAFETY: `ptr` was returned by `alloc`/`alloc_zeroed`/`realloc`
            // above (all backed by the `malloc` family), so `free` is valid.
            unsafe { free(ptr as *mut c_void) };
        }

        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            if layout.align() <= MIN_ALIGN && layout.align() <= new_size {
                // SAFETY: the original block came from the `malloc` family with
                // `MIN_ALIGN` alignment, so `realloc` preserves the layout.
                unsafe { realloc(ptr as *mut c_void, new_size) as *mut u8 }
            } else {
                // Over-aligned: `realloc` cannot preserve the alignment, so
                // allocate a fresh aligned block, copy, and free the old one.
                // SAFETY: `new_size` and `layout.align()` form a valid layout.
                let new_layout =
                    unsafe { Layout::from_size_align_unchecked(new_size, layout.align()) };
                // SAFETY: delegates to the checked `alloc` above.
                let new_ptr = unsafe { self.alloc(new_layout) };
                if !new_ptr.is_null() {
                    let copy = core::cmp::min(layout.size(), new_size);
                    // SAFETY: both regions are valid for `copy` bytes and do not
                    // overlap (distinct allocations).
                    unsafe { core::ptr::copy_nonoverlapping(ptr, new_ptr, copy) };
                    // SAFETY: `ptr` is a live allocation from this allocator.
                    unsafe { free(ptr as *mut c_void) };
                }
                new_ptr
            }
        }
    }

    #[global_allocator]
    static GLOBAL: LibcAllocator = LibcAllocator;

    // In a `no_std` build there is no unwinding runtime (the crate is compiled
    // with `panic = "abort"`; see `Cargo.toml`), so the panic handler simply
    // terminates the process via the C runtime's `abort()`. This matches the
    // `panic = "abort"` strategy and upholds the crate-wide invariant that a
    // Rust panic never unwinds across the C ABI.
    #[panic_handler]
    fn panic(_info: &core::panic::PanicInfo) -> ! {
        // SAFETY: `abort` is the libc process-termination routine; it never
        // returns and has no preconditions.
        unsafe { abort() }
    }

    // Supply a `rust_eh_personality` symbol for the freestanding (`no_std`)
    // `cdylib`/`staticlib` build so the emitted `libzlib_rs.{so,a}` is
    // self-contained and C-linkable on hosted GNU targets — WITHOUT leaking a
    // non-zlib symbol into the exported C ABI.
    //
    // The precompiled sysroot `alloc` crate — which this crate depends on for
    // `Box`/`Vec`/`String` — emits a `DW.ref.rust_eh_personality` relocation
    // that references the language runtime's exception-handling *personality*
    // routine. In the default `std` build the standard library's unwinding
    // runtime is statically linked into the artifact and defines that symbol,
    // so the relocation resolves internally. A `--no-default-features`
    // (`no_std`) build links no `std`, which would otherwise leave
    // `rust_eh_personality` **undefined** in `libzlib_rs.{so,a}`; a downstream
    // C consumer would then fail to link with
    // `undefined reference to 'rust_eh_personality'`.
    //
    // Because both build profiles set `panic = "abort"` (see `Cargo.toml`),
    // stack unwinding never occurs and this personality routine is **never
    // invoked** — the relocation is spurious. Defining the symbol therefore
    // makes the freestanding artifact self-contained without altering any
    // runtime behavior, exactly mirroring why this module already supplies its
    // own `#[global_allocator]` and `#[panic_handler]` for the same build.
    //
    // The symbol is defined with **hidden** ELF visibility (`.hidden`) via
    // `global_asm!` rather than as a `#[unsafe(no_mangle)] pub extern "C"` fn.
    // A hidden *global* symbol still satisfies the internal `DW.ref.*`
    // relocation at static-link time, yet it is NOT placed in the artifact's
    // dynamic symbol table — so the exported surface stays exactly the zlib C
    // ABI and keeps byte-for-byte parity with the `zlib.map` version script
    // (AAP §0.6.1 / §0.7.1). A `#[unsafe(no_mangle)]` Rust fn, by contrast, is
    // forced into the cdylib export list and would export `rust_eh_personality`
    // as a public symbol; localizing it afterward with a version script then
    // conflicts with that export list and emits a linker diagnostic. The
    // `#[lang = "eh_personality"]` item is nightly-only, so the hidden symbol is
    // emitted by name in assembly instead. The routine is never called
    // (`panic = "abort"`); `ret` is a valid no-op return on the hosted GNU
    // architectures this freestanding artifact targets. Only architecture-
    // independent GNU-assembler directives are used (no `@`-prefixed operands,
    // which are comment markers on some targets), so this assembles portably.
    core::arch::global_asm!(
        ".globl rust_eh_personality",
        ".hidden rust_eh_personality",
        "rust_eh_personality:",
        "ret",
    );
}

// ===========================================================================
// Version macros (ported from `zlib.h` L44-L49)
//
// These mirror the C `ZLIB_VERSION`/`ZLIB_VERNUM` family exactly and are the
// crate-wide single source of truth. `util::version::zlib_version()` (and the
// FFI `zlibVersion` shim) return `ZLIB_VERSION`, so this string must stay
// byte-identical to the C `ZLIB_VERSION` macro for compatibility.
// ===========================================================================

/// zlib version string, mirroring the C `ZLIB_VERSION` macro.
///
/// This is the authoritative version reported by [`util::version::zlib_version`]
/// (and its FFI shim `zlibVersion`); it must remain byte-identical to the C
/// `ZLIB_VERSION` macro for drop-in compatibility.
pub const ZLIB_VERSION: &str = "1.3.2.1-motley";

/// zlib version number, mirroring the C `ZLIB_VERNUM` macro (`0x1321`).
///
/// The nibbles encode the version as `0xMNRS` (major, minor, revision,
/// sub-revision): `0x1321` is `1.3.2` sub-revision `1`.
pub const ZLIB_VERNUM: u32 = 0x1321;

/// zlib major version, mirroring the C `ZLIB_VER_MAJOR` macro.
pub const ZLIB_VER_MAJOR: u32 = 1;

/// zlib minor version, mirroring the C `ZLIB_VER_MINOR` macro.
pub const ZLIB_VER_MINOR: u32 = 3;

/// zlib revision, mirroring the C `ZLIB_VER_REVISION` macro.
pub const ZLIB_VER_REVISION: u32 = 2;

/// zlib sub-revision, mirroring the C `ZLIB_VER_SUBREVISION` macro.
pub const ZLIB_VER_SUBREVISION: u32 = 1;

// ===========================================================================
// Module declarations — the six C layers + public-API-types + FFI boundary
//
// `lib.rs` only DECLARES these modules; each is implemented in its own file.
// The graph mirrors the C `#include` layering exactly (AAP §0.3.1) with no
// extra top-level modules.
// ===========================================================================

pub mod checksum;
pub mod constants;
pub mod deflate;
pub mod error;
pub mod gz_header;
pub mod inflate;
pub mod stream;
pub mod util;

// The gzip file-I/O layer fundamentally requires the standard library
// (`std::fs`/`std::io`), so it is compiled only when the `gz-io` feature is
// enabled (which implies `std` + `gzip`). This matches the feature gating in
// `src/gz/**` and `src/ffi/gz.rs`; a bare `no_std` build omits it entirely.
#[cfg(feature = "gz-io")]
pub mod gz;

// The FFI drop-in boundary is declared UNCONDITIONALLY so the emitted
// `cdylib`/`staticlib` always presents the full zlib C symbol table for
// linkage. Any part of `ffi` that needs `std` is gated internally; the module
// declaration itself is never feature-gated.
pub mod ffi;

// ===========================================================================
// Idiomatic public API — curated crate-root re-exports
//
// These mirror the surface `zlib.h` exposes so that `use zlib_rs::{…}` is
// ergonomic. The FFI symbols are intentionally NOT re-exported here: the `ffi`
// module stands alone as the C ABI surface.
// ===========================================================================

// The error model: the exhaustive return-code enum and its error twin.
pub use error::{ReturnCode, ZlibError};

// Public-API enums plus the four compression-level constants callers most often
// reference when configuring the engine.
pub use constants::{
    DataType, FlushMode, Method, Strategy, WrapMode, Z_BEST_COMPRESSION, Z_BEST_SPEED,
    Z_DEFAULT_COMPRESSION, Z_NO_COMPRESSION,
};

// The idiomatic stream and its allocator abstraction (mirrors `z_stream`).
pub use stream::{Allocator, DefaultAllocator, ZStream};

// The idiomatic gzip header (mirrors `gz_header`).
pub use gz_header::GzHeader;

// One-call, whole-buffer wrappers (ported from `compress.c`/`uncompr.c`). Both
// the idiomatic snake_case names and the zlib-style `compressBound` alias are
// surfaced.
pub use util::{compress, compress_bound, compress2, compressBound, uncompress, uncompress2};

// Version, compile-flag, and error-string reporting (ported from `zutil.c`),
// each paired with its zlib-style camelCase alias.
pub use util::{z_error, zError, zlib_compile_flags, zlib_version, zlibCompileFlags, zlibVersion};

// Checksums (ported from `adler32.c`/`crc32.c`): the running checksum functions
// and their stream-combining companions.
pub use checksum::{adler32, adler32_combine, crc32, crc32_combine};

// ===========================================================================
// Prelude — grouped re-exports for `use zlib_rs::prelude::*;`
// ===========================================================================

/// Common imports for working with `zlib-rs`.
///
/// Bring the most frequently used types, traits, and enums into scope with a
/// single glob import:
///
/// ```
/// use zlib_rs::prelude::*;
///
/// // The re-exported types are now in scope.
/// let _stream: ZStream = ZStream::new();
/// let _header: GzHeader = GzHeader::new();
/// ```
///
/// The prelude intentionally excludes the free functions (`compress`,
/// `adler32`, …) and the C-ABI [`crate::ffi`] surface to avoid polluting the
/// caller's namespace; reach those through their crate paths
/// (`zlib_rs::compress`, `zlib_rs::ffi::…`).
pub mod prelude {
    pub use crate::constants::{DataType, FlushMode, Method, Strategy, WrapMode};
    pub use crate::error::{ReturnCode, ZlibError};
    pub use crate::gz_header::GzHeader;
    pub use crate::stream::{Allocator, DefaultAllocator, ZStream};
}

// ===========================================================================
// Sanity tests — version constants and re-export resolution
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// The version constants must equal the values from `zlib.h` L44-L49.
    #[test]
    fn version_constants_match_zlib_h() {
        assert_eq!(ZLIB_VERSION, "1.3.2.1-motley");
        assert_eq!(ZLIB_VERNUM, 0x1321);
        assert_eq!(ZLIB_VER_MAJOR, 1);
        assert_eq!(ZLIB_VER_MINOR, 3);
        assert_eq!(ZLIB_VER_REVISION, 2);
        assert_eq!(ZLIB_VER_SUBREVISION, 1);
    }

    /// The version reporting functions re-exported at the crate root must agree
    /// with the authoritative [`ZLIB_VERSION`] constant.
    #[test]
    fn version_functions_agree_with_constant() {
        assert_eq!(zlib_version(), ZLIB_VERSION);
        assert_eq!(zlibVersion(), ZLIB_VERSION);
    }

    /// The value-namespace re-exports (functions and constants) must resolve
    /// through the crate root and behave as their zlib counterparts do on
    /// trivial, algorithm-guaranteed inputs.
    #[test]
    fn value_reexports_resolve_and_work() {
        // Compression-level constants carry their canonical zlib values.
        assert_eq!(Z_NO_COMPRESSION, 0);
        assert_eq!(Z_BEST_SPEED, 1);
        assert_eq!(Z_BEST_COMPRESSION, 9);
        assert_eq!(Z_DEFAULT_COMPRESSION, -1);

        // The `compress_bound` alias and its snake_case form are the same fn.
        assert_eq!(compress_bound(0), compressBound(0));

        // Adler-32 seeded with 1 and CRC-32 seeded with 0 are unchanged by an
        // empty input — a property that holds for every conforming impl.
        assert_eq!(adler32(1, b""), 1);
        assert_eq!(crc32(0, b""), 0);

        // The combine companions resolve and are identity over a zero-length
        // second stream.
        assert_eq!(
            adler32_combine(adler32(1, b"abc"), 1, 0),
            adler32(1, b"abc")
        );
        assert_eq!(crc32_combine(crc32(0, b"abc"), 0, 0), crc32(0, b"abc"));

        // The compile-flags/error-string reporters resolve through the root.
        let _ = zlib_compile_flags();
        let _ = zlibCompileFlags();
        assert_eq!(z_error(0), zError(0));
    }

    /// The type-namespace re-exports (enums, structs, and the [`Allocator`]
    /// trait) must resolve through both the crate root and the [`prelude`].
    #[test]
    fn type_reexports_resolve() {
        // A no-bound generic proves each path names a real type.
        fn assert_type<T>() {}
        assert_type::<ReturnCode>();
        assert_type::<ZlibError>();
        assert_type::<FlushMode>();
        assert_type::<Strategy>();
        assert_type::<WrapMode>();
        assert_type::<DataType>();
        assert_type::<Method>();
        assert_type::<DefaultAllocator>();
        assert_type::<ZStream>();
        assert_type::<GzHeader>();

        // The same items must also resolve through the prelude.
        assert_type::<prelude::ReturnCode>();
        assert_type::<prelude::ZStream>();
        assert_type::<prelude::GzHeader>();

        // The `Allocator` trait is re-exported and usable as a bound, and the
        // default allocator satisfies it.
        fn needs_allocator<A: Allocator>() {}
        needs_allocator::<DefaultAllocator>();
    }
}
