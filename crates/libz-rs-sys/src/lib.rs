//! `libz-rs-sys` — the C ABI facade over the safe `zlib-rs` core.
//!
//! This crate is the drop-in replacement for the C `libz`. It contributes no
//! compression logic of its own: every algorithm lives in [`zlib_rs`], which is
//! compiled under `#![forbid(unsafe_code)]` and has an empty `[dependencies]`
//! table. What this crate contributes is the *boundary* — the place where a raw
//! pointer handed over by a C caller is validated and turned into a safe Rust
//! slice exactly once, before anything in the core is reached.
//!
//! That division is the whole safety argument of the port. Unsafety cannot be
//! eliminated at an FFI edge, because the edge is where the type system stops;
//! it can only be *relocated* to a surface small enough to audit exhaustively.
//! This crate is that surface, and it is the only crate in the workspace
//! permitted to write `unsafe`.
//!
//! This file is the barrel. It declares the eleven-module tree, installs the
//! crate-wide lint attributes that make the safety claim checkable by the
//! compiler rather than by review, and re-exports the ABI types and the public
//! constant surface for Rust consumers. It defines no entry point and adds no
//! symbol of its own to the dynamic symbol table.
//!
//! # The artifact contract
//!
//! `Cargo.toml` declares `[lib] name = "z"` with
//! `crate-type = ["cdylib", "staticlib", "rlib"]`, so one crate produces all
//! three artifacts and the exported C surface is defined in exactly one place:
//!
//! | Artifact | File | Consumer |
//! |---|---|---|
//! | `cdylib` | `libz.so` | the shared drop-in replacement; `-lz` resolves to it |
//! | `staticlib` | `libz.a` | `infcover` links it, and the shared object is *relinked* from it whenever `zlib.map` has to be applied |
//! | `rlib` | — | lets `zlib-rs-differential`, the `fuzz/` targets and this crate's own `tests/` depend on it as an ordinary Rust library |
//!
//! ## The import name, which is not the package name
//!
//! ★ Note the asymmetry, and note it precisely, because it is easy to get
//! backwards. `[lib] name = "z"` renames the **library target**, and a library
//! target's name is also its extern crate name — it does not merely set the
//! emitted file name. So the *package* is `libz-rs-sys`, which is what a
//! `cargo -p` argument and a dependency line name, while the *library target*
//! is `z`, which is the default extern crate name a dependent gets.
//!
//! Two rules follow, and they differ:
//!
//! * **Inside this crate** — its doctests and its own `tests/` integration
//!   targets — the name is `z`, and cannot be anything else: a crate's own test
//!   targets link its library target under that target's name, with no
//!   opportunity to rename. Verified rather than assumed:
//!   `cargo test -p libz-rs-sys --doc` reports the suite as `Doc-tests z`.
//!
//!   ```text
//!   // this crate's own tests/, and every doctest in this crate
//!   use z::{z_stream, z_streamp};
//!   ```
//!
//! * **Outside this crate** a dependent may rename the dependency, and the two
//!   Rust consumers in this workspace both do, restoring the conventional
//!   spelling:
//!
//!   ```text
//!   # crates/zlib-rs-differential/Cargo.toml and fuzz/Cargo.toml
//!   libz_rs_sys = { package = "libz-rs-sys", path = "../crates/libz-rs-sys" }
//!   ```
//!
//!   ```text
//!   // and therefore, in those two crates only
//!   use libz_rs_sys::z_stream;
//!   ```
//!
//! Neither spelling reaches a C caller. C resolves `deflate` and the other
//! ninety-four exports through the dynamic symbol table, where Rust crate names
//! do not exist at all.
//!
//! ## Why the shared object is relinked
//!
//! The relink is not an implementation detail that can be optimised away. A
//! version script cannot be attached to a rustc-built `cdylib`: rustc already
//! passes its own anonymous-tag script, and `ld` refuses to combine an
//! anonymous version tag with the named tags `zlib.map` declares. Reproducing
//! the C build's dynamic symbol table therefore requires linking `libz.a` with
//! the C linker under `--version-script`. `Makefile.in`'s `rust` target owns
//! that step; this crate's `build.rs` owns only the link arguments.
//!
//! # The export contract
//!
//! The reference C shared library exports **111 dynamic global symbols**: 95
//! `nm` type-`T` functions plus 16 type-`A` symbol-version nodes, under
//! `SONAME libz.so.1`. That set is the parity target, and it is checked by
//! diffing `nm -D --defined-only --extern-only` against the C baseline —
//! `Makefile.in`'s `rust` and `rust-symbols` targets perform exactly that diff,
//! and `tests/symbol_parity.rs` is where it belongs as a `cargo test`. The root
//! `Cargo.toml` records the three numbers under `[workspace.metadata.zlib]` as
//! well, so the expectation lives in the manifest and not only in a test.
//!
//! Measured on this platform, with the packaged artefact `make rust` stages: 111
//! symbols, an empty diff against the C `libz.so.1.3.2.1-motley`, 95 type-`T`
//! and 16 type-`A`, `SONAME libz.so.1`, and none of the `zlib.map` `local:`
//! names visible.
//!
//! ## Where 95 comes from
//!
//! `zlib.h` contains 119 `ZEXTERN` declarations naming **101 distinct**
//! functions. The 18 duplicate declarations are all deliberate: the large-file
//! section re-declares fourteen names across its `#if` arms — the seven `*64`
//! entry points and their seven unsuffixed counterparts, with the three
//! `*combine*` names appearing three times each — and `gzprintf` is declared
//! twice, with a prototype under `#if defined(STDC) || defined(Z_HAVE_STDARG_H)`
//! (L1549) and with an empty parameter list in the `#else` arm (L1551), for a
//! pre-ANSI compiler that has no `<stdarg.h>`. Of the 101, six are not exported:
//!
//! | Name | `zlib.h` | Why it is not a symbol |
//! |---|---|---|
//! | `deflateInit` | L232 | macro over `deflateInit_` (L1932) |
//! | `inflateInit` | L382 | macro over `inflateInit_` (L1934) |
//! | `deflateInit2` | L543 | macro over `deflateInit2_` (L1936) |
//! | `inflateInit2` | L859 | macro over `inflateInit2_` (L1939) |
//! | `inflateBackInit` | L1113 | macro over `inflateBackInit_` (L1942) |
//! | `gzopen_w` | L2042 | `#if defined(_WIN32)` only |
//!
//! The five macros exist so that a caller passes its own compile-time
//! `ZLIB_VERSION` and `sizeof(z_stream)` to the `_`-suffixed real function; the
//! version and layout check that makes drop-in replacement safe is precisely
//! what those macros arrange. So 101 − 5 = **96 real entry points**, and
//! 96 − 1 = **95 exported on a non-Windows target**. There are zero
//! exported-but-not-declared symbols: the surface is neither a subset nor a
//! superset of the header.
//!
//! ## Per-module allocation
//!
//! | Module | Exports | Contents |
//! |---|---|---|
//! | `deflate` | 17 | the `deflate*` family, including both `_`-suffixed init forms |
//! | `inflate` | 18 | the `inflate*` family, plus `inflate_table` |
//! | `infback` | 3 | `inflateBackInit_`, `inflateBack`, `inflateBackEnd` |
//! | `compress` | 10 | five one-shot wrappers and their five `_z` `size_t` forms |
//! | `gz` | 32 | 30 `gz*` functions here, plus 2 from `csrc/gzprintf_shim.c` |
//! | `checksum` | 11 | the Adler-32 and CRC-32 families |
//! | `util` | 4 | `zlibVersion`, `zlibCompileFlags`, `zError`, `get_crc_table` |
//!
//! 17 + 18 + 3 + 10 + 32 + 11 + 4 = **95**. The tally reconciles to the Rust
//! sources as follows: 97 items carry `#[no_mangle]` across the seven modules;
//! `gzopen_w` is `#[cfg(windows)]`, leaving 96 compiled on Linux, which is
//! exactly what `nm` reports for `target/release/libz.so`; `zlib.map` then hides
//! three of them — `inflate_table` and the two `_zlib_rs_gzprintf_*` helpers,
//! the latter pair by its `_*` wildcard — leaving 93; and the packaging link
//! adds `gzprintf` and `gzvprintf` from the one C translation unit this library
//! contains, restoring 95.
//!
//! Two counts in the plan that produced this port do not reconcile, and the
//! discrepancy is recorded here so that nobody re-derives the wrong number: the
//! transformation mapping lists 20 `deflate*` exports where only 17 exist, and
//! it counts `get_crc_table` in both the checksum module and the introspection
//! module. Its per-module figures therefore sum to 99. **The 111-symbol parity
//! diff is the authority**; per-module counts are bookkeeping that has to
//! reconcile to it, never the other way round.
//!
//! ## Version nodes and decoration
//!
//! Of the 111 symbols, **16 are version nodes** — `ZLIB_1.2.0`, `ZLIB_1.2.0.2`,
//! `ZLIB_1.2.0.8`, `ZLIB_1.2.2`, `ZLIB_1.2.2.3`, `ZLIB_1.2.2.4`,
//! `ZLIB_1.2.3.3`, `ZLIB_1.2.3.4`, `ZLIB_1.2.3.5`, `ZLIB_1.2.5.1`,
//! `ZLIB_1.2.5.2`, `ZLIB_1.2.7.1`, `ZLIB_1.2.9`, `ZLIB_1.2.12`,
//! `ZLIB_1.3.1.2` and `ZLIB_1.3.2`. `zlib.map` names 54 functions inside those
//! nodes, so **54 exports carry `@@VERSION` decoration** and the remaining
//! **41 are undecorated** base-set names.
//!
//! Nothing in the Rust sources creates the decoration: it is produced entirely
//! by the `--version-script=zlib.map` argument the packaging link passes. A
//! `#[no_mangle]` function is emitted undecorated, and the linker attaches the
//! node when it applies the script.
//!
//! # Crate-level attributes, and why each one is required
//!
//! ### `#![allow(non_camel_case_types)]`
//!
//! The C ABI names the boundary types: `z_stream`, `z_streamp`,
//! `internal_state`, `gz_header`, `gz_headerp`, `alloc_func`, `free_func`,
//! `in_func`, `out_func`. Every one of those spellings is fixed by `zlib.h`,
//! which is immutable, and the cbindgen gate diffs the generated header against
//! it — so renaming them to satisfy Rust's casing convention is not available.
//! Since the workspace lint policy is enforced with `-D warnings`, the default
//! `non_camel_case_types` warning would otherwise be a hard error on every
//! boundary type. The allow is scoped to this crate alone; `zlib-rs` keeps the
//! convention.
//!
//! No matching `non_snake_case` or `non_upper_case_globals` allow is needed at
//! the crate root, and neither is present. The C function names are spelled on
//! `#[no_mangle]` items, whose export name is not subject to the naming lints,
//! and the `Z_*` constants below are already upper case. Adding either allow
//! would widen the exemption past what the ABI actually forces.
//!
//! ### `#![deny(unsafe_op_in_unsafe_fn)]`
//!
//! Without it, the body of an `unsafe fn` is an implicit unsafe block, so a
//! pointer dereference twenty lines below the signature carries no marker and
//! no `// SAFETY:` comment is prompted for. With it, every unsafe operation
//! must sit inside an explicit `unsafe { … }` that names its invariant, which
//! is what makes the audit in the next section mechanically checkable rather
//! than aspirational.
//!
//! ### `#![deny(missing_docs)]`
//!
//! The exported surface *is* the product. An undocumented public item here is a
//! gap in the contract, not a stylistic lapse.
//!
//! ### Deliberately absent: `#![forbid(unsafe_code)]`
//!
//! That attribute belongs to `zlib-rs` and to `zlib-rs` alone. Applying it here
//! would make the crate unable to do its one job, and the failure would be a
//! build error rather than a subtle regression — so this note exists to stop a
//! well-meant "hardening" pass from adding it. The workspace consequently
//! declines to blanket-forbid `unsafe_code` at `[workspace.lints]` level, and
//! the containment guarantee is expressed structurally instead: exactly one
//! crate may write `unsafe`, and it is this one.
//!
//! ### Deliberately absent: `#![no_std]`
//!
//! `zlib-rs` is `no_std` + `alloc` by default and that is correct for it. This
//! crate is unconditionally `std`, and the reason is measured rather than
//! stylistic: building the `cdylib` and `staticlib` targets under `#![no_std]`
//! fails with three errors — `no global memory allocator found but one is
//! required`, `` `#[panic_handler]` function required, but not found``, and
//! `unwinding panics are not supported without std`. Satisfying them would mean
//! installing a `#[global_allocator]` and a panic handler into every C process
//! that loads `libz.so`, which a drop-in replacement for a system library must
//! never do — all the more so because caller-visible memory is required to come
//! from the caller's own `zalloc` and go back to the caller's own `zfree`.
//!
//! Where a narrowed feature set makes part of `std` unnecessary, the gate goes
//! on the module that needs it — as `gz` does — never on the crate root.
//!
//! ### Deliberately absent: `#![feature(…)]`
//!
//! The declared MSRV is 1.80 on the stable channel, so no unstable feature may
//! be required to build this crate. Nightly appears in this port only for Miri
//! and for `-Zsanitizer=address`, both of which analyse code that already
//! compiles on stable.
//!
//! # Unsafe containment
//!
//! Unsafety is confined to six categories of site. They are the sites the FFI
//! contract *forces*; anywhere else, `unsafe` in this crate is a defect. Each
//! block carries a `// SAFETY:` comment naming the invariant it relies on.
//!
//! 1. **Stream-pointer validation.** Every exported stream function receives a
//!    `z_streamp`. The pointer must be non-null, aligned, and addressing a
//!    caller-allocated `z_stream`. Nullability is part of the published
//!    contract — a null stream yields `Z_STREAM_ERROR` — so the guard always
//!    precedes the first dereference.
//! 2. **Reconstructing the input and output slices.** `(next_in, avail_in)` and
//!    `(next_out, avail_out)` are pointer/length pairs. `from_raw_parts` is
//!    called once, on entry, and only the resulting safe slices travel inward.
//!    An `avail_in` of zero with a null `next_in` must produce an empty slice
//!    rather than undefined behaviour.
//! 3. **The opaque `state` round-trip.** `z_stream::state` is caller-visible but
//!    caller-opaque. Before it is treated as state it is tag-validated, exactly
//!    as `deflateStateCheck` and `inflateStateCheck` do in the C sources, so a
//!    foreign, stale, or already-freed stream is rejected instead of trusted.
//! 4. **Invoking `zalloc` and `zfree`.** These are caller-supplied C function
//!    pointers, modelled as `Option<unsafe extern "C" fn(…)>` so that `Z_NULL`
//!    is `None` and selects the internal default. A block obtained from a
//!    caller's `zalloc` is released only through that same caller's `zfree`.
//! 5. **C strings and varargs.** Paths reaching `gzopen` are assumed
//!    NUL-terminated and readable for the duration of the call; returned strings
//!    are `'static` or owned by the stream and outlive the caller's use of them.
//!    `gzprintf` is variadic and `gzvprintf` takes a `va_list`.
//! 6. **Writing through a caller's `gz_header`.** `inflateGetHeader` fills
//!    caller buffers, and every write is clamped to `extra_max`, `name_max` and
//!    `comm_max`. This is historically the source of gzip-header overflow
//!    defects, and it becomes a length-checked slice write.
//!
//! # Panic discipline
//!
//! A panic must never unwind into a C caller: the caller was not compiled to
//! support unwinding, and letting an unwind cross the boundary is undefined
//! behaviour. Three mechanisms enforce that, and they are deliberately
//! redundant:
//!
//! * Every one of the 95 exported functions is declared `extern "C"`, never
//!   `extern "C-unwind"`. A panic reaching an `extern "C"` boundary aborts.
//! * The root `[profile.release]` sets `panic = "abort"`, so in the shipped
//!   configuration there is no unwinding machinery to escape through at all.
//! * [`panic_guard`] concentrates the behaviour in one auditable place instead
//!   of leaving it implicit across the whole exported surface.
//!
//! The lint policy inherited from the workspace additionally denies
//! `unwrap_used`, `expect_used`, `indexing_slicing`, `panic`, `todo`,
//! `unimplemented` and `unreachable`, so an abort should be unreachable rather
//! than merely contained.
//!
//! # Feature flags
//!
//! | Feature | Default | What it does |
//! |---|---|---|
//! | `libz-compat` | on | Gates the exported `extern "C"` surface — the unmangled symbols that make this library ABI-compatible with the C original. |
//! | `gz` | on | Gates the `gzFile` layer. File I/O is the only part of the port that needs `std` in the core, so this forwards into `zlib-rs/std`. |
//! | `simd` | off | Pass-through to `zlib-rs/simd`: vectorised CRC-32 and Adler-32 backends. |
//! | `libc` | off | Explicit opt-in for the optional pinned `libc` dependency, for the rare platform type or raw syscall that `std::fs` and `std::io` cannot express. |
//!
//! `libz-compat` is named exactly as the documented build command requires, so
//! that `cargo build --release --features libz-compat` selects the C ABI layer
//! by the name the port's own documentation uses. Because it is also in
//! `default`, that command and a plain `cargo build --release` resolve to the
//! same feature set; the explicit form exists to make the intent legible.
//!
//! There is no `rust-api` feature here. The idiomatic Rust surface is
//! `zlib-rs`'s concern and lives behind `zlib-rs/rust-api`, which this crate
//! deliberately does not enable — the facade needs the core's algorithms, not
//! its ergonomic wrappers.
//!
//! `--no-default-features` is a supported configuration. It compiles cleanly
//! and yields a library whose exported surface and gz layer are simply gated
//! out, leaving [`types`] and [`panic_guard`] — which is useful for checking the
//! feature matrix and for a consumer that wants the ABI type mirror without the
//! symbols. It is not a shipping configuration: a `libz.so` with no exports
//! replaces nothing. Nothing in this crate may raise a `compile_error!` for it.
//!
//! `simd` is confined to the two checksums and is **output-neutral**: a checksum
//! is one scalar however it is computed, so vectorising it cannot perturb the
//! bitstream. Vectorising match-finding would change the compressed output and
//! is prohibited outright, because byte-identical output is a hard requirement
//! of this port. Runtime target-feature detection lives in the core, so a
//! `simd`-enabled build still executes correctly on hardware lacking the
//! instructions; nothing in this file performs CPU detection.
//!
//! ## The `ZLIB_RS_SIMD` build-time toggle
//!
//! `ZLIB_RS_SIMD=0|1` is read by this crate's `build.rs`, which declares
//! `cargo::rerun-if-env-changed=ZLIB_RS_SIMD` so that flipping it forces a
//! rebuild. It is an *assertion about the resolved feature set*, not a second
//! way of selecting one: a build script cannot switch a Cargo feature on or
//! off, so the environment variable is validated against the `simd` feature and
//! the outer build system performs the translation. `Makefile.in`'s `rust`
//! target turns `ZLIB_RS_SIMD=1` into `--features libz-rs-sys/simd`.
//!
//! The build script reports the outcome to the crate two ways, and both are
//! consumed here rather than left dangling:
//!
//! * `cargo::rustc-cfg=zlib_rs_simd`, emitted only when the feature actually
//!   resolved on, and surfaced by [`simd_enabled`]. Its paired
//!   `cargo::rustc-check-cfg=cfg(zlib_rs_simd)` is emitted unconditionally by
//!   the build script, which is what keeps the cfg free of the
//!   `unexpected_cfgs` warning that `-D warnings` would turn into a build
//!   failure. This file must not re-declare it.
//! * `cargo::rustc-env=ZLIB_RS_CHECKSUM_BACKEND=simd|scalar`, surfaced by
//!   [`checksum_backend`], so that a benchmark can state which implementation it
//!   measured instead of inferring it.
//!
//! # Version identity
//!
//! The library's self-reported version is `ZLIB_VERSION "1.3.2.1-motley"` with
//! `ZLIB_VERNUM 0x1321`. That string has four components and is therefore not
//! valid Cargo semver, so it must live as a literal and must never be derived
//! from `env!("CARGO_PKG_VERSION")`, which is the three-component `1.3.2`.
//! The distinction is load-bearing rather than cosmetic: `deflateInit_` and
//! `inflateInit_` compare the caller's compile-time `ZLIB_VERSION` against the
//! library's and return `Z_VERSION_ERROR` on a major mismatch, and
//! `test/example.c` performs exactly that check at startup.
//!
//! # Header immutability
//!
//! `zlib.h`, `zconf.h` and `zlib.map` are **immutable**. They are the contract,
//! not an artefact of this implementation, and no part of this port edits them.
//! Conformance is machine-verified four independent ways, so that a divergence
//! is caught by tooling rather than by a reviewer's memory:
//!
//! 1. The compile-time assertions in `layout_assertions` pin every struct size,
//!    field offset and integer width the two headers fix. Layout drift is a
//!    build failure rather than silent memory corruption in every program that
//!    links the result.
//! 2. A cbindgen run generates a header from this crate and diffs it against
//!    `zlib.h`, catching a changed public signature.
//! 3. An `nm` diff against the 111-symbol baseline catches both a missing and an
//!    extra export.
//! 4. The `c-std.yml` workflow keeps compiling the unchanged `zlib.h` from C89
//!    through gnu2x, so the header stays valid for every dialect a caller may
//!    use.
//!
//! # cbindgen compatibility
//!
//! This file is the crate root cbindgen parses:
//!
//! ```text
//! cbindgen --config cbindgen.toml --crate libz-rs-sys --output generated_zlib.h
//! diff -u zlib.h generated_zlib.h
//! ```
//!
//! Three consequences shape the module tree below.
//!
//! * **Reachability under the default feature set.** cbindgen sees only what it
//!   can reach from here, so every module holding a `#[repr(C)]` type or a
//!   `#[no_mangle] pub extern "C"` function is declared unconditionally or
//!   behind a default-on feature. A module gated off in the configuration
//!   cbindgen parses would silently become a missing declaration in the diff.
//! * **A plain module tree.** No `mod` declaration is macro-generated and no
//!   `include!` appears anywhere in this crate. cbindgen parses the source
//!   syntactically without expanding macros, so an item it cannot see is an item
//!   it cannot emit.
//! * **The generated header is never committed.** `/generated_zlib.h` is in
//!   `.gitignore`, `cbindgen.toml`'s own banner marks the output a verification
//!   artefact that must not be installed or hand-edited, and `zlib.h` is
//!   read-only and is never regenerated from it. The header guard is `ZLIB_H` on
//!   purpose so the generated file cannot double-declare the API alongside the
//!   real header — which also means it shadows `zlib.h` and must never sit on an
//!   include path a real build searches.
//!
//! # Modules
//!
//! * [`panic_guard`] — the boundary's panic policy: the guards that turn a
//!   panic into an abort, or into the caller's documented failure value, and the
//!   [`panic_guard::fallback`] constants that keep those values tied to what
//!   `zlib.h` documents.
//! * [`types`] — the `#[repr(C)]` ABI mirror layer: `z_stream`, `gz_header`,
//!   `gzFile_s`, `code`, the `zconf.h` primitive aliases, the four nullable hook
//!   types, the [`types::StreamAllocator`] that calls a caller's `zalloc` and
//!   `zfree`, and the shared entry-point helpers that each hold one of the
//!   unsafe categories above. Every other module depends on it, and the ABI
//!   types are re-exported at the crate root from here.
//! * `layout_assertions` — the ABI gate. A private module of nothing but
//!   `const _: () = assert!(…)` items pinning every struct size, field offset
//!   and integer width `zlib.h`, `zconf.h` and `inftrees.h` fix, so that layout
//!   drift is a build failure rather than silent memory corruption in every
//!   program that links the result. It exports nothing — deliberately, since an
//!   extra name in the dynamic symbol table would fail the 111-symbol parity
//!   diff — and is the first of the four mechanisms above.
//! * `checksum` — the eleven exported Adler-32 and CRC-32 entry points:
//!   `adler32`, `adler32_z`, `adler32_combine`, `adler32_combine64`, `crc32`,
//!   `crc32_z`, `crc32_combine`, `crc32_combine64`, `crc32_combine_gen`,
//!   `crc32_combine_gen64` and `crc32_combine_op`. Width conversion,
//!   null-pointer handling and sentinel pass-through only; all arithmetic lives
//!   in `zlib_rs`. `get_crc_table` belongs to the same C family but is exported
//!   from the introspection module, not from here. Named in a code span rather
//!   than as an intra-doc link for the same reason as `compress` and `util`: the
//!   module is `cfg`-gated, so a link would be unresolvable — and therefore a
//!   rustdoc error under `-D warnings` — in the `--no-default-features` build this
//!   crate supports.
//! * `compress` — the ten one-shot exports (`compress`, `compress_z`,
//!   `compress2`, `compress2_z`, `compressBound`, `compressBound_z`,
//!   `uncompress`, `uncompress_z`, `uncompress2`, `uncompress2_z`) over
//!   `compress.c` and `uncompr.c`. Gated on `libz-compat`, because it is
//!   exported surface rather than shared machinery, and **private**: an export
//!   module reaches its consumers through the dynamic symbol table, never
//!   through the crate root. That is the arrangement the root manifest records
//!   where it explains why `unreachable_pub` is not enabled, and it is why
//!   `zlib.map` rather than Rust visibility governs what the library exports.
//! * `deflate` — the seventeen exported `deflate*` entry points (`deflateInit_`,
//!   `deflateInit2_`, `deflate`, `deflateEnd`, `deflateSetDictionary`,
//!   `deflateGetDictionary`, `deflateCopy`, `deflateReset`, `deflateResetKeep`,
//!   `deflateParams`, `deflateTune`, `deflateBound`, `deflateBound_z`,
//!   `deflatePending`, `deflateUsed`, `deflatePrime`, `deflateSetHeader`) over
//!   `deflate.c`. Pointer validation, allocator construction, slice
//!   reconstruction, the opaque `state` round-trip and width mapping only; every
//!   compression decision lives in `zlib_rs`. `deflateInit` and `deflateInit2` are
//!   deliberately **absent**: `zlib.h` declares both, but both are macros over the
//!   `_`-suffixed functions and neither is an exported symbol, so exporting them
//!   would fail the 111-symbol parity diff. Gated on `libz-compat` and private,
//!   for the same two reasons as `compress`, and named in a code span rather than
//!   as an intra-doc link for the same reason too.
//! * `inflate` — the eighteen exported `inflate*` entry points over `inflate.c`,
//!   plus `inflate_table` over `inftrees.c`. `inflateInit` and `inflateInit2`
//!   are absent for the same reason their deflate counterparts are. Gated and
//!   private like the other export modules.
//! * `infback` — `inflateBackInit_`, `inflateBack` and `inflateBackEnd` over
//!   `infback.c`, adapting the C `in`/`out` function pointers to the core's
//!   callback traits. It sits beside `inflate` rather than inside it because its
//!   public entry points are distinct, which is exactly why `infback.c` is a
//!   separate translation unit that shares `inflate.h`, `inftrees.h` and
//!   `inffast.h` with `inflate.c`.
//! * `gz` — the `gzFile` layer over `gzlib.c`, `gzread.c`, `gzwrite.c` and
//!   `gzclose.c`. Gated on `libz-compat` **and** `gz`, and private like the
//!   other export modules.
//! * `util` — library introspection: `zlibVersion`, `zlibCompileFlags`, `zError`
//!   and `get_crc_table`, four of the ninety-five exports. Also the home of the
//!   version identity — `util::ZLIB_VERSION` and the `ZLIB_VER_*` numbers — which
//!   is why it is the module the init entry points take their comparison string
//!   from, and which is re-exported at the crate root below. Gated behind
//!   `libz-compat`, like every module that contributes exported symbols. Named in
//!   code spans rather than as intra-doc links on purpose: the module is
//!   `cfg`-gated, so a link would be unresolvable — and therefore a rustdoc error
//!   under `-D warnings` — in a `--no-default-features` build, which this crate
//!   supports.
//!
//! # The crate-root Rust surface
//!
//! Everything re-exported below exists for *Rust* consumers — this crate's own
//! `tests/`, `zlib-rs-differential`, the `fuzz/` targets and the benchmarks. It
//! creates no dynamic symbol: a `pub use` is a Rust-level path and nothing more,
//! which is why adding one cannot disturb the 111-symbol parity diff.
//!
//! The barrel deliberately stops short in two places:
//!
//! * **No `#[no_mangle]` item is declared in this file.** Its job is wiring, and
//!   every extra exported symbol fails the parity diff. The 95 exports live in
//!   the seven export modules and nowhere else.
//! * **Nothing `zlib.map` marks `local:` is re-exported.** That covers
//!   `deflate_copyright`, `inflate_copyright`, `inflate_fast`, `inflate_table`,
//!   `zcalloc`, `zcfree`, `z_errmsg`, `gz_error`, `gz_intmax`, the `_*` wildcard
//!   and `inflate_fixed`. A `pub use` would not itself create a dynamic symbol,
//!   but putting a hidden name in the barrel invites a future `#[no_mangle]`, so
//!   the names stay out. The one documented exception is not an exception to
//!   this rule: `inflate` exports `inflate_table` as a real symbol because the
//!   unmodified `test/infcover.c` calls it directly and links `libz.a`, where the
//!   version script does not apply — and even so the name is not re-exported
//!   here.

