//! # `zlib-rs` — a safe-Rust rewrite of zlib
//!
//! `zlib-rs` (import path `zlib_rs`) is a memory-safe Rust reimplementation of
//! the [zlib](https://zlib.net/) general-purpose lossless data-compression
//! library. It reproduces zlib's externally observable behaviour — the DEFLATE
//! (RFC 1951), zlib (RFC 1950) and gzip (RFC 1952) stream framings, the
//! Adler-32 and CRC-32 checksums, and the full compression-level / strategy /
//! flush-mode surface — while replacing C's manual memory management with Rust
//! ownership semantics. The core compression and decompression logic is 100%
//! safe Rust; `unsafe` is confined to the inflate fast-path inner loop and the
//! C-ABI FFI boundary.
//!
//! ## Crate layout
//!
//! The crate is organised as a small module hierarchy that mirrors the C
//! translation units it was ported from:
//!
//! | Module | Responsibility | C source |
//! |--------|----------------|----------|
//! | [`constants`] | All `Z_*` constants and the [`FlushMode`]/[`Strategy`]/[`DataType`] enums | `zlib.h` |
//! | [`error`] | [`ZlibError`] / [`ReturnCode`] and the `z_errmsg` table | `zutil.h` |
//! | [`stream`] | [`ZStream`] and the [`Allocator`] abstraction | `zlib.h` |
//! | [`gz_header`] | [`GzHeader`] gzip metadata (gated by `gzip`) | `zlib.h` |
//! | [`checksum`] | [`adler32`] / [`crc32`] families | `adler32.c`, `crc32.c` |
//! | [`inflate`] | The DEFLATE decompression engine | `inflate.c` and friends |
//! | [`deflate`] | The DEFLATE compression engine (foundational preview) | `deflate.c` and friends |
//! | [`util`] | Version reporting and one-shot helpers | `zutil.c` |
//!
//! ## Cargo features
//!
//! The C preprocessor conditionals are re-expressed as Cargo features:
//!
//! * `std` *(default)* — heap plus `std::io`; the default C build.
//! * `gzip` *(default)* — the gzip wrapper format (`#ifdef GZIP`).
//! * `gz-io` *(default)* — the gzip FILE-I/O layer; implies `std` + `gzip`.
//! * `simd` *(default)* — SIMD-accelerated CRC-32 via `crc32fast`.
//! * `no-std` — the bare-metal `Z_SOLO` build; **mutually exclusive** with
//!   `std` (enforced by the [`compile_error!`] guard below). Requires `alloc`.
//! * `capi` — gates the `#[no_mangle] extern "C"` drop-in C-ABI exports
//!   (`src/ffi.rs`). Enabled for the `cdylib`/`staticlib` artifacts that act as
//!   a libz replacement; left off for `cargo test` so the canonical C symbol
//!   names do not collide with the `flate2` dev-dependency's bundled C zlib.
//!
//! ## Quick start
//!
//! ```
//! use zlib_rs::{adler32, crc32};
//!
//! // The checksum engines are bit-for-bit compatible with C zlib.
//! assert_eq!(adler32(1, b"123456789"), 0x091e_01de);
//! assert_eq!(crc32(0, b"123456789"), 0xcbf4_3926);
//! ```

// Under every build except the default `std` build the crate is `#![no_std]`.
// The `no-std` feature (the `Z_SOLO` analogue) and a bare `--no-default-features`
// build both omit `std`, so the attribute keys off the *absence* of `std`.
#![cfg_attr(not(feature = "std"), no_std)]
// Deny a few correctness-critical lints crate-wide. Byte-identical output is the
// project's central contract, so accidental truncation/wrap lints are surfaced.
#![forbid(unsafe_op_in_unsafe_fn)]

// `Vec`, `Box` and `String` are used throughout the engines in BOTH the `std`
// and `no-std` builds, so the `alloc` crate is always brought into scope. Under
// `std` the `alloc` items are also re-exported by `std`, but declaring the
// crate explicitly keeps the `alloc::` paths in the engine modules valid in
// every configuration.
extern crate alloc;

