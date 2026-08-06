//! `zlib-rs` -- the safe, dependency-free algorithmic core of the zlib Rust port.
//!
//! Every compression and decompression algorithm the reference implementation defines lives in
//! this crate, reimplemented from scratch in idiomatic safe Rust: the DEFLATE encoder and its
//! Huffman coder, the resumable inflate state machine, the callback-driven `inflateBack` decoder,
//! Adler-32 and CRC-32, the gzip file layer, and the one-shot `compress`/`uncompress` wrappers.
//! The in-tree C sources are the algorithmic oracle for all of it -- they are read, never
//! translated mechanically, and never edited.
//!
//! This crate exports **no C symbol and holds no raw pointer**. Those belong to
//! `crates/libz-rs-sys`, the facade that reproduces the `libz` ABI on top of what is defined
//! here.
//!
//! # Where `unsafe` lives, and why it is not here
//!
//! The whole point of the port is that the algorithms are memory-safe by construction rather
//! than by convention, and the crate root is where that claim is made enforceable:
//!
//! ```text
//!   crates/zlib-rs          #![forbid(unsafe_code)]           <-- this crate. No unsafe, ever.
//!         |                 no_std + alloc, zero dependencies
//!         v
//!   crates/libz-rs-sys      #![deny(unsafe_op_in_unsafe_fn)]  <-- the ONLY unsafe in the port
//!         |                 extern "C", #[no_mangle], repr(C)
//!         v
//!   libz.so / libz.a        the drop-in replacement C callers link against
//! ```
//!
//! `unsafe` is unavoidable at an FFI edge; it is *not* unavoidable behind one. The facade
//! validates each raw `z_streamp`, rebuilds the `(next_in, avail_in)` and
//! `(next_out, avail_out)` slices exactly once on entry, round-trips the opaque `state`
//! pointer, calls the caller's `zalloc`/`zfree`, handles C strings and varargs, and writes
//! through the caller's `gz_header`. Everything past that boundary -- which is everything in
//! this crate -- is ordinary safe Rust operating on slices and indices, so it can be analysed
//! exhaustively by Miri precisely because it contains no FFI.
//!
//! Three consequences are worth stating outright, because they are what the safety argument for
//! the port actually rests on:
//!
//! * **The prohibition is asserted here and nowhere else.** The workspace `[lints]` table
//!   deliberately does *not* set `unsafe_code`, because the facade legitimately needs it and a
//!   workspace-wide `forbid` would make that crate unbuildable. The compiler-checked guarantee
//!   for the safe core therefore exists solely because of the `#![forbid(unsafe_code)]` below.
//!   `forbid` -- not `deny` -- is used on purpose: it cannot be relaxed by an `#[allow]`
//!   anywhere in the crate, not even inside a test module.
//! * **The window is a slice, not a pointer.** `deflate.c` walks the sliding window with raw
//!   pointer arithmetic (`Bytef *scan = s->window + s->strstart`), guarded only by debug-only
//!   `Assert`s such as `"wild scan"`. Here the window, the hash chains and the pending buffer
//!   are owned buffers addressed by integer cursors, which is what retires that entire class of
//!   defect. [`weak_slice`] holds the split-borrow views the match finder needs without
//!   reintroducing aliasing.
//! * **Nothing in this crate can become a linker symbol.** No item is `#[no_mangle]`, and there
//!   is no `extern "C"` or `extern "C-unwind"` anywhere, so the `local:` block of `zlib.map` --
//!   which hides `inflate_table`, `inflate_fast`, `inflate_fixed`, `z_errmsg`, `gz_error`,
//!   `gz_intmax` and every `_*` name -- is satisfied by construction rather than by the linker.
//!   The one `#[repr(C)]` the port permits is the 24-byte `gzFile_s` prefix in
//!   [`gz::state`], and it exists only because the `gzgetc` macro in `zlib.h` compiles field
//!   arithmetic into *caller* object code.
//!
//! # Module map
//!
//! The module tree mirrors the C translation units one for one, so a maintainer diffing a Rust
//! module against its C source is comparing like with like. Every module's own documentation
//! names the file and line ranges it was ported from.
//!
//! | Module | Ported from | Responsibility |
//! |---|---|---|
//! | [`allocate`] | `zutil.c` (`zcalloc`, `zcfree`) | the caller-supplied allocator, injected as a type |
//! | [`error`] | `zutil.c` (`z_errmsg`, `zError`) + `zlib.h` `Z_*` | return codes and their messages |
//! | [`config`] | `zlib.h`, `zconf.h` | the `deflateInit2_`/`inflateInit2_` parameter sets and every bound |
//! | [`read_buf`] | `deflate.c` (`read_buf`) | safe cursors over the caller's input and output buffers |
//! | [`weak_slice`] | `deflate.h`, `deflate.c` | window, hash-chain and pending-buffer views |
//! | [`mod@adler32`] | `adler32.c` | Adler-32, its combine operation and its backends |
//! | [`mod@crc32`] | `crc32.c`, `crc32.h` | CRC-32, the braided path, combine, and the `const` tables |
//! | [`deflate`] | `deflate.c`, `deflate.h` | the DEFLATE compressor and all five block strategies |
//! | [`trees`] | `trees.c`, `trees.h` | Huffman tree construction and bit emission |
//! | [`inflate`] | `inflate.c`, `inflate.h`, `inftrees.c`, `inffast.c`, `inffixed.h` | the decompressor |
//! | [`infback`] | `infback.c` | callback-driven decompression |
//! | [`mod@compress`] | `compress.c` | the one-shot compression wrappers |
//! | [`mod@uncompress`] | `uncompr.c` | the one-shot decompression wrappers |
//! | [`gz`] | `gzlib.c`, `gzread.c`, `gzwrite.c`, `gzclose.c`, `gzguts.h` | the `gzFile` layer |
//!
//! The mapping departs from the C layout in exactly two places, both deliberate:
//!
//! * `inftrees.c` and `inffast.c` are **folded into [`inflate`]** as `inflate::inftrees` and
//!   `inflate::inffast`. They are not independent units: `inflate.c`, `inffast.c`, `inftrees.c`
//!   and `infback.c` all include the identical four-header set, and the two folded files exist
//!   only to serve the decoder. [`infback`] stays at crate level because its entry points are
//!   distinct public API.
//! * `zutil.c` is **split by concern** into [`allocate`] and [`error`] rather than transcribed
//!   as one grab-bag module. Its remaining introspection duties -- `zlibVersion`,
//!   `zlibCompileFlags`, `zError` and `get_crc_table` -- surface through the facade's `util.rs`,
//!   because each of them is a C-ABI concern.
//!
//! [`trees`] is a *sibling* of [`deflate`] and not a layer of its own, because `trees.c`
//! includes only `deflate.h`: the Huffman coder is intrinsically part of the compressor and
//! operates on `&mut DeflateState`, which is why its `_tr_*` entry points are crate-private.
//!
//! # Public surface and visibility discipline
//!
//! Two sibling crates consume this one by path -- `libz-rs-sys`, which builds the 96
//! `extern "C"` exports on top of it, and `zlib-rs-differential`, whose suites compare this
//! implementation against the C oracle byte for byte. Between them they fix the surface:
//!
//! * Every module below is `pub`, so an engine entry point is always reachable at its natural
//!   path -- `deflate::deflate`, `inflate::inflate`, `infback::inflate_back`. The names
//!   re-exported at the root are the ones a caller reaches for constantly: the state and stream
//!   types, the configuration types and bounds, the checksums, the one-shot wrappers, and the
//!   generated tables.
//! * Anything the C sources declared `local` (file-static) or `ZLIB_INTERNAL` is `pub(crate)`
//!   here, mirroring `zlib.map`. `inflate_table`, `inflate_fixed`, `inflate_fast`, the raw
//!   `z_errmsg` array, `gz_error`, `gz_intmax`, the whole `_tr_*` API and every ported helper
//!   such as `longest_match`, `fill_window`, `build_tree` or `gz_look` are invisible outside
//!   this crate.
//! * The generated tables are the exception that proves the rule: `CRC_TABLE` and its braided
//!   and big-endian siblings, [`static_ltree`], [`static_dtree`], [`_dist_code`],
//!   [`_length_code`], [`base_length`], [`base_dist`], [`lenfix`], [`distfix`] and
//!   [`CONFIGURATION_TABLE`] are public **data** even though the functions that consume them are
//!   not. Comparing them element for element against `crc32.h`, `trees.h` and `inffixed.h` is
//!   the cheapest high-signal check in the whole port, and it needs them reachable.
//!
//! # Behaviour preservation
//!
//! This crate is held to a stricter standard than RFC conformance. RFC 1951 constrains the
//! *format*, not the encoder's *choices*, and zlib's specific choices are what callers actually
//! depend on -- so the compressed output must be byte-identical to the reference for every
//! level, window size, memory level, strategy and flush mode. That makes a handful of
//! deliberately non-optimal heuristics load-bearing, and each is a verbatim port: the per-level
//! [`CONFIGURATION_TABLE`], the hash-chain walk and early-rejection ordering in
//! `deflate::longest_match`, the `TOO_FAR` lazy-match rejection, the depth tie-breaker in
//! `trees::build`, and the integer-truncating block-type comparison. A "better" DEFLATE encoder
//! would be a regression here, so improving any of them is prohibited rather than merely
//! discouraged.
//!
//! Optimisation is admissible only where it cannot perturb a single emitted byte -- which is why
//! the `simd` feature is scoped strictly to the two checksums, whose results are one scalar
//! however they are computed.
//!
//! # Feature flags
//!
//! | Feature | Default | Effect |
//! |---|---|---|
//! | `std` | off | Opts the core into `std` and compiles in the [`gz`] file layer |
//! | `simd` | off | Vectorised Adler-32 and CRC-32 backends; output-neutral by construction |
//! | `rust-api` | off | Reserved for the idiomatic layer; see below |
//!
//! The default build is `no_std` plus `alloc`, with scalar backends and no file I/O -- exactly
//! what the C ABI facade consumes.
//!
//! **There is no `gz` feature on this crate.** `gz` belongs to `libz-rs-sys`, is enabled by
//! default there, and switches on `zlib-rs/std`. The [`gz`] module is therefore gated on
//! `feature = "std"`, because a `gzFile` is a real file and `std::fs`/`std::io` are the only
//! part of this port that needs `std` at all. A `--no-default-features` build compiles that
//! subtree out entirely and is genuinely `no_std`.
//!
//! `rust-api` is declared, resolvable and guaranteed to compile, and it deliberately gates
//! nothing today. The typed surface it nominally describes -- [`DeflateConfig`], [`Strategy`],
//! [`Flush`], the slice-based wrappers -- is *required unconditionally* by `libz-rs-sys`, which
//! does not enable `rust-api`; gating any of it would break the drop-in library outright. The
//! feature is kept so that a future surface which is genuinely additive has a home, and so that
//! consumers such as the differential harness can request it without their manifest going stale.
//!
//! # Minimum supported Rust version
//!
//! Stable **1.80**, edition 2021, as declared by `rust-version` in the workspace manifest and
//! mirrored by `msrv` in `clippy.toml`. Nothing in this crate uses an unstable feature: there is
//! no `#![feature(...)]` anywhere, so it builds on the stable channel with no exceptions.
//! Nightly is reached only through an explicit `cargo +nightly` for Miri, and edition 2024 is
//! not an option at this floor because it requires rustc 1.85.
//!
//! # Version identity
//!
//! The library version is **not** defined here, and must never be derived from
//! `CARGO_PKG_VERSION`. `zlib.h` is the single source of truth -- `ZLIB_VERSION` is
//! `"1.3.2.1-motley"` and `ZLIB_VERNUM` is `0x1321` -- and reproducing those strings is the
//! facade's job, because they exist to be compared against a *caller's* compile-time values by
//! `deflateInit_` and `inflateInit_`. The Cargo version is a semver stand-in for a
//! four-component number Cargo cannot express; publishing it as the zlib version would make
//! every correctly built caller fail its own version check with `Z_VERSION_ERROR`.
//!
//! # Examples
//!
//! The checksums are the whole of the always-available surface, since they need neither an
//! allocator nor a stream:
//!
//! ```
//! use zlib_rs::{adler32, crc32, get_crc_table};
//!
//! // The published CRC-32 check value of the nine ASCII digits, and the first non-trivial
//! // entry of the table `get_crc_table` hands back (`crc32.h`).
//! assert_eq!(crc32(0, b"123456789"), 0xcbf4_3926);
//! assert_eq!(get_crc_table()[1], 0x7707_3096);
//!
//! // Adler-32 starts from 1, per RFC 1950, and chunking cannot change the answer.
//! let one_shot = adler32(1, b"hello, hello!");
//! let chunked = adler32(adler32(1, b"hello, "), b"hello!");
//! assert_eq!(one_shot, chunked);
//! ```
//!
//! A full round trip through the one-shot wrappers -- the ports of `compress2()` and
//! `uncompress()`, which between them exercise the encoder, the Huffman coder, the decoder and
//! the zlib container:
//!
//! ```
//! use zlib_rs::{compress2, compress_bound, uncompress, ReturnCode, Z_BEST_COMPRESSION};
//!
//! let source = b"hello, hello! and hello again, hello.";
//!
//! // `compress_bound` is the size the caller must supply; it is never an underestimate.
//! let mut packed = vec![0_u8; compress_bound(source.len())];
//! let done = compress2(&mut packed, source, Z_BEST_COMPRESSION);
//! assert_eq!(done.code, ReturnCode::OK);
//! packed.truncate(done.produced);
//!
//! let mut unpacked = vec![0_u8; source.len()];
//! let back = uncompress(&mut unpacked, &packed);
//! assert_eq!(back.code, ReturnCode::OK);
//! assert_eq!(&unpacked[..back.produced], source);
//! assert_eq!(back.consumed, packed.len());
//! ```