// ---------------------------------------------------------------------------
//  Crate-level attributes
// ---------------------------------------------------------------------------
//
// Each of these is justified at length in the crate documentation above, under
// "Crate-level attributes, and why each one is required" -- including the three
// attributes that are deliberately ABSENT: `forbid(unsafe_code)`, `no_std` and
// `feature(...)`.  Adding any of the three would break this crate, so read that
// section before touching this block.
//
// The workspace lint policy is NOT restated here.  The root `Cargo.toml` sets
// `[workspace.lints]` -- `clippy::all` and `clippy::pedantic` at deny, plus
// `unwrap_used`, `expect_used`, `indexing_slicing`, `panic`, `panic_in_result_fn`,
// `todo`, `unimplemented`, `unreachable` and `exit` -- and this crate opts in with
// `[lints] workspace = true`.  Re-declaring any of them here would let the two
// copies drift; weakening any of them here would defeat the point of a single
// workspace policy.
#![allow(non_camel_case_types)]
#![deny(unsafe_op_in_unsafe_fn)]
#![deny(missing_docs)]

use core::ffi::c_int;

// The core's error newtype and its parameter constants, used only by the
// compile-time agreement checks further down.  Aliased so that the imported
// names cannot be confused with this crate's own `Z_*` constants, which are
// spelled exactly as `zlib.h` spells them.
use zlib_rs::config as core_config;
use zlib_rs::error::ReturnCode;

