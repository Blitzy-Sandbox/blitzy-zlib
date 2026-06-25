//! `libz-rs-sys-cdylib` — the drop-in `libz.so` / `libz.a` artifact.
//!
//! This member compiles the [`libz_rs_sys`] C-ABI shim into a `cdylib` and
//! `staticlib` so C programs can link against the Rust implementation as a
//! drop-in replacement for the system zlib (AAP §0.3.1).
//!
//! At the current foundation milestone the shim exposes the `#[repr(C)]` ABI
//! type definitions; the `extern "C"` / `#[unsafe(no_mangle)]` function exports
//! that materialize the C symbol table are added in a subsequent milestone.
//! Re-exporting the shim here keeps it linked into the artifact and gives the
//! member a single, stable dependency edge onto `libz-rs-sys`.

#![allow(non_camel_case_types)]

pub use libz_rs_sys::*;
