//! `zlib-rs` — a safe, idiomatic, pure-Rust port of the zlib compression
//! library.
//!
//! This crate is the **core** of the workspace: it implements the DEFLATE
//! algorithm (RFC 1951) together with the zlib (RFC 1950) and gzip (RFC 1952)
//! container formats, the Adler-32 and CRC-32 checksums, and the streaming
//! `z_stream` state machine — all in safe Rust. The thin `libz-rs-sys` crate
//! layers the exact C ABI on top of this core; application code that wants an
//! idiomatic Rust API uses this crate directly.
//!
//! # Safety
//!
//! The entire core is compiled under `#![forbid(unsafe_code)]` (see the crate
//! attribute below). Manual C memory management
//! (`malloc`/`free`, `FAR` pointers, the `zalloc`/`zfree` callbacks) is replaced
//! by Rust ownership: owned [`alloc::boxed::Box`]/[`alloc::vec::Vec`] buffers
//! released through `Drop`. All `unsafe` required at the C boundary lives
//! exclusively in the `libz-rs-sys` shim.
//!
//! # `no_std`
//!
//! With the default `std` feature disabled (`--no-default-features`) the crate
//! is `#![no_std]` and relies only on `core` and `alloc`, mirroring the C
//! library's `Z_SOLO` configuration. The `gz*` file-I/O layer and a handful of
//! diagnostics require `std` and are feature-gated accordingly.
//!
//! # Module map
//!
//! Each C translation unit becomes a cohesive Rust module:
//!
//! | Module          | C source                                        |
//! |-----------------|-------------------------------------------------|
//! | [`constants`]   | `zlib.h` / `zconf.h` (`Z_*` constants)          |
//! | [`error`]       | `zutil.h` (status codes → `Result`)             |
//! | [`stream`]      | `zlib.h` (`z_stream` → owned [`stream::ZStream`])|
//! | [`gz_header`]   | `zlib.h` (`gz_header`)                           |
//! | [`checksum`]    | `adler32.c` / `crc32.c` / `crc32.h`             |
//! | [`deflate`]     | `deflate.c` / `deflate.h` / `trees.c`           |
//! | [`inflate`]     | `inflate.c` / `inffast.c` / `inftrees.c`        |
//! | [`util`]        | `compress.c` / `uncompr.c` / `zutil.c`          |

// Core safety + portability contract for the whole crate. `no_std` is active
// whenever the `std` feature is off; `alloc` is always available (see below).
#![cfg_attr(not(feature = "std"), no_std)]
// AAP §0.6.2 / Rule 6: zero `unsafe` anywhere in the safe core. This is a hard
// compile-time guarantee — any `unsafe` block fails the build.
#![forbid(unsafe_code)]

// The crate uses heap-allocated owned buffers (`Box`/`Vec`) but does not require
// the full standard library, so it depends on the `alloc` crate directly. This
// is valid under both `std` and `no_std` builds.
extern crate alloc;

// ---------------------------------------------------------------------------
// Public module tree
// ---------------------------------------------------------------------------

pub mod constants;
pub mod error;
pub mod gz_header;
pub mod stream;

pub mod checksum;
pub mod deflate;
pub mod inflate;
pub mod util;

// Buffered gzip file I/O (`gz*` API). It depends on `std::fs::File` / `std::io`
// for real file handling, so it is gated behind `gz-io` (which implies `std` +
// `gzip`); the pure-`no_std` engine build never pulls it in.
#[cfg(feature = "gz-io")]
pub mod gz;