// ---------------------------------------------------------------------------
//  Module tree -- eleven modules, no twelfth
// ---------------------------------------------------------------------------
//
// Declared in dependency order: the three support modules first, then the seven
// export modules that build on them.  cbindgen parses this list to find the
// `#[repr(C)]` types and the `#[no_mangle]` functions, so it is kept plain --
// no macro-generated `mod`, no `include!`.

// Compile-time only, and intentionally not `pub`: the module declares no
// nameable item, so there is nothing for a consumer to reach. Declaring it is
// what makes its assertions run — a `const` item is evaluated because it exists,
// not because something uses it.
mod layout_assertions;
pub mod panic_guard;
pub mod types;

// Gated at the crate root, as `Cargo.toml` requires of every export: a
// `--no-default-features` build is supported and must simply have the exported
// `extern "C"` surface absent, never raise a `compile_error!`.
#[cfg(feature = "libz-compat")]
pub mod checksum;

// Exported `extern "C"` surface. Two properties, both deliberate:
//
//   * `libz-compat` gates it, as the feature table above describes: with the
//     feature off the crate still compiles and still provides `types` and
//     `panic_guard`, it simply exports no unmangled symbols.
//   * The module is PRIVATE. Its `#[no_mangle]` items are reachable through the
//     dynamic symbol table, so a `pub mod` would add a second, Rust-side path to
//     the same functions and put the crate root in the business of deciding what
//     is exported -- which is `zlib.map`'s job. The root manifest states this
//     arrangement where it explains why `unreachable_pub` is switched off.
#[cfg(feature = "libz-compat")]
mod compress;

