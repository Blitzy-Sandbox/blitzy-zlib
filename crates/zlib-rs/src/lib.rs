//! `zlib-rs` -- the safe, dependency-free algorithmic core of the Rust zlib
//! implementation.
//!
//! Every compression and decompression algorithm the reference implementation defines lives in
//! this crate, written from scratch in idiomatic safe Rust: the DEFLATE encoder and its Huffman
//! coder, the resumable inflate state machine, the callback-driven `inflateBack` decoder,
//! Adler-32 and CRC-32, the gzip file layer, and the one-shot `compress`/`uncompress` wrappers.
//! The in-tree C sources are the algorithmic oracle for all of it; they are read and never
//! edited.
//!
//! This crate exports **no C symbol**, and **every unsafe operation -- every dereference, every
//! FFI call, every layout transmutation -- is confined to `crates/libz-rs-sys`**, the facade that
//! reproduces the `libz` ABI on top of what is defined here.
//!
//! What this crate does *not* claim is that no raw pointer value ever appears in it. Two do, and
//! both are inert by construction, because `#![forbid(unsafe_code)]` makes dereferencing them
//! impossible here:
//!
//! * [`allocate::Opaque`] wraps the caller's `opaque` cookie as a `*mut c_void`. It is stored,
//!   copied and handed back verbatim; the core never reads through it. Only the facade, which
//!   passes it to the caller's `zalloc`/`zfree`, ever gives it meaning.
//! * `gz::state::GzFileExposed::next` is a `*mut u8`, because the `gzgetc` macro in `zlib.h`
//!   dereferences and post-increments that exact field inside *caller* object code. The core
//!   only recomputes it from an index (see `gz::state`).
//!
//! Likewise, this crate contains exactly **two** `#[repr(C)]` types, not one, and both are in the
//! gzip layer for the same caller-macro reason: `gz::state::GzFileExposed` (the 24-byte
//! `gzFile_s` prefix) and its container `gz::state::GzState`, whose `repr(C)` is what pins that
//! prefix at offset 0. Every other type in the crate has a Rust-chosen layout.
//!
//! # Where `unsafe` lives, and why it is not here
//!
//! The algorithms are memory-safe by construction rather than by convention, and the crate root
//! is where that claim is made enforceable:
//!
//! ```text
//!   crates/zlib-rs          #![forbid(unsafe_code)]           <-- this crate. No unsafe, ever.
//!         |                 no_std + alloc, zero dependencies
//!         v
//!   crates/libz-rs-sys      #![deny(unsafe_op_in_unsafe_fn)]  <-- the only unsafe there is
//!         |                 extern "C", #[no_mangle], repr(C)
//!         v
//!   libz.so / libz.a        the drop-in replacement C callers link against
//! ```
//!
//! `unsafe` is unavoidable at an FFI edge; it is *not* unavoidable behind one. The facade
//! validates each raw `z_streamp`, rebuilds the `(next_in, avail_in)` and
//! `(next_out, avail_out)` slices exactly once on entry, round-trips the opaque `state` pointer,
//! calls the caller's `zalloc`/`zfree`, handles C strings and varargs, and writes through the
//! caller's `gz_header`. Everything past that boundary -- which is everything in this crate --
//! is ordinary safe Rust operating on slices and indices, so Miri can analyse it exhaustively
//! precisely because it contains no FFI.
//!
//! Three consequences are worth stating outright, because the safety argument rests on them:
//!
//! * **The prohibition is asserted here and nowhere else.** The workspace `[lints]` table
//!   deliberately omits `unsafe_code`, because the facade needs it and a workspace-wide `forbid`
//!   would make that crate unbuildable. `forbid` -- not `deny` -- is used below on purpose: it
//!   cannot be relaxed by an `#[allow]` anywhere in the crate, not even in a test module.
//! * **The window is a slice, not a pointer.** `deflate.c` walks the sliding window with raw
//!   pointer arithmetic (`Bytef *scan = s->window + s->strstart`), guarded only by debug-only
//!   `Assert`s such as `"wild scan"`. Here the window, the hash chains and the pending buffer
//!   are owned buffers addressed by integer cursors, which retires that entire class of defect.
//!   [`weak_slice`] holds the split-borrow views the match finder needs without reintroducing
//!   aliasing.
//! * **Nothing in this crate can become a linker symbol.** No item is `#[no_mangle]` and there
//!   is no `extern "C"` or `extern "C-unwind"` anywhere, so the `local:` block of `zlib.map` --
//!   which hides `inflate_table`, `inflate_fast`, `inflate_fixed`, `z_errmsg`, `gz_error`,
//!   `gz_intmax` and every `_*` name -- is satisfied by construction rather than by the linker.
//!   The only `#[repr(C)]` the port permits in this crate is the 24-byte `gzFile_s` prefix in
//!   `gz::state` plus the `gz::state::GzState` container that pins it at offset 0, and they
//!   exist only because the `gzgetc` macro in `zlib.h` compiles field arithmetic into *caller*
//!   object code. `#[repr(C)]` fixes a layout; it does not create a symbol, so the
//!   linker-visibility argument above is unaffected by it.
//!
//! # Module map
//!
//! The module tree mirrors the C translation units one for one, so a maintainer diffing a Rust
//! module against its C source is comparing like with like. Every module's own documentation
//! cites the file and line ranges its behaviour is fixed by.
//!
//! | Module | C source | Responsibility |
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
//! | `gz` | `gzlib.c`, `gzread.c`, `gzwrite.c`, `gzclose.c`, `gzguts.h` | the `gzFile` layer |
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
//!   `zlibCompileFlags`, `zError` and `get_crc_table` -- surface through the facade, because
//!   each of them is a C-ABI concern.
//!
//! [`trees`] is a *sibling* of [`deflate`] and not a layer of its own, because `trees.c`
//! includes only `deflate.h`: the Huffman coder is intrinsically part of the compressor and
//! operates on `&mut DeflateState`, which is why its `_tr_*` entry points are crate-private.
//!
//! # Public surface and visibility discipline
//!
//! Two sibling crates consume this one by path -- `libz-rs-sys`, which builds the `extern "C"`
//! exports on top of it, and `zlib-rs-differential`, whose planned suites will compare this
//! implementation against the C oracle byte for byte. Between them they fix the surface.
//!
//! The export target is **95 functions**, which together with 16 symbol-version nodes make the
//! **111 dynamic globals** the reference `libz.so.1.3.2.1-motley` defines -- that is the measured
//! baseline an `nm` parity diff is run against. (`zlib.h` declares 96 distinct function names;
//! exactly 95 of them resolve off Windows, because `gzopen_w` is `_WIN32`-only.)
//!
//! * Every module below is `pub` **unconditionally**, so an engine entry point is always
//!   reachable at its natural path -- `deflate::deflate`, `inflate::inflate`,
//!   `infback::inflate_back`, `config::DeflateConfig`, `error::ReturnCode`. This is the surface
//!   `libz-rs-sys` is built on, and no feature may take any of it away.
//! * On top of that, a flat convenience surface is re-exported at the crate root -- the state and
//!   stream types, the configuration types and bounds, the checksums, the one-shot wrappers and
//!   the generated tables. That flattening is the *idiomatic layer*, and it is gated on
//!   `feature = "rust-api"`; see "Feature flags" below. Nothing is exclusive to it: every name it
//!   offers is the same item, reachable at its module path either way.
//! * Anything the C sources declared `local` (file-static) or `ZLIB_INTERNAL` is `pub(crate)`
//!   here, mirroring `zlib.map`. `inflate_table`, `inflate_fixed`, `inflate_fast`, the raw
//!   `z_errmsg` array, `gz_error`, `gz_intmax`, the whole `_tr_*` API and every ported helper
//!   such as `longest_match`, `fill_window`, `build_tree` or `gz_look` are invisible outside
//!   this crate.
//! * The generated tables are the exception that proves the rule: [`crc32::CRC_TABLE`] and its
//!   braided and big-endian siblings, [`trees::static_ltree`], [`trees::static_dtree`],
//!   [`trees::_dist_code`], [`trees::_length_code`], [`trees::base_length`],
//!   [`trees::base_dist`], [`inflate::lenfix`], [`inflate::distfix`] and
//!   [`deflate::CONFIGURATION_TABLE`] are public **data** even though the functions that consume
//!   them are not. Comparing them element for element against `crc32.h`, `trees.h` and
//!   `inffixed.h` is the cheapest high-signal check in the whole port, and it needs them
//!   reachable. They are named by module path here on purpose: that path is the one that exists
//!   in every feature configuration.
//!
//! # Behaviour preservation
//!
//! This crate is held to a stricter standard than RFC conformance. RFC 1951 constrains the
//! *format*, not the encoder's *choices*, and zlib's specific choices are what callers actually
//! depend on -- so the compressed output must be byte-identical to the reference for every
//! level, window size, memory level, strategy and flush mode. That makes a handful of
//! deliberately non-optimal heuristics load-bearing, and each is a verbatim port: the per-level
//! [`deflate::CONFIGURATION_TABLE`], the hash-chain walk and early-rejection ordering in
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
//! | `std` | off | Opts the core into `std` and compiles in the `gz` file layer |
//! | `simd` | off | Vectorised Adler-32 and CRC-32 backends; output-neutral by construction |
//! | `rust-api` | off | Adds the flat idiomatic surface at the crate root; see below |
//!
//! Building **this crate on its own** with no features gives `no_std` plus `alloc`, with scalar
//! backends and no file I/O.
//!
//! That is *not* what a default build of the C ABI facade produces, and the distinction matters
//! enough to state rather than imply. **There is no `gz` feature on this crate.** `gz` belongs to
//! `libz-rs-sys`, is **in that crate's `default` feature set**, and is defined as
//! `gz = ["zlib-rs/std"]` -- so a default `cargo build -p libz-rs-sys` turns `std` on here
//! transitively and compiles the `gz` subtree in. The `gz` module is therefore gated on
//! `feature = "std"`, because a `gzFile` is a real file and `std::fs`/`std::io` are the only
//! part of this crate that needs `std` at all. A `--no-default-features` build compiles that
//! subtree out entirely and is genuinely `no_std`.
//!
//! ## What `rust-api` gates
//!
//! `rust-api` is the "idiomatic layer" half of the port's two-surface split -- the other half
//! being `libz-compat` on `libz-rs-sys`, which builds the C ABI. Concretely, it gates **the flat
//! re-export surface at this crate's root**: the ~80 names listed in the "Re-exports" section of
//! this page, covering [`error::ReturnCode`], the [`allocate`] contract types, the
//! [`config::DeflateConfig`] / [`config::Strategy`] / [`config::Flush`] family and its bounds
//! constants, the checksum entry points, the state and stream types, the cursors, the one-shot
//! wrappers and the generated tables.
//!
//! It gates *only the flattening*. Every one of those names is the same item as the one at its
//! module path, and every module stays `pub` in every configuration, so:
//!
//! * with the feature **off** -- the default, and what `libz-rs-sys` builds against -- the engine
//!   is used as `config::DeflateConfig`, `error::ReturnCode`, `compress::compress2`,
//!   `crc32::crc32`, `deflate::CONFIGURATION_TABLE`. Nothing is missing, and nothing the facade
//!   needs is behind a feature it does not enable;
//! * with the feature **on**, those same items are additionally in scope as
//!   `zlib_rs::DeflateConfig`, `zlib_rs::ReturnCode`, `zlib_rs::compress2`, `zlib_rs::crc32`,
//!   `zlib_rs::CONFIGURATION_TABLE`, which is what a Rust caller reaching for the library wants.
//!
//! Being purely additive is what makes the split safe: enabling `rust-api` can never take a name
//! away, so Cargo's feature unification cannot break a sibling that leaves it off. That is why
//! `zlib-rs-differential` and the `fuzz/` targets both request it while `libz-rs-sys` does not.
//! One consequence worth stating: because the flattening is what the feature controls, a doc link
//! written in *ungated* documentation must name the module path -- a link to a gated root
//! re-export would dangle in the default configuration.
//!
//! The corollary covers items whose *definition* is gated rather than merely their root
//! re-export, and it is stricter: a `gz::state::GzState` or a `crc32::Simd` cannot be linked from
//! ungated documentation at all, because in a build without `std` or without `simd` the item does
//! not exist and `#![deny(rustdoc::broken_intra_doc_links)]` would fail the build. Those names are
//! therefore written as plain code spans throughout this crate's ungated prose, and only
//! documentation compiled under the same feature links them.
//!
//! | Configuration | How the same item is named |
//! |---|---|
//! | `--features rust-api` | `zlib_rs::ReturnCode`, `zlib_rs::compress2`, `zlib_rs::DeflateConfig` |
//! | default | `zlib_rs::error::ReturnCode`, `zlib_rs::compress::compress2`, `zlib_rs::config::DeflateConfig` |
//!
//! That is the split the port needs, because the two consumers want opposite things.
//! `libz-rs-sys` does **not** enable the feature: its exports call the engine by module path --
//! `deflate::deflate_init2`, `inflate::inflate`, `infback::inflate_back`, `compress::compress2` --
//! so that the C function each one implements is legible beside it, and a bare verb at the crate
//! root would cost exactly that traceability. The differential harness *does* enable it
//! (`features = ["rust-api", "std"]`), because enumerating the level × `windowBits` × `memLevel` ×
//! strategy × flush matrix in Rust is what an idiomatic surface is for.
//!
//! One consequence to keep in mind when reading the examples below: `cargo test` runs doctests
//! with default features, so every example in this crate imports by module path -- the examples
//! here and the ones in every module's own `//!` block alike, because `cargo test -p zlib-rs`
//! compiles and runs all of them with `rust-api` off. Add `--features rust-api` and the flattened
//! names become available as well.
//!
//! # Minimum supported Rust version, and version identity
//!
//! Stable **1.80**, edition 2021, as declared by `rust-version` in the workspace manifest and
//! mirrored by `msrv` in `clippy.toml`. There is no `#![feature(...)]` anywhere, so this crate
//! builds on the stable channel; nightly is reached only through an explicit `cargo +nightly`
//! for Miri, and edition 2024 is not an option at this floor because it requires rustc 1.85.
//!
//! The *library* version is **not** defined here and must never be derived from
//! `CARGO_PKG_VERSION`. `zlib.h` is the single source of truth -- `ZLIB_VERSION` is
//! `"1.3.2.1-motley"` and `ZLIB_VERNUM` is `0x1321` -- and reproducing those strings is the
//! facade's job, because a caller's compile-time `ZLIB_VERSION` is passed in and checked by the
//! initialisers. The Cargo version is a semver stand-in for a four-component number Cargo cannot
//! express; publishing it as the zlib version would make every correctly built caller fail its
//! own version check with `Z_VERSION_ERROR`.
//!
//! **What the initialisers actually compare is narrower than the whole string, and the facade
//! must reproduce it exactly.** `deflateInit_` (`deflate.c` L392-L397), `inflateInit2_`
//! (`inflate.c` L178-L180) and `inflateBackInit_` (`infback.c` L30-L32) each return
//! `Z_VERSION_ERROR` when any of three conditions holds:
//!
//! 1. `version == Z_NULL`;
//! 2. `version[0] != ZLIB_VERSION[0]` -- **the first character only**, so it is a major-version
//!    check and not a full-string comparison. A caller compiled against `"1.2.11"` passes it;
//!    one compiled against `"2.0.0"` does not. (`deflateInit_` compares against a local
//!    `static const char my_version[] = ZLIB_VERSION`; the inflate side indexes `ZLIB_VERSION`
//!    directly. The observable result is the same.)
//! 3. `stream_size != sizeof(z_stream)` -- the caller's `sizeof` is passed in by the `zlib.h`
//!    macro, so a caller built against a different `z_stream` layout is rejected here rather
//!    than corrupting memory later.
//!
//! Ordering is part of the contract too: all three run **before** `strm == Z_NULL` is tested, so
//! a null stream combined with a bad version yields `Z_VERSION_ERROR`, not `Z_STREAM_ERROR`.
//!
//! # Examples
//!
//! The checksums are the whole of the always-available surface, since they need neither an
//! allocator nor a stream. Both examples import by module path, so they are exactly what the
//! default `no_std`, no-`rust-api` build offers; add `rust-api` and the same names are reachable
//! at the crate root as well.
//!
//! ```
//! use zlib_rs::adler32::adler32;
//! use zlib_rs::crc32::{crc32, get_crc_table};
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
//! A full round trip through the one-shot wrappers -- `compress2()` and `uncompress()`, which
//! between them exercise the encoder, the Huffman coder, the decoder and the zlib container:
//!
//! ```
//! use zlib_rs::compress::{compress2, compress_bound};
//! use zlib_rs::config::Z_BEST_COMPRESSION;
//! use zlib_rs::error::ReturnCode;
//! use zlib_rs::uncompress::uncompress;
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

