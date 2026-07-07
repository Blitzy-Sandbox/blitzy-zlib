//! # `zlib-rs` — a safe-Rust reimplementation of zlib
//!
//! `zlib-rs` is an idiomatic, memory-safe Rust port of zlib `1.3.2.1-motley`
//! ([`ZLIB_VERNUM`] `0x1321`). It reproduces the DEFLATE (RFC 1951), zlib
//! (RFC 1950), and gzip (RFC 1952) stream formats with **byte-for-byte** output
//! compatibility with the reference C library, and exposes a C-compatible FFI so
//! the emitted `cdylib`/`staticlib` (`libzlib_rs.so` / `libzlib_rs.a`) can drop
//! in for the system `libz`.
//!
//! ## Safe core, unsafe boundary
//!
//! The crate is organized as a **safe core** wrapped by a thin **unsafe
//! boundary**. Every compression, decompression, checksum, and one-call engine
//! is written in fully safe Rust — the deflate engine ([`deflate`]) contains
//! **zero** `unsafe` — while all C-ABI raw-pointer handling is confined to the
//! [`ffi`] module (and, at most, the inflate fast-path decode loop). The manual
//! `zcalloc`/`zcfree` memory management of the C sources is replaced by Rust
//! ownership: a [`ZStream`] owns its boxed engine state and releases it in
//! [`Drop`], so the "uninitialized-state pointer" hazards of the C API become
//! unrepresentable.
//!
//! ## Architecture
//!
//! The crate mirrors the six functional layers of the C baseline as modules,
//! adds a public-API-types layer, and isolates the C ABI behind [`ffi`]:
//!
//! | Module        | Ported from (C)                           | Responsibility                              |
//! |---------------|-------------------------------------------|---------------------------------------------|
//! | [`error`]     | `zutil.h`                                 | [`ReturnCode`] / [`ZlibError`] error model  |
//! | [`constants`] | `zlib.h`                                  | flush modes, levels, strategies, wrap modes |
//! | [`stream`]    | `zlib.h`                                  | idiomatic [`ZStream`] + [`Allocator`]       |
//! | [`gz_header`] | `zlib.h`                                  | idiomatic [`GzHeader`]                      |
//! | [`checksum`]  | `adler32.c`, `crc32.c`                    | Adler-32 and CRC-32                         |
//! | [`deflate`]   | `deflate.c`, `trees.c`                    | compression engine (zero `unsafe`)          |
//! | [`inflate`]   | `inflate.c`, `inffast.c`, `infback.c`, …  | decompression engine incl. `inflateBack`    |
//! | [`util`]      | `compress.c`, `uncompr.c`, `zutil.c`      | one-call wrappers and version reporting     |
//! | [`gz`]        | `gzlib.c`, `gzread.c`, `gzwrite.c`, …     | gzip file-I/O (Cargo feature `gz-io`)       |
//! | [`ffi`]       | `zlib.h`, `zconf.h`, `zlib.map`           | `extern "C"` drop-in boundary               |
//!
//! ## `no_std`
//!
//! The crate is `#![no_std]` + [`alloc`] whenever the `std` feature is disabled,
//! so the compression core can be embedded in freestanding environments. The
//! `std` feature (enabled by default) links the standard library for the gz
//! file-I/O layer ([`std::fs`]/[`std::io`]) and for `catch_unwind` panic guards
//! at the FFI boundary. Core modules use [`alloc`] types ([`alloc::boxed::Box`],
//! [`alloc::vec::Vec`], [`alloc::string::String`]) rather than their `std`
//! re-exports, so the whole crate participates in `no_std` builds.
//!
//! ## Feature flags
//!
//! | Feature          | Default | Effect                                                       |
//! |------------------|:-------:|--------------------------------------------------------------|
//! | `std`            |   yes   | Standard-library build (file I/O, `catch_unwind` guards).    |
//! | `gzip`           |   yes   | gzip framing within deflate/inflate (mirrors C `#ifdef GZIP`). |
//! | `gz-io`          |   yes   | gzip file-I/O `gz*` API; implies `std` + `gzip`.             |
//! | `simd`           |   yes   | SIMD-accelerated CRC-32 via the `crc32fast` crate.          |
//! | `c-variadic`     |   no    | Variadic `gzprintf`/`gzvprintf` shims (requires nightly).   |
//! | `inflate_strict` |   no    | Stricter inflate distance validation (mirrors C `INFLATE_STRICT`). |
//!
//! Building with `--no-default-features` yields the `no_std` compression and
//! decompression core without the gz file-I/O layer (the [`gz`] module is gated
//! behind `gz-io`, which implies `std`).
//!
//! ## Quick start
//!
//! ```
//! use zlib_rs::{compress2, compress_bound, uncompress, Z_BEST_COMPRESSION};
//!
//! let source = b"the quick brown fox jumps over the lazy dog";
//!
//! // Size the destination with the same bound formula the C library uses.
//! let mut compressed = vec![0u8; compress_bound(source.len())];
//! let produced = compress2(&mut compressed, source, Z_BEST_COMPRESSION).unwrap();
//! compressed.truncate(produced);
//!
//! // Decompress into a buffer sized to the known original length.
//! let mut restored = vec![0u8; source.len()];
//! let written = uncompress(&mut restored, &compressed).unwrap();
//! restored.truncate(written);
//!
//! assert_eq!(&restored[..], &source[..]);
//! ```

