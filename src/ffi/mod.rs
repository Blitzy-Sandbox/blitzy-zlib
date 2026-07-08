//! # `ffi` — the C-compatible drop-in boundary of `zlib-rs`
//!
//! This module is the crate's **foreign-function-interface (FFI) boundary**: the
//! single place where the idiomatic, memory-safe Rust engines are exposed to C
//! callers with **byte-for-byte zlib C ABI fidelity**. Compiled into the
//! `cdylib`/`staticlib` artifacts (see the crate's `[lib] crate-type`), it turns
//! `zlib-rs` into a drop-in replacement for the reference `libz`: a C program can
//! link against the emitted object, or an operator can inject it via an
//! `LD_PRELOAD`-style substitution, and every documented `zlib.h` entry point
//! resolves to a safe-Rust implementation without any source changes on the
//! caller's side.
//!
//! ## What lives here
//!
//! * `#[unsafe(no_mangle)] extern "C"` **shims** — one per public `zlib.h`
//!   prototype — that validate raw C inputs, bridge to the safe engines, and
//!   re-materialize zlib's integer return codes at the boundary. They are defined
//!   in the submodules [`util`], [`deflate`](mod@deflate), [`inflate`](mod@inflate), and [`gz`].
//! * `#[repr(C)]` **struct mirrors** ([`z_stream`], [`gz_header`], [`gzFile_s`])
//!   whose field order and widths match `zlib.h` exactly, the C scalar aliases
//!   ([`Bytef`], [`uInt`], [`uLong`], …), the C function-pointer typedefs
//!   ([`alloc_func`], [`free_func`], [`in_func`], [`out_func`]), and the
//!   caller-allocator bridge [`CAllocator`]. These are defined in [`types`] and
//!   re-exported here (see also the raw ↔ idiomatic conversion helpers in
//!   [`types`], which turn a raw [`z_stream`]/[`gz_header`] into the idiomatic
//!   [`ZStream`](crate::stream::ZStream)/[`GzHeader`](crate::gz_header::GzHeader)).
//!
//! ## Safe core, unsafe boundary
//!
//! Per the migration's unsafe-isolation strategy (AAP §0.6.2 / §0.7.2), **all**
//! `unsafe` in the crate is confined to this `ffi` tree. The compression,
//! decompression, checksum, gzip-I/O, and one-call engines — [`crate::deflate`],
//! [`crate::inflate`], [`crate::checksum`], [`crate::gz`], and [`crate::util`] —
//! contain the actual logic and are written in fully safe Rust (the deflate
//! engine has **zero** `unsafe`). Every `unsafe` operation in the sibling shim
//! files carries a `// SAFETY:` justification, and every fallible shim body is
//! wrapped in a `catch_unwind` guard so a Rust panic can never unwind across the
//! `extern "C"` boundary into C.
//!
//! This root file itself contains **no** `unsafe`, **no** `#[unsafe(no_mangle)]`,
//! and **no** business logic: it only declares the submodules, re-exports their
//! public surface, and documents the exported symbol table. The
//! `#[unsafe(no_mangle)]` shims are emitted into the object file as exported
//! symbols by virtue of being compiled into the crate — *independent* of any
//! re-export. The `pub use` statements below therefore exist to present a single,
//! centralized Rust API surface (`zlib_rs::ffi::<symbol>`) and to document the
//! ABI; they do **not** change symbol emission.
//!
//! ## Submodule map
//!
//! | Submodule   | C sources                                                     | Responsibility                                             |
//! |-------------|---------------------------------------------------------------|------------------------------------------------------------|
//! | [`types`]   | `zlib.h`, `zconf.h`                                            | `#[repr(C)]` ABI mirrors, scalar aliases, allocator bridge |
//! | [`util`]    | `compress.c`, `uncompr.c`, `adler32.c`, `crc32.c`, `zutil.c`   | one-call, checksum, and version/error shims                |
//! | [`deflate`](mod@deflate) | `deflate.c`, `zlib.h`                                          | `deflate*` compression shims                               |
//! | [`inflate`](mod@inflate) | `inflate.c`, `infback.c`, `zlib.h`                             | `inflate*` / `inflateBack*` decompression shims            |
//! | [`gz`]      | `gzlib.c`, `gzread.c`, `gzwrite.c`, `gzclose.c`, `gzguts.h`    | `gz*` file-I/O shims (Cargo feature `gz-io`)               |
//!
//! ## Feature gating
//!
//! The [`gz`] submodule maps zlib's gzip file-I/O layer, which fundamentally
//! requires the standard library for filesystem access. It is therefore compiled
//! only when the `gz-io` Cargo feature is enabled (which implies `std` and
//! `gzip`), matching the gating in [`crate::gz`] and the `#[cfg(feature =
//! "gz-io")] pub mod gz;` declaration in `src/lib.rs`. Because the sibling
//! `src/ffi/gz.rs` additionally self-gates with a module-level
//! `#![cfg(feature = "gz-io")]`, the `#[cfg(feature = "gz-io")]` attribute on the
//! `pub mod gz;` declaration below is kept consistent to avoid a module/`cfg`
//! mismatch. A build **without** `gz-io` still links the complete set of core
//! (`deflate`/`inflate`/`checksum`/one-call/version) symbols: [`types`],
//! [`util`], [`deflate`](mod@deflate), and [`inflate`](mod@inflate) are always compiled so the core zlib
//! symbol table is always present for linkage.
//!
//! Within the `gz-io` layer, `gzprintf`/`gzvprintf` are always exported as
//! ABI-compatible **error-returning stubs** that yield `Z_STREAM_ERROR`:
//! rendering a C `va_list` from Rust requires the unstable (nightly-only)
//! `c_variadic` feature, so — precisely as zlib documents for a build without
//! secure `*printf` (and reflected by `zlibCompileFlags` bit 27; see
//! [`crate::util::version`]) — the symbols exist but return an error. They
//! always resolve, so a C caller's link never fails.
//!
//! ## `zlib.map` symbol-versioning contract
//!
//! zlib ships a linker version script (`zlib.map`) that assigns each exported
//! symbol to an ELF version node so consumers link against a specific
//! `libz.so.1` symbol version. Reproducing symbol versioning on the emitted
//! `cdylib` is **optional** for a functional drop-in — the `#[unsafe(no_mangle)]`
//! shims are exported *unversioned* by default, which satisfies ordinary linking
//! and `LD_PRELOAD` injection. The mapping is recorded here verbatim so a
//! downstream packaging step can generate a version script and achieve strict
//! `libz.so.1` symbol-versioning parity if required.
//!
//! **Unversioned base symbols** (predate the versioning scheme introduced in
//! zlib 1.2.0; they are *not* listed in any `zlib.map` node and are exported
//! without a version tag): `deflate`, `inflate`, `deflateInit_`, `deflateInit2_`,
//! `inflateInit_`, `inflateInit2_`, `deflateEnd`, `inflateEnd`, `deflateReset`,
//! `inflateReset`, `deflateParams`, `deflateSetDictionary`,
//! `inflateSetDictionary`, `deflateCopy`, `compress`, `compress2`, `uncompress`,
//! `adler32`, `crc32`, `get_crc_table`, `gzopen`, `gzdopen`, `gzread`, `gzwrite`,
//! `gzprintf`, `gzputs`, `gzgets`, `gzputc`, `gzgetc`, `gzflush`, `gzseek`,
//! `gzrewind`, `gztell`, `gzeof`, `gzclose`, `gzerror`, `gzsetparams`,
//! `zlibVersion`, `zError`, `inflateSync`, `inflateSyncPoint`.
//!
//! **Versioned nodes** (exactly as enumerated in the repository's `zlib.map`;
//! each node inherits the symbols of the node it extends):
//!
//! * `ZLIB_1.2.0`: `compressBound`, `deflateBound`, `inflateBack`,
//!   `inflateBackEnd`, `inflateBackInit_`, `inflateCopy`.
//! * `ZLIB_1.2.0.2`: `gzclearerr`, `gzungetc`, `zlibCompileFlags`.
//! * `ZLIB_1.2.0.8`: `deflatePrime`.
//! * `ZLIB_1.2.2`: `adler32_combine`, `crc32_combine`, `deflateSetHeader`,
//!   `inflateGetHeader`.
//! * `ZLIB_1.2.2.3`: `deflateTune`, `gzdirect`.
//! * `ZLIB_1.2.2.4`: `inflatePrime`.
//! * `ZLIB_1.2.3.3`: `adler32_combine64`, `crc32_combine64`, `gzopen64`,
//!   `gzseek64`, `gztell64`, `inflateUndermine`.
//! * `ZLIB_1.2.3.4`: `inflateReset2`, `inflateMark`.
//! * `ZLIB_1.2.3.5`: `gzbuffer`, `gzoffset`, `gzoffset64`, `gzclose_r`,
//!   `gzclose_w`.
//! * `ZLIB_1.2.5.1`: `deflatePending`.
//! * `ZLIB_1.2.5.2`: `deflateResetKeep`, `gzgetc_`, `inflateResetKeep`.
//! * `ZLIB_1.2.7.1`: `inflateGetDictionary`, `gzvprintf`.
//! * `ZLIB_1.2.9`: `inflateCodesUsed`, `inflateValidate`, `uncompress2`,
//!   `gzfread`, `gzfwrite`, `deflateGetDictionary`, `adler32_z`, `crc32_z`.
//! * `ZLIB_1.2.12`: `crc32_combine_gen`, `crc32_combine_gen64`,
//!   `crc32_combine_op`.
//! * `ZLIB_1.3.1.2`: `deflateUsed`.
//! * `ZLIB_1.3.2`: `compressBound_z`, `deflateBound_z`, `compress_z`,
//!   `compress2_z`, `uncompress_z`, `uncompress2_z`.
//!
//! **`local:` symbols — never exported** (internal to the C sources; the Rust
//! ports keep these private and deliberately do **not** apply
//! `#[unsafe(no_mangle)]` to their equivalents): `deflate_copyright`,
//! `inflate_copyright`, `inflate_fast`, `inflate_table`, `zcalloc`, `zcfree`,
//! `z_errmsg`, `gz_error`, `gz_intmax`, `inflate_fixed`, plus the catch-all
//! `_*` pattern that hides any remaining underscore-prefixed symbols.