// The four crate attributes below are this crate's contract with the rest of the
// workspace.  Nothing may weaken them: there is no `#![allow(unsafe_code)]` and no
// `#![feature(...)]` anywhere here, and the clippy lint *levels* come from the root
// `[workspace.lints]` table that Cargo.toml opts into -- restating
// `deny(clippy::all, clippy::pedantic)` here would only create a second place for the
// policy to drift.

// The load-bearing line of the whole safety argument.
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
// -----------------------------------------------------------------------------------------
//  Documentation lints
// -----------------------------------------------------------------------------------------
//
// `#![deny(missing_docs)]` above only proves documentation EXISTS. These four prove it is
// still true: a link that no longer resolves, or that points into the crate's private
// interior, is a documentation defect exactly as a stale sentence is, and it is the kind that
// accumulates silently because `cargo build` never looks at a doc comment. Denying them makes
// `cargo doc` a gate:
//
//   cargo doc -p zlib-rs --all-features --no-deps
//
// exits non-zero on a broken link. `rustdoc::` is a tool-lint namespace, so `rustc`, `clippy`
// and `cargo build` accept these attributes and ignore them; only `rustdoc` acts on them.
//
// * `broken_intra_doc_links`   -- `[`Foo`]` naming nothing that resolves. Renders as literal
//                                 bracket text, so the reader sees the mistake and gets no link.
// * `private_intra_doc_links`  -- public documentation linking to a `pub(crate)` or private
//                                 item. The target is absent from the generated documentation,
//                                 so the link is dead for every reader outside this crate.
//                                 Where the prose genuinely wants to name an internal helper --
//                                 `inflate_table`, `gz_look`, `crc32_braid`, the `_tr_*` family
//                                 -- it must use a plain code span, not a link.
// * `redundant_explicit_links` -- `[`Foo`](Foo)`, where the label already resolves to the
//                                 destination. Harmless when written, misleading when `Foo`
//                                 later moves and only one of the two is updated.
// * `invalid_codeblock_attributes` -- a fenced block whose info string rustdoc does not
//                                 recognise (` ```ignpre `, ` ```shoud_panic `). It would be
//                                 compiled as Rust, or silently skipped, rather than doing what
//                                 the author meant. Non-Rust listings must say ` ```text `.
//
// ONE RESOLUTION RULE IS WORTH KNOWING BEFORE EDITING ANY MODULE DOCUMENTATION HERE, because
// it is not obvious and it is the reason the `//!` block of a dozen modules in this crate ends
// in a list of link-reference definitions. When a `mod` declaration carries an outer doc
// comment -- as every one below does -- rustdoc joins that comment to the module's own `//!`
// block and resolves the links of the whole joined text in the scope of the module that
// *declares* the child, not in the child's own scope. So inside `gz/mod.rs`, whose declaration
// lives here, `[`state`]` does not resolve even though `pub mod state;` is three lines away in
// that same file: resolution happens in `zlib_rs`, where no `state` exists. The fix is to name
// the target in full. Doing it inline would force a table row or a wrapped sentence to grow, so
// each module instead closes its `//!` block with `[`state`]: crate::gz::state` and leaves the
// prose untouched. An item's own `///` documentation is not affected -- that resolves in the
// module the item lives in, as one would expect.
#![deny(rustdoc::broken_intra_doc_links)]
#![deny(rustdoc::private_intra_doc_links)]
#![deny(rustdoc::redundant_explicit_links)]
#![deny(rustdoc::invalid_codeblock_attributes)]

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

