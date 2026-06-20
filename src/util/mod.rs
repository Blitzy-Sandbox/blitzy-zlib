//! Shared utilities for `zlib-rs`: one-shot compression / decompression helpers
//! and version / diagnostic functions. Safe-Rust port of the C utility layer
//! (`compress.c`, `uncompr.c`, `zutil.c`, `zutil.h`).
//!
//! # Role
//!
//! This is the module root of the `util` layer. It wires together the three
//! submodules and re-exports their public functions as a single flat surface so
//! that:
//!
//! * the crate root (`src/lib.rs`) can lift the idiomatic one-shot helpers
//!   directly — `pub use util::{compress, compress2, compress_bound, uncompress,
//!   uncompress2};` — exposing them as `zlib_rs::compress`, `zlib_rs::uncompress`,
//!   and so on; and
//! * the C-ABI shim (`src/ffi.rs`) can reach every helper, including the
//!   `*_to_buf` C-semantics cores, through `crate::util::...`.
//!
//! This file is the **single coordination point** for the `util` public surface:
//! the re-export list below is the authoritative roster of what the layer
//! exposes, kept in lock-step with the identifiers actually defined by the
//! submodules.
//!
//! # Submodules
//!
//! | Submodule      | C source     | Responsibility                                |
//! |----------------|--------------|-----------------------------------------------|
//! | [`version`]    | `zutil.c`    | `zlibVersion` / `zError` / `zlibCompileFlags` |
//! | [`compress`]   | `compress.c` | one-shot whole-buffer compression             |
//! | [`uncompress`] | `uncompr.c`  | one-shot whole-buffer decompression           |
//!
//! # Safety & `no_std`
//!
//! The entire `util` layer is 100% safe Rust (no `unsafe`) and `no_std`-clean:
//! it depends only on [`core`], the `alloc` crate, and sibling crate modules —
//! never on `std`. This module root itself holds no logic beyond the module
//! wiring, the re-exports, and the [`OS_CODE`] diagnostic constant.

pub mod compress;
pub mod uncompress;
pub mod version;

// ---------------------------------------------------------------------------
// Flat public surface.
//
// Re-export EXACTLY the public identifiers defined by the submodules so the
// layer presents one cohesive namespace. Two tiers are exposed:
//
//   * the idiomatic helpers (`compress`, `compress2`, `compress_bound`,
//     `uncompress`, `uncompress2`) that the crate root lifts to `zlib_rs::*`; and
//   * the C-semantics cores (`compress2_to_buf` / `compress_to_buf` /
//     `uncompress2_to_buf` / `uncompress_to_buf`) that the `src/ffi.rs` shim
//     wraps to reproduce the exact C `destLen` / `sourceLen` in/out contract.
//
// `compress` / `uncompress` each name both a submodule (the *type* namespace)
// and a re-exported function (the *value* namespace); the two coexist without
// conflict, so `util::compress` resolves to the function in value position and
// to the module as a path prefix.
// ---------------------------------------------------------------------------

pub use compress::{compress, compress_bound, compress_to_buf, compress2, compress2_to_buf};
pub use uncompress::{uncompress, uncompress_to_buf, uncompress2, uncompress2_to_buf};
pub use version::{z_error, zlib_compile_flags, zlib_version, zlib_version_num};

/// Operating-system code stored in gzip headers (RFC 1952).
///
/// Defaults to Unix (`3`), matching the C `zutil.h` fallback
/// `#ifndef OS_CODE #define OS_CODE 3` (`zutil.h` L187-189). zlib defines a
/// range of platform codes (for reference: `AMIGA` = 1, `VMS` = 2, `MACOS` = 7,
/// `WIN32` = 10, `APPLE` = 19); the gzip layer in `src/gz` may select a
/// per-target value, but the util layer publishes only this portable Unix
/// default — the value used whenever no more specific code applies.
pub const OS_CODE: u8 = 3;

