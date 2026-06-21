//! High-level gzip file I/O — the `gz*` API family (`gzopen` / `gzread` /
//! `gzwrite` / `gzclose` and friends).
//!
//! This module is the safe-Rust port of zlib's gzip *file* interface: the
//! `FILE`-like layer declared in `zlib.h` and implemented across the C
//! translation units `gzlib.c`, `gzread.c`, `gzwrite.c`, and `gzclose.c`, with
//! the shared declarations from `gzguts.h`. It sits on top of an owned
//! [`File`](std::fs::File) and drives the crate's [`inflate`](crate::inflate) /
//! [`deflate`](crate::deflate) engines to read or write gzip-framed (or,
//! transparently, raw) data. The idiomatic streaming adapter
//! [`GzReader`] implements the standard library's [`Read`](std::io::Read) /
//! [`BufRead`](std::io::BufRead) / [`Seek`](std::io::Seek) traits, and the
//! write-side [`GzWriter`] implements [`Write`](std::io::Write).
//!
//! # Layout — C translation unit → Rust submodule (AAP §0.3.1, §0.4.1)
//!
//! | C source    | Rust submodule        | Responsibility                                   |
//! |-------------|-----------------------|--------------------------------------------------|
//! | `gzguts.h`  | [`state`]             | [`GzState`] model + [`Mode`] / [`How`] sentinels |
//! | `gzlib.c`   | [`open`]              | open / mode-parse / `gzseek` / `gztell` / status |
//! | `gzread.c`  | [`read`]              | read path: LOOK/COPY/GZIP machine + `gzread*`    |
//! | `gzwrite.c` | [`write`](mod@write)  | write path: `gzwrite*` + deflate driving         |
//! | `gzclose.c` | [`close`]             | [`gzclose`] dispatcher → `gzclose_r`/`gzclose_w`  |
//!
//! # Owned handle: [`GzFile`]
//!
//! The C API models a gzip file as an opaque, heap-allocated handle:
//!
//! ```c
//! typedef struct gzFile_s *gzFile;   /* a heap-allocated gz_statep */
//! ```
//!
//! The idiomatic Rust equivalent is an **owning** `Box<GzState>`, aliased here
//! as [`GzFile`]. Using an owning `Box` (rather than a borrowed reference)
//! mirrors the C library's heap-allocated, caller-owned handle and makes this
//! type the single linchpin of the FFI boundary — see [`GzFile`] for the exact
//! `Box::into_raw` / `Box::from_raw` contract that `src/ffi.rs` upholds.
//!
//! # Memory-ownership model (AAP §0.6.3)
//!
//! Every `gz` entry point threads a `&mut `[`GzState`] (or consumes an owned
//! [`GzFile`] on [`gzclose`]). Because all resources — the embedded
//! [`File`](std::fs::File), the input/output buffers, and the embedded
//! compression engine — are owned and released by `Drop`, this layer is the
//! RAII replacement for the C `gzclose` teardown and needs **no `unsafe`**:
//! C-string marshalling and raw `*mut z_stream` / `*mut c_void` handling are
//! confined to `src/ffi.rs` (AAP §0.6.2).
//!
//! # Feature gating
//!
//! The whole module is gated by the `gz-io` Cargo feature (which implies `std`
//! and `gzip`) at the `pub mod gz` site in `lib.rs`, mirroring the C
//! `#ifndef NO_GZCOMPRESS` / `NO_GZIP` conditional. Submodules may therefore use
//! `std` freely without per-file `#![cfg(...)]` guards.

// ---------------------------------------------------------------------------
// Submodule declarations.
//
// Declared alphabetically (rustfmt's canonical order). Conceptually `state` is
// the foundational unit — it defines `GzState` plus the `Mode` / `How`
// sentinels that every other submodule builds upon — while `open`, `read`,
// `write`, and `close` implement the lifecycle. Each submodule is `pub` so its
// ported, C-faithful entry points are reachable both through the curated flat
// re-exports below (`crate::gz::gzopen`, …) and through their canonical module
// paths (`crate::gz::open::gzopen`, …). The latter is also how crate-internal
// `pub(crate)` helpers are reached — see the "Crate-internal helpers" note at
// the bottom of this file.
// ---------------------------------------------------------------------------
pub mod close;
pub mod open;
pub mod read;
pub mod state;
pub mod write;

// ---------------------------------------------------------------------------
// `GzFile` — the owned gzip-file handle (the idiomatic twin of C `gzFile`).
// ---------------------------------------------------------------------------

