//! C-compatible FFI bindings for zlib-rs, a pure Rust zlib implementation.
//!
//! This crate provides `extern "C"` function wrappers that expose the zlib API
//! through C-compatible symbols, enabling use as a drop-in replacement for
//! the C zlib library (`libz.so` / `zlib1.dll`).
//!
//! # Safety
//! This crate is the ONLY crate in the workspace permitted to contain
//! `unsafe` code. All unsafe code is confined to FFI boundary operations.

#![allow(non_snake_case)]
#![allow(non_camel_case_types)]
#![allow(clippy::missing_safety_doc)]
#![allow(clippy::module_name_repetitions)]
#![allow(clippy::wildcard_imports)]

pub mod checksum;
pub mod types;

pub use types::*;
