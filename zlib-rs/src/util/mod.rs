//! Library utilities (`compress.c` / `uncompr.c` / `zutil.c`).
//!
//! This module root groups the safe-Rust ports of zlib's small "utility"
//! translation units:
//!
//! * `compress` (`compress.c`) — the one-shot in-memory buffer compressors
//!   [`compress2`] and [`compress`](compress::compress), plus the
//!   [`compress_bound`] output-sizing helper.
//! * [`uncompress`] — the one-shot `uncompress()` / `uncompress2()` buffer
//!   decompression wrappers (`uncompr.c`); see [`uncompress2`].
//! * [`version`] (`zutil.c`) — `zlibVersion()`, `zlibCompileFlags()`, and
//!   `zError()`.

pub mod compress;
pub mod uncompress;
pub mod version;

// Re-export the one-shot buffer helpers at the `util` level for ergonomic
// access (`crate::util::compress2()` etc.). The `compress` module and the
// re-exported `compress` function share a name but occupy distinct namespaces
// (type vs. value), so they coexist without conflict — exactly like a macro and
// a function of the same name.
pub use compress::{compress, compress_bound, compress2};

// Re-export the diagnostic helpers at the `util` level for ergonomic access
// (`crate::util::zlib_version()` etc.).
pub use version::{z_error, zlib_compile_flags, zlib_compile_flags_for, zlib_version};

// Re-export the one-shot decompression helpers so callers (and the FFI shim)
// can reach them as `crate::util::uncompress(..)` / `crate::util::uncompress2(..)`,
// mirroring the flat C `uncompr.c` API. The free function `uncompress` and the
// `uncompress` module share a name but live in different namespaces (value vs.
// module), so both remain reachable.
pub use uncompress::{uncompress, uncompress2};