// =============================================================================
//  Crate attributes
// =============================================================================
//
//  These four lines are the crate's contract with the rest of the workspace.
//  Nothing below may weaken them: there is no `#![allow(unsafe_code)]` and no
//  `#![feature(...)]` anywhere in this crate, and the lint *levels* for clippy
//  come from the root `[workspace.lints]` table that Cargo.toml opts into --
//  restating `deny(clippy::all, clippy::pedantic)` here would only create a
//  second place for the policy to drift.
// =============================================================================

// The load-bearing line of the entire port.
//
// The root manifest's `[workspace.lints]` table deliberately omits `unsafe_code`, because the
// facade crate needs `unsafe` at the FFI boundary and a workspace-wide prohibition would make it
// unbuildable. This attribute is therefore the *only* place the safe core's guarantee is
// asserted, and it is `forbid` rather than `deny` so that no module, and no test module, can
// reintroduce `unsafe` with a local `#[allow]`.
#![forbid(unsafe_code)]
// `no_std` unless the caller asks for `std`, rather than an unconditional `#![no_std]`.
//
// The conditional form is what makes the `gz` layer possible. A `gzFile` is a real operating
// system file, so `gz` needs `std::fs` and `std::io` -- it is the only part of this crate that
// needs `std` at all -- and the module is gated on the same feature below. With the feature off
// the crate is genuinely `no_std`: the subtree is not compiled, and `alloc` is reached through
// the `extern crate` declaration that follows.
#![cfg_attr(not(feature = "std"), no_std)]
// Every public item carries documentation, and the compiler checks it. The workspace table
// omits `missing_docs` on purpose -- applied there it would also fire across the facade's
// `#[no_mangle]` shims, whose documentation is `zlib.h` itself -- so it belongs here, on the
// crate that owns the idiomatic surface.
#![deny(missing_docs)]