// Exported `extern "C"` surface, private and `libz-compat`-gated for exactly the two reasons
// given above `mod compress`.
#[cfg(feature = "libz-compat")]
mod deflate;

// The decompression exports. Private for the same reason `compress` is: its
// `#[no_mangle]` items are reached through the dynamic symbol table, so a `pub mod`
// would add a second, Rust-side path to the same functions and put the crate root in
// the business of deciding what is exported -- which is `zlib.map`'s job.
//
// ★ This module additionally exports `inflate_table`, which `zlib.map`'s `local:`
// block hides from the shared library's dynamic table but which the unmodified
// `test/infcover.c` calls directly and links from `libz.a`. See the module's own
// documentation for why the two requirements are compatible rather than in conflict.
#[cfg(feature = "libz-compat")]
mod inflate;

// The three `inflateBack*` exports. Private and `libz-compat`-gated for the same
// two reasons `compress` is.
//
// ★ It sits beside `inflate` rather than inside it because its public entry points
// are distinct, which is exactly why `infback.c` is a separate translation unit
// that shares `inflate.h`, `inftrees.h` and `inffast.h` with `inflate.c`. The two
// modules likewise share one state representation and one message table: this one
// reads `inflate`'s `version_error` and `message_ptr` rather than growing second
// copies that could drift.
#[cfg(feature = "libz-compat")]
mod infback;