// ===========================================================================
// Submodule declarations
// ===========================================================================
//
// `types` is the foundational module every shim builds on; `util`, `deflate`,
// and `inflate` are always compiled so the core zlib symbol table is always
// present for linkage. `gz` is feature-gated (see the module-level docs above).

pub mod deflate;
pub mod inflate;
pub mod types;
pub mod util;

// The caller-allocator (`zalloc`/`zfree`) buffer bridge. This is internal
// plumbing (no `extern "C"` symbols), not part of the public C surface, so it is
// `pub(crate)` rather than `pub` and is intentionally NOT glob-re-exported below.
// It is the sanctioned home of the raw allocator-hook `unsafe`, which the safe
// core (`src/stream.rs`) delegates to via `AllocHook::try_alloc_zeroed` (M6).
pub(crate) mod alloc;

#[cfg(feature = "gz-io")]
pub mod gz;

// ===========================================================================
// Re-exports — centralized Rust-visible symbol surface
// ===========================================================================
//
// These globs surface the shim functions, the `#[repr(C)]` mirrors, and the C
// type aliases under a single `zlib_rs::ffi::*` path for idiomatic Rust
// consumers and for documentation. They do NOT affect `#[unsafe(no_mangle)]`
// symbol emission (which happens from each defining submodule regardless).
//
// The re-exported surfaces are disjoint by construction — `types` exports the
// `#[repr(C)]` structs, scalar/pointer aliases, and conversion helpers, while
// each shim submodule exports only uniquely named `extern "C"` functions — so
// no glob collision or ambiguous re-export arises. (The `deflate`/`inflate`
// module names live in the type namespace while the identically named `deflate`
// / `inflate` shim functions live in the value namespace, so both coexist.)

