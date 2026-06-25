//! `libz-rs-sys` — the C-ABI drop-in shim for the safe `zlib-rs` core.
//!
//! This crate re-exposes `zlib-rs` through the exact C ABI declared in
//! `zlib.h`: `#[repr(C)]` structs that match the C layout byte-for-byte and
//! (in a subsequent milestone) an `extern "C"` / `#[unsafe(no_mangle)]` export
//! surface covering every public zlib symbol. It is the single place in the
//! workspace where `unsafe` is permitted (AAP §0.6.2) — all raw-pointer ↔ slice
//! and integer ↔ `ReturnCode` translation is confined here.
//!
//! At the current foundation milestone the crate provides the ABI **type**
//! definitions only:
//!
//! * [`zstream`] — the `#[repr(C)]` `z_stream`, `gz_header`, and `gzFile`
//!   structures plus the C-style scalar typedefs (`uInt`, `uLong`, `z_off_t`,
//!   the `alloc_func`/`free_func` callback pointers, ...).
//!
//! The `#[unsafe(no_mangle)]` function exports and the raw-pointer translation
//! helpers are added in a subsequent milestone, once the corresponding engines
//! exist in the core. The one exception is [`zlib_compile_flags`], present now
//! because its correctness depends on *this* crate's `z_off_t` definition (see
//! below).

// The C-style type and symbol names (`z_stream`, `uLong`, `gz_header_s`, ...)
// intentionally violate Rust's `CamelCase`/`snake_case` conventions to match the
// zlib ABI verbatim.
#![allow(non_camel_case_types)]

pub mod zstream;

// The raw-pointer ↔ slice and integer ↔ `ReturnCode` translation layer — the
// only place in the workspace that contains `unsafe`. It is wired into the
// crate here so it is actually compiled and type-checked against the core APIs
// it bridges (`zlib_rs::deflate::deflate`, `zlib_rs::inflate::inflate`, the
// checksum helpers, …); previously it was an orphaned file that `cargo check`
// never saw.
//
// It is a **private** module, not a `pub` export surface: its `run_deflate` /
// `run_inflate` drivers and conversion helpers are `pub(crate)` building blocks
// that the `#[unsafe(no_mangle)]` `extern "C"` symbol exports consume in a
// subsequent milestone. Until those exported wrappers exist, the helpers have
// no in-crate callers (outside their own unit tests), so the module carries
// `#[allow(dead_code)]` to keep the `cargo clippy --all-targets -- -D warnings`
// gate green without prematurely widening the public surface (which would be
// out-of-scope scope creep at this milestone).
#[allow(dead_code)]
mod translate;

use core::mem::size_of;

use libc::{c_ulong, off_t};

/// `zlibCompileFlags()` — the compile-time configuration bitmask, reported with
/// **this crate's** `z_off_t` definition.
///
/// The pure-Rust core's `zlib_rs::util::version::zlib_compile_flags` reports the
/// C `long` width for the `z_off_t` size field (bits 6-7), matching zlib's
/// default typedef. The C ABI, however, maps `z_off_t` to `libc::off_t` (see
/// [`zstream::z_off_t`]), which is **not** always the same width as `long` — it
/// differs on 64-bit Windows / LLP64 and on 32-bit large-file builds. This shim
/// therefore computes the flags via the core's parameterized
/// `zlib_rs::util::version::zlib_compile_flags_for`, passing `size_of::<off_t>()`,
/// so the value the C ABI advertises matches the `z_off_t` it actually exposes.
///
/// The return type is [`c_ulong`] (`uLong` in `zlib.h`). At the current
/// foundation milestone this is a plain Rust function; the
/// `extern "C"` / `#[unsafe(no_mangle)]` export surface is layered on in a
/// subsequent milestone, at which point this becomes the body of the exported
/// `zlibCompileFlags` symbol.
#[must_use]
pub fn zlib_compile_flags() -> c_ulong {
    zlib_rs::util::version::zlib_compile_flags_for(size_of::<off_t>()) as c_ulong
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compile_flags_anchored_to_off_t() {
        // The C-ABI flags must report *this* crate's `z_off_t` (== `off_t`)
        // width in the bits-6-7 size field, i.e. the core helper evaluated with
        // `size_of::<off_t>()` — never the C `long` default the pure-Rust core
        // uses. This is the ABI-parity guarantee from review finding #14.
        let expected =
            zlib_rs::util::version::zlib_compile_flags_for(size_of::<off_t>()) as c_ulong;
        assert_eq!(zlib_compile_flags(), expected);

        // And the encoded 2-bit size code matches off_t's actual width.
        let code = (zlib_compile_flags() >> 6) & 0b11;
        let expected_code: c_ulong = match size_of::<off_t>() {
            2 => 0,
            4 => 1,
            8 => 2,
            _ => 3,
        };
        assert_eq!(code, expected_code);
    }
}
