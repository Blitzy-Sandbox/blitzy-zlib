//! Library utilities (`compress.c` / `uncompr.c` / `zutil.c`).
//!
//! At the current foundation milestone this module root exposes the diagnostic
//! helpers ported from `zutil.c`:
//!
//! * [`version`] — `zlibVersion()`, `zlibCompileFlags()`, and `zError()`.
//!
//! The one-shot [`compress`](version)/`uncompress` convenience wrappers
//! (`compress.c` / `uncompr.c`) layer on top of the streaming engines in a
//! subsequent milestone.

pub mod version;

// Re-export the diagnostic helpers at the `util` level for ergonomic access
// (`crate::util::zlib_version()` etc.).
pub use version::{z_error, zlib_compile_flags, zlib_compile_flags_for, zlib_version};
