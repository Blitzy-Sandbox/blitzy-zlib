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
//! | [`mod@read`]   | `gzread.c`  | gzip read path: drives the inflate engine through the `windowBits = 31` gzip wrapper (sniffing the gzip magic, else transparent passthrough) and reads from the file. Defines the `pub(crate)` pipeline (`gz_load`/`gz_avail`/`gz_look`/`gz_decomp`/`gz_fetch`/`gz_skip`/`gz_read`/`gzclose_r`), the public `gzread`/`gzfread`/`gzgetc`/`gzungetc`/`gzgets`/`gzdirect` API, and the idiomatic `impl Read`/`BufRead` adapters |
//! | [`mod@write`]  | `gzwrite.c` | gzip write path: drives the deflate engine through the `windowBits = 31` gzip wrapper and writes to the file. Defines the shared `pub(crate)` helpers (`gz_init`/`gz_comp`/`gz_zero`/`set_params`/`finish`/`gzclose_w`) reused by `open`/`close`/`Drop`, plus the public `gzwrite`/`gzfwrite`/`gzputc`/`gzputs`/`gzprintf`/`gzflush` API and the idiomatic [`GzWriter`](write::GzWriter) |
//! | [`mod@close`]  | `gzclose.c` | gzip close dispatcher: the thin top-level [`gzclose`] that routes to `gzclose_r` (read) or `gzclose_w` (write) by [`Mode`](state::Mode). The teardown bodies stay in `read`/`write` and in `GzState`'s `Drop`, faithful to the C module split |
//!
//! The [`close`](mod@close) submodule (`gzclose.c`, AAP §0.4.1) has landed and
//! provides the top-level [`gzclose`] dispatcher; the `open` submodule
//! (`gzlib.c`) layers on top of this `state` foundation and joins this list as
//! it is completed. Until every consumer (and the `extern "C"` FFI shim) lands,
//! parts of the crate-internal `GzState` API still have no in-crate caller, so
//! the `pub mod gz` declaration in `src/lib.rs` carries a scoped
//! `#[allow(dead_code)]` — exactly as the sibling `deflate` engine root does —
//! to keep the strict `-D warnings` policy clean.
//!
//! # Constraints
//!
//! * **100% safe Rust** — no `unsafe` (AAP §0.6.2); `unsafe` is confined to
//!   `crate::ffi` and `crate::inflate::fast`.

pub mod close;
pub mod read;
pub mod state;
pub mod write;

// The top-level `gzclose` dispatcher (ported from `gzclose.c`). The read-side
// and write-side teardown bodies (`gzclose_r`/`gzclose_w`) remain in `read`
// and `write`, faithful to the C module split.
pub use close::gzclose;