// None of the module declarations below carries a doc comment, and none may be given
// one.  The module map in the crate documentation above already names every module, its
// C source and its responsibility, and each module's own `//!` header carries the detail
// and the citations.  The duplication is not the only reason to keep it that way: an
// outer doc comment written here would become the first fragment of the module's merged
// documentation, and rustdoc would then resolve every intra-doc link in that module's
// own `//!` header in *this* scope rather than the module's -- silently breaking every
// `[`local_item`]` link inside it.

// Foundations: the allocator, the parameter types, the status codes and the safe views
// that replace the reference's raw-pointer buffer walks.
pub mod allocate;
pub mod config;
pub mod error;
pub mod read_buf;
pub mod weak_slice;

// The two checksums, which depend on nothing above and are the whole of the surface
// that needs neither an allocator nor a stream.
pub mod adler32;
pub mod crc32;

// The compressor.  `trees` is a sibling rather than a layer because the Huffman coder
// operates on `&mut DeflateState`.
pub mod deflate;
pub mod trees;

// The decompressor.  `infback` is a sibling rather than a submodule because its entry
// points are distinct public API.
pub mod infback;
pub mod inflate;

// The layers built on the two engines: whole-buffer wrappers, then the file interface.
pub mod compress;
pub mod uncompress;

// The only `std`-gated module: a `gzFile` is a real file, so this subtree needs
// `std::fs` and `std::io`.  A `--no-default-features` build compiles it out.
#[cfg(feature = "std")]
pub mod gz;

