//! Convenience / utility layer for the `zlib-rs` core.
//!
//! This module root aggregates the safe-Rust ports of zlib's three small
//! "utility" C translation units and re-exports their public API at the
//! [`crate::util`] level, giving both the idiomatic Rust callers and the
//! `libz-rs-sys` C-ABI shim a single, flat entry point:
//!
//! | C source     | Rust submodule                 | Flat re-exports |
//! |--------------|--------------------------------|-----------------|
//! | `compress.c` | [`compress`](mod@compress)     | [`compress`](fn@compress), [`compress2`], [`compress_bound`] |
//! | `uncompr.c`  | [`uncompress`](mod@uncompress) | [`uncompress`](fn@uncompress), [`uncompress2`] |
//! | `zutil.c`    | [`version`]                    | [`zlib_version`], [`zlib_compile_flags`], [`z_error`] + the `ZLIB_VER*` constants |
//!
//! # Design
//!
//! This file is a *thin aggregator*: it contains **no logic** — only `pub mod`
//! declarations and explicit, named `pub use` re-exports. The actual work
//! (driving the deflate/inflate engines, formatting diagnostics) lives in the
//! submodules, and all heap allocation is performed by those engines, never by
//! this layer. Consequently this module:
//!
//! * contains **zero `unsafe`** — the whole crate is built under
//!   `#![forbid(unsafe_code)]` (see [`crate`]);
//! * is **`no_std`-capable** — it references nothing from the standard library,
//!   so it compiles unchanged with `--no-default-features`; and
//! * exposes a stable, collision-free public surface via *named* re-exports
//!   (never a glob `*` that could silently shadow a sibling symbol).
//!
//! The three submodules are intentionally declared `pub` (rather than private
//! with re-export only): the C-ABI shim reaches the version diagnostics through
//! their fully-qualified path — for example
//! `zlib_rs::util::version::zlib_compile_flags` — in addition to using the flat
//! re-exports below.

pub mod compress;
pub mod uncompress;
pub mod version;

// ---------------------------------------------------------------------------
// Flat re-export surface (reachable as `crate::util::<name>`)
// ---------------------------------------------------------------------------

// One-shot in-memory buffer compressors (`compress.c`). Note that `compress`
// names both this re-exported free function (the *value* namespace) and the
// `compress` submodule (the *type/module* namespace); Rust keeps the two in
// distinct namespaces, so they coexist without conflict.
pub use compress::{compress, compress_bound, compress2};

// One-shot in-memory buffer decompressors (`uncompr.c`). As with `compress`
// above, the `uncompress` free function and the `uncompress` submodule occupy
// distinct namespaces and both remain reachable.
pub use uncompress::{uncompress, uncompress2};

// Version / diagnostic helpers (`zutil.c`) together with the numeric
// `ZLIB_VER*` version components (originating in `zlib.h`, defined once in
// `crate::constants` and re-exported through `version`). Re-exporting the full
// set here lets the `libz-rs-sys` shim assemble the entire
// `zlibVersion` / `zlibCompileFlags` / `zError` symbol family — and the
// `ZLIB_VERNUM` ABI-parity check — from this single `crate::util` path.
pub use version::{
    ZLIB_VER_MAJOR, ZLIB_VER_MINOR, ZLIB_VER_REVISION, ZLIB_VER_SUBREVISION, ZLIB_VERNUM, z_error,
    zlib_compile_flags, zlib_version,
};