// Gated with the exported surface it belongs to: `libz-compat` is what turns the
// unmangled C symbols on, so a `--no-default-features` build compiles this module
// out along with the rest of them.
#[cfg(feature = "libz-compat")]
pub mod util;

// The `gzFile` layer: thirty exported `gz*` symbols plus the two hidden helpers
// `_zlib_rs_gzprintf_begin` and `_zlib_rs_gzprintf_commit` that `csrc/gzprintf_shim.c`
// calls. Three properties, all deliberate:
//
//   * TWO features gate it, not one. `libz-compat` turns the unmangled C symbols on,
//     as it does for every export module; `gz` additionally forwards into
//     `zlib-rs/std`, because file I/O is the only part of the port that needs `std` in
//     the core and `zlib-rs` declares its own `gz` subtree `#[cfg(feature = "std")]`.
//     With either feature off the crate still compiles and simply exports no `gz*`
//     symbols -- `--no-default-features` is a supported configuration and nothing here
//     may raise a `compile_error!` for it.
//   * The module is PRIVATE, for the same reason `compress` is: its `#[no_mangle]`
//     items reach their consumers through the dynamic symbol table, so a `pub mod`
//     would add a second, Rust-side path to the same functions and put the crate root
//     in the business of deciding what is exported -- which is `zlib.map`'s job.
//   * `gzprintf` and `gzvprintf` are NOT here. Stable Rust at the declared MSRV can
//     neither define a C-variadic function nor name a `va_list`, so those two symbols
//     come from the single C translation unit the packaging step links in; the module
//     documentation records the full reasoning and the helper contract.
#[cfg(all(feature = "libz-compat", feature = "gz"))]
mod gz;