// The engine allocates: the window, the hash chains, the pending buffer, the symbol buffer and
// the inflate window are all heap blocks whose sizes depend on `windowBits` and `memLevel`.
// Under `#![no_std]`, `alloc` is not in the extern prelude, so it is declared here for the whole
// crate. Note that caller-visible memory does *not* come from Rust's global allocator: it comes
// from the caller's `zalloc` through `allocate::Allocator`, and is returned to the matching
// `zfree`, because that is the contract `zlib.h` documents and `test/infcover.c` verifies.
extern crate alloc;

// Puts `std` in the extern prelude for the `gz` subtree, whose modules name `std::fs`,
// `std::io` and `std::path` directly. Redundant when the crate is not `no_std`, and harmless:
// `unused_extern_crates` is allow-by-default and the workspace table pointedly does not promote
// it, precisely so that `extern crate` declarations like this one and the `alloc` one above stay
// warning-free in every feature configuration.
#[cfg(feature = "std")]
extern crate std;

// =============================================================================
//  The module tree
// =============================================================================
//
//  One module per C translation unit, in dependency order: the foundations
//  first, then the checksums, then the two engines, then the layers built on
//  top of them.  Each module's own `//!` documentation cites the file and line
//  ranges it was ported from.
//
//  Every module is `pub`, which is what makes the engine reachable at its
//  natural path (`deflate::deflate`, `inflate::inflate`) for the facade and the
//  differential harness.  That is not a licence to expose internals: each
//  module publishes only its own entry points and keeps every ported `local`
//  helper `pub(crate)`, mirroring the `local:` block of `zlib.map`.
// =============================================================================

