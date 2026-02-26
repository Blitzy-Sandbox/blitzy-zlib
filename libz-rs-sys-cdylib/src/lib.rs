//! C-compatible shared library providing zlib API symbols.
//!
//! This crate re-exports all `#[no_mangle] extern "C"` functions from
//! [`libz_rs_sys`], producing a platform-native shared library (cdylib) that
//! serves as a drop-in replacement for the C zlib shared library
//! (`libz.so` / `zlib1.dll` / `libz.dylib`).
//!
//! # How It Works
//!
//! The `libz-rs-sys` crate contains all 105+ `#[no_mangle] extern "C"`
//! function implementations that match the symbols listed in `win32/zlib.def`.
//! This crate simply performs a wildcard re-export (`pub use libz_rs_sys::*;`)
//! so that all those symbols appear in this crate's public surface. Because
//! the `Cargo.toml` specifies `crate-type = ["cdylib"]` with `name = "z"`,
//! Cargo and rustc export every `#[no_mangle]` symbol into the dynamic
//! library's symbol table, yielding a shared library that C/C++ applications
//! can link against exactly as they would the original C `libz`.
//!
//! # Exported Symbol Categories (105 symbols from `win32/zlib.def`)
//!
//! - **Basic functions (5):** `zlibVersion`, `deflate`, `deflateEnd`,
//!   `inflate`, `inflateEnd`
//! - **Advanced deflate (12):** `deflateSetDictionary`, `deflateGetDictionary`,
//!   `deflateCopy`, `deflateReset`, `deflateParams`, `deflateTune`,
//!   `deflateBound`, `deflateBound_z`, `deflatePending`, `deflateUsed`,
//!   `deflatePrime`, `deflateSetHeader`
//! - **Advanced inflate (12):** `inflateSetDictionary`,
//!   `inflateGetDictionary`, `inflateSync`, `inflateCopy`, `inflateReset`,
//!   `inflateReset2`, `inflatePrime`, `inflateMark`, `inflateGetHeader`,
//!   `inflateBack`, `inflateBackEnd`, `zlibCompileFlags`
//! - **Compress utilities (10):** `compress`, `compress2`, `compress_z`,
//!   `compress2_z`, `compressBound`, `compressBound_z`, `uncompress`,
//!   `uncompress2`, `uncompress_z`, `uncompress2_z`
//! - **Gzip file I/O (27):** `gzopen`, `gzdopen`, `gzbuffer`, `gzsetparams`,
//!   `gzread`, `gzfread`, `gzwrite`, `gzfwrite`, `gzprintf`, `gzvprintf`,
//!   `gzputs`, `gzgets`, `gzputc`, `gzgetc`, `gzungetc`, `gzflush`,
//!   `gzseek`, `gzrewind`, `gztell`, `gzoffset`, `gzeof`, `gzdirect`,
//!   `gzclose`, `gzclose_r`, `gzclose_w`, `gzerror`, `gzclearerr`
//! - **Large-file variants (7):** `gzopen64`, `gzseek64`, `gztell64`,
//!   `gzoffset64`, `adler32_combine64`, `crc32_combine64`,
//!   `crc32_combine_gen64`
//! - **Checksum functions (8):** `adler32`, `adler32_z`, `crc32`, `crc32_z`,
//!   `adler32_combine`, `crc32_combine`, `crc32_combine_gen`,
//!   `crc32_combine_op`
//! - **Init / compat functions (15):** `deflateInit_`, `deflateInit2_`,
//!   `inflateInit_`, `inflateInit2_`, `inflateBackInit_`, `gzgetc_`,
//!   `zError`, `inflateSyncPoint`, `get_crc_table`, `inflateUndermine`,
//!   `inflateValidate`, `inflateCodesUsed`, `inflateResetKeep`,
//!   `deflateResetKeep`, `gzopen_w`
//! - **Types and constants:** `z_stream`, `gz_header`, `gzFile`,
//!   `alloc_func`, `free_func`, `ZLIB_VERSION`, `ZLIB_VERNUM`, and all
//!   C-ABI constants (`Z_OK` through `Z_VERSION_ERROR`, flush modes,
//!   strategies, compression levels)
//!
//! # Feature Flags
//!
//! Feature gates are inherited transitively from `libz-rs-sys`:
//!
//! - **`gz-io`** (default: enabled) — When disabled, the `gz` module in
//!   `libz-rs-sys` is excluded and all `gz*` symbols are omitted from the
//!   shared library.
//! - **`gzip`** (default: enabled) — When disabled, gzip format support is
//!   excluded from compression/decompression routines.
//!
//! No explicit `#[cfg(feature = "...")]` annotations are needed in this file;
//! the feature gating happens in `libz-rs-sys/src/lib.rs` where modules are
//! conditionally declared.
//!
//! # Safety
//!
//! This file contains **zero** `unsafe` code — it is purely a re-export layer.
//! All `unsafe` operations are confined to the `libz-rs-sys` crate where the
//! actual `extern "C"` function implementations live. The `Cargo.toml` for
//! this crate ensures:
//!
//! - `crate-type = ["cdylib"]` — produces the platform-native shared library.
//! - `panic = "abort"` (via workspace release profile) — prevents undefined
//!   behavior from stack unwinding across FFI boundaries.
//! - All FFI functions use `extern "C"` (not `extern "C-unwind"`).

// ---------------------------------------------------------------------------
// Crate-level lint configuration
// ---------------------------------------------------------------------------
//
// The `non_snake_case` allow is required because the C zlib API uses camelCase
// function names (deflateInit2_, compressBound, inflateGetHeader, etc.) that
// are re-exported through this crate.
//
// The `non_camel_case_types` allow is required because the C zlib types use
// snake_case (z_stream, gz_header, alloc_func, free_func, gzFile, etc.).
//
// Clippy pedantic and other strict lints are NOT applied here since this is
// purely a re-export layer with no logic of its own. The actual implementations
// in `libz-rs-sys` carry full lint enforcement.
#![allow(non_snake_case)]
#![allow(non_camel_case_types)]

// ===========================================================================
// Core re-export — surfaces all FFI symbols into the cdylib symbol table
// ===========================================================================
//
// This single wildcard re-export is sufficient because:
//
// 1. `libz-rs-sys/src/lib.rs` declares sub-modules (types, deflate, inflate,
//    checksum, compress, version, gz) and re-exports them via `pub use mod::*`.
// 2. Every FFI function is marked `#[no_mangle] pub extern "C"` (or
//    `pub unsafe extern "C"`).
// 3. By re-exporting here, all `#[no_mangle]` functions become part of this
//    crate's public surface.
// 4. Since this crate's `Cargo.toml` specifies `crate-type = ["cdylib"]`,
//    Cargo/rustc emits every `#[no_mangle]` symbol into the dynamic library's
//    export table.
// 5. The result is a shared library containing all 105 symbols matching
//    `win32/zlib.def`, plus `#[repr(C)]` types and version constants.
pub use libz_rs_sys::*;
