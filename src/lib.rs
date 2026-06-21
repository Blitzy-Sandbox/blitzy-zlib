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
//! | [`deflate`] | The DEFLATE compression engine | `deflate.c` and friends |
//! | [`gz`] | The gzip `FILE`-I/O layer (gated by `gz-io`) | `gzlib.c`, `gzread.c`, `gzwrite.c`, `gzclose.c` |
//! | [`util`] | Version reporting and one-shot `compress`/`uncompress` helpers | `compress.c`, `uncompr.c`, `zutil.c` |
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
//! One-shot whole-buffer compression and decompression round-trip:
//!
//! ```
//! use zlib_rs::{compress, uncompress};
//!
//! let original = b"the quick brown fox jumps over the lazy dog";
//! let compressed = compress(original).expect("compression succeeds");
//!
//! // `uncompress` writes into a caller-sized buffer and returns the byte count.
//! let mut restored = vec![0u8; original.len()];
//! let written = uncompress(&mut restored, &compressed).expect("decompression succeeds");
//! assert_eq!(&restored[..written], original);
//! ```
//!
//! The Adler-32 and CRC-32 checksum engines are bit-for-bit compatible with C
//! zlib:
//!
//! ```
//! use zlib_rs::{adler32, crc32};
//!
//! assert_eq!(adler32(1, b"123456789"), 0x091e_01de);
//! assert_eq!(crc32(0, b"123456789"), 0xcbf4_3926);
//! ```

// The crate drops `std` only for the explicit, self-consistent `no-std`
// (Z_SOLO) build — i.e. when `no-std` is requested AND `std` is not. Keying
// `#![no_std]` off the *presence* of `no-std` (rather than the *absence* of
// `std`) means a bare `--no-default-features` build (which omits `std` but does
// not request `no-std`) still links `std`, so it supplies the global allocator,
// panic handler, and unwinding support that the `cdylib` / `staticlib`
// crate-types require. Without this, `cargo build --no-default-features` failed
// to produce the C-linkable artifacts ("no global memory allocator",
// "#[panic_handler] required", "unwinding panics are not supported without
// std"). The dedicated `no-std` rlib build (`--no-default-features --features
// no-std`) is unaffected: it requests `no-std` without `std`, so `#![no_std]`
// still applies there. The `not(feature = "std")` conjunct also keeps the
// mutually-exclusive `std` + `no-std` misconfiguration failing cleanly with the
// `compile_error!` below (rather than additionally tripping "cannot find crate
// std" errors): in that contradictory case `std` stays linked and the guard is
// the single, authoritative diagnostic.
#![cfg_attr(all(feature = "no-std", not(feature = "std")), no_std)]
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
//
// A few `pub(crate)` items are ported verbatim from the C `deflate_state` for
// struct/API parity but are not read on any current safe-Rust path — e.g. the
// `method` / `hash_size` fields (carried for layout fidelity) and helpers such
// as `is_valid_status` (reserved for the `capi`-gated FFI shim). `dead_code` is
// therefore allowed *only* for this subtree, so `-D warnings` stays green
// without deleting parity-relevant state that the C baseline defines.
#[allow(dead_code)]
pub mod deflate;

pub mod util;

// The gzip file-I/O layer: the `gz*` API (`gzopen`/`gzread`/`gzwrite`/`gzclose`
// and friends) layered over an owned `std::fs::File` and the inflate/deflate
// engines. Gated by `gz-io` (which implies `std` + `gzip`), mirroring the C
// `#ifndef NO_GZCOMPRESS` / `NO_GZIP` conditional.
//
// As with `deflate`, a few `pub(crate)` helpers mirror C `gzguts.h` internals
// for parity and back the `capi`-gated FFI shim (e.g. `is_writing`,
// `gz_intmax`); they are not reached on a current safe-Rust path, so
// `dead_code` is allowed for this subtree to keep `-D warnings` green without
// dropping C-parity helpers.
#[cfg(feature = "gz-io")]
#[allow(dead_code)]
pub mod gz;