// The core engines are `no_std` + `alloc`; the standard library is linked only
// under the `std` feature (the default). Because the FFI layer and several
// engines allocate (`Box`/`Vec`), `alloc` is imported unconditionally below.
#![cfg_attr(not(feature = "std"), no_std)]
// The variadic gz shims (`gzprintf`/`gzvprintf`) rely on the unstable
// `c_variadic` language feature; enabling the `c-variadic` Cargo feature
// therefore also requires a nightly toolchain. On stable (the default) the
// `c-variadic` feature is off and this attribute is inert.
#![cfg_attr(feature = "c-variadic", feature(c_variadic))]
// Every public item in the crate must be documented (AAP: "document all public
// items"). This is a `warn`, never a `deny`, so it can never break the build.
#![warn(missing_docs)]

// `Box`, `Vec`, and `String` are provided by `alloc` in both `std` and `no_std`
// builds. Importing the crate here makes those types available crate-wide
// without pulling in the whole standard library.
extern crate alloc;

// ===========================================================================
// Version macros (ported from `zlib.h` L44-L49)
//
// These mirror the C `ZLIB_VERSION`/`ZLIB_VERNUM` family exactly and are the
// crate-wide single source of truth. `util::version::zlib_version()` (and the
// FFI `zlibVersion` shim) return `ZLIB_VERSION`, so this string must stay
// byte-identical to the C `ZLIB_VERSION` macro for compatibility.
// ===========================================================================

/// zlib version string, mirroring the C `ZLIB_VERSION` macro.
///
/// This is the authoritative version reported by [`util::version::zlib_version`]
/// (and its FFI shim `zlibVersion`); it must remain byte-identical to the C
/// `ZLIB_VERSION` macro for drop-in compatibility.
pub const ZLIB_VERSION: &str = "1.3.2.1-motley";

/// zlib version number, mirroring the C `ZLIB_VERNUM` macro (`0x1321`).
///
/// The nibbles encode the version as `0xMNRS` (major, minor, revision,
/// sub-revision): `0x1321` is `1.3.2` sub-revision `1`.
pub const ZLIB_VERNUM: u32 = 0x1321;

/// zlib major version, mirroring the C `ZLIB_VER_MAJOR` macro.
pub const ZLIB_VER_MAJOR: u32 = 1;