// ---------------------------------------------------------------------------
//  Build-script bridge
// ---------------------------------------------------------------------------
//
// `build.rs` reports its decisions to the crate through one cfg and one
// environment variable, and both are surfaced here so that a consumer never has
// to guess which backend it linked.  Neither accessor is `extern "C"`, so
// neither adds a symbol to the dynamic table and neither appears in the header
// cbindgen generates.
//
// ★ The `cargo::rustc-check-cfg=cfg(zlib_rs_simd)` declaration that keeps
// `cfg!(zlib_rs_simd)` free of the `unexpected_cfgs` warning is emitted by
// `build.rs`, unconditionally and for every build.  Do NOT re-declare it here:
// two declarations of the same cfg are redundant, and moving the declaration
// into the source would make it disappear whenever the build script is skipped,
// which is exactly when the warning would be hardest to attribute.

/// Whether the vectorised checksum backends are compiled into this build.
///
/// `true` when the `simd` Cargo feature resolved on, which is when `build.rs`
/// emits `cargo::rustc-cfg=zlib_rs_simd`. The value is a property of the build,
/// not of the machine: SIMD is **output-neutral**, so this reports which code
/// path is present, never a different result. Runtime target-feature detection
/// lives in the core, so a `true` here on hardware without the instructions
/// still computes the same checksums by a different route.
#[must_use]
pub const fn simd_enabled() -> bool {
    cfg!(zlib_rs_simd)
}

/// The name of the checksum backend compiled into this build.
///
/// `"simd"` or `"scalar"`, taken from the `ZLIB_RS_CHECKSUM_BACKEND` value
/// `build.rs` emits with `cargo::rustc-env`. A benchmark can report the string
/// directly instead of inferring which implementation it measured.
#[must_use]
pub const fn checksum_backend() -> &'static str {
    env!("ZLIB_RS_CHECKSUM_BACKEND")
}

// The build script derives the cfg and the backend name from the RESOLVED
// feature set -- never from `ZLIB_RS_SIMD`, which is an assertion about that set
// rather than a way of choosing it.  These checks hold the two reports to that
// rule from the crate's side, so a build script edit that let them diverge
// would fail to compile instead of silently mislabelling a benchmark.
const _: () = {
    assert!(
        simd_enabled() == cfg!(feature = "simd"),
        "cfg(zlib_rs_simd) disagrees with the `simd` feature: build.rs must derive \
         the cfg from CARGO_FEATURE_SIMD, never from ZLIB_RS_SIMD"
    );
    // `str` equality is not const-callable at the declared MSRV and indexing is
    // denied by the workspace lint policy, so the two spellings are discriminated
    // by length -- which is exact, because `"simd"` and `"scalar"` are the only
    // two values build.rs emits and they differ in length.  build.rs owns the
    // spelling; this owns the agreement.
    let expected = if simd_enabled() { "simd" } else { "scalar" };
    assert!(
        checksum_backend().len() == expected.len(),
        "ZLIB_RS_CHECKSUM_BACKEND disagrees with cfg(zlib_rs_simd)"
    );
};

