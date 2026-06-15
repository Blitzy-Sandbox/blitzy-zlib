//! Gzip FILE-I/O layer (`gz*` API) — the safe-Rust port of zlib's stdio-like
//! gzip file interface (`gzlib.c` / `gzread.c` / `gzwrite.c` / `gzclose.c`)
//! built on the `gzguts.h` state model.
//!
//! This is the module root for `crate::gz`. The whole layer is gated by the
//! `gz-io` Cargo feature (which implies `std` + `gzip`), mirroring the C
//! `#ifndef NO_GZCOMPRESS` / `NO_GZIP` build (AAP §0.5.2). Because the gate
//! lives here on the `pub mod gz` declaration in `src/lib.rs`, the submodules
//! may freely use `std`.
//!
//! | Submodule      | C source    | Responsibility                                |
//! |----------------|-------------|-----------------------------------------------|
//! | [`mod@state`]  | `gzguts.h`  | [`GzState`](state::GzState) handle, the frozen `Mode`/`How` sentinels, `gz_error` parity, and the RAII `Drop` that replaces `gzclose` |
//!
//! The `open` / `read` / `write` / `close` submodules (`gzlib.c` /
//! `gzread.c` / `gzwrite.c` / `gzclose.c`, AAP §0.4.1) layer on top of this
//! `state` foundation and join this list as they are completed. Until those
//! consumers (and the `extern "C"` FFI shim) land, the crate-internal
//! `GzState` API has no in-crate caller, so the `pub mod gz` declaration in
//! `src/lib.rs` carries a scoped `#[allow(dead_code)]` — exactly as the sibling
//! `deflate` engine root does — to keep the strict `-D warnings` policy clean.
//!
//! # Constraints
//!
//! * **100% safe Rust** — no `unsafe` (AAP §0.6.2); `unsafe` is confined to
//!   `crate::ffi` and `crate::inflate::fast`.

pub mod state;