/// The allocator abstraction: the caller's `(zalloc, zfree, opaque)` triple as a Rust type.
///
/// Ported from `zcalloc` and `zcfree` (`zutil.c` L215-L253). Injecting the allocator rather
/// than assuming a global one is what makes the C contract explicit in the type system: memory
/// obtained from a caller's `zalloc` can only be handed back to that same caller's `zfree`.
pub mod allocate;

/// Return codes and the messages that accompany them.
///
/// Ported from the `Z_*` constants (`zlib.h` L181-L189) and the `z_errmsg[10]` table
/// (`zutil.c` L13-L24). The raw table stays crate-private because `zlib.map` hides `z_errmsg`;
/// only the accessors are public.
pub mod error;

/// The `deflateInit2_` and `inflateInit2_` parameter sets, and every bound they are checked
/// against.
///
/// Ported from `zlib.h` and `zconf.h`. This is the single definition of [`Flush`],
/// [`Strategy`], [`Method`] and the window-bits classification, and the single place the
/// `windowBits`/`memLevel`/`level` ranges are enforced.
pub mod config;

/// Safe cursors over the caller's input and output buffers.
///
/// Ported from `read_buf` (`deflate.c` L219-L250). Replaces the reference's pointer arithmetic
/// with a slice plus an index, and folds the running Adler-32 or CRC-32 update into the copy
/// exactly where C does.
pub mod read_buf;

