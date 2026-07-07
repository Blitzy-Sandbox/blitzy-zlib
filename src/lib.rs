//! # `zlib-rs` — a safe-Rust reimplementation of zlib
//!
//! `zlib-rs` is an idiomatic, memory-safe Rust port of zlib `1.3.2.1-motley`. It
//! reproduces the DEFLATE (RFC 1951), zlib (RFC 1950), and gzip (RFC 1952)
//! formats with **byte-for-byte** output compatibility with the reference C
//! library, and exposes a C-compatible FFI so it can drop in for `libz`.
//!
//! ## Architecture
//!
//! The crate mirrors the six functional layers of the C baseline as modules,
//! adds a public-API-types layer, and isolates all `unsafe` behind an FFI
//! boundary:
//!
//! * [`constants`] — flush modes, compression levels, strategies, `windowBits`.
//! * [`error`] — the [`ReturnCode`] / [`ZlibError`] error model.
//! * [`stream`] — the idiomatic [`ZStream`] and its [`Allocator`] abstraction.
//! * [`gz_header`] — the idiomatic [`GzHeader`].
//! * [`checksum`] — Adler-32 and CRC-32.
//! * [`deflate`] — the compression engine (zero `unsafe`).
//! * [`inflate`] — the decompression engine, including `inflateBack`.
//! * [`gz`] — the gzip file-I/O layer (Cargo feature `gz-io`).
//! * [`util`] — one-call wrappers and version reporting.
//! * [`ffi`] — the `#[unsafe(no_mangle)] extern "C"` drop-in boundary; the crate's
//!   sole `unsafe` region. It is declared **unconditionally** so the C symbol
//!   table is always emitted for linkage.
//!
//! ## `no_std`
//!
//! The crate is `#![no_std]` + `alloc` by default and links `std` only when the
//! `std` feature is enabled (the default). The `gz` file-I/O layer requires the
//! filesystem and is therefore compiled only under the `gz-io` feature (which
//! implies `std` and `gzip`).
//!
//! ## Feature flags
//!
//! | Feature      | Default | Effect                                                        |
//! |--------------|:-------:|---------------------------------------------------------------|
//! | `std`        |   yes   | Standard-library build (I/O, `catch_unwind` panic guards).    |
//! | `gzip`       |   yes   | gzip framing within deflate/inflate (C `#ifdef GZIP`).        |
//! | `gz-io`      |   yes   | gzip file-I/O `gz*` API (implies `std` + `gzip`).             |
//! | `simd`       |   yes   | SIMD-accelerated CRC-32 via `crc32fast`.                      |
//! | `c-variadic` |   no    | Variadic `gzprintf`/`gzvprintf` shims (requires nightly).     |

// The core engines are `no_std` + `alloc`; `std` is linked only under the `std`
// feature. The FFI layer references `alloc` (`Box`) for opaque state handles, so
// `alloc` is always available crate-wide.
#![cfg_attr(not(feature = "std"), no_std)]
// The variadic gz shims (`gzprintf`/`gzvprintf`) need the unstable `c_variadic`
// language feature; enabling the `c-variadic` Cargo feature therefore also
// requires a nightly toolchain. On stable (the default) this attribute is inert.
#![cfg_attr(feature = "c-variadic", feature(c_variadic))]

extern crate alloc;

// ===========================================================================
// Version macros (<- zlib.h L44-L49)
// ===========================================================================

/// zlib version string, mirroring `ZLIB_VERSION` from `zlib.h`. This is the
/// crate-wide single source of truth consumed by [`util::version`] and the FFI
/// `zlibVersion` shim.
pub const ZLIB_VERSION: &str = "1.3.2.1-motley";
/// zlib version number, mirroring `ZLIB_VERNUM` from `zlib.h` (`0x1321`).
pub const ZLIB_VERNUM: i32 = 0x1321;
/// zlib major version, mirroring `ZLIB_VER_MAJOR`.
pub const ZLIB_VER_MAJOR: i32 = 1;
/// zlib minor version, mirroring `ZLIB_VER_MINOR`.
pub const ZLIB_VER_MINOR: i32 = 3;
/// zlib revision, mirroring `ZLIB_VER_REVISION`.
pub const ZLIB_VER_REVISION: i32 = 2;
/// zlib sub-revision, mirroring `ZLIB_VER_SUBREVISION`.
pub const ZLIB_VER_SUBREVISION: i32 = 1;

// ===========================================================================
// Module declarations — the six C layers + public-API-types + FFI boundary
// ===========================================================================

pub mod checksum;
pub mod constants;
pub mod deflate;
pub mod error;
pub mod gz_header;
pub mod inflate;
pub mod stream;
pub mod util;

// The gzip file-I/O layer fundamentally requires `std` (filesystem); compile it
// only when `gz-io` is enabled. This matches the module-level `#![cfg(feature =
// "gz-io")]` self-gate in `src/gz/**` and `src/ffi/gz.rs`.
#[cfg(feature = "gz-io")]
pub mod gz;

// The FFI drop-in boundary is declared UNCONDITIONALLY so the emitted
// `cdylib`/`staticlib` always presents the full zlib C symbol table.
pub mod ffi;

// ===========================================================================
// Idiomatic prelude — curated public re-exports
// ===========================================================================

pub use error::{ReturnCode, ZlibError};
pub use gz_header::GzHeader;
pub use stream::{Allocator, ZStream};