// ---------------------------------------------------------------------------
// C constructs from `zutil.h` intentionally NOT ported here, recorded with the
// location of their replacement so the mapping from the C source is explicit
// and nothing appears to be missing:
//
//   * `z_errmsg[10]` / `ERR_MSG` — the error-string table lives canonically in
//     `crate::error` (`err_msg`); `version::z_error` delegates to it, so the
//     messages are defined in exactly one place and can never be duplicated or
//     diverge here.
//   * `ZALLOC` / `ZFREE` / `TRY_FREE` — manual allocation macros, replaced by
//     Rust ownership and `Drop` (RAII) inside the engine state structs.
//   * `ZSWAP32` — byte reversal, expressed with `u32::swap_bytes` where the
//     checksum / gzip layers need it; unused by `util`.
//   * `Z_ONCE` — one-time CRC-table init, handled by `build.rs` /
//     `crate::checksum` rather than at run time.
//   * the `uch` / `ush` / `ulg` type aliases — replaced by `u8` / `u16` / `u32`.
//   * the common constants `STORED_BLOCK` / `STATIC_TREES` / `DYN_TREES` /
//     `MIN_MATCH` / `MAX_MATCH` / `PRESET_DICT` / `DEF_WBITS` / `DEF_MEM_LEVEL`
//     — defined once in `crate::constants` and consumed by the engines; they are
//     deliberately NOT re-declared here.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn os_code_is_unix_default() {
        // Mirrors C `zutil.h`'s `#define OS_CODE 3` (the Unix fallback).
        assert_eq!(OS_CODE, 3);
    }

    #[test]
    fn version_surface_is_reachable_and_exact() {
        // The version helpers are reachable through the flat `util` surface and
        // report the frozen baseline identity.
        assert_eq!(zlib_version(), "1.3.2.1-motley");
        assert_eq!(zlib_version_num(), 0x1321);
    }

    #[test]
    fn z_error_surface_is_reachable() {
        // `z_error` is reachable through the flat surface and delegates to the
        // canonical `crate::error::err_msg`: `Z_OK` (0) maps to the empty string.
        assert_eq!(z_error(0), "");
        // A defined error code maps to a non-empty message (the text itself is
        // asserted canonically by the `crate::error` suite, never duplicated).
        assert!(!z_error(-3).is_empty());
    }

    #[test]
    fn compile_flags_surface_is_reachable() {
        // `zlib_compile_flags` is reachable through the flat surface; the low two
        // bits encode the size of the C `uInt` type and are never zero-width.
        let flags = zlib_compile_flags();
        assert_ne!(flags & 0b11, 0, "the uInt size field is never zero-width");
    }

    #[test]
    fn compress_bound_surface_is_reachable() {
        // `compress_bound` is reachable through the flat surface; an empty input
        // yields the fixed 13-byte wrapper / Huffman overhead.
        assert_eq!(compress_bound(0), 13);
    }

    #[test]
    fn compress_then_uncompress_round_trips_through_flat_surface() {
        // Prove the idiomatic `compress` / `uncompress` helpers are reachable
        // through the flat `util` surface and interoperate end-to-end.
        let original = b"abcabcabcabcabcabcabcabc";
        let packed = compress(original).expect("compress must succeed at the default level");
        assert!(!packed.is_empty());

        let mut restored = [0u8; 64];
        let produced = uncompress(&mut restored, &packed).expect("uncompress must round-trip");
        assert_eq!(&restored[..produced], original);
    }

    #[test]
    fn c_semantics_cores_are_reachable_through_flat_surface() {
        // The `*_to_buf` C-semantics cores that `src/ffi.rs` wraps are reachable
        // through the flat surface; round-trip a buffer through each of them.
        let original = b"hello hello hello hello";

        let mut packed = [0u8; 64];
        let n = compress2_to_buf(&mut packed, original, 6).expect("compress2_to_buf");
        let n_default = compress_to_buf(&mut [0u8; 64], original).expect("compress_to_buf");
        assert!(n > 0 && n_default > 0);

        let mut restored = [0u8; 64];
        let (produced, consumed) =
            uncompress2_to_buf(&mut restored, &packed[..n]).expect("uncompress2_to_buf");
        assert_eq!(&restored[..produced], original);
        assert_eq!(consumed, n);

        // `uncompress_to_buf` is the by-value-`sourceLen` alias; it behaves
        // identically to `uncompress2_to_buf`.
        let mut restored_alias = [0u8; 64];
        let (produced_alias, _consumed_alias) =
            uncompress_to_buf(&mut restored_alias, &packed[..n]).expect("uncompress_to_buf");
        assert_eq!(&restored_alias[..produced_alias], original);
    }
}