/// The sliding window, the hash chains and the pending buffer, as borrow-checked views.
///
/// Ported from the `deflate_state` buffer members (`deflate.h`) and the pointer walks in
/// `deflate.c`. The views hand out the disjoint sub-slices the match finder needs without
/// reintroducing the aliasing that the raw-pointer original depends on.
pub mod weak_slice;

/// Adler-32: the checksum RFC 1950 puts in the zlib trailer.
///
/// Ported from `adler32.c`, including the `NMAX` chunking schedule and `adler32_combine_`.
pub mod adler32;

/// CRC-32: the checksum RFC 1952 puts in the gzip trailer.
///
/// Ported from `crc32.c` and `crc32.h`. The tables are `const`-evaluated rather than built at
/// run time, which removes the reference's unsynchronised `DYNAMIC_CRC_TABLE` initialisation and
/// its `<stdatomic.h>` dependency outright.
pub mod crc32;

/// The DEFLATE compressor.
///
/// Ported from `deflate.c` and `deflate.h`. This is where byte-identical output is decided: the
/// per-level [`CONFIGURATION_TABLE`], the hash-chain walk, the lazy-match rejection and the
/// block-type choice are verbatim ports, and improving any of them is prohibited.
pub mod deflate;

/// Huffman tree construction and bit emission.
///
/// Ported from `trees.c` and `trees.h`. A sibling of [`deflate`] rather than a layer of its own,
/// because `trees.c` includes only `deflate.h`: the coder operates on `&mut DeflateState`, so
/// the `_tr_*` entry points are crate-private and only the generated tables are public.
pub mod trees;

/// The decompressor, with `inftrees` and `inffast` folded in.
///
/// Ported from `inflate.c`, `inflate.h`, `inftrees.c`, `inffast.c` and `inffixed.h`. The
/// reference's `inflate_mode` enumeration becomes an exhaustively matched [`Mode`], which turns
/// "a state was added but never handled" from a silent fall-through into a compile error.
pub mod inflate;

/// Callback-driven decompression: the `inflateBack` interface.
///
/// Ported from `infback.c`. Kept at crate level rather than folded into [`inflate`] because its
/// entry points are distinct public API; the C `in`/`out` function pointers become a pair of
/// traits here, and the facade adapts the raw pointers to them.
pub mod infback;

/// The one-shot compression wrappers.
///
/// Ported from `compress.c`: `compress`, `compress2`, `compressBound` and their `_z`
/// `size_t`-aware forms, which are the primary implementations.
pub mod compress;

/// The one-shot decompression wrappers.
///
/// Ported from `uncompr.c`: `uncompress`, `uncompress2` and their `_z` forms. (The file is
/// `uncompr.c`, not `uncompress.c`; the module is named for the function.)
pub mod uncompress;

/// The `gzFile` layer: gzip files as stdio-shaped streams.
///
/// Ported from `gzlib.c`, `gzread.c`, `gzwrite.c`, `gzclose.c` and `gzguts.h`.
///
/// Gated on `feature = "std"`, and on nothing else -- there is no `gz` feature on this crate.
/// `gz` is a feature of `libz-rs-sys`, where it is on by default and enables `zlib-rs/std`.
/// This subtree is the only part of the port that needs `std`, because `#include <stdio.h>` and
/// `<fcntl.h>` become `std::fs` and `std::io` here, and a `--no-default-features` build compiles
/// it out entirely.
#[cfg(feature = "std")]
pub mod gz;

// =============================================================================
//  The crate root's re-exported surface
// =============================================================================
//
//  Convenience, never additional capability: every name below is already public
//  at its module path, and nothing is re-exported that was not.  Two rules keep
//  the barrel honest.
//
//  1. Each name is imported exactly ONCE, from the module that DEFINES it.
//     Several names have more than one path -- `Flush` is defined in `config`
//     and re-exported by both `deflate::algorithm` and `deflate`, for instance --
//     and importing the same name twice is a hard error, so the defining module
//     is always the one used here.
//
//  2. Where two modules spell the same C constant differently, only one
//     spelling reaches the root.  `weak_slice` declares `MIN_MATCH`, `MAX_MATCH`,
//     `MIN_WBITS` and `MAX_WBITS` for the window views it hands out, and
//     `config` declares them for the bounds it enforces -- the same numbers from
//     `zutil.h` and `zconf.h`, but `config`'s window-bits pair is `i32` (the
//     type of the `windowBits` argument that is actually range-checked) while
//     `weak_slice`'s is `u32`.  The `config` spellings are the ones exported
//     here; `weak_slice`'s remain reachable at `weak_slice::`.
//
//  The engine entry points are deliberately NOT flattened into the root.  The
//  facade's 20 `deflate*`, 18 `inflate*` and 3 `inflateBack*` exports call them
//  as `deflate::deflate_init2`, `inflate::inflate`, `infback::inflate_back` and
//  so on, which keeps the C function each one implements obvious and avoids 41
//  bare verbs at the crate root.
// =============================================================================