/// Owned handle to an open gzip file — the idiomatic Rust equivalent of the C
/// `gzFile` (`typedef struct gzFile_s *gzFile`, i.e. a heap-allocated
/// `gz_statep`).
///
/// A `GzFile` is an owning `Box<`[`GzState`]`>`. Choosing an owning `Box`
/// (rather than a borrowed reference) mirrors the C library's heap-allocated,
/// caller-owned handle and makes this type the single linchpin of the FFI
/// boundary in `src/ffi.rs`, where the C type is `gzFile = *mut c_void`:
///
/// * **Open** — `gzopen` / `gzdopen` in `src/ffi.rs` obtain a `GzFile` from the
///   safe constructor (which returns `Option<GzFile>`) and hand the raw pointer
///   to C:
///   ```ignore
///   let handle: GzFile = /* Box<GzState> from gz_open(..) */;
///   let raw: *mut core::ffi::c_void = Box::into_raw(handle).cast();
///   ```
/// * **Close** — `gzclose` in `src/ffi.rs` reclaims ownership and routes through
///   the safe [`gzclose`] dispatcher, which drops the box deterministically:
///   ```ignore
///   // SAFETY: `raw` came from `Box::into_raw` above and is reclaimed once.
///   let handle: GzFile = unsafe { Box::from_raw(raw.cast::<GzState>()) };
///   let rc: i32 = crate::gz::gzclose(Some(handle));
///   ```
///
/// The `unsafe` raw-pointer conversion lives **only** in `src/ffi.rs`; this
/// module and its submodules remain 100% safe Rust (AAP §0.6.2). Safe-Rust
/// callers never touch a raw pointer: they pass an owned `Option<GzFile>`
/// straight to [`gzclose`].
pub type GzFile = Box<crate::gz::state::GzState>;

// ---------------------------------------------------------------------------
// `state` re-exports — foundational model & sentinels (ported from `gzguts.h`).
//
// The single source of truth for `GzState` and the `Mode` / `How` sentinels
// (and the `GZBUFSIZE` default buffer size) is `state.rs`; they are merely
// surfaced here so callers and the FFI shim can use the flat
// `crate::gz::{GzState, Mode, How, GZBUFSIZE}` paths. Sentinels are NOT
// redefined here — `state.rs` remains authoritative (AAP Phase D).
// ---------------------------------------------------------------------------
pub use state::{GZBUFSIZE, GzState, How, Mode};

// ---------------------------------------------------------------------------
// `open` re-exports — construction, positioning, and status (ported from
// `gzlib.c`, plus `gzsetparams` whose body comes from `gzwrite.c`).
//
// [`GzSource`] is the safe input to the internal constructor (the FFI layer
// builds its `File` variant from a raw fd inside its own `unsafe` boundary).
// Both 32- and 64-bit offset variants (`gzseek`/`gzseek64`, `gztell`/`gztell64`,
// `gzoffset`/`gzoffset64`, `gzopen`/`gzopen64`) are surfaced so the FFI shim can
// map the corresponding C `*64` symbols 1:1.
// ---------------------------------------------------------------------------
pub use open::{
    GzSource, gzbuffer, gzclearerr, gzdopen, gzeof, gzerror, gzoffset, gzoffset64, gzopen,
    gzopen64, gzrewind, gzseek, gzseek64, gzsetparams, gztell, gztell64, mode_is_valid,
};

// ---------------------------------------------------------------------------
// `read` re-exports — the read path (ported from `gzread.c`): the C-faithful
// `gzread*` entry points plus the idiomatic [`GzReader`] adapter, which
// implements the `std::io` traits over a borrowed [`GzState`].
// ---------------------------------------------------------------------------
pub use read::{GzReader, gzdirect, gzfread, gzgetc, gzgetc_, gzgets, gzread, gzungetc};

// ---------------------------------------------------------------------------
// `write` re-exports — the write path (ported from `gzwrite.c`): the `gzwrite*`
// entry points plus the write-side close dispatcher `gzclose_w`, and the
// idiomatic [`GzWriter`] adapter, which implements [`Write`](std::io::Write)
// over a borrowed [`GzState`] (the write-side twin of [`GzReader`]). The
// variadic `gzprintf` is exposed here as a `core::fmt::Arguments`-based core;
// the C variadic marshalling lives at the `src/ffi.rs` boundary, which formats
// into a buffer and feeds the byte-oriented `gzprintf_bytes` core.
// ---------------------------------------------------------------------------
pub use write::{
    GzWriter, gzclose_w, gzflush, gzfwrite, gzprintf, gzprintf_bytes, gzputc, gzputs, gzwrite,
};

// ---------------------------------------------------------------------------
// `close` re-export — the top-level [`gzclose`] dispatcher (ported from
// `gzclose.c`). It inspects the handle's [`Mode`] and routes to the read- or
// write-side teardown, then drops the owning [`GzFile`].
// ---------------------------------------------------------------------------
pub use close::gzclose;

// ---------------------------------------------------------------------------
// Crate-internal helpers (intentionally NOT re-exported here)
//
// A few cross-module routines are `pub(crate)` rather than `pub`:
//   * the shared constructor `crate::gz::open::gz_open`
//   * the read-side teardown `crate::gz::read::gzclose_r`
//
// They are deliberately given no flattened `crate::gz::*` re-export. A
// `pub(crate) use` that no live path references yet — the C-ABI shim
// `src/ffi.rs` lands in a later checkpoint — would be flagged `unused_imports`
// under `cargo clippy -- -D warnings` (a lint distinct from the `dead_code`
// allowance on `pub mod gz` in `lib.rs`). The `pub mod` declarations above
// already make these reachable at their canonical module paths, which is
// exactly how `close.rs` consumes them today (`use crate::gz::read::gzclose_r;`).
// When `src/ffi.rs` is implemented it delegates through those same module paths,
// so the C surface still maps 1:1 onto this module's functions.
// ---------------------------------------------------------------------------
