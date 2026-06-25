//! Buffered gzip file I/O (`gz*` API) — the safe-Rust port of the C `gz*`
//! family from `gzguts.h`, `gzlib.c`, `gzread.c`, `gzwrite.c`, and `gzclose.c`.
//!
//! This is the buffered, `FILE`-style convenience layer that sits on top of the
//! streaming deflate/inflate core: it reads and writes `.gz` files (RFC 1952)
//! and transparently passes through non-gzip data. Each C translation unit maps
//! to a submodule:
//!
//! | Submodule        | C source      | Responsibility                                |
//! |------------------|---------------|-----------------------------------------------|
//! | [`state`]        | `gzguts.h`    | The owned [`GzState`](state::GzState) struct  |
//! | [`open`]         | `gzlib.c`     | open / buffer / seek / position / error API   |
//! | [`read`]         | `gzread.c`    | the decompression engine + read API           |
//! | [`write`]        | `gzwrite.c`   | the compression engine (`gz_init`/`gz_comp`/`gz_zero`) |
//! | [`close`]        | `gzclose.c`   | close dispatch (`gzclose`/`gzclose_r`/`gzclose_w`) |
//!
//! The state, open, read, write, and close layers together form the complete
//! buffered engine. The public `GzFile` handle and the `gz*` C entry points
//! that drive these `pub(crate)` items are reconstructed in the `libz-rs-sys`
//! FFI shim (a later checkpoint); until then the only callers are this module's
//! own `#[cfg(test)]` suites.
//!
//! # Availability
//!
//! This entire module is gated behind the `gz-io` Cargo feature (which implies
//! `std` + `gzip`) at its declaration site in the crate root (`lib.rs`). The
//! submodules therefore carry no inner `#![cfg(...)]` attributes.
//!
//! # Safety
//!
//! Like the rest of the core, every file here is compiled under the crate-wide
//! `#![forbid(unsafe_code)]`: the C `int fd` becomes an owned
//! [`File`](std::fs::File), the `malloc`/`free` buffers become owned `Vec<u8>`,
//! and the raw `z_stream` data pointers become plain `usize` offsets. No raw
//! pointers and no `unsafe` appear anywhere in the `gz` subsystem; the
//! raw-pointer `gzFile` ABI is reconstructed exclusively in the `libz-rs-sys`
//! FFI shim.

// The gz foundation (state + open + write-engine) is fully implemented and
// compiled, but it is not yet reachable from a public entry point: the public
// `GzFile` handle, the `gz*` functions, and the FFI export surface that drive
// these `pub(crate)` items are introduced in a later checkpoint. Until then the
// only callers are this module's own `#[cfg(test)]` suites, so every item is
// "dead" under a plain (non-test) build. This mirrors the established pattern
// for the not-yet-wired FFI translation layer (`libz-rs-sys`'s
// `#[allow(dead_code)] mod translate;`): allow `dead_code` for the foundation
// so it compiles and is exercised by tests now, without prematurely exposing a
// public API (which would be scope creep). The allow propagates to the
// submodules below; it will be removed once the public/FFI surface wires these
// functions in.
#![allow(dead_code)]

pub(crate) mod close;
pub(crate) mod open;
pub(crate) mod read;
pub(crate) mod state;
pub(crate) mod write;
