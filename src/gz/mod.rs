//! High-level gzip file I/O — the safe-Rust port of zlib's stdio-like gzip file
//! interface (the `gzopen`/`gzread`/`gzwrite`/`gzclose` family).
//!
//! This module is the **root** of the `crate::gz` subtree. It is glue only: it
//! declares the five submodules, re-exports their public idiomatic surface so
//! callers reach it as `zlib_rs::gz::*`, and defines the [`GzFile`] handle alias
//! that the C-ABI shim marshals across the FFI boundary. All behaviour lives in
//! the submodules; there is no logic here.
//!
//! The layer is ported from zlib's `gzlib.c` / `gzread.c` / `gzwrite.c` /
//! `gzclose.c` and is built on the shared `gzguts.h` state model. It sits on top
//! of [`std::fs::File`] and the [`Read`](std::io::Read) /
//! [`Write`](std::io::Write) / [`BufRead`](std::io::BufRead) /
//! [`Seek`](std::io::Seek) traits.
//!
//! | Submodule      | C source    | Responsibility                                |
//! |----------------|-------------|-----------------------------------------------|
//! | [`mod@state`]  | `gzguts.h`  | The [`GzState`] handle, the frozen [`Mode`]/[`How`] sentinels, [`GZBUFSIZE`], `gz_error` parity, and the RAII [`Drop`] that replaces `gzclose` |
//! | [`mod@open`]   | `gzlib.c` (+ `gzwrite.c` for `gzsetparams`) | open / positioning / status: the `gz_open` constructor with character-for-character mode-string parsing, plus the public [`gzopen`]/[`gzopen64`]/[`gzdopen`]/[`gzbuffer`]/[`gzrewind`]/[`gzseek`]/[`gzseek64`]/[`gztell`]/[`gztell64`]/[`gzoffset`]/[`gzoffset64`]/[`gzeof`]/[`gzerror`]/[`gzclearerr`]/[`gzsetparams`] surface. Engine init stays lazy in `read`/`write` |
//! | [`mod@read`]   | `gzread.c`  | gzip read path: drives the inflate engine through the `windowBits = 31` gzip wrapper (sniffing the gzip magic, else transparent pass-through). Exposes the public [`gzread`]/[`gzfread`]/[`gzgetc`]/[`gzgetc_`]/[`gzungetc`]/[`gzgets`]/[`gzdirect`] API and the idiomatic [`Read`](std::io::Read)/[`BufRead`](std::io::BufRead) adapters implemented directly on [`GzState`] |
//! | [`mod@write`]  | `gzwrite.c` | gzip write path: drives the deflate engine through the `windowBits = 31` gzip wrapper. Exposes the public [`gzwrite`]/[`gzfwrite`]/[`gzputc`]/[`gzputs`]/[`gzprintf`]/[`gzflush`]/[`gzclose_w`] API and the idiomatic [`GzWriter`] wrapper |
//! | [`mod@close`]  | `gzclose.c` | gzip close dispatcher: the thin top-level [`gzclose`] that routes to the read-side or write-side teardown by [`Mode`]. The teardown bodies stay in `read`/`write` and in [`GzState`]'s [`Drop`], faithful to the C module split |
//!
//! # Feature gating
//!
//! The whole layer is gated by the `gz-io` Cargo feature (which implies
//! `std` + `gzip`), mirroring the C `#ifndef NO_GZCOMPRESS` / `NO_GZIP` build
//! (AAP §0.5.2). Because that gate lives on the `pub mod gz` declaration in
//! `src/lib.rs`, neither this file nor its submodules need a per-file
//! `#![cfg(...)]` guard, and they may use `std` freely.
//!
//! The `pub mod gz` declaration in `src/lib.rs` also carries a scoped
//! `#[allow(dead_code)]` (the same rationale as the sibling `deflate` engine
//! root): several crate-internal helpers and C-API entry points are consumed
//! only by the `extern "C"` FFI shim rather than by any current in-crate caller,
//! and the attribute keeps them from tripping the strict `-D warnings` policy
//! until that shim lands.
//!
//! # Public surface
//!
//! The re-exports below curate the *idiomatic* API — the C-faithful function
//! identities (`gz*`), the [`GzState`] handle and its [`Mode`]/[`How`]
//! sentinels, the [`GzWriter`] streaming wrapper, and the [`GzFile`] alias.
//! Internal pipeline helpers (`gz_open`, `gz_reset`, `gz_look`, `gz_decomp`,
//! `gz_comp`, …) remain `pub(crate)` and are reached through their submodule
//! paths (e.g. `crate::gz::open::gz_open`); they are deliberately **not** lifted
//! into this public surface.
//!
//! # Constraints
//!
//! * **100% safe Rust** — every construct in this module is safe (AAP §0.6.2);
//!   the crate confines raw-pointer / FFI code to `crate::ffi` and the decode
//!   hot loop in `crate::inflate::fast`.

