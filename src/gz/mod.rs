//! gzip (`.gz`) stdio-like file I/O layer — the `gz*` API of the `zlib-rs`
//! crate.
//!
//! This module is the root of the `src/gz/` tree and the idiomatic, memory-safe
//! Rust port of the C gzip file-I/O sources — `gzlib.c`, `gzread.c`,
//! `gzwrite.c`, `gzclose.c`, and their shared internal header `gzguts.h`. It is
//! layered directly over [`std::fs::File`] + [`std::io`] and is driven by the
//! crate's own deflate/inflate engines ([`crate::deflate`] / [`crate::inflate`])
//! rather than by any C code, so the shipped artifact carries zero C dependency.
//!
//! # Feature gating
//!
//! The gz layer is the one part of the crate that fundamentally requires the
//! standard library (for filesystem access), so the entire module is compiled
//! only when the `gz-io` Cargo feature is enabled — `src/lib.rs` wires it in
//! with `#[cfg(feature = "gz-io")] pub mod gz;`. Because `gz-io` implies `std`
//! (and `gzip`), items here may freely use [`std`] without an additional
//! `#[cfg(feature = "std")]` guard; finer-grained gating is applied inside the
//! submodules only where genuinely needed.
//!
//! # Structure: private submodules + curated re-exports
//!
//! The concrete behavior lives in five sibling submodules, each a port of one C
//! translation unit (plus the shared state ported from `gzguts.h`):
//!
//! | Submodule | C source    | Responsibility                                   |
//! |-----------|-------------|--------------------------------------------------|
//! | `state`   | `gzguts.h`  | [`GzState`] / [`GzMode`] / `How` — the open file |
//! | `open`    | `gzlib.c`   | open, configure, position, and error inspection  |
//! | `read`    | `gzread.c`  | decompressing reads + [`std::io::Read`] adapter  |
//! | `write`   | `gzwrite.c` | compressing writes + [`std::io::Write`] adapter  |
//! | `close`   | `gzclose.c` | flush-and-close teardown                         |
//!
//! Unlike the sibling engine layers (which expose their submodules with
//! `pub mod`), these submodules are declared **private** and the public surface
//! is assembled here through explicit re-exports. This yields a single, curated
//! `zlib_rs::gz::<fn>` façade for downstream Rust users and one stable point for
//! the FFI shim layer (`src/ffi/gz.rs`) to depend on via `crate::gz::*`. The C
//! idiom `#include "gzguts.h"` therefore becomes `use crate::gz::state::GzState;`
//! internally, while external callers see only the re-exported API. Sibling
//! submodules still reference one another by absolute path (for example
//! `crate::gz::read::gz_look`), which resolves because each submodule is a
//! descendant of this (public) `gz` module even though the submodules are
//! private.
//!
//! # Idiomatic Rust API vs. C-compatible functions
//!
//! Two complementary surfaces are provided:
//!
//! * A **first-class Rust API**: [`GzState`] implements [`std::io::Read`],
//!   [`std::io::BufRead`], and [`std::io::Write`], so an open file behaves like
//!   any other reader or writer, and is opened with [`gzopen`] / [`gzdopen`].
//!   Its [`Drop`] impl only **releases resources** — it frees the I/O buffers,
//!   ends the deflate/inflate stream, and closes the file descriptor — but it
//!   deliberately does **not** finalize gzip output: it emits no `Z_FINISH`
//!   flush and surfaces no deferred write error. Dropping a *writer* therefore
//!   leaves a truncated, invalid member, so a write handle must be closed
//!   explicitly with [`gzclose`] / [`gzclose_w`] to produce a complete, valid
//!   gzip stream and to observe any pending write error.
//! * A set of **C-compatible entry points** (`gzread`, `gzwrite`, `gzgetc`, …)
//!   that mirror the exact zlib prototypes. These are re-exported publicly (and
//!   are also what the FFI boundary consumes via `crate::gz::*`); for everyday
//!   Rust code the trait-based API above is usually more convenient.
//!
//! # Safety
//!
//! This layer contains **zero `unsafe`**. The moving C output pointer `x.next`
//! is modelled as a bounds-checked [`usize`] index, owned [`std::fs::File`] and
//! `Vec<u8>` buffers replace the raw `fd`/`in`/`out` members, and [`Drop`]
//! subsumes the manual `inflateEnd`/`deflateEnd`/`close` teardown. All
//! raw-pointer, raw-fd, and C-string handling for the FFI `gz*` shims lives in
//! `src/ffi/gz.rs`, never here. Targets the Rust 2024 edition, MSRV 1.85.0.

