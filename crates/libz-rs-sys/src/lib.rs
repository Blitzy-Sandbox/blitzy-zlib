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
//! ★ Note the asymmetry, and note it precisely, because it is easy to get
//! backwards. `[lib] name = "z"` renames the **library target**, and a library
//! target's name is also its extern crate name — it does not merely set the
//! emitted file name. So the *package* is `libz-rs-sys`, which is what a
//! `Cargo.toml` dependency line and `cargo build -p` name, but the *extern
//! crate* is `z`, which is what Rust code writes:
//!
//! ```text
//! # Cargo.toml of a consumer
//! libz-rs-sys = { path = "../libz-rs-sys" }
//! ```
//!
//! ```text
//! // Rust source of that consumer -- `z`, never `libz_rs_sys`
//! use z::types::z_stream;
//! ```
//!
//! This applies to every consumer without exception: the integration tests under
//! `tests/`, the doctests in this crate, `zlib-rs-differential`, the `fuzz/`
//! targets and `benches/`. Verified rather than assumed — `use libz_rs_sys::…`
//! fails to compile with "unresolved module or unlinked crate `libz_rs_sys`",
//! and `cargo test --doc` reports the suite as "Doc-tests z".
//!
//! The relink is not an implementation detail that can be optimised away. A
//! version script cannot be attached to a rustc-built `cdylib`: rustc already
//! passes its own anonymous-tag script, and `ld` refuses to combine an
//! anonymous version tag with the named tags `zlib.map` declares. The C build
//! exports **111** dynamic globals — 95 functions plus 16 symbol-version nodes
//! — and reproducing that set requires linking `libz.a` with the C linker under
//! `--version-script`. Packaging owns that step; this crate's `build.rs` owns
//! only the link arguments.
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
//! would make the crate unable to do its one job. The workspace consequently
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
//! * Every exported function is declared `extern "C"`, never
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
//! | `libz-compat` | on | Gates the exported `extern "C"` surface — the unmangled symbols that make this library ABI-compatible with the C original. Named exactly as the documented build command requires: `cargo build --release --features libz-compat`. |
//! | `gz` | on | Gates the `gzFile` layer. File I/O is the only part of the port that needs `std` in the core, so this forwards into `zlib-rs/std`. |
//! | `simd` | off | Pass-through to `zlib-rs/simd`: vectorised CRC-32 and Adler-32 backends. |
//! | `libc` | off | Explicit opt-in for the optional `libc` dependency, for the rare platform type or raw syscall that `std::fs` and `std::io` cannot express. |
//!
//! `--no-default-features` is a supported configuration. It compiles cleanly
//! and yields a library whose exported surface and gz layer are simply gated
//! out; it exists for feature-matrix checking rather than for shipping. Nothing
//! in this crate may raise a `compile_error!` for it.
//!
//! `simd` is confined to the two checksums and is **output-neutral**: a checksum
//! is one scalar however it is computed, so vectorising it cannot perturb the
//! bitstream. Vectorising match-finding would change the compressed output and
//! is prohibited outright, because byte-identical output is a hard requirement
//! of this port.
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
//!   unsafe categories above. Every other module depends on it.
//! * `layout_assertions` — the ABI gate. A private module of nothing but
//!   `const _: () = assert!(…)` items pinning every struct size, field offset
//!   and integer width `zlib.h`, `zconf.h` and `inftrees.h` fix, so that layout
//!   drift is a build failure rather than silent memory corruption in every
//!   program that links the result. It exports nothing — deliberately, since an
//!   extra name in the dynamic symbol table would fail the 111-symbol parity
//!   diff — and is the first of the four mechanisms that enforce header
//!   immutability, alongside the cbindgen header diff, the `nm` symbol-parity
//!   diff and the `c-std.yml` C89-through-gnu2x sweep.
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
//! * `util` — library introspection: `zlibVersion`, `zlibCompileFlags`, `zError`
//!   and `get_crc_table`, four of the ninety-five exports. Also the home of the
//!   version identity — `util::ZLIB_VERSION` and the `ZLIB_VER_*` numbers — which
//!   is why it is the module the init entry points take their comparison string
//!   from. Gated behind `libz-compat`, like every module that contributes exported
//!   symbols. Named in code spans rather than as intra-doc links on purpose: the
//!   module is `cfg`-gated, so a link would be unresolvable — and therefore a
//!   rustdoc error under `-D warnings` — in a `--no-default-features` build, which
//!   this crate supports.

#![allow(non_camel_case_types)]
#![deny(unsafe_op_in_unsafe_fn)]
#![deny(missing_docs)]

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