// -----------------------------------------------------------------------------
//  Status reporting
// -----------------------------------------------------------------------------

/// The status of every fallible operation -- see [`error::ReturnCode`].
pub use crate::error::ReturnCode;

/// The message for a status code, i.e. the whole body of C's `zError` -- see [`error::err_msg`].
pub use crate::error::err_msg;

// -----------------------------------------------------------------------------
//  Allocation
// -----------------------------------------------------------------------------

/// The caller-supplied allocator contract -- see [`allocate::Allocator`].
///
/// The facade implements this over the `(zalloc, zfree, opaque)` triple in the caller's
/// `z_stream`; [`GlobalAllocator`] is the stand-in for C's "both null, use `malloc`" default.
pub use crate::allocate::Allocator;

/// Identity of the allocator that owns a block, so a block can only be freed by its owner --
/// see [`allocate::AllocatorId`].
pub use crate::allocate::AllocatorId;

/// An owned allocation, allocator-agnostic -- see [`allocate::Buffer`].
pub use crate::allocate::Buffer;

/// The default allocator, used when the caller supplies neither `zalloc` nor `zfree` --
/// see [`allocate::GlobalAllocator`].
pub use crate::allocate::GlobalAllocator;

/// The caller's `opaque` cookie, passed back to `zalloc` and `zfree` untouched --
/// see [`allocate::Opaque`].
pub use crate::allocate::Opaque;

/// The outcome of returning a [`Buffer`] to an allocator -- see [`allocate::Release`].
pub use crate::allocate::Release;

// -----------------------------------------------------------------------------
//  Configuration
// -----------------------------------------------------------------------------

/// The `deflateInit2_` parameter set -- see [`config::DeflateConfig`].
pub use crate::config::DeflateConfig;

/// A [`DeflateConfig`] whose every field has been range-checked --
/// see [`config::ValidatedDeflateConfig`].
pub use crate::config::ValidatedDeflateConfig;

/// The `inflateInit2_` parameter set -- see [`config::InflateConfig`].
pub use crate::config::InflateConfig;

/// An [`InflateConfig`] whose every field has been range-checked --
/// see [`config::ValidatedInflateConfig`].
pub use crate::config::ValidatedInflateConfig;

/// The compression method, of which `Z_DEFLATED` is the only one -- see [`config::Method`].
pub use crate::config::Method;

/// The five compression strategies -- see [`config::Strategy`].
pub use crate::config::Strategy;

/// The seven flush modes, `Z_NO_FLUSH` through `Z_TREES` -- see [`config::Flush`].
///
/// The single definition in the crate: `deflate` re-exports this very type, so
/// `deflate::Flush` and this name are one item.
pub use crate::config::Flush;

/// What container the compressor wraps its DEFLATE stream in -- see [`config::Wrap`].
pub use crate::config::Wrap;

/// What containers the decompressor will accept -- see [`config::InflateWrap`].
pub use crate::config::InflateWrap;

// -----------------------------------------------------------------------------
//  Bounds and level constants
// -----------------------------------------------------------------------------

/// `Z_NO_COMPRESSION` (0) -- see [`config::Z_NO_COMPRESSION`].
pub use crate::config::Z_NO_COMPRESSION;

/// `Z_BEST_SPEED` (1) -- see [`config::Z_BEST_SPEED`].
pub use crate::config::Z_BEST_SPEED;

/// `Z_BEST_COMPRESSION` (9) -- see [`config::Z_BEST_COMPRESSION`].
pub use crate::config::Z_BEST_COMPRESSION;

/// `Z_DEFAULT_COMPRESSION` (-1), which resolves to level 6 --
/// see [`config::Z_DEFAULT_COMPRESSION`].
pub use crate::config::Z_DEFAULT_COMPRESSION;

/// `Z_DEFLATED` (8), the only method `zlib.h` defines -- see [`config::Z_DEFLATED`].
pub use crate::config::Z_DEFLATED;

/// `MIN_WBITS` (8) -- see [`config::MIN_WBITS`].
pub use crate::config::MIN_WBITS;

/// `MAX_WBITS` (15), the 32 KiB window -- see [`config::MAX_WBITS`].
pub use crate::config::MAX_WBITS;

/// `DEF_WBITS` (15), what `deflateInit` and `inflateInit` supply --
/// see [`config::DEF_WBITS`].
pub use crate::config::DEF_WBITS;