// ---------------------------------------------------------------------------
// Submodule declarations (listed `state` first — it is the foundational type
// that every other submodule threads through; declaration order does not affect
// compilation).
// ---------------------------------------------------------------------------

pub mod state;

pub mod open;
pub mod read;
pub mod write;

pub mod close;

// ---------------------------------------------------------------------------
// State foundation re-exports (single source of truth lives in `state.rs`;
// these are surfaced here, never redefined — AAP Phase D).
// ---------------------------------------------------------------------------

// The owning gzip file-state handle plus its frozen sentinels and the default
// I/O buffer size. `Mode` (`GZ_NONE`/`GZ_READ`/`GZ_WRITE`) and `How`
// (`LOOK`/`COPY`/`GZIP`) carry the exact zlib `#define` values from `gzguts.h`,
// and `GZBUFSIZE` is the 8 KiB default working-buffer size.
pub use state::{GZBUFSIZE, GzState, How, Mode};

/// The owned gzip file handle — the idiomatic counterpart of C's opaque
/// `typedef struct gzFile_s *gzFile`.
///
/// In C, `gzFile` is a pointer to a heap-allocated `gz_state`. The idiomatic
/// Rust equivalent is an owning [`Box`] of the ported [`GzState`]: the `Box`
/// mirrors the single C heap allocation and, via [`Drop`], reproduces the
/// deterministic teardown that C performs in `gzclose` (closing the file and
/// releasing the inflate/deflate engine and working buffers).
///
/// # FFI contract (load-bearing — `crate::ffi` matches this exactly)
///
/// The `extern "C"` shim represents the C `gzFile` as a `*mut c_void` and
/// converts to and from this owned handle at the FFI boundary:
///
/// * **open** (`gzopen`/`gzdopen`/…): take the [`Box<GzState>`](GzState)
///   produced by the safe constructor and hand C a raw pointer with
///   [`Box::into_raw`], cast to `*mut c_void`. Ownership is transferred to the
///   caller; no [`Drop`] runs yet.
/// * **use** (`gzread`/`gzwrite`/…): cast the `*mut c_void` back to
///   `*mut GzState` and reborrow it as `&mut GzState` for the duration of the
///   call, **without** reclaiming ownership.
/// * **close** (`gzclose`/`gzclose_r`/`gzclose_w`): reclaim ownership with
///   [`Box::from_raw`] and let the resulting `Box<GzState>` drop, which runs the
///   RAII teardown exactly once.
///
/// Keeping `GzFile` defined as `Box<GzState>` is what makes the
/// `Box::into_raw` / `Box::from_raw` round-trip in `crate::ffi` sound: the
/// pointer C holds is exactly a `Box<GzState>` pointer.
pub type GzFile = Box<crate::gz::state::GzState>;

// ---------------------------------------------------------------------------
// Curated public API re-exports (the C-faithful `gz*` identities). Only the
// genuinely `pub` entry points are surfaced here; the `pub(crate)` pipeline
// helpers in each submodule are reached via their module paths.
// ---------------------------------------------------------------------------

// Open / positioning / status (`gzlib.c`, plus `gzsetparams` from `gzwrite.c`).
// Includes the large-file (`*64`) variants so the FFI shim can delegate 1:1 to
// `gzopen64`/`gzseek64`/`gztell64`/`gzoffset64`.
pub use open::{
    gzbuffer, gzclearerr, gzdopen, gzeof, gzerror, gzoffset, gzoffset64, gzopen, gzopen64,
    gzrewind, gzseek, gzseek64, gzsetparams, gztell, gztell64,
};

// Read path (`gzread.c`). The idiomatic `Read`/`BufRead` adapters are
// implemented directly on `GzState` (re-exported above), so there is no
// separate reader wrapper type to re-export.
pub use read::{gzdirect, gzfread, gzgetc, gzgetc_, gzgets, gzread, gzungetc};

// Write path (`gzwrite.c`). `gzprintf` exposes an `fmt::Arguments`-based safe
// core; the C variadic marshalling lives at the `crate::ffi` boundary.
// `GzWriter` is the idiomatic streaming wrapper implementing `Write`.
pub use write::{GzWriter, gzclose_w, gzflush, gzfwrite, gzprintf, gzputc, gzputs, gzwrite};

// Top-level close dispatcher (`gzclose.c`). Routes to the read- or write-side
// teardown by `Mode`; the per-side bodies (`gzclose_r`/`gzclose_w`) stay in
// `read`/`write`, faithful to the C module split.
pub use close::gzclose;
