//! `zlib-rs` — a safe, idiomatic, pure-Rust port of the zlib compression
//! library (compatible with C zlib `1.3.2.1-motley`, `ZLIB_VERNUM 0x1321`).
//!
//! This crate is the **core** of the workspace. It implements the DEFLATE
//! algorithm and the three wire formats zlib speaks, entirely in safe Rust:
//!
//! * **DEFLATE** — raw compressed blocks, [RFC 1951](https://www.rfc-editor.org/rfc/rfc1951).
//! * **zlib** — the DEFLATE stream wrapped with a 2-byte header and a
//!   big-endian Adler-32 trailer, [RFC 1950](https://www.rfc-editor.org/rfc/rfc1950).
//! * **gzip** — the `.gz` file container with a little-endian CRC-32 trailer,
//!   [RFC 1952](https://www.rfc-editor.org/rfc/rfc1952).
//!
//! It also provides the Adler-32 and CRC-32 checksums and the streaming
//! `z_stream` state machine. The design goal is **bit-identical output** to C
//! zlib for every `(level, strategy, windowBits)` combination, so the produced
//! bytes can be fed to — and consumed from — any existing zlib/gzip consumer
//! unchanged.
//!
//! # Relationship to the C ABI
//!
//! Application code that wants an *idiomatic Rust* API uses **this** crate
//! directly (`use zlib_rs::ZStream;`, `use zlib_rs::compress;`, …). The exact
//! C ABI drop-in — the `extern "C"` / `#[repr(C)]` surface that lets existing C
//! binaries relink against this implementation without source changes — lives
//! in the sibling [`libz-rs-sys`] crate, which is layered *on top of* this
//! safe core.
//!
//! [`libz-rs-sys`]: https://crates.io/crates/libz-rs-sys
//!
//! # Safety
//!
//! The entire core is compiled under `#![forbid(unsafe_code)]`
//! (see the crate attribute below): **there is no `unsafe` anywhere in this
//! crate**, and any `unsafe` block would fail the build outright. Manual C
//! memory management — `malloc`/`free`, `FAR` pointers, and the `zalloc`/`zfree`
//! callbacks — is replaced by Rust ownership: owned [`alloc::boxed::Box`] /
//! [`alloc::vec::Vec`] buffers released deterministically through `Drop`. All
//! `unsafe` required at the C boundary is confined to the `libz-rs-sys` shim.
//!
//! # `no_std`
//!
//! The crate is `#![no_std]` whenever the default `std` feature is disabled
//! (`--no-default-features`), relying only on `core` and `alloc`. This mirrors
//! the C library's `Z_SOLO` configuration. The `gz*` file-I/O layer and a few
//! diagnostics need the operating-system facilities of `std` and are
//! feature-gated accordingly (see [Feature flags](#feature-flags)).
//!
//! # Module map
//!
//! Each C translation unit becomes a cohesive Rust module:
//!
//! | Module        | C source                                          |
//! |---------------|---------------------------------------------------|
//! | [`error`]     | `zutil.h` (integer status codes → [`Result`])     |
//! | [`constants`] | `zlib.h` / `zconf.h` (the `Z_*` constants)        |
//! | [`gz_header`] | `zlib.h` (`gz_header` → [`GzHeader`])             |
//! | [`stream`]    | `zlib.h` (`z_stream` → owned [`ZStream`])         |
//! | [`checksum`]  | `adler32.c` / `crc32.c` / `crc32.h`               |
//! | [`deflate`]   | `deflate.c` / `deflate.h` / `trees.c` / `trees.h` |
//! | [`inflate`]   | `inflate.c` / `inffast.c` / `inftrees.c` / `infback.c` |
//! | [`util`]      | `compress.c` / `uncompr.c` / `zutil.c`            |
//! | `gz`          | `gzlib.c` / `gzread.c` / `gzwrite.c` / `gzclose.c` (feature `gz-io`) |
//!
//! # Feature flags
//!
//! These mirror the C library's compile-time configuration and **must** stay in
//! lock-step with the sibling `zlib-rs/Cargo.toml`:
//!
//! * **`std`** *(default)* — enables the standard library (the `gz*` file-I/O
//!   layer, the `std::error::Error` impl, run-time SIMD detection, …). When it
//!   is **off**, the crate is `#![no_std]`.
//! * **`gzip`** — gzip *container* support (mirrors the C `GZIP` define). The
//!   gzip-specific code lives inside [`deflate`] / [`inflate`] behind their own
//!   `#[cfg(feature = "gzip")]` gates, which is why those modules are always
//!   declared here regardless of this flag.
//! * **`gz-io`** *(= `std` + `gzip`)* — the buffered `gz*` *file* API (mirrors
//!   the C `#ifndef NO_GZCOMPRESS`). The [`gz`] module is compiled **only**
//!   under this feature.
//! * **`no-std`** — an explicit marker mirroring the C `Z_SOLO` define, kept for
//!   parity and discoverability. See the note below about how the actual switch
//!   is wired.
//! * **`simd`** *(default)* — the hardware-accelerated CRC-32 path via the
//!   `crc32fast` crate. Disabling it selects the always-correct scalar table.
//!
//! The C-ABI / drop-in layer is **not** a feature of this crate. It lives
//! entirely in the sibling members (`libz-rs-sys` and `libz-rs-sys-cdylib`) and
//! is gated by *their* features (e.g. `gz-io` / `c-variadic` on `libz-rs-sys`),
//! so this pure-Rust core can always be built and tested without ever pulling in
//! the FFI surface.