//  Convenience, never additional capability: every name below is already public
//  at its module path, and nothing is re-exported that was not.
//
//  EVERY re-export in this section carries `#[cfg(feature = "rust-api")]`, and
//  that is what the feature gates -- the flattening, and only the flattening.
//  The three rules below keep the barrel honest.
//
//  0. The gate goes on the re-export, NEVER on the item or its module.  Making
//     `config::DeflateConfig` or `error::ReturnCode` itself conditional would
//     break `libz-rs-sys`, which needs them and deliberately does not enable
//     `rust-api`; every `pub mod` above is therefore unconditional apart from
//     `gz`, whose gate is `std` and predates this one.  Because the feature can
//     only ever ADD names, Cargo's feature unification cannot make one member of
//     the workspace break another by turning it on -- which matters, since
//     `zlib-rs-differential` and the `fuzz/` targets do turn it on while
//     `libz-rs-sys` does not.
//
//     Practical consequence for documentation: a doc link in *ungated* prose --
//     the crate-level `//!` block, or any module's own docs -- must name the
//     module path.  A link to a gated root re-export resolves only when the
//     feature happens to be on, and dangles in the default configuration.
//
//    * `--features rust-api`   `zlib_rs::ReturnCode`, `zlib_rs::compress2`,
//                              `zlib_rs::DeflateConfig`, ... 81 flattened names.
//    * default (feature off)   the same items, reached as
//                              `zlib_rs::error::ReturnCode`,
//                              `zlib_rs::compress::compress2`,
//                              `zlib_rs::config::DeflateConfig`, ...
//
//  `crates/libz-rs-sys` deliberately does NOT enable the feature.  The facade
//  drives the engine by module path -- `deflate::deflate_init2`, `inflate::inflate`,
//  `infback::inflate_back`, `compress::compress2` -- because that keeps the C
//  function each export implements legible next to it, so the C ABI layer needs
//  none of this barrel.  `crates/zlib-rs-differential` DOES enable it
//  (`features = ["rust-api", "std"]`), because a harness enumerating the
//  differential matrix in Rust is precisely the consumer the idiomatic surface
//  exists for.
//
//  Adding a name here therefore has two obligations: it must already be public at
//  its module path, and it must carry the `#[cfg(feature = "rust-api")]` line.  An
//  ungated entry would put the item on the default surface and quietly dissolve
//  the split again.  Doctests ANYWHERE IN THIS CRATE -- this file's own
//  documentation and every module's `//!` block alike -- use MODULE PATHS for the
//  same reason: `cargo test -p zlib-rs` runs every one of them with default
//  features, where the flattened names do not exist, so an example that imports
//  `zlib_rs::ReturnCode` rather than `zlib_rs::error::ReturnCode` fails to compile
//  and takes the whole test run down with it.
//
//  Two further rules keep the barrel honest.
//
// The engine entry points are deliberately NOT flattened into the root.  The facade's
// 17 `deflate*`, 18 `inflate*` and 3 `inflateBack*` exports -- family counts measured
// on the reference library, not estimated -- call them as `deflate::deflate_init2`,
// `inflate::inflate`, `infback::inflate_back` and so on, which keeps the C function
// each one implements obvious and avoids 38 bare verbs at the crate root.

