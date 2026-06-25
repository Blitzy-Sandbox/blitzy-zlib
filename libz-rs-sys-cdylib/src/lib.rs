//! `libz-rs-sys-cdylib`: the C drop-in artifact for `zlib-rs`.
//!
//! This crate is built as a `cdylib` + `staticlib` (library base name `z`),
//! producing `libz.so` / `libz.a` so existing C programs can link against it as
//! a drop-in replacement for the system `libz` without source changes.
//!
//! Its sole responsibility is to re-export the entire `libz_rs_sys` C-ABI
//! surface. A `#[no_mangle] extern "C"` function that nothing references can be
//! dead-stripped from a `cdylib`/`staticlib`; the glob re-export below keeps
//! every zlib symbol reachable so it is emitted into the produced artifacts —
//! the `deflate*`, `inflate*`, `gz*`, `adler32*`, `crc32*`, `compress*`,
//! `uncompress*`, `zlibVersion`, `zlibCompileFlags`, `zError`, and
//! `get_crc_table` families, together with the `#[repr(C)]` `z_stream` /
//! `gz_header` / `gzFile` types and the C scalar typedefs.
//!
//! The implementation lives entirely in `libz_rs_sys` (the thin `unsafe` C-ABI
//! shim) layered over the safe, `#![forbid(unsafe_code)]` `zlib-rs` core; the
//! workspace dependency chain is
//! `libz-rs-sys-cdylib` -> `libz-rs-sys` -> `zlib-rs`. This member declares no
//! symbols of its own: every `extern "C"` function, every `unsafe` block, and
//! every `#[repr(C)]` type definition belongs to `libz-rs-sys`. The re-export
//! also transparently mirrors whichever symbols the shim compiles under the
//! active Cargo features (such as `gz-io`, which governs the `gz*` family), so
//! no per-feature code is needed here.

pub use libz_rs_sys::*;