// AAP §0.2.2 / §0.6.2 — zero `unsafe` anywhere in the safe core. This is a hard
// compile-time guarantee: any `unsafe` block (including in `inflate/fast.rs`)
// fails the build. All `unsafe` lives exclusively in the `libz-rs-sys` shim.
#![forbid(unsafe_code)]
// The module/algorithm documentation deliberately cross-references private
// implementation items (e.g. the internal `BASE` / `NMAX` checksum constants,
// the `StreamState` ownership enum, the `DeflateContext` worker) from the docs
// of public items, so that a `--document-private-items` developer build renders
// those links and the prose stays precise. In a public-only `cargo doc` build
// those targets are not published, which rustdoc would otherwise flag. The
// links are intentional, so allow them crate-wide rather than degrading the
// internal documentation; this keeps `cargo doc` (and a `-D warnings` doc build)
// clean without losing the cross-references.
#![allow(rustdoc::private_intra_doc_links)]
// The crate is `no_std` whenever the `std` feature is absent. NOTE: the switch
// is deliberately keyed off `not(feature = "std")` and **not** off the `no-std`
// marker feature — `no-std` is only a `Z_SOLO`-parity alias and does not, by
// itself, remove the standard library. This keeps "std present?" a single,
// unambiguous question across the whole crate.
#![cfg_attr(not(feature = "std"), no_std)]

// The core uses heap-allocated owned buffers (`Box`/`Vec`/`String`) but does not
// require the full standard library, so it depends on the `alloc` crate
// directly. Declaring it unconditionally is valid under both `std` and `no_std`
// builds (`alloc` is a subset of `std`), and it is what makes the owned-buffer
// ownership model usable in the `Z_SOLO`/`no_std` configuration.
extern crate alloc;

// ---------------------------------------------------------------------------
// Public module tree
//
// Declared foundational-first. The four flat files come first, then the five
// engine module directories. `gz` is the only conditionally compiled module.
// ---------------------------------------------------------------------------

pub mod error;

pub mod constants;

pub mod gz_header;

pub mod stream;

pub mod checksum;

pub mod deflate;

pub mod inflate;

pub mod util;

// Buffered gzip file I/O (the `gz*` API). It is built on `std::fs::File` /
// `std::io`, so it is gated behind `gz-io` (which implies `std` + `gzip`); the
// pure-`no_std` engine build never pulls it in. The gate is applied here at the
// declaration site (the module root may also self-gate).
#[cfg(feature = "gz-io")]
pub mod gz;

// ---------------------------------------------------------------------------
// Public API re-exports — the idiomatic crate-root surface
//
// These let consumers (integration tests, benches, and the `libz-rs-sys` shim)
// write `use zlib_rs::ZStream;`, `use zlib_rs::compress;`, etc. Every name below
// is re-exported from one of the `depends_on_files` modules. The richer engine
// entry points (`deflate::deflate_init2`, `inflate::inflate`, the `gz*` family,
// …) remain reachable through their `pub mod` paths above.
// ---------------------------------------------------------------------------

// Error model: the `ReturnCode`/`ZlibError` split plus the crate-wide `Result`
// alias (`zutil.h` status codes → `Result`).
pub use error::{Result, ReturnCode, ZlibError};

// All `Z_*` constants, the `Flush`/`Strategy`/`DataType` enums, the `Level`
// newtype, and the `MIN_MATCH` / `MAX_MATCH` / `MAX_WBITS` / `ZLIB_VERSION` /
// `ZLIB_VERNUM` limits. A glob is used deliberately: `constants` is a pure
// constant/enum module with no `pub use` of its own, so this is the single
// glob re-export in the crate and cannot create an ambiguous-glob conflict.
pub use constants::*;

// The owned streaming handle and the custom-allocator extension point
// (`z_stream` + `zalloc`/`zfree` → `ZStream` + `Allocator`).
pub use stream::{Allocator, DefaultAllocator, ZStream};

// The gzip header builder/accessor (`gz_header`) and its default-OS sentinel.
pub use gz_header::{GzHeader, OS_UNKNOWN};

// One-shot in-memory helpers (`compress.c` / `uncompr.c`) and the version /
// diagnostics family (`zutil.c`). `ZLIB_VER*` / `ZLIB_VERNUM` are intentionally
// *not* re-pulled from `util` here — they already arrive through the
// `constants` glob above, and re-exporting the same item twice is needless.
pub use util::{
    compress, compress_bound, compress2, uncompress, uncompress2, z_error, zlib_compile_flags,
    zlib_version,
};

// Adler-32 and CRC-32 entry points plus their `_combine` operations and the
// CRC table accessor (`adler32.c` / `crc32.c` / `crc32.h`). `crc32` / `crc32_z`
// are defined for both the `simd` and scalar configurations, so these
// re-exports resolve regardless of the `simd` feature.
pub use checksum::{
    adler32, adler32_combine, adler32_z, crc32, crc32_combine, crc32_combine_gen, crc32_combine_op,
    crc32_z, get_crc_table,
};