/// The status of every fallible operation -- see [`error::ReturnCode`].
#[cfg(feature = "rust-api")]
pub use crate::error::ReturnCode;

/// The message for a status code, i.e. the whole body of C's `zError` -- see [`error::err_msg`].
#[cfg(feature = "rust-api")]
pub use crate::error::err_msg;

/// The caller-supplied allocator contract.
///
/// The facade implements this over the `(zalloc, zfree, opaque)` triple in the caller's
/// `z_stream`; [`GlobalAllocator`] is the stand-in for C's "both null, use `malloc`" default.
#[cfg(feature = "rust-api")]
pub use crate::allocate::Allocator;

/// Identity of the allocator that owns a block, so a block can only be freed by its owner --
/// see [`allocate::AllocatorId`].
#[cfg(feature = "rust-api")]
pub use crate::allocate::AllocatorId;

/// An owned allocation, allocator-agnostic -- see [`allocate::Buffer`].
#[cfg(feature = "rust-api")]
pub use crate::allocate::Buffer;

/// The default allocator, used when the caller supplies neither `zalloc` nor `zfree` --
/// see [`allocate::GlobalAllocator`].
#[cfg(feature = "rust-api")]
pub use crate::allocate::GlobalAllocator;

/// The caller's `opaque` cookie, passed back to `zalloc` and `zfree` untouched --
/// see [`allocate::Opaque`].
#[cfg(feature = "rust-api")]
pub use crate::allocate::Opaque;