// `std` and `no-std` describe opposite worlds (hosted vs. `Z_SOLO`). Cargo
// cannot express mutual exclusion in the manifest, so the invariant is enforced
// here: enabling both at once is a hard compile error rather than a silently
// inconsistent build.
#[cfg(all(feature = "std", feature = "no-std"))]
compile_error!(
    "the `std` and `no-std` features are mutually exclusive; enable exactly one \
     (omit `no-std` for a hosted build, or use `--no-default-features --features no-std` \
     for the Z_SOLO build)"
);

// ===========================================================================
// Module declarations
// ===========================================================================

pub mod constants;
pub mod error;
pub mod stream;

// The gzip header type and its parser/emitter are only meaningful when the
// `gzip` wrapper format is compiled in, mirroring the C `#ifdef GUNZIP` guard.
#[cfg(feature = "gzip")]
pub mod gz_header;

pub mod checksum;
pub mod inflate;

// The DEFLATE compression engine. The module root (`deflate::mod`) provides the
// `deflate()` driver, the full public `deflate*` API, and the idiomatic
// [`Deflate`] wrapper over the persistent `DeflateState`, the Huffman `trees`
// builder, the per-level `strategy` table, and the five strategy routines.
// A handful of state/tree helpers exist purely to back the forthcoming
// `src/ffi.rs` C-ABI shim (e.g. raw-status validation, MSB byte emission) and
// are not yet reached from a live Rust path; `dead_code` is therefore allowed
// *only* for this subtree until the FFI shim wires them in.
#[allow(dead_code)]
pub mod deflate;

pub mod util;

// The gzip file-I/O layer: the `gz*` API (`gzopen`/`gzread`/`gzwrite`/`gzclose`
// and friends) layered over an owned `std::fs::File` and the inflate/deflate
// engines. Gated by `gz-io` (which implies `std` + `gzip`), mirroring the C
// `#ifndef NO_GZCOMPRESS` / `NO_GZIP` conditional. As with `deflate`, a handful
// of cross-module entry points (e.g. the read-side `gzclose_r`, reused by the
// `close.rs` dispatcher) are not yet reached from a live path until every gz
// submodule lands, so `dead_code` is allowed for this subtree until the full
// `gz` API and the FFI shim wire them in.
#[cfg(feature = "gz-io")]
#[allow(dead_code)]
pub mod gz;

// ===========================================================================
// Public re-exports — the idiomatic crate-root surface
// ===========================================================================

// Checksums: the six canonical entry points plus their 64-bit `*_combine`
// variants, re-exported so callers can write `zlib_rs::crc32(..)` directly.
pub use checksum::{
    adler32, adler32_combine, adler32_combine64, adler32_z, crc32, crc32_combine, crc32_combine64,
    crc32_z,
};

// Error handling: the idiomatic [`ZlibError`] and the C-compatible
// [`ReturnCode`] enumeration.
pub use error::{ReturnCode, ZlibError};

// The gzip metadata container (only when the `gzip` feature is enabled).
#[cfg(feature = "gzip")]
pub use gz_header::GzHeader;

// Streaming primitives: the `z_stream` analogue and its allocator abstraction.
pub use stream::{Allocator, GlobalAlloc, ZStream};

// The idiomatic decompressor.
pub use inflate::Inflate;

// The idiomatic compressor and its per-call outcome.
pub use deflate::{Deflate, DeflateOutcome};

// The one-shot whole-buffer helpers from the `util` layer, lifted to the crate
// root so callers can write `zlib_rs::compress(..)` / `zlib_rs::uncompress(..)`
// directly, mirroring the C `compress` / `compress2` / `compressBound` /
// `uncompress` / `uncompress2` entry points (`compress.c`, `uncompr.c`).
pub use util::{compress, compress_bound, compress2, uncompress, uncompress2};

// All `Z_*` constants and the typed [`FlushMode`]/[`Strategy`]/[`DataType`]
// enumerations are part of the public surface.
pub use constants::*;