// The C-ABI drop-in shim: the `#[no_mangle] extern "C"` exports that make the
// crate binary-compatible with `libz` (`deflate`, `inflate`, `crc32`, the
// `gz*` family, …). This is one of the crate's two `unsafe` zones (the other is
// the inflate fast path). It is gated behind the non-default `capi` feature so
// the canonical C symbol names are present only in the `cdylib`/`staticlib`
// drop-in artifacts and are kept OUT of `cargo test`, where they would
// otherwise collide at link time with the `flate2` dev-dependency's bundled C
// `zlib` (AAP §0.6.2). cbindgen parses this module to regenerate the C header.
#[cfg(feature = "capi")]
pub mod ffi;

// ===========================================================================
// Public re-exports — the idiomatic crate-root surface
// ===========================================================================

// Checksums: the six canonical entry points plus their 64-bit `*_combine`
// variants, re-exported so callers can write `zlib_rs::crc32(..)` directly.
pub use checksum::{
    adler32, adler32_combine, adler32_combine64, adler32_z, crc32, crc32_combine, crc32_combine64,
    crc32_z,
};

// Error handling: the idiomatic [`ZlibError`], the C-compatible [`ReturnCode`]
// enumeration, and the crate-wide [`Result`] type alias
// (`Result<T> = core::result::Result<T, ZlibError>`) that the idiomatic API
// returns. Re-exporting `Result` lets callers write `zlib_rs::Result<T>`.
pub use error::{Result, ReturnCode, ZlibError};

// The gzip metadata container (only when the `gzip` feature is enabled).
#[cfg(feature = "gzip")]
pub use gz_header::GzHeader;

// Streaming primitives: the `z_stream` analogue and its allocator abstraction.
pub use stream::{Allocator, GlobalAlloc, ZStream};

// The idiomatic decompressor and its per-call outcome.
pub use inflate::{Inflate, InflateOutcome};

// The idiomatic compressor and its per-call outcome.
pub use deflate::{Deflate, DeflateOutcome};

// The one-shot whole-buffer helpers from the `util` layer, lifted to the crate
// root so callers can write `zlib_rs::compress(..)` / `zlib_rs::uncompress(..)`
// directly, mirroring the C `compress` / `compress2` / `compressBound` /
// `uncompress` / `uncompress2` entry points (`compress.c`, `uncompr.c`).
pub use util::{compress, compress_bound, compress2, uncompress, uncompress2};

// The gzip FILE-I/O layer: the owned [`GzFile`] handle, the idiomatic
// [`GzReader`] adapter (which implements the `std::io` traits), and the
// C-faithful `gz*` entry points (`gzopen`/`gzread`/`gzwrite`/`gzclose` and
// friends), lifted to the crate root so the `gzip_compat` integration tests and
// downstream callers can drive gzip files via `zlib_rs::gzopen(..)`. Gated by
// `gz-io` (which implies `std` + `gzip`), so this block is compiled out of the
// `no-std` build automatically.
#[cfg(feature = "gz-io")]
pub use gz::{
    GzFile, GzReader, gzbuffer, gzclearerr, gzclose, gzdirect, gzdopen, gzeof, gzerror, gzflush,
    gzgetc, gzgets, gzoffset, gzopen, gzputc, gzputs, gzread, gzrewind, gzseek, gzsetparams,
    gztell, gzungetc, gzwrite,
};

// All `Z_*` constants and the typed [`FlushMode`]/[`Strategy`]/[`DataType`]
// enumerations are part of the public surface.
pub use constants::*;

// ===========================================================================
// Crate-level version constants and helpers
// ===========================================================================

/// The zlib version string this crate is compatible with — `"1.3.2.1-motley"`.
///
/// A convenience alias for [`ZLIB_VERSION`] surfaced at the crate root for
/// discoverability. The packed numeric form is [`ZLIB_VERNUM`] (`0x1321`); both
/// are reported verbatim to preserve C-API parity with `zlib.h`.
///
/// ```
/// assert_eq!(zlib_rs::VERSION, "1.3.2.1-motley");
/// assert_eq!(zlib_rs::VERSION, zlib_rs::zlib_version());
/// ```
pub const VERSION: &str = constants::ZLIB_VERSION;

// The runtime version reporters, mirroring C `zlibVersion()` and the
// `ZLIB_VERNUM` constant. `zlib_version()` returns the same `&'static str` as
// [`VERSION`]; `zlib_version_num()` returns the packed hexadecimal form
// (`0x1321`). Both are surfaced at the crate root so callers can query the
// version without reaching into the `util` module.
pub use util::{zlib_version, zlib_version_num};