/// The outcome of returning a [`Buffer`] to an allocator -- see [`allocate::Release`].
#[cfg(feature = "rust-api")]
pub use crate::allocate::Release;

/// The `deflateInit2_` parameter set -- see [`config::DeflateConfig`].
#[cfg(feature = "rust-api")]
pub use crate::config::DeflateConfig;

/// A [`DeflateConfig`] whose every field has been range-checked --
/// see [`config::ValidatedDeflateConfig`].
#[cfg(feature = "rust-api")]
pub use crate::config::ValidatedDeflateConfig;

/// The `inflateInit2_` parameter set -- see [`config::InflateConfig`].
#[cfg(feature = "rust-api")]
pub use crate::config::InflateConfig;

/// An [`InflateConfig`] whose every field has been range-checked --
/// see [`config::ValidatedInflateConfig`].
#[cfg(feature = "rust-api")]
pub use crate::config::ValidatedInflateConfig;

/// The compression method, of which `Z_DEFLATED` is the only one -- see [`config::Method`].
#[cfg(feature = "rust-api")]
pub use crate::config::Method;

/// The five compression strategies -- see [`config::Strategy`].
#[cfg(feature = "rust-api")]
pub use crate::config::Strategy;

/// The seven flush modes, `Z_NO_FLUSH` through `Z_TREES`.
///
/// The single definition in the crate: `deflate` re-exports this very type, so
/// `deflate::Flush` and this name are one item.
#[cfg(feature = "rust-api")]
pub use crate::config::Flush;

/// What container the compressor wraps its DEFLATE stream in -- see [`config::Wrap`].
#[cfg(feature = "rust-api")]
pub use crate::config::Wrap;

/// What containers the decompressor will accept -- see [`config::InflateWrap`].
#[cfg(feature = "rust-api")]
pub use crate::config::InflateWrap;

/// `Z_NO_COMPRESSION` (0) -- see [`config::Z_NO_COMPRESSION`].
#[cfg(feature = "rust-api")]
pub use crate::config::Z_NO_COMPRESSION;

/// `Z_BEST_SPEED` (1) -- see [`config::Z_BEST_SPEED`].
#[cfg(feature = "rust-api")]
pub use crate::config::Z_BEST_SPEED;

/// `Z_BEST_COMPRESSION` (9) -- see [`config::Z_BEST_COMPRESSION`].
#[cfg(feature = "rust-api")]
pub use crate::config::Z_BEST_COMPRESSION;

/// `Z_DEFAULT_COMPRESSION` (-1), which resolves to level 6 --
/// see [`config::Z_DEFAULT_COMPRESSION`].
#[cfg(feature = "rust-api")]
pub use crate::config::Z_DEFAULT_COMPRESSION;

/// `Z_DEFLATED` (8), the only method `zlib.h` defines -- see [`config::Z_DEFLATED`].
#[cfg(feature = "rust-api")]
pub use crate::config::Z_DEFLATED;

/// `MIN_WBITS` (8) -- see [`config::MIN_WBITS`].
#[cfg(feature = "rust-api")]
pub use crate::config::MIN_WBITS;

/// `MAX_WBITS` (15), the 32 KiB window -- see [`config::MAX_WBITS`].
#[cfg(feature = "rust-api")]
pub use crate::config::MAX_WBITS;

/// `DEF_WBITS` (15), the default window size **`inflateInit` supplies** --
/// see [`config::DEF_WBITS`].
///
/// The two init macros do not use the same constant, even though both evaluate to 15.
/// `inflateInit_` forwards `DEF_WBITS` (`inflate.c` L216), whereas `deflateInit_` forwards
/// `MAX_WBITS` (`deflate.c` L381) together with `DEF_MEM_LEVEL`. Keep the names distinct: the
/// numeric coincidence is not a guarantee, and only `MAX_WBITS` is the range bound that
/// `deflateInit2_` validates against.
#[cfg(feature = "rust-api")]
pub use crate::config::DEF_WBITS;

/// `MIN_MEM_LEVEL` (1) -- see [`config::MIN_MEM_LEVEL`].
#[cfg(feature = "rust-api")]
pub use crate::config::MIN_MEM_LEVEL;

/// `MAX_MEM_LEVEL` (9) -- see [`config::MAX_MEM_LEVEL`].
#[cfg(feature = "rust-api")]
pub use crate::config::MAX_MEM_LEVEL;

/// `DEF_MEM_LEVEL` (8), what `deflateInit` supplies -- see [`config::DEF_MEM_LEVEL`].
#[cfg(feature = "rust-api")]
pub use crate::config::DEF_MEM_LEVEL;

/// `MIN_MATCH` (3), the shortest LZ77 match -- see [`config::MIN_MATCH`].
#[cfg(feature = "rust-api")]
pub use crate::config::MIN_MATCH;

