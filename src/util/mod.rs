//! Utility layer for the `zlib-rs` crate.
//!
//! This module is the root of the utility layer and is the idiomatic-Rust
//! counterpart to the C header `zutil.h`. In the C sources, `zutil.h` is the
//! shared internal interface pulled in by nearly every translation unit via
//! `#include "zutil.h"`; the Rust convention that replaces that include is
//! `use crate::{error::ZlibError, util::*};`, where `crate::util` is exactly
//! this module.
//!
//! The concrete logic lives in three sibling submodules, all re-exported here:
//!
//! - One-call, whole-buffer compression wrappers ported from C `compress.c`
//!   (`compress`, `compress2`, `compress_bound`), in [`compress`].
//! - One-call, whole-buffer decompression wrappers ported from C `uncompr.c`
//!   (`uncompress`, `uncompress2`), in [`uncompress`].
//! - Version, compile-flag, and error-string reporting ported from C
//!   `zutil.c` (`zlib_version`, `zlib_compile_flags`, `z_error`), in
//!   [`version`].
//!
//! In addition, this module is the canonical home for the small set of shared
//! internal constants that `zutil.h` owned (the block-type tags, the LZ77
//! match-length bounds, and the preset-dictionary flag) together with the
//! gzip-header operating-system identifier byte [`OS_CODE`].
//!
//! # `zmem*` helper mapping
//!
//! The C library defines `zmemcpy`, `zmemcmp`, and `zmemzero` as thin wrappers
//! over `memcpy`, `memcmp`, and `memset`. In safe Rust these have no standalone
//! function form; each collapses into an inline slice operation performed at
//! the call site:
//!
//! - `zmemcpy(dst, src, n)` becomes `dst[..n].copy_from_slice(&src[..n])`
//! - `zmemcmp(a, b, n)` becomes `a[..n] == b[..n]` (or `a[..n].cmp(&b[..n])`
//!   when an ordering result is required)
//! - `zmemzero(b, n)` becomes `b[..n].fill(0)`
//!
//! These operations are emitted directly by their consumers (the deflate and
//! inflate engines), so no `zmem*` functions are defined in this module by
//! design.
//!
//! # Safety and environment
//!
//! Every item in this module is safe Rust; the layer contains no `unsafe`
//! blocks. It references neither `std` nor `alloc`, so it participates cleanly
//! in `no_std` builds. Raw-pointer and `extern "C"` variants of the wrappers
//! live in `crate::ffi`, not here.

/// One-call, whole-buffer compression wrappers ported from C `compress.c`.
pub mod compress;
/// One-call, whole-buffer decompression wrappers ported from C `uncompr.c`.
pub mod uncompress;
/// Version, compile-flag, and error-string reporting ported from C `zutil.c`.
pub mod version;

// ---------------------------------------------------------------------------
// Shared internal constants (ported from `zutil.h`).
//
// `src/constants.rs` deliberately does not define these; this module is their
// canonical public home. Engine modules (deflate/inflate) may keep their own
// private copies for locality - those are distinct items in distinct module
// paths and therefore do not conflict with the public copies below.
// ---------------------------------------------------------------------------

/// Block-type tag for a *stored* (uncompressed) DEFLATE block.
///
/// Mirrors `STORED_BLOCK` in C `zutil.h` (line 87).
pub const STORED_BLOCK: u8 = 0;

/// Block-type tag for a block compressed with the *static* Huffman trees.
///
/// Mirrors `STATIC_TREES` in C `zutil.h` (line 88).
pub const STATIC_TREES: u8 = 1;

/// Block-type tag for a block compressed with *dynamic* Huffman trees.
///
/// Mirrors `DYN_TREES` in C `zutil.h` (line 89).
pub const DYN_TREES: u8 = 2;

/// Minimum LZ77 match length DEFLATE encodes as a length/distance pair.
///
/// Mirrors `MIN_MATCH` in C `zutil.h` (line 92). Typed as `usize` because it
/// participates in buffer indexing and match-length arithmetic.
pub const MIN_MATCH: usize = 3;

/// Maximum LZ77 match length DEFLATE encodes as a length/distance pair.
///
/// Mirrors `MAX_MATCH` in C `zutil.h` (line 93). Typed as `usize` because it
/// participates in buffer indexing and match-length arithmetic.
pub const MAX_MATCH: usize = 258;

/// Preset-dictionary flag bit (`FDICT`) in the zlib header `FLG` byte.
///
/// Mirrors `PRESET_DICT` in C `zutil.h` (line 96).
pub const PRESET_DICT: u8 = 0x20;

// ---------------------------------------------------------------------------
// Platform `OS_CODE` selection (ported from `zutil.h` lines 98-189).
//
// The gzip header (RFC 1952) carries a one-byte operating-system identifier.
// C selects it through a long `#ifdef` cascade; here it collapses to a single
// compile-time constant chosen with `cfg` attributes. Only targets that have a
// stable Rust `cfg` predicate are distinguished (Windows and Apple); every
// other target - including Linux and the historical codes C enumerated
// (Amiga = 1, VMS = 2, ATARI = 5, OS/2 = 6, old Mac OS = 7, RISC OS = 13,
// BeOS = 16, OS/400 = 18, MS-DOS = 0) - collapses to the Unix default of 3,
// matching C's `#ifndef OS_CODE` fallback. Exactly one variant below is
// compiled for any given target.
// ---------------------------------------------------------------------------

/// The gzip-header operating-system identifier byte for the current target.
///
/// On Windows this is `10`, mirroring the C `WIN32 && !__CYGWIN__` branch of
/// `zutil.h` (lines 156-158). Cygwin reports the Unix code and is excluded by
/// Rust's `cfg(windows)` predicate.
#[cfg(windows)]
pub const OS_CODE: u8 = 10;

/// The gzip-header operating-system identifier byte for the current target.
///
/// On Apple platforms this is `19`, mirroring the C `__APPLE__` branch of
/// `zutil.h` (lines 168-170).
#[cfg(all(not(windows), target_vendor = "apple"))]
pub const OS_CODE: u8 = 19;

/// The gzip-header operating-system identifier byte for the current target.
///
/// This is the Unix default of `3`, mirroring the C `#ifndef OS_CODE` fallback
/// in `zutil.h` (lines 187-189). It applies to Linux and every other target
/// that lacks a more specific stable `cfg` predicate, and is the value used
/// for byte-exact gzip-header parity in continuous integration.
#[cfg(all(not(windows), not(target_vendor = "apple")))]
pub const OS_CODE: u8 = 3;

// ---------------------------------------------------------------------------
// Public API re-exports.
//
// `src/lib.rs` re-exports these through the crate prelude so that both
// `crate::util::<item>` and the top-level crate path resolve. Both the
// idiomatic snake_case names and the camelCase C-parity aliases defined by the
// child modules are surfaced. The `compress`/`uncompress` module names (type
// namespace) and the re-exported functions of the same spelling (value
// namespace) coexist without conflict.
// ---------------------------------------------------------------------------

/// One-call compression entry points: `compress` and `compress2`, plus the
/// output-bound helper `compress_bound` and its C-parity alias `compressBound`.
pub use compress::{compress, compress_bound, compress2, compressBound};

/// One-call decompression entry points: `uncompress` and `uncompress2`.
pub use uncompress::{uncompress, uncompress2};

/// Version, compile-flag, and error-string reporting entry points, each paired
/// with its camelCase C-parity alias (`zlibVersion`, `zlibCompileFlags`,
/// `zError`).
pub use version::{
    z_error, zError, zlib_compile_flags, zlib_version, zlibCompileFlags, zlibVersion,
};
