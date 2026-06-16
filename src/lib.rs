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
//! # One-shot compression
//!
//! The [`compress`] / [`uncompress`] helpers round-trip a whole buffer through
//! the zlib (RFC 1950) wrapper format in a single call. `compress` emits a
//! stream byte-identical to C zlib at the default level; `uncompress` decodes
//! it back into a caller-provided buffer (sized to the known original length):
//!
//! ```
//! use zlib_rs::{compress, uncompress};
//!
//! let original = b"hello, hello, hello, world!";
//! let packed = compress(original).unwrap();
//!
//! let mut restored = [0u8; 64];
//! let n = uncompress(&mut restored, &packed).unwrap();
//! assert_eq!(&restored[..n], original);
//! ```
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

// `no_std` is opt-in via the explicit `no-std` feature (the `Z_SOLO`-equivalent
// build, AAP §0.5.2), not merely the absence of `std`. This crate relies only
// on `core` and `alloc` in that mode. Two deliberate refinements:
//
//   * `not(test)` — under `cargo test` the crate always links `std` even when
//     `no-std` is enabled, because the libtest harness (and integration tests,
//     doctests, and benches that link this crate as a non-test dependency) is
//     built with `panic = "unwind"`, which `no_std` cannot support ("unwinding
//     panics are not supported without std"). Gating on `not(test)` lets the
//     unit-test modules use the `std` prelude (`Vec`, `vec!`, `format!`) and
//     keeps the harness linkable.
//   * Triggering on `feature = "no-std"` (rather than `not(feature = "std")`)
//     means a bare `--no-default-features` build is a reduced **std-linked**
//     build: its `cdylib`/`staticlib` inherit `std`'s allocator and panic
//     handler, and `cargo test --no-default-features` / `cargo check
//     --all-targets --no-default-features` compile cleanly. The genuine
//     bare-metal artifact is `--no-default-features --features no-std`, which
//     supplies its own runtime (see `crate::ffi::no_std_runtime`).
#![cfg_attr(all(feature = "no-std", not(test)), no_std)]

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
// `capi`-gated `extern "C"` FFI shim (`crate::ffi`) rather than by the idiomatic
// wrapper, so in the default build — where `capi` is off and the shim is not
// compiled — they have no in-crate caller. The scoped `#[allow(dead_code)]`
// keeps them from tripping the strict `-D warnings` policy in that
// configuration. The attribute on the module declaration propagates to the
// whole `deflate` subtree.
#[allow(dead_code)]
pub mod deflate;

// The gzip FILE-I/O layer (`gzopen`/`gzread`/`gzwrite`/`gzclose`, ...). Gated by
// the `gz-io` feature (implies `std` + `gzip`), mirroring the C
// `#ifndef NO_GZCOMPRESS` / `NO_GZIP` build. The `state` foundation (`GzState` +
// the `Mode`/`How` sentinels + RAII `Drop`) and the `open`/`read`/`write`/
// `close` submodules are all present; their crate-internal API is consumed by
// the `capi`-gated `extern "C"` FFI shim (`crate::ffi`) rather than by any
// in-crate caller, so in the default build — where `capi` is off and the shim
// is not compiled — the scoped `#[allow(dead_code)]` keeps them from tripping
// the strict `-D warnings` policy (the same rationale as the `deflate` engine
// root above). The attribute propagates to the whole `gz` subtree.
#[cfg(feature = "gz-io")]
#[allow(dead_code)]
pub mod gz;

// The C-ABI drop-in shim: `#[no_mangle] extern "C"` functions whose names and
// signatures match the zlib C API exactly, operating over `#[repr(C)]`
// structures and delegating to the safe engines above. This is the crate's
// designated `unsafe` FFI boundary (AAP §0.6.2); the compression engines remain
// 100% safe Rust. The module compiles under either of two configurations
// (the inner `#![cfg(...)]` in `ffi.rs` matches this gate exactly):
//   * `capi` — the C-ABI drop-in. Kept *non-default* so the canonical zlib
//     symbol names (`deflate`, `inflate`, `crc32`, ...) are NOT linked during
//     `cargo test`, where the development-only `flate2` oracle pulls in
//     canonical C zlib and would otherwise collide (duplicate symbols). The
//     cdylib/staticlib drop-in build enables `capi` explicitly.
//   * `all(feature = "no-std", not(test))` — the genuine bare-metal build,
//     whose `#![no_std]` artifact has no standard library and so needs the
//     `no_std_runtime` (`#[global_allocator]` + `#[panic_handler]`) that lives
//     in `ffi.rs`. Placing that C-runtime plumbing in the FFI boundary (rather
//     than here in the crate root) is what keeps `lib.rs` — and every engine
//     module — free of `unsafe`. The `not(test)` clause keeps the no-std build
//     `std`-linked under `cargo test`, leaving `capi` as the only in-test
//     trigger.
#[cfg(any(feature = "capi", all(feature = "no-std", not(test))))]
pub mod ffi;

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

// The gzip FILE-I/O layer's idiomatic handles: `GzFile` (the owned
// `Box<GzState>` produced by the `gzopen`-family entry points, implementing
// `Read`/`BufRead`) and `GzWriter` (the streaming `Write` adapter). The full
// C-faithful `gz*` free-function family (`gzopen`, `gzread`, `gzwrite`,
// `gzclose`, …) stays reachable under the `zlib_rs::gz` module path — the form
// the README documents. Gated by `gz-io` (which implies `std` + `gzip`),
// mirroring the C `#ifndef NO_GZCOMPRESS` / `NO_GZIP` build.
#[cfg(feature = "gz-io")]
pub use gz::{GzFile, GzWriter};

// One-shot whole-buffer helpers (`compress.c` / `uncompr.c`): the convenience
// API the README documents as `use zlib_rs::{compress, uncompress};`.
// `compress`/`compress2` emit a complete zlib stream, `compress_bound` sizes a
// destination buffer up front, and `uncompress`/`uncompress2` decode a stream
// back. The version/diagnostic helpers (`zutil.c`) round out the flat utility
// surface; `zlib_version()` backs the C-ABI `zlibVersion()` shim.
pub use util::{
    compress, compress_bound, compress2, uncompress, uncompress2, zlib_version, zlib_version_num,
};

// ---------------------------------------------------------------------------
// Crate-level version constants
// ---------------------------------------------------------------------------

/// The zlib version string this crate reproduces, `"1.3.2.1-motley"` — a
/// convenience alias for [`ZLIB_VERSION`] (re-exported from [`constants`]).
/// Both resolve to the same value; [`zlib_version`] returns it at run time for
/// the C-ABI `zlibVersion()` entry point. The numeric form is [`ZLIB_VERNUM`]
/// (`0x1321`).
pub const VERSION: &str = constants::ZLIB_VERSION;