/// `MAX_MATCH` (258), the longest LZ77 match -- see [`config::MAX_MATCH`].
#[cfg(feature = "rust-api")]
pub use crate::config::MAX_MATCH;

/// `PRESET_DICT` (0x20), the zlib header flag for a preset dictionary --
/// see [`config::PRESET_DICT`].
#[cfg(feature = "rust-api")]
pub use crate::config::PRESET_DICT;

/// Adler-32 over a slice -- see [`adler32::adler32`].
#[cfg(feature = "rust-api")]
pub use crate::adler32::adler32;

/// Adler-32 with a `size_t`-shaped length, the primary implementation --
/// see [`adler32::adler32_z`].
#[cfg(feature = "rust-api")]
pub use crate::adler32::adler32_z;

/// Concatenates two Adler-32 checksums.
///
/// Backs both `adler32_combine` and `adler32_combine64` in the facade, because the two differ
/// only in the C type of their length argument.
#[cfg(feature = "rust-api")]
pub use crate::adler32::adler32_combine;

/// The swappable Adler-32 backend -- see [`adler32::Adler32Backend`].
#[cfg(feature = "rust-api")]
pub use crate::adler32::Adler32Backend;

/// CRC-32 over a slice -- see [`crc32::crc32`].
#[cfg(feature = "rust-api")]
pub use crate::crc32::crc32;

/// CRC-32 with a `size_t`-shaped length, the primary implementation -- see [`crc32::crc32_z`].
#[cfg(feature = "rust-api")]
pub use crate::crc32::crc32_z;

/// The 256-entry CRC-32 table C callers may ask for -- see [`crc32::get_crc_table`].
#[cfg(feature = "rust-api")]
pub use crate::crc32::get_crc_table;

/// Concatenates two CRC-32 checksums -- see [`crc32::crc32_combine`].
#[cfg(feature = "rust-api")]
pub use crate::crc32::crc32_combine;

/// The 64-bit-length form of [`crc32_combine`] -- see [`crc32::crc32_combine64`].
#[cfg(feature = "rust-api")]
pub use crate::crc32::crc32_combine64;

/// Precomputes the operator [`crc32_combine_op`] applies -- see [`crc32::crc32_combine_gen`].
#[cfg(feature = "rust-api")]
pub use crate::crc32::crc32_combine_gen;

/// The 64-bit-length form of [`crc32_combine_gen`] -- see [`crc32::crc32_combine_gen64`].
#[cfg(feature = "rust-api")]
pub use crate::crc32::crc32_combine_gen64;

/// Applies a precomputed combine operator -- see [`crc32::crc32_combine_op`].
#[cfg(feature = "rust-api")]
pub use crate::crc32::crc32_combine_op;

/// The swappable CRC-32 backend -- see [`crc32::Crc32Backend`].
#[cfg(feature = "rust-api")]
pub use crate::crc32::Crc32Backend;

/// The compressor's state, everything C keeps behind `z_stream.state` --
/// see [`deflate::DeflateState`].
#[cfg(feature = "rust-api")]
pub use crate::deflate::DeflateState;

/// The caller-visible half of a compression stream: the buffers, the cursors and the scalars
/// C keeps in `z_stream` itself -- see [`deflate::DeflateStream`].
#[cfg(feature = "rust-api")]
pub use crate::deflate::DeflateStream;

/// The decompressor's state -- see [`inflate::InflateState`].
#[cfg(feature = "rust-api")]
pub use crate::inflate::InflateState;

/// The caller-visible half of a decompression stream -- see [`inflate::InflateStream`].
#[cfg(feature = "rust-api")]
pub use crate::inflate::InflateStream;

/// The decoder's state machine, one variant per C `inflate_mode`.
///
/// Public for the state and differential suites, and not part of the ABI: an `inflate_state`
/// only ever reaches a C caller through the opaque `z_stream.state` pointer.
#[cfg(feature = "rust-api")]
pub use crate::inflate::Mode;

/// The `inflateBack` input callback, C's `in()` -- see [`infback::InflateBackInput`].
#[cfg(feature = "rust-api")]
pub use crate::infback::InflateBackInput;

/// The `inflateBack` output callback, C's `out()` -- see [`infback::InflateBackOutput`].
#[cfg(feature = "rust-api")]
pub use crate::infback::InflateBackOutput;

/// A read cursor over the caller's input buffer -- see [`read_buf::InputCursor`].
#[cfg(feature = "rust-api")]
pub use crate::read_buf::InputCursor;

/// A write cursor over the caller's output buffer -- see [`read_buf::OutputCursor`].
#[cfg(feature = "rust-api")]
pub use crate::read_buf::OutputCursor;

#[cfg(feature = "std")]
/// The `gzFile` state.
///
/// The one re-export here that needs two gates. `rust-api` because it is part of the flat
/// idiomatic surface like everything else in this section, and `std` because an ungated re-export
/// would name a module that the default `no_std` build does not compile at all.
#[cfg(all(feature = "rust-api", feature = "std"))]
pub use crate::gz::GzState;

/// Compresses in one call at the default level -- see [`compress::compress`].
#[cfg(feature = "rust-api")]
pub use crate::compress::compress;

/// The `size_t`-shaped form of [`compress()`] -- see [`compress::compress_z`].
#[cfg(feature = "rust-api")]
pub use crate::compress::compress_z;

