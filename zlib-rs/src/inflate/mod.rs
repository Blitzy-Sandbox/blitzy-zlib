//! DEFLATE **decompressor** — the safe-Rust port of zlib's inflate engine.
//!
//! This module is the root of the inflate (decompression) subsystem. It decodes
//! every container framing the C library understands:
//!
//! * the **zlib** stream format (RFC 1950),
//! * **raw DEFLATE** with no wrapper (RFC 1951), and
//! * the **gzip** stream format (RFC 1952), behind the `gzip` Cargo feature.
//!
//! The engine is a faithful, `#![forbid(unsafe_code)]` safe-Rust port of the C
//! sources `inflate.c`, `inffast.c`, `inftrees.c`, `inffixed.h`, and
//! `infback.c`. Manual C memory management and raw-pointer window arithmetic are
//! replaced by owned `Box`/`Vec` buffers (released through `Drop`) and
//! bounds-checked slice indexing; the C `switch (state->mode)` dispatch with its
//! `goto inf_leave` exits becomes an exhaustive `match` over [`InflateMode`]
//! inside a labeled `'inf:` loop (see [`state`]).
//!
//! # Public API shape
//!
//! Unlike the C library — which exposes a flat family of `inflateXxx(z_streamp,
//! …)` functions threaded through a caller-owned `z_stream` — the idiomatic Rust
//! surface is **method-based**: a single owned [`InflateState`] carries the
//! decompressor, and every `inflateXxx` operation is one of its inherent
//! methods. The table below maps the canonical `zlib.h` C entry points to their
//! Rust counterparts. The `libz-rs-sys` shim re-exposes the exact C symbol names
//! and ABI (via `#[no_mangle]` / `extern "C"`) on top of these — those
//! `unsafe`, C-named wrappers live *only* in that shim, never here.
//!
//! | C symbol (`zlib.h`)    | Rust API                                       |
//! |------------------------|------------------------------------------------|
//! | `inflateInit_`         | `InflateState::new_default`                    |
//! | `inflateInit2_`        | `InflateState::new`                            |
//! | `inflate`              | `InflateState::inflate` → [`InflateResult`]    |
//! | `inflateEnd`           | `Drop` for [`InflateState`] (RAII)             |
//! | `inflateReset`         | `InflateState::reset`                          |
//! | `inflateReset2`        | `InflateState::reset2`                         |
//! | `inflateResetKeep`     | `InflateState::reset_keep`                     |
//! | `inflatePrime`         | `InflateState::prime`                          |
//! | `inflateSetDictionary` | `InflateState::set_dictionary`                 |
//! | `inflateGetDictionary` | `InflateState::get_dictionary`                 |
//! | `inflateGetHeader`     | `InflateState::get_header` (`gzip` feature)    |
//! | `inflateSync`          | `InflateState::sync`                           |
//! | `inflateSyncPoint`     | `InflateState::sync_point`                     |
//! | `inflateCopy`          | `InflateState::copy`                           |
//! | `inflateMark`          | `InflateState::mark`                           |
//! | `inflateValidate`      | `InflateState::validate`                       |
//! | `inflateUndermine`     | `InflateState::undermine`                      |
//! | `inflateCodesUsed`     | `InflateState::codes_used`                     |
//! | `inflateBackInit_`     | [`inflate_back_init`]                          |
//! | `inflateBack`          | [`inflate_back`]                               |
//! | `inflateBackEnd`       | [`inflate_back_end`]                           |
//!
//! Because the `inflateXxx` operations are inherent methods on [`InflateState`],
//! re-exporting the state type makes the entire decompressor API reachable from
//! `crate::inflate`. The `inflateBack` family — which the C library implements
//! as standalone functions in `infback.c` — stays as the three free functions
//! [`inflate_back_init`], [`inflate_back`], and [`inflate_back_end`],
//! re-exported alongside it.
//!
//! # Submodules
//!
//! Declared in dependency order (foundational data model first):
//!
//! * [`tables`] — the Huffman decode-table builder ([`tables::inflate_table`]),
//!   the [`tables::Code`] decode-entry type, and the `ENOUGH` capacity
//!   constants (`inftrees.c` / `inftrees.h`).
//! * [`fixed`] — the precomputed fixed literal/length and distance decode tables
//!   [`fixed::LENFIX`] / [`fixed::DISTFIX`] (`inffixed.h`).
//! * [`state`] — the owned decompressor [`state::InflateState`], its
//!   [`state::InflateMode`] machine and `'inf:` driver loop, and every lifecycle
//!   and auxiliary operation (`inflate.c`).
//! * [`fast`] — the [`fast::inflate_fast`] hot-path decoder for the common case
//!   of ample input and output (`inffast.c`); `unsafe`-free.
//! * [`back`] — the callback-driven raw-DEFLATE [`back::inflate_back`] decoder
//!   (`infback.c`), which reuses [`state`], [`tables`], and [`fixed`].
//!
//! # `gzip` feature
//!
//! gzip-container decoding (magic-header parsing, the `inflateGetHeader` capture
//! API, and the trailing CRC-32 check) is gated behind the `gzip` Cargo feature
//! *inside* [`state`]. This module root re-exports only feature-agnostic symbols
//! (the state type, its mode/result types, and the `inflateBack` free
//! functions), so it compiles unchanged whether `gzip` is enabled or not; with
//! `gzip` disabled the zlib and raw-DEFLATE paths remain fully available.
//!
//! # `no_std`
//!
//! This module names only `core`/`alloc` items (never `std`), so it participates
//! in the crate's `--no-default-features` `no_std` build, mirroring the C
//! library's `Z_SOLO` configuration.