/// `MIN_MEM_LEVEL` (1) -- see [`config::MIN_MEM_LEVEL`].
pub use crate::config::MIN_MEM_LEVEL;

/// `MAX_MEM_LEVEL` (9) -- see [`config::MAX_MEM_LEVEL`].
pub use crate::config::MAX_MEM_LEVEL;

/// `DEF_MEM_LEVEL` (8), what `deflateInit` supplies -- see [`config::DEF_MEM_LEVEL`].
pub use crate::config::DEF_MEM_LEVEL;

/// `MIN_MATCH` (3), the shortest LZ77 match -- see [`config::MIN_MATCH`].
pub use crate::config::MIN_MATCH;

/// `MAX_MATCH` (258), the longest LZ77 match -- see [`config::MAX_MATCH`].
pub use crate::config::MAX_MATCH;

/// `PRESET_DICT` (0x20), the zlib header flag for a preset dictionary --
/// see [`config::PRESET_DICT`].
pub use crate::config::PRESET_DICT;

// -----------------------------------------------------------------------------
//  Checksums
// -----------------------------------------------------------------------------

/// Adler-32 over a slice -- see [`adler32::adler32`].
pub use crate::adler32::adler32;

/// Adler-32 with a `size_t`-shaped length, the primary implementation --
/// see [`adler32::adler32_z`].
pub use crate::adler32::adler32_z;

/// Concatenates two Adler-32 checksums -- see [`adler32::adler32_combine`].
///
/// Backs both `adler32_combine` and `adler32_combine64` in the facade, because the two differ
/// only in the C type of their length argument.
pub use crate::adler32::adler32_combine;

/// The swappable Adler-32 backend -- see [`adler32::Adler32Backend`].
pub use crate::adler32::Adler32Backend;

/// CRC-32 over a slice -- see [`crc32::crc32`].
pub use crate::crc32::crc32;

/// CRC-32 with a `size_t`-shaped length, the primary implementation -- see [`crc32::crc32_z`].
pub use crate::crc32::crc32_z;

/// The 256-entry CRC-32 table C callers may ask for -- see [`crc32::get_crc_table`].
pub use crate::crc32::get_crc_table;

/// Concatenates two CRC-32 checksums -- see [`crc32::crc32_combine`].
pub use crate::crc32::crc32_combine;

/// The 64-bit-length form of [`crc32_combine`] -- see [`crc32::crc32_combine64`].
pub use crate::crc32::crc32_combine64;

/// Precomputes the operator [`crc32_combine_op`] applies -- see [`crc32::crc32_combine_gen`].
pub use crate::crc32::crc32_combine_gen;

/// The 64-bit-length form of [`crc32_combine_gen`] -- see [`crc32::crc32_combine_gen64`].
pub use crate::crc32::crc32_combine_gen64;

/// Applies a precomputed combine operator -- see [`crc32::crc32_combine_op`].
pub use crate::crc32::crc32_combine_op;

/// The swappable CRC-32 backend -- see [`crc32::Crc32Backend`].
pub use crate::crc32::Crc32Backend;

// -----------------------------------------------------------------------------
//  Engine state and the buffer views the engines are driven through
// -----------------------------------------------------------------------------

/// The compressor's state, everything C keeps behind `z_stream.state` --
/// see [`deflate::DeflateState`].
pub use crate::deflate::DeflateState;

/// The caller-visible half of a compression stream: the buffers, the cursors and the scalars
/// C keeps in `z_stream` itself -- see [`deflate::DeflateStream`].
pub use crate::deflate::DeflateStream;

/// The decompressor's state -- see [`inflate::InflateState`].
pub use crate::inflate::InflateState;

/// The caller-visible half of a decompression stream -- see [`inflate::InflateStream`].
pub use crate::inflate::InflateStream;

/// The decoder's state machine, one variant per C `inflate_mode` -- see [`inflate::Mode`].
///
/// Public for the state and differential suites, and not part of the ABI: an `inflate_state`
/// only ever reaches a C caller through the opaque `z_stream.state` pointer.
pub use crate::inflate::Mode;

/// The `inflateBack` input callback, C's `in()` -- see [`infback::InflateBackInput`].
pub use crate::infback::InflateBackInput;

/// The `inflateBack` output callback, C's `out()` -- see [`infback::InflateBackOutput`].
pub use crate::infback::InflateBackOutput;

/// A read cursor over the caller's input buffer -- see [`read_buf::InputCursor`].
pub use crate::read_buf::InputCursor;

/// A write cursor over the caller's output buffer -- see [`read_buf::OutputCursor`].
pub use crate::read_buf::OutputCursor;