/// zlib minor version, mirroring the C `ZLIB_VER_MINOR` macro.
pub const ZLIB_VER_MINOR: u32 = 3;

/// zlib revision, mirroring the C `ZLIB_VER_REVISION` macro.
pub const ZLIB_VER_REVISION: u32 = 2;

/// zlib sub-revision, mirroring the C `ZLIB_VER_SUBREVISION` macro.
pub const ZLIB_VER_SUBREVISION: u32 = 1;

// ===========================================================================
// Module declarations — the six C layers + public-API-types + FFI boundary
//
// `lib.rs` only DECLARES these modules; each is implemented in its own file.
// The graph mirrors the C `#include` layering exactly (AAP §0.3.1) with no
// extra top-level modules.
// ===========================================================================

pub mod checksum;
pub mod constants;
pub mod deflate;
pub mod error;
pub mod gz_header;
pub mod inflate;
pub mod stream;
pub mod util;

// The gzip file-I/O layer fundamentally requires the standard library
// (`std::fs`/`std::io`), so it is compiled only when the `gz-io` feature is
// enabled (which implies `std` + `gzip`). This matches the feature gating in
// `src/gz/**` and `src/ffi/gz.rs`; a bare `no_std` build omits it entirely.
#[cfg(feature = "gz-io")]
pub mod gz;

// The FFI drop-in boundary is declared UNCONDITIONALLY so the emitted
// `cdylib`/`staticlib` always presents the full zlib C symbol table for
// linkage. Any part of `ffi` that needs `std` is gated internally; the module
// declaration itself is never feature-gated.
pub mod ffi;

// ===========================================================================
// Idiomatic public API — curated crate-root re-exports
//
// These mirror the surface `zlib.h` exposes so that `use zlib_rs::{…}` is
// ergonomic. The FFI symbols are intentionally NOT re-exported here: the `ffi`
// module stands alone as the C ABI surface.
// ===========================================================================

// The error model: the exhaustive return-code enum and its error twin.
pub use error::{ReturnCode, ZlibError};

// Public-API enums plus the four compression-level constants callers most often
// reference when configuring the engine.
pub use constants::{
    DataType, FlushMode, Method, Strategy, WrapMode, Z_BEST_COMPRESSION, Z_BEST_SPEED,
    Z_DEFAULT_COMPRESSION, Z_NO_COMPRESSION,
};

// The idiomatic stream and its allocator abstraction (mirrors `z_stream`).
pub use stream::{Allocator, DefaultAllocator, ZStream};

// The idiomatic gzip header (mirrors `gz_header`).
pub use gz_header::GzHeader;

// One-call, whole-buffer wrappers (ported from `compress.c`/`uncompr.c`). Both
// the idiomatic snake_case names and the zlib-style `compressBound` alias are
// surfaced.
pub use util::{compress, compress_bound, compress2, compressBound, uncompress, uncompress2};

// Version, compile-flag, and error-string reporting (ported from `zutil.c`),
// each paired with its zlib-style camelCase alias.
pub use util::{z_error, zError, zlib_compile_flags, zlib_version, zlibCompileFlags, zlibVersion};

// Checksums (ported from `adler32.c`/`crc32.c`): the running checksum functions
// and their stream-combining companions.
pub use checksum::{adler32, adler32_combine, crc32, crc32_combine};

// ===========================================================================
// Prelude — grouped re-exports for `use zlib_rs::prelude::*;`
// ===========================================================================

/// Common imports for working with `zlib-rs`.
///
/// Bring the most frequently used types, traits, and enums into scope with a
/// single glob import:
///
/// ```
/// use zlib_rs::prelude::*;
///
/// // The re-exported types are now in scope.
/// let _stream: ZStream = ZStream::new();
/// let _header: GzHeader = GzHeader::new();
/// ```
///
/// The prelude intentionally excludes the free functions (`compress`,
/// `adler32`, …) and the C-ABI [`crate::ffi`] surface to avoid polluting the
/// caller's namespace; reach those through their crate paths
/// (`zlib_rs::compress`, `zlib_rs::ffi::…`).
pub mod prelude {
    pub use crate::constants::{DataType, FlushMode, Method, Strategy, WrapMode};
    pub use crate::error::{ReturnCode, ZlibError};
    pub use crate::gz_header::GzHeader;
    pub use crate::stream::{Allocator, DefaultAllocator, ZStream};
}