// ---------------------------------------------------------------------------
//  Crate-root re-exports -- the ABI types
// ---------------------------------------------------------------------------
//
// The types a C caller passes across the boundary, lifted to the crate root so
// that a Rust consumer writes `z::z_stream` rather than `z::types::z_stream`.
// The canonical definitions -- and their documentation, their `#[repr(C)]`
// attributes and their layout assertions -- stay in `types`, which remains `pub`
// for the helper machinery that is not part of the C contract:
// `StreamAllocator`, `StateBlock`, `stream_ref`, `input_slice` and the rest.
//
// This creates no dynamic symbol.  A `pub use` is a Rust-level path and nothing
// more, so the 111-symbol parity diff is untouched by anything in this section.
pub use crate::types::{
    alloc_func, charf, code, free_func, gzFile, gzFile_s, gz_header, gz_headerp, in_func,
    internal_state, intf, out_func, uInt, uIntf, uLong, uLongf, voidp, voidpc, voidpf, z_crc_t,
    z_off64_t, z_off_t, z_size_t, z_stream, z_streamp, Byte, Bytef,
};

// The version identity, re-exported from the module that owns it. Gated exactly
// as `util` is: with `libz-compat` off there is no introspection module to take
// it from, and a `--no-default-features` build must still compile.
#[cfg(feature = "libz-compat")]
pub use crate::util::{
    ZLIB_VERNUM, ZLIB_VERSION, ZLIB_VER_MAJOR, ZLIB_VER_MINOR, ZLIB_VER_REVISION,
    ZLIB_VER_SUBREVISION,
};

// ---------------------------------------------------------------------------
//  Crate-root re-exports -- the public constant surface
// ---------------------------------------------------------------------------
//
// `zlib.h` publishes its constants as `#define`s and `zconf.h` publishes two
// more, so a Rust consumer that wants to say `Z_FINISH` rather than `4` needs
// them as items.  They are declared here, at the barrel, because that is where a
// consumer looks and because no single export module owns them: the flush values
// are read by `deflate` and `inflate`, the strategies by `deflate`, the return
// codes by all seven.
//
// THREE decisions, each deliberate:
//
//   * The VALUE is the literal from the header, not an expression referring to
//     the core.  The header is the immutable contract, so quoting it directly is
//     the honest form -- and it is the only form cbindgen can render, because
//     `[parse] parse_deps = false` means cbindgen cannot resolve a `zlib_rs::…`
//     initialiser and would skip the constant with a warning, exactly as it
//     already does for the `panic_guard::fallback` constants typed on
//     `ReturnCode`, `c_ulong` and `usize`.
//
//   * The TYPE is `c_int`, because a bare integer `#define` used in `int`
//     position is an `int` in C.  `zconf.h` maps nothing else onto these, and
//     `layout_assertions` already pins `size_of::<c_int>() == 4` on LP64.
//
//   * AGREEMENT with the core is asserted at compile time, immediately after
//     each family.  The literal therefore cannot drift: if `zlib-rs` ever
//     disagreed with `zlib.h`, this crate would fail to build rather than export
//     a value the algorithms do not honour.  The nine return codes live in the
//     core as private constants reachable only through `ReturnCode`, which is
//     why those assertions go through `as_i32()` while the rest compare against
//     `zlib_rs::config`.

/// Successful completion — `zlib.h` L181.
pub const Z_OK: c_int = 0;
/// End of the compressed stream reached — `zlib.h` L182.
pub const Z_STREAM_END: c_int = 1;
/// A preset dictionary is needed before decompression can continue — `zlib.h` L183.
pub const Z_NEED_DICT: c_int = 2;
/// A file-system error occurred; consult `errno` — `zlib.h` L184.
pub const Z_ERRNO: c_int = -1;
/// The stream state is inconsistent, or a parameter was invalid — `zlib.h` L185.
pub const Z_STREAM_ERROR: c_int = -2;
/// The input data is corrupt or was not produced by a compatible encoder — `zlib.h` L186.
pub const Z_DATA_ERROR: c_int = -3;
/// Memory could not be allocated — `zlib.h` L187.
pub const Z_MEM_ERROR: c_int = -4;
/// No progress was possible: no input consumed and no output produced — `zlib.h` L188.
pub const Z_BUF_ERROR: c_int = -5;
/// The caller's `ZLIB_VERSION` is incompatible with the library's — `zlib.h` L189.
pub const Z_VERSION_ERROR: c_int = -6;

// The nine return codes, held to the core's `ReturnCode` newtype.  A mismatch
// here would mean an exported function returning a value the algorithms never
// produce, which is why it is a build failure rather than a test.
const _: () = {
    assert!(Z_OK == ReturnCode::OK.as_i32());
    assert!(Z_STREAM_END == ReturnCode::STREAM_END.as_i32());
    assert!(Z_NEED_DICT == ReturnCode::NEED_DICT.as_i32());
    assert!(Z_ERRNO == ReturnCode::ERRNO.as_i32());
    assert!(Z_STREAM_ERROR == ReturnCode::STREAM_ERROR.as_i32());
    assert!(Z_DATA_ERROR == ReturnCode::DATA_ERROR.as_i32());
    assert!(Z_MEM_ERROR == ReturnCode::MEM_ERROR.as_i32());
    assert!(Z_BUF_ERROR == ReturnCode::BUF_ERROR.as_i32());
    assert!(Z_VERSION_ERROR == ReturnCode::VERSION_ERROR.as_i32());
};

/// Accumulate input without forcing output — `zlib.h` L172.
pub const Z_NO_FLUSH: c_int = 0;
/// Flush to a byte boundary, then continue — `zlib.h` L173.
pub const Z_PARTIAL_FLUSH: c_int = 1;
/// Flush and align to a byte boundary with an empty stored block — `zlib.h` L174.
pub const Z_SYNC_FLUSH: c_int = 2;
/// Flush, align, and reset the compression state so decoding can restart here — `zlib.h` L175.
pub const Z_FULL_FLUSH: c_int = 3;
/// Finish the stream: no further input will be supplied — `zlib.h` L176.
pub const Z_FINISH: c_int = 4;
/// Stop at a block boundary, for `inflate` — `zlib.h` L177.
pub const Z_BLOCK: c_int = 5;
/// Stop when the deflate block header has been decoded — `zlib.h` L178.
///
/// Valid for `inflate` only. `deflate` rejects it: `zlib.h` documents the
/// allowed deflate flush values as `Z_NO_FLUSH` through `Z_BLOCK`, and passing
/// `Z_TREES` to `deflate` yields `Z_STREAM_ERROR`.
pub const Z_TREES: c_int = 6;