pub use deflate::*;
pub use inflate::*;
pub use types::*;
pub use util::*;

#[cfg(feature = "gz-io")]
pub use gz::*;

// ===========================================================================
// Compile-time symbol-presence guards
// ===========================================================================

#[cfg(test)]
mod tests {
    //! Lightweight signature guards.
    //!
    //! Each guard binds the address of a representative `extern "C"` shim to a
    //! function pointer of the exact expected C signature. These are pure
    //! type-checks — no shim is ever invoked — so the module has no runtime
    //! effect but fails to compile if a sibling shim's signature ever drifts,
    //! catching an ABI regression at build time. Coercing a function *item* to a
    //! function *pointer* does not require an `unsafe` block, so this module
    //! upholds the "no `unsafe` in `mod.rs`" rule.

    #[test]
    fn core_symbols_have_expected_c_signatures() {
        use crate::ffi::types::{Bytef, uInt, uLong, uLongf, z_streamp};
        use core::ffi::c_int;

        // Compression / decompression drivers.
        let _deflate: unsafe extern "C" fn(z_streamp, c_int) -> c_int =
            crate::ffi::deflate::deflate;
        let _inflate: unsafe extern "C" fn(z_streamp, c_int) -> c_int =
            crate::ffi::inflate::inflate;

        // One-call helpers and the pre-allocation bound.
        let _compress: unsafe extern "C" fn(*mut Bytef, *mut uLongf, *const Bytef, uLong) -> c_int =
            crate::ffi::util::compress;
        let _compress_bound: unsafe extern "C" fn(uLong) -> uLong = crate::ffi::util::compressBound;

        // Checksums.
        let _adler32: unsafe extern "C" fn(uLong, *const Bytef, uInt) -> uLong =
            crate::ffi::util::adler32;
        let _crc32: unsafe extern "C" fn(uLong, *const Bytef, uInt) -> uLong =
            crate::ffi::util::crc32;

        // Bind each to a shared reference so the guards are observed (and thus
        // fully type-checked) without triggering `unused_variables`.
        let _ = (
            _deflate,
            _inflate,
            _compress,
            _compress_bound,
            _adler32,
            _crc32,
        );
    }

    #[cfg(feature = "gz-io")]
    #[test]
    fn gz_symbols_have_expected_c_signatures() {
        use crate::ffi::types::gzFile;
        use core::ffi::c_char;

        let _gzopen: unsafe extern "C" fn(*const c_char, *const c_char) -> gzFile =
            crate::ffi::gz::gzopen;
        let _ = _gzopen;
    }

    // `gzprintf`/`gzvprintf` are exported as error-returning stubs with these
    // fixed signatures. This guard fails to compile if either symbol is dropped
    // or its fixed leading parameters drift, catching the "missing
    // gzprintf/gzvprintf" regression at build time.
    #[cfg(feature = "gz-io")]
    #[test]
    fn default_gzprintf_stub_symbols_are_present() {
        use crate::ffi::types::gzFile;
        use core::ffi::{c_char, c_int, c_void};

        let _gzprintf: extern "C" fn(gzFile, *const c_char) -> c_int = crate::ffi::gz::gzprintf;
        let _gzvprintf: extern "C" fn(gzFile, *const c_char, *mut c_void) -> c_int =
            crate::ffi::gz::gzvprintf;
        let _ = (_gzprintf, _gzvprintf);
    }
}