// ===========================================================================
// Sanity tests — version constants and re-export resolution
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// The version constants must equal the values from `zlib.h` L44-L49.
    #[test]
    fn version_constants_match_zlib_h() {
        assert_eq!(ZLIB_VERSION, "1.3.2.1-motley");
        assert_eq!(ZLIB_VERNUM, 0x1321);
        assert_eq!(ZLIB_VER_MAJOR, 1);
        assert_eq!(ZLIB_VER_MINOR, 3);
        assert_eq!(ZLIB_VER_REVISION, 2);
        assert_eq!(ZLIB_VER_SUBREVISION, 1);
    }

    /// The version reporting functions re-exported at the crate root must agree
    /// with the authoritative [`ZLIB_VERSION`] constant.
    #[test]
    fn version_functions_agree_with_constant() {
        assert_eq!(zlib_version(), ZLIB_VERSION);
        assert_eq!(zlibVersion(), ZLIB_VERSION);
    }

    /// The value-namespace re-exports (functions and constants) must resolve
    /// through the crate root and behave as their zlib counterparts do on
    /// trivial, algorithm-guaranteed inputs.
    #[test]
    fn value_reexports_resolve_and_work() {
        // Compression-level constants carry their canonical zlib values.
        assert_eq!(Z_NO_COMPRESSION, 0);
        assert_eq!(Z_BEST_SPEED, 1);
        assert_eq!(Z_BEST_COMPRESSION, 9);
        assert_eq!(Z_DEFAULT_COMPRESSION, -1);

        // The `compress_bound` alias and its snake_case form are the same fn.
        assert_eq!(compress_bound(0), compressBound(0));

        // Adler-32 seeded with 1 and CRC-32 seeded with 0 are unchanged by an
        // empty input — a property that holds for every conforming impl.
        assert_eq!(adler32(1, b""), 1);
        assert_eq!(crc32(0, b""), 0);

        // The combine companions resolve and are identity over a zero-length
        // second stream.
        assert_eq!(
            adler32_combine(adler32(1, b"abc"), 1, 0),
            adler32(1, b"abc")
        );
        assert_eq!(crc32_combine(crc32(0, b"abc"), 0, 0), crc32(0, b"abc"));

        // The compile-flags/error-string reporters resolve through the root.
        let _ = zlib_compile_flags();
        let _ = zlibCompileFlags();
        assert_eq!(z_error(0), zError(0));
    }

    /// The type-namespace re-exports (enums, structs, and the [`Allocator`]
    /// trait) must resolve through both the crate root and the [`prelude`].
    #[test]
    fn type_reexports_resolve() {
        // A no-bound generic proves each path names a real type.
        fn assert_type<T>() {}
        assert_type::<ReturnCode>();
        assert_type::<ZlibError>();
        assert_type::<FlushMode>();
        assert_type::<Strategy>();
        assert_type::<WrapMode>();
        assert_type::<DataType>();
        assert_type::<Method>();
        assert_type::<DefaultAllocator>();
        assert_type::<ZStream>();
        assert_type::<GzHeader>();

        // The same items must also resolve through the prelude.
        assert_type::<prelude::ReturnCode>();
        assert_type::<prelude::ZStream>();
        assert_type::<prelude::GzHeader>();

        // The `Allocator` trait is re-exported and usable as a bound, and the
        // default allocator satisfies it.
        fn needs_allocator<A: Allocator>() {}
        needs_allocator::<DefaultAllocator>();
    }
}