/// Compresses in one call at a chosen level -- see [`compress::compress2`].
#[cfg(feature = "rust-api")]
pub use crate::compress::compress2;

/// The `size_t`-shaped form of [`compress2`], and the primary implementation --
/// see [`compress::compress2_z`].
#[cfg(feature = "rust-api")]
pub use crate::compress::compress2_z;

/// The destination size a caller must supply, and never an underestimate.
///
/// Never an underestimate. A caller sizes its buffer with this, so the number must match the
/// reference exactly: too small would overflow the *caller's* buffer.
#[cfg(feature = "rust-api")]
pub use crate::compress::compress_bound;

/// The `size_t`-shaped form of [`compress_bound`] -- see [`compress::compress_bound_z`].
#[cfg(feature = "rust-api")]
pub use crate::compress::compress_bound_z;

/// What a one-shot compression produced -- see [`compress::Compressed`].
#[cfg(feature = "rust-api")]
pub use crate::compress::Compressed;

/// Decompresses in one call -- see [`uncompress::uncompress`].
#[cfg(feature = "rust-api")]
pub use crate::uncompress::uncompress;

/// The `size_t`-shaped form of [`uncompress()`] -- see [`uncompress::uncompress_z`].
#[cfg(feature = "rust-api")]
pub use crate::uncompress::uncompress_z;

/// Decompresses in one call, reporting input consumed as well --
/// see [`uncompress::uncompress2`].
#[cfg(feature = "rust-api")]
pub use crate::uncompress::uncompress2;

/// The `size_t`-shaped form of [`uncompress2`], and the primary implementation --
/// see [`uncompress::uncompress2_z`].
#[cfg(feature = "rust-api")]
pub use crate::uncompress::uncompress2_z;

/// What a one-shot decompression produced and consumed -- see [`uncompress::Decompressed`].
#[cfg(feature = "rust-api")]
pub use crate::uncompress::Decompressed;

//  Public DATA whose consuming FUNCTIONS stay crate-private, which is exactly
//  the asymmetry `zlib.map` describes: `inflate_fixed`, `inflate_fast`
//  and the `_tr_*` family are all hidden, while the arrays they
//  read are the port's cheapest correctness check.  The planned
//  `crates/zlib-rs-differential/tests/table_equality.rs` will compare every one
//  of them element for element against `crc32.h`, `trees.h` and `inffixed.h`, so
//  that a transcription slip anywhere in these thousands of literals shows up
//  there immediately -- long before any stream is compressed.  That file has not
//  landed yet; until it does, the arrays are public for it and nothing checks
//  them automatically.

/// The byte-at-a-time CRC-32 table -- see [`crc32::tables::CRC_TABLE`].
#[cfg(feature = "rust-api")]
pub use crate::crc32::CRC_TABLE;

/// The word-sized big-endian CRC-32 table -- see [`crc32::tables::CRC_BIG_TABLE`].
#[cfg(feature = "rust-api")]
pub use crate::crc32::CRC_BIG_TABLE;

/// The braided little-endian CRC-32 tables -- see [`crc32::tables::CRC_BRAID_TABLE`].
#[cfg(feature = "rust-api")]
pub use crate::crc32::CRC_BRAID_TABLE;

/// The braided big-endian CRC-32 tables -- see [`crc32::tables::CRC_BRAID_BIG_TABLE`].
#[cfg(feature = "rust-api")]
pub use crate::crc32::CRC_BRAID_BIG_TABLE;

/// The `x^2^n mod p(x)` table behind the CRC-32 combine operations --
/// see [`crc32::tables::X2N_TABLE`].
#[cfg(feature = "rust-api")]
pub use crate::crc32::X2N_TABLE;

/// The per-level tuning table, the first of the eight byte-identity decision points --
/// see [`deflate::CONFIGURATION_TABLE`].
#[cfg(feature = "rust-api")]
pub use crate::deflate::CONFIGURATION_TABLE;

/// One row of [`CONFIGURATION_TABLE`] -- see [`deflate::Config`].
#[cfg(feature = "rust-api")]
pub use crate::deflate::Config;

/// The static literal/length tree -- see [`trees::static_ltree`].
#[cfg(feature = "rust-api")]
pub use crate::trees::static_ltree;

/// The static distance tree -- see [`trees::static_dtree`].
#[cfg(feature = "rust-api")]
pub use crate::trees::static_dtree;

/// Distance-to-code mapping -- see [`trees::_dist_code`].
#[cfg(feature = "rust-api")]
pub use crate::trees::_dist_code;

/// Length-to-code mapping -- see [`trees::_length_code`].
#[cfg(feature = "rust-api")]
pub use crate::trees::_length_code;

/// First length of each length code -- see [`trees::base_length`].
#[cfg(feature = "rust-api")]
pub use crate::trees::base_length;

/// First distance of each distance code -- see [`trees::base_dist`].
#[cfg(feature = "rust-api")]
pub use crate::trees::base_dist;

/// The fixed literal/length decode table -- see [`inflate::lenfix`].
#[cfg(feature = "rust-api")]
pub use crate::inflate::lenfix;

/// The fixed distance decode table -- see [`inflate::distfix`].
#[cfg(feature = "rust-api")]
pub use crate::inflate::distfix;