// ---------------------------------------------------------------------------
// Submodule declarations
// ---------------------------------------------------------------------------
//
// Listed foundational-first to mirror the dependency hierarchy: `tables` and
// `fixed` are leaf data models; `state` builds the streaming engine on top of
// them; `fast` is the inner-loop decoder `state` calls; and `back` layers the
// callback decoder over all of the above. (They are separated by blank lines so
// this human-readable ordering survives `rustfmt`'s module reordering, which
// only sorts *contiguous* `mod` runs.)
//
// `pub` visibility is deliberate: it keeps every `crate::inflate::*` path named
// by the cross-module contract (AAP §0.4.2) resolvable for the in-crate engine
// and the `libz-rs-sys` shim — in particular `crate::inflate::state::InflateState`
// (named by `crate::stream`), `crate::inflate::tables::{inflate_table, Code,
// ENOUGH}`, `crate::inflate::fast::inflate_fast`, and
// `crate::inflate::fixed::{LENFIX, DISTFIX}`.

pub mod tables;

pub mod fixed;

pub mod state;

pub mod fast;

pub mod back;

// ---------------------------------------------------------------------------
// Public decompressor API surface (re-exports)
// ---------------------------------------------------------------------------

// The idiomatic Rust decompressor API is the owned [`InflateState`] plus its
// inherent methods (see the C-symbol map in the module docs); re-exporting the
// type here makes `inflate`, `inflateInit2_`, `inflateReset2`,
// `inflateSetDictionary`, … all reachable as `crate::inflate::InflateState`
// methods. [`InflateMode`] is the public state enum, and [`InflateResult`] is
// the value returned by `InflateState::inflate`. The exact C symbol names and
// the `#[no_mangle]` / `extern "C"` surface live ONLY in `libz-rs-sys`.
pub use state::{InflateMode, InflateResult, InflateState};

// The `inflateBack` family (C `infback.c`) is a low-level, callback-driven raw
// DEFLATE decoder; it is exposed as free functions, matching its C shape, plus
// the borrowed [`InflateBackResult`] returned by [`inflate_back`].
pub use back::{InflateBackResult, inflate_back, inflate_back_end, inflate_back_init};

// NOTE — internal helpers are intentionally NOT re-exported at this root:
// `tables::{inflate_table, Code, CodeType, ENOUGH}`, `fast::inflate_fast`, and
// `fixed::{LENFIX, DISTFIX}` are decoder implementation details. They remain
// reachable at their submodule paths (e.g. `crate::inflate::tables::inflate_table`)
// for in-crate use and the FFI shim, but are not part of the idiomatic public
// decompressor API.