// ---------------------------------------------------------------------------
// Shared constant (ported from `gzguts.h`).
// ---------------------------------------------------------------------------

/// Default gz I/O buffer size — the direct port of the C `#define GZBUFSIZE
/// 8192` from `gzguts.h`.
///
/// This is the default value of a file's requested buffer size (`want`) and is
/// **doubled** for the working buffers: the output buffer when reading and the
/// input buffer when writing. As the C header notes, "this and twice this must
/// be able to fit in an unsigned type" — a constraint trivially satisfied by
/// [`usize`].
///
/// It is defined here at the module root (rather than in a submodule) because it
/// is shared across the `open`, `read`, and `write` families: `open.rs` seeds a
/// new file's `want` from `crate::gz::GZBUFSIZE`, and the read/write drivers
/// size their working buffers from that `want`.
pub(crate) const GZBUFSIZE: usize = 8192;

// ---------------------------------------------------------------------------
// Submodule declarations.
//
// These are intentionally PRIVATE: the public API is assembled from the
// re-exports below, giving a single curated `zlib_rs::gz::*` façade and one
// dependency point for `src/ffi/gz.rs`. `state` is the foundational module —
// every other submodule operates on the `GzState` it defines — though the
// declaration order is cosmetic to the compiler (and normalized alphabetically
// by `rustfmt`, matching the sibling engine modules).
// ---------------------------------------------------------------------------
mod close;
mod open;
mod read;
mod state;
mod write;

// ---------------------------------------------------------------------------
// Public re-exports — the gz* API and the FFI-visible state types.
// ---------------------------------------------------------------------------

/// Open-file state ([`GzState`]) and its open-mode enum ([`GzMode`]).
///
/// `GzState` carries the [`std::io::Read`] / [`std::io::BufRead`] /
/// [`std::io::Write`] / [`Drop`] impls, so re-exporting the type alone gives
/// downstream users the full idiomatic API. `src/ffi/gz.rs` uses these to build
/// the opaque `gzFile` handle and its `#[repr(C)] gzFile_s` mirror.
pub use state::{GzMode, GzState};

// The read look-ahead mode (`LOOK`/`COPY`/`GZIP` in C) is an internal detail
// surfaced only at crate visibility so the layer (and the FFI shim) can name it;
// it is not part of the public API. The `pub(crate)` re-export matches the
// type's own `pub(crate)` visibility (a `pub use` of it would fail to compile).
//
// `#[allow(unused_imports)]`: this re-export exists so the forthcoming
// `src/ffi/gz.rs` can reach the look-ahead mode via `crate::gz::How` — the
// `state` submodule is private, so the FFI shim layer has no other path to it.
// Until that shim lands, the library-only target has no in-crate consumer of the
// re-exported name (the module's own `#[cfg(test)]` block references it, but
// that is a separate compilation target), so the unused-import lint would
// otherwise fire here. The re-export is mandated by the layer contract.
#[allow(unused_imports)]
pub(crate) use state::How;

/// Crate-internal mode-string validator ([`open::validate_mode`]) used by the
/// raw-descriptor FFI shim (`src/ffi/gz.rs`) to validate a `gzdopen` mode string
/// *before* it adopts the caller's file descriptor. It is intentionally
/// `pub(crate)` — not part of the public `zlib_rs::gz` API — because it exists
/// solely to preserve zlib's "`gzdopen` does not close fd if it fails" contract
/// at the FFI boundary, mirroring the `pub(crate)` visibility of [`How`] above.
pub(crate) use open::validate_mode;

