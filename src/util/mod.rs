//! Shared utilities ported from `zutil.c` / `zutil.h` and the one-shot
//! convenience helpers from `compress.c` / `uncompr.c`.
//!
//! In the C baseline these utilities are spread across several translation
//! units (`zutil.c` for version reporting and the `zError` table, `compress.c`
//! and `uncompr.c` for the one-shot buffer helpers). In the Rust port they are
//! grouped under this `util` module.
//!
//! ## Checkpoint scope
//!
//! This checkpoint ships the [`version`] submodule, which reproduces
//! `zlibVersion`, `zlibVersionNum`, `zError`, and `zlibCompileFlags`. The
//! one-shot `compress` / `uncompress` helpers (`compress.c` / `uncompr.c`)
//! depend on the DEFLATE driver and therefore arrive together with the
//! compression engine in the next checkpoint; their submodules are declared
//! then.

pub mod version;

// Re-export the version-reporting surface at the `util` level so callers can
// reach it as `zlib_rs::util::zlib_version()` without naming the submodule.
pub use version::{z_error, zlib_compile_flags, zlib_version, zlib_version_num};
