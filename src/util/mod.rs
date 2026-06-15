//! Shared utilities for `zlib-rs` — the safe-Rust port of zlib's utility layer
//! (`zutil.h` glue plus the one-shot `compress.c` / `uncompr.c` helpers and the
//! `zutil.c` diagnostics).
//!
//! This is the module root for `crate::util`. It declares the utility
//! submodules and re-exports their public surface so the whole utility API
//! resolves under a single path, `crate::util::<name>`, mirroring zlib's flat C
//! namespace.
//!
//! | Submodule           | C source     | Responsibility                                  |
//! |---------------------|--------------|-------------------------------------------------|
//! | [`mod@version`]     | `zutil.c`    | [`zlib_version`]/[`zlib_compile_flags`]/[`z_error`] |
//! | [`mod@uncompress`]  | `uncompr.c`  | one-shot [`uncompress`]/[`uncompress2`]         |
//!
//! The one-shot `compress`/`uncompress` helpers (`compress.c` → `compress.rs`,
//! `uncompr.c` → `uncompress.rs`, AAP §0.4.1) are layered on top of this root
//! and re-exported here as the utility engine is completed; this file owns the
//! shared utility constants and the diagnostic re-exports they build on.
//!
//! # Error-string single-sourcing
//!
//! [`z_error`] does **not** carry its own message table; it delegates to
//! [`crate::error::err_msg`], the single canonical reproduction of the C
//! `z_errmsg[10]` table. No message literal appears in `crate::util`.
//!
//! # Constraints
//!
//! * **100% safe Rust** — no `unsafe` (AAP §0.6.2).
//! * **`no_std`-clean** — `core`/`alloc` only, never `std`.

pub mod version;

// The one-shot DEFLATE convenience helpers (`uncompr.c` → `uncompress.rs`).
// `compress.rs` joins this list as the compression half is completed.
pub mod uncompress;

// Re-export the version/diagnostic surface from `zutil.c`. These mirror the
// `zlib.h`-declared C entry points (`zlibVersion`, `zlibCompileFlags`,
// `zError`) so both the idiomatic Rust API (`src/lib.rs`) and the future C-ABI
// shim (`src/ffi.rs`) can build on a single flat utility namespace.
pub use version::{z_error, zlib_compile_flags, zlib_version, zlib_version_num};

// Re-export the one-shot decompression API (`uncompr.c`'s `uncompress` /
// `uncompress2`) so it resolves as `crate::util::uncompress` /
// `crate::util::uncompress2` for both the idiomatic `src/lib.rs` re-export and
// the `src/ffi.rs` C-ABI shim. The module `uncompress` and the re-exported
// function `uncompress` share a name in different namespaces (type vs. value),
// so both `crate::util::uncompress` (the function) and
// `crate::util::uncompress::*` (the module) remain reachable.
pub use uncompress::{uncompress, uncompress2};

/// Operating-system code stored in the gzip header (RFC 1952), matching C
/// `zutil.h`'s `#define OS_CODE 3` default (Unix).
///
/// zlib selects this byte per build target (e.g. AMIGA=1, VMS=2, MACOS=7,
/// WIN32=10, APPLE=19), but the portable default — and the value this safe
/// port uses for byte-exact reproducibility — is Unix (`3`). The gzip file I/O
/// layer (`src/gz/`) may override it per target; the utility layer only
/// publishes the default.
pub const OS_CODE: u8 = 3;

#[cfg(test)]
mod tests {
    //! Light reachability checks: prove the flat `crate::util::*` surface is
    //! wired correctly. The substantive tests live in the submodules and in
    //! `tests/*.rs`.

    use super::{OS_CODE, z_error, zlib_version, zlib_version_num};

    #[test]
    fn os_code_is_unix_default() {
        assert_eq!(OS_CODE, 3);
    }

    #[test]
    fn version_surface_is_reachable() {
        assert_eq!(zlib_version(), "1.3.2.1-motley");
        assert_eq!(zlib_version_num(), 0x1321);
    }

    #[test]
    fn z_error_delegates_to_error_table() {
        // Z_OK -> empty message; out-of-range -> empty sentinel.
        assert_eq!(z_error(0), "");
        assert_eq!(z_error(-100), "");
    }
}