/// The `gzFile` state -- see [`gz::state::GzState`].
///
/// Gated exactly as the [`gz`] module is: an ungated re-export would name a module that the
/// default `no_std` build does not compile.
#[cfg(feature = "std")]
pub use crate::gz::GzState;

// -----------------------------------------------------------------------------
//  One-shot wrappers
// -----------------------------------------------------------------------------

/// Compresses in one call at the default level -- see [`compress::compress`].
pub use crate::compress::compress;

/// The `size_t`-shaped form of [`compress()`] -- see [`compress::compress_z`].
pub use crate::compress::compress_z;

/// Compresses in one call at a chosen level -- see [`compress::compress2`].
pub use crate::compress::compress2;

/// The `size_t`-shaped form of [`compress2`], and the primary implementation --
/// see [`compress::compress2_z`].
pub use crate::compress::compress2_z;

/// The destination size a caller must supply -- see [`compress::compress_bound`].
///
/// Never an underestimate. A caller sizes its buffer with this, so the number must match the
/// reference exactly: too small would overflow the *caller's* buffer.
pub use crate::compress::compress_bound;

/// The `size_t`-shaped form of [`compress_bound`] -- see [`compress::compress_bound_z`].
pub use crate::compress::compress_bound_z;

/// What a one-shot compression produced -- see [`compress::Compressed`].
pub use crate::compress::Compressed;

/// Decompresses in one call -- see [`uncompress::uncompress`].
pub use crate::uncompress::uncompress;

/// The `size_t`-shaped form of [`uncompress()`] -- see [`uncompress::uncompress_z`].
pub use crate::uncompress::uncompress_z;

/// Decompresses in one call, reporting input consumed as well --
/// see [`uncompress::uncompress2`].
pub use crate::uncompress::uncompress2;

/// The `size_t`-shaped form of [`uncompress2`], and the primary implementation --
/// see [`uncompress::uncompress2_z`].
pub use crate::uncompress::uncompress2_z;

/// What a one-shot decompression produced and consumed -- see [`uncompress::Decompressed`].
pub use crate::uncompress::Decompressed;

// -----------------------------------------------------------------------------
//  The generated tables
// -----------------------------------------------------------------------------
//
//  Public DATA whose consuming FUNCTIONS stay crate-private, which is exactly
//  the asymmetry `zlib.map` describes: `inflate_table`, `inflate_fixed`,
//  `inflate_fast` and the `_tr_*` family are all hidden, while the arrays they
//  read are the port's cheapest correctness check.
//  `crates/zlib-rs-differential/tests/table_equality.rs` compares every one of
//  them element for element against `crc32.h`, `trees.h` and `inffixed.h`, and a
//  transcription slip anywhere in these thousands of literals shows up there
//  immediately -- long before any stream is compressed.
// -----------------------------------------------------------------------------

/// The byte-at-a-time CRC-32 table -- see [`crc32::tables::CRC_TABLE`].
pub use crate::crc32::CRC_TABLE;

/// The word-sized big-endian CRC-32 table -- see [`crc32::tables::CRC_BIG_TABLE`].
pub use crate::crc32::CRC_BIG_TABLE;

/// The braided little-endian CRC-32 tables -- see [`crc32::tables::CRC_BRAID_TABLE`].
pub use crate::crc32::CRC_BRAID_TABLE;

/// The braided big-endian CRC-32 tables -- see [`crc32::tables::CRC_BRAID_BIG_TABLE`].
pub use crate::crc32::CRC_BRAID_BIG_TABLE;

/// The `x^2^n mod p(x)` table behind the CRC-32 combine operations --
/// see [`crc32::tables::X2N_TABLE`].
pub use crate::crc32::X2N_TABLE;

/// The per-level tuning table, the first of the eight byte-identity decision points --
/// see [`deflate::CONFIGURATION_TABLE`].
pub use crate::deflate::CONFIGURATION_TABLE;

/// One row of [`CONFIGURATION_TABLE`] -- see [`deflate::Config`].
pub use crate::deflate::Config;

/// The static literal/length tree -- see [`trees::static_ltree`].
pub use crate::trees::static_ltree;

/// The static distance tree -- see [`trees::static_dtree`].
pub use crate::trees::static_dtree;

/// Distance-to-code mapping -- see [`trees::_dist_code`].
pub use crate::trees::_dist_code;

/// Length-to-code mapping -- see [`trees::_length_code`].
pub use crate::trees::_length_code;

/// First length of each length code -- see [`trees::base_length`].
pub use crate::trees::base_length;

/// First distance of each distance code -- see [`trees::base_dist`].
pub use crate::trees::base_dist;

/// The fixed literal/length decode table -- see [`inflate::lenfix`].
pub use crate::inflate::lenfix;

/// The fixed distance decode table -- see [`inflate::distfix`].
pub use crate::inflate::distfix;