/// Open / configure / position / error-inspection family, ported from
/// `gzlib.c`. Every function is part of the public zlib API and is re-exported
/// publicly so callers use `zlib_rs::gz::gzopen(...)` and friends.
pub use open::{
    gzbuffer, gzclearerr, gzdirect, gzdopen, gzeof, gzerror, gzoffset, gzoffset64, gzopen,
    gzopen64, gzrewind, gzseek, gzseek64, gzsetparams, gztell, gztell64,
};

/// Read family, ported from `gzread.c` — the C-compatible reading entry points.
/// Re-exported publicly so callers may use `zlib_rs::gz::gzread(...)`, and the
/// FFI shim layer can reach them via `crate::gz::*`. The idiomatic alternative
/// is the [`std::io::Read`] / [`std::io::BufRead`] impls on [`GzState`].
pub use read::{gzfread, gzgetc, gzgetc_, gzgets, gzread, gzungetc};

/// Write family, ported from `gzwrite.c` — the C-compatible writing entry
/// points. Re-exported publicly (and reachable by the FFI shim layer via
/// `crate::gz::*`); the idiomatic alternative is the [`std::io::Write`] impl on
/// [`GzState`]. Both the formatting entry point `gzprintf` and its idiomatic
/// [`core::fmt::Arguments`]-based `gzvprintf` form are surfaced.
pub use write::{gzflush, gzfwrite, gzprintf, gzputc, gzputs, gzvprintf, gzwrite};

/// Close family, ported from `gzclose.c`. Explicit teardown mirroring
/// `gzclose` / `gzclose_r` / `gzclose_w`. Only [`gzclose_w`] finalizes a write
/// stream — it emits the `Z_FINISH` flush and the gzip trailer and surfaces any
/// pending write error. Simply dropping a [`GzState`] releases its buffers,
/// stream, and file descriptor but does **not** finalize gzip output, so a
/// writer must be closed explicitly to produce a valid stream.
pub use close::{gzclose, gzclose_r, gzclose_w};

#[cfg(test)]
mod tests {
    //! Module-root smoke tests: verify the load-bearing shared constant and that
    //! the curated re-export façade resolves to the underlying submodule items.

    /// `GZBUFSIZE` must match the C `#define GZBUFSIZE 8192` byte-for-byte: the
    /// read/write buffer sizing (and hence the streaming/buffering behavior)
    /// depends on it.
    #[test]
    fn gzbufsize_matches_c_define() {
        assert_eq!(super::GZBUFSIZE, 8192);
    }

    /// Compile-time proof that the curated re-export façade resolves: importing
    /// every re-exported name fails to compile if any re-export path is wrong.
    /// The imports are intentionally unused at runtime, so the `unused_imports`
    /// lint is locally allowed.
    #[test]
    fn reexports_resolve() {
        #[allow(unused_imports)]
        use super::{
            GzMode, GzState, How, gzbuffer, gzclearerr, gzclose, gzclose_r, gzclose_w, gzdirect,
            gzdopen, gzeof, gzerror, gzflush, gzfread, gzfwrite, gzgetc, gzgetc_, gzgets, gzoffset,
            gzoffset64, gzopen, gzopen64, gzprintf, gzputc, gzputs, gzread, gzrewind, gzseek,
            gzseek64, gzsetparams, gztell, gztell64, gzungetc, gzvprintf, gzwrite,
        };

        // Also exercise the value-carrying items so the load-bearing `GzMode`
        // and `How` discriminants (mirrored verbatim from `gzguts.h`) are
        // asserted, not merely named.
        assert_eq!(GzMode::None as i32, 0);
        assert_eq!(GzMode::Append as i32, 1);
        assert_eq!(GzMode::Read as i32, 7247);
        assert_eq!(GzMode::Write as i32, 31153);
        assert_eq!(How::Look as u8, 0);
        assert_eq!(How::Copy as u8, 1);
        assert_eq!(How::Gzip as u8, 2);
    }
}