// The seven flush values, held to the core's `Flush` parameter constants.
const _: () = {
    assert!(Z_NO_FLUSH == core_config::Z_NO_FLUSH);
    assert!(Z_PARTIAL_FLUSH == core_config::Z_PARTIAL_FLUSH);
    assert!(Z_SYNC_FLUSH == core_config::Z_SYNC_FLUSH);
    assert!(Z_FULL_FLUSH == core_config::Z_FULL_FLUSH);
    assert!(Z_FINISH == core_config::Z_FINISH);
    assert!(Z_BLOCK == core_config::Z_BLOCK);
    assert!(Z_TREES == core_config::Z_TREES);
};

/// Store only, no compression — `zlib.h` L194.
pub const Z_NO_COMPRESSION: c_int = 0;
/// Fastest compression — `zlib.h` L195.
pub const Z_BEST_SPEED: c_int = 1;
/// Smallest output — `zlib.h` L196.
pub const Z_BEST_COMPRESSION: c_int = 9;
/// Let the library choose; equivalent to level 6 — `zlib.h` L197.
pub const Z_DEFAULT_COMPRESSION: c_int = -1;

/// Tuned for data produced by a filter or predictor — `zlib.h` L200.
pub const Z_FILTERED: c_int = 1;
/// Never look for matches; emit Huffman-coded literals only — `zlib.h` L201.
pub const Z_HUFFMAN_ONLY: c_int = 2;
/// Limit match distances to one, for run-length encoding — `zlib.h` L202.
pub const Z_RLE: c_int = 3;
/// Forbid dynamic Huffman codes, for a simpler decoder — `zlib.h` L203.
pub const Z_FIXED: c_int = 4;
/// The normal strategy — `zlib.h` L204.
pub const Z_DEFAULT_STRATEGY: c_int = 0;

// The four levels and the five strategies, held to the core's parameter
// constants.  These feed `deflateInit2_`, `deflateParams` and `deflateTune`, and
// the level in particular selects a row of the configuration table that decides
// the emitted bytes -- so a divergence would break byte-identical output.
const _: () = {
    assert!(Z_NO_COMPRESSION == core_config::Z_NO_COMPRESSION);
    assert!(Z_BEST_SPEED == core_config::Z_BEST_SPEED);
    assert!(Z_BEST_COMPRESSION == core_config::Z_BEST_COMPRESSION);
    assert!(Z_DEFAULT_COMPRESSION == core_config::Z_DEFAULT_COMPRESSION);
    assert!(Z_FILTERED == core_config::Z_FILTERED);
    assert!(Z_HUFFMAN_ONLY == core_config::Z_HUFFMAN_ONLY);
    assert!(Z_RLE == core_config::Z_RLE);
    assert!(Z_FIXED == core_config::Z_FIXED);
    assert!(Z_DEFAULT_STRATEGY == core_config::Z_DEFAULT_STRATEGY);
};

/// `z_stream::data_type`: the input looks like binary data — `zlib.h` L207.
pub const Z_BINARY: c_int = 0;
/// `z_stream::data_type`: the input looks like text — `zlib.h` L208.
pub const Z_TEXT: c_int = 1;
/// Retained spelling of [`Z_TEXT`], for callers written against 1.2.2 and earlier — `zlib.h` L209.
pub const Z_ASCII: c_int = Z_TEXT;
/// `z_stream::data_type`: the classification is not yet decided — `zlib.h` L210.
pub const Z_UNKNOWN: c_int = 2;

/// The DEFLATE method, the only compression method this format defines — `zlib.h` L213.
pub const Z_DEFLATED: c_int = 8;

// The three data-type values, their compatibility alias, and the method.
// `detect_data_type` in the core is what actually produces the first three, so
// the assertion ties the reported `data_type` to the code that computes it.
const _: () = {
    assert!(Z_BINARY == core_config::Z_BINARY);
    assert!(Z_TEXT == core_config::Z_TEXT);
    assert!(Z_ASCII == core_config::Z_ASCII);
    assert!(Z_UNKNOWN == core_config::Z_UNKNOWN);
    assert!(Z_DEFLATED == core_config::Z_DEFLATED);
};

/// The null value for `zalloc`, `zfree` and `opaque` — `zlib.h` L216.
///
/// `zlib.h` spells it `#define Z_NULL 0`, an untyped zero used in both integer
/// and pointer position, so the Rust form is the integer and this constant
/// exists for parity and for reading C code alongside this crate. Rust callers
/// should not use it to build a `z_stream`: the hook types are
/// `Option<unsafe extern "C" fn(…)>`, so a null hook is [`None`], and `opaque`
/// is a raw pointer, so a null opaque is `core::ptr::null_mut()`. Modelling the
/// hooks as `Option` is what lets the compiler prove a null hook was considered.
pub const Z_NULL: c_int = 0;

/// The largest `windowBits` `deflateInit2_` and `inflateInit2_` accept — `zconf.h` L287.
///
/// A 32 KiB LZ77 window. `zconf.h` guards the definition with `#ifndef`, so a
/// caller may compile against a smaller window — and warns that reducing it
/// makes `minigzip` unable to extract files produced by `gzip`. This library is
/// built at the full 15, which is what the installed header declares.
pub const MAX_WBITS: c_int = 15;

/// The largest `memLevel` `deflateInit2_` accepts — `zconf.h` L277.
///
/// `zconf.h` defines 8 under `MAXSEG_64K` — the 16-bit segmented configurations,
/// none of which is a supported target here — and 9 otherwise, which is the
/// value this library is built with.
pub const MAX_MEM_LEVEL: c_int = 9;

// The two `zconf.h` bounds, held to the core's validation limits.  These are the
// values `deflateInit2_` and `inflateInit2_` range-check their arguments
// against, so a divergence would accept a configuration the core cannot honour
// or reject one it can.
const _: () = {
    assert!(MAX_WBITS == core_config::MAX_WBITS);
    assert!(MAX_MEM_LEVEL == core_config::MAX_MEM_LEVEL);
};
