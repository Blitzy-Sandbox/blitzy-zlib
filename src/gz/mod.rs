//! The gzip file-I/O layer (`gz*` API), ported from the zlib `gz*.c` sources.
//!
//! This module groups the safe-Rust port of zlib's gzip file interface — the
//! `gzopen` / `gzread` / `gzwrite` / `gzclose` family declared in `zlib.h` and
//! implemented across `gzlib.c`, `gzread.c`, `gzwrite.c`, and `gzclose.c`. It
//! sits on top of an owned [`std::fs::File`] and drives the crate's
//! [`inflate`](crate::inflate) / [`deflate`](crate::deflate) engines to read or
//! write gzip-framed (or, transparently, raw) data.
//!
//! # Layout
//!
//! The C translation units map onto the following submodules (AAP §0.3.1):
//!
//! | C source    | Rust submodule        | Responsibility                              |
//! |-------------|-----------------------|---------------------------------------------|
//! | `gzguts.h`  | [`state`]             | `GzState` model + `Mode` / `How` sentinels  |
//! | `gzread.c`  | [`read`]              | read path: LOOK/COPY/GZIP machine + `gzread*`|
//! | `gzwrite.c` | [`write`]             | write path: `gzwrite*` + deflate driving    |
//! | `gzlib.c`   | [`open`]              | open / mode-parse / `gzseek` / `gztell` / status |
//!
//! The remaining C translation unit (`gzclose.c` → `close`) is created by its
//! own agent; this module root declares the submodules that exist so the layer
//! compiles incrementally.
//!
//! # Memory-ownership model (AAP §0.6.3)
//!
//! Every `gz` function threads a `&mut `[`GzState`](state::GzState) — the
//! idiomatic, ownership-enforcing twin of the C `gz_statep`. The owned
//! [`File`](std::fs::File), the input/output [`Vec<u8>`](alloc::vec::Vec)
//! buffers, and the embedded [`ZStream`](crate::stream::ZStream) engine are all
//! released deterministically by `Drop` (the RAII replacement for `gzclose`),
//! so this layer needs **no `unsafe`**: C-string marshalling and raw-pointer
//! handling are confined to `src/ffi.rs`.
//!
//! # Feature gating
//!
//! The whole module is gated by the `gz-io` Cargo feature (which implies
//! `std` and `gzip`) at the `pub mod gz` site in `lib.rs`, mirroring the C
//! `#ifndef NO_GZCOMPRESS` / `NO_GZIP` conditional. Submodules may therefore use
//! `std` freely without per-file `#![cfg(...)]` guards.

pub mod state;

pub mod read;

// -- read-path public API re-exports (ported from `gzread.c`) ---------------
//
// Surface the faithful-C-name read entry points at the `crate::gz` level so the
// forthcoming `src/ffi.rs` C-ABI shim and the idiomatic crate-root re-exports
// can reach them as `crate::gz::gzread` etc. The read-side close dispatcher
// (`gzclose_r`) is `pub(crate)` and re-exported for the `close.rs` dispatcher.
pub use read::{GzReader, gzdirect, gzfread, gzgetc, gzgetc_, gzgets, gzread, gzungetc};

pub mod write;

// -- write-path public API re-exports (ported from `gzwrite.c`) -------------
//
// Surface the faithful-C-name write entry points at the `crate::gz` level,
// mirroring the read path above, so the forthcoming `src/ffi.rs` C-ABI shim and
// the idiomatic crate-root re-exports can reach them as `crate::gz::gzwrite`
// etc. The write-side close dispatcher (`gzclose_w`) is re-exported for the
// `close.rs` dispatcher.
pub use write::{gzclose_w, gzflush, gzfwrite, gzprintf, gzputc, gzputs, gzwrite};

pub mod open;

// -- open / positioning / status public API re-exports (ported from
//    `gzlib.c`, plus `gzsetparams` whose body comes from `gzwrite.c`) --------
//
// Surface the faithful-C-name open/seek/status entry points at the
// `crate::gz` level so the forthcoming `src/ffi.rs` C-ABI shim and the
// idiomatic crate-root re-exports can reach them as `crate::gz::gzopen` etc.
// [`GzSource`] is the safe `gz_open` input (the FFI layer constructs its
// `File` variant from a raw fd inside its `unsafe` boundary).
pub use open::{
    GzSource, gzbuffer, gzclearerr, gzdopen, gzeof, gzerror, gzoffset, gzoffset64, gzopen,
    gzopen64, gzrewind, gzseek, gzseek64, gzsetparams, gztell, gztell64,
};

pub mod close;

// -- close-path public API re-export (ported from `gzclose.c`) --------------
//
// The top-level `gzclose` dispatcher inspects the handle's mode and routes to
// `gzclose_r` (read) or `gzclose_w` (write); its body lives in `close.rs` and
// the read/write teardown bodies stay in `read.rs` / `write.rs`. Surface the
// dispatcher at the `crate::gz` level so the forthcoming `src/ffi.rs` C-ABI
// shim and the idiomatic crate-root re-exports can reach it as
// `crate::gz::gzclose`.
pub use close::gzclose;
