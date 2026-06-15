//! Gzip close dispatcher, ported from zlib `gzclose.c`.
//!
//! This module is the safe-Rust port of the **entire** C translation unit
//! `gzclose.c` (23 LOC). In zlib, `gzclose()` deliberately lives in its own
//! object file so that a program linking it pulls in *only* the close path it
//! actually uses; the read-side and write-side teardown bodies stay in
//! `gzread.c` / `gzwrite.c`. This port keeps that exact split: the heavy
//! teardown lives in [`crate::gz::read::gzclose_r`] and
//! [`crate::gz::write::gzclose_w`], and this file contributes *only* the thin
//! top-level dispatcher.
//!
//! [`gzclose`] inspects the handle's access [`Mode`] and routes to:
//!
//! * [`gzclose_r`](crate::gz::read::gzclose_r) when the handle was opened for
//!   reading ([`Mode::Read`]), or
//! * [`gzclose_w`](crate::gz::write::gzclose_w) for every other mode (the C
//!   ternary's "else" arm — a write handle, or the transient/degenerate
//!   [`Mode::None`]/[`Mode::Append`] states, which `gzclose_w` itself rejects
//!   with [`Z_STREAM_ERROR`]).
//!
//! # Ownership and teardown
//!
//! The C `gzclose(gzFile file)` *consumes* the opaque handle: after it returns,
//! the `gz_statep` has been freed and must not be touched again. The idiomatic
//! safe equivalent takes the handle **by value** as an
//! [`Option<Box<GzState>>`] (the `Box<GzState>` is this crate's `gzFile`; `None`
//! is the C `NULL`). Because the value is moved in and never handed back, the
//! borrow checker statically guarantees the caller cannot use it after the
//! close — the safe-Rust replacement for the C "do not touch a freed handle"
//! rule.
//!
//! Bulk resource release (closing the file, freeing the engine state and the
//! working buffers) is the job of [`Drop for GzState`](crate::gz::state::GzState)
//! and runs automatically when the owned `Box<GzState>` falls out of scope at
//! the end of [`gzclose`]. The fallible *flush* on the write path (emitting the
//! trailing deflate data + gzip trailer) is performed by `gzclose_w`, which
//! returns a status the caller can observe. Both `gzclose_r` and `gzclose_w`
//! call [`GzState::finalize`](crate::gz::state::GzState::finalize) once their
//! work succeeds, so the subsequent `Drop` is a no-op under the at-most-once
//! finalize guard — teardown therefore runs **exactly once**, whether the
//! caller closes explicitly through this function or simply drops the handle.
//!
//! # FFI relationship
//!
//! The C-ABI shim `extern "C" gzclose(file: *mut c_void)` (in `crate::ffi`)
//! reconstitutes the `Box<GzState>` from the raw pointer via `Box::from_raw`
//! and forwards to this function; the `unsafe` pointer marshalling lives there,
//! not here.
//!
//! # Safety
//!
//! This file contains **no `unsafe`** (AAP §0.6.2 — `unsafe` is confined to
//! `crate::ffi` and `crate::inflate::fast`).

use crate::constants::Z_STREAM_ERROR;
use crate::gz::read::gzclose_r;
use crate::gz::state::{GzState, Mode};
use crate::gz::write::gzclose_w;

/// Closes a gzip file handle, dispatching to the read- or write-side close by
/// access mode — the safe-Rust port of zlib's `gzclose()` (`gzclose.c`).
///
/// This is a faithful 1:1 translation of the C dispatcher:
///
/// ```c
/// int ZEXPORT gzclose(gzFile file) {
///     gz_statep state;
///     if (file == NULL)
///         return Z_STREAM_ERROR;
///     state = (gz_statep)file;
///     return state->mode == GZ_READ ? gzclose_r(file) : gzclose_w(file);
/// }
/// ```
///
/// # Parameters
///
/// * `file` — the owned handle to close. `Some(handle)` is the C non-null
///   `gzFile`; `None` is the C `NULL`. The handle is **consumed**: ownership
///   moves into this function and the value is dropped before it returns, so
///   the caller cannot reuse it afterwards (the compile-time analogue of C's
///   "the handle is freed by `gzclose`").
///
/// # Returns
///
/// A zlib status code (`i32`), propagated verbatim from the delegated close:
///
/// * [`Z_STREAM_ERROR`] if `file` is `None`, or if the handle's mode is not a
///   valid open mode for the path taken (e.g. a non-write handle reaching
///   `gzclose_w`).
/// * Otherwise the result of [`gzclose_r`](crate::gz::read::gzclose_r) (read
///   handles) or [`gzclose_w`](crate::gz::write::gzclose_w) (all other modes):
///   typically [`Z_OK`](crate::constants::Z_OK), or a buffered error such as
///   `Z_BUF_ERROR` (premature EOF on read) or an I/O error code surfaced while
///   flushing the final write.
///
/// # Examples
///
/// Closing a null handle is a safe, defined error:
///
/// ```
/// # #[cfg(feature = "gz-io")] {
/// use zlib_rs::gz::close::gzclose;
/// use zlib_rs::Z_STREAM_ERROR;
///
/// assert_eq!(gzclose(None), Z_STREAM_ERROR);
/// # }
/// ```
pub fn gzclose(file: Option<Box<GzState>>) -> i32 {
    // C: `if (file == NULL) return Z_STREAM_ERROR;`
    //
    // The `Box<GzState>` is this crate's safe `gzFile`; `None` is the C NULL
    // pointer. `let ... else` mirrors the early C return precisely.
    let Some(mut state) = file else {
        return Z_STREAM_ERROR;
    };

    // C: `return state->mode == GZ_READ ? gzclose_r(file) : gzclose_w(file);`
    //
    // Dispatch strictly on `mode == Read`. Every other mode — a write handle,
    // or the transient/degenerate `None`/`Append` states — takes the
    // write-close path, exactly as the C ternary's "else" arm does;
    // `gzclose_w` itself rejects a non-write handle with `Z_STREAM_ERROR`,
    // matching C behavior for a corrupt or mismatched handle.
    //
    // The value of this `if`/`else` is the function's return value. Right after
    // it is computed, the owned `Box<GzState>` (`state`) goes out of scope and
    // is dropped — the deterministic, leak-free replacement for C's `free`. The
    // delegated `gzclose_r`/`gzclose_w` already called `GzState::finalize`, so
    // that `Drop` is a no-op (at-most-once guard): teardown runs exactly once
    // and a double free is unrepresentable.
    if state.mode == Mode::Read {
        gzclose_r(&mut state)
    } else {
        gzclose_w(&mut state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::Z_OK;
    use crate::gz::write::gzwrite;
    use std::fs::File;
    use std::io::{Read, Write};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Generate a unique temp path (no external `tempfile` crate needed),
    /// mirroring the helper used by the `gzwrite`/`gzread` test suites.
    fn temp_path(tag: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let mut dir = std::env::temp_dir();
        dir.push(format!("zlibrs_gzclose_test_{tag}_{pid}_{nanos}_{n}.gz"));
        dir
    }

    /// RAII temp-file holder: owns a unique path and removes the backing file on
    /// drop (even if a test assertion panics). The `GzState` handles it produces
    /// are independent owned values, so they can be moved into `gzclose`.
    struct TempGz {
        path: PathBuf,
    }

    impl TempGz {
        fn new(tag: &str) -> TempGz {
            TempGz {
                path: temp_path(tag),
            }
        }

        /// Create the backing file and a write-mode [`GzState`] over it.
        fn open_write(&self) -> GzState {
            let file = File::create(&self.path).expect("create temp file");
            GzState::new(file, self.path.to_string_lossy().into_owned(), Mode::Write)
        }

        /// Write a real single-member gzip stream to the path with the canonical
        /// C-zlib oracle (`flate2`), then open it read-only as a [`GzState`].
        fn open_read_with(&self, payload: &[u8]) -> GzState {
            {
                let f = File::create(&self.path).expect("create temp file");
                let mut enc = flate2::write::GzEncoder::new(f, flate2::Compression::default());
                enc.write_all(payload).expect("encode payload");
                enc.finish().expect("finish gzip stream");
            }
            let file = File::open(&self.path).expect("open gzip file for reading");
            GzState::new(file, self.path.to_string_lossy().into_owned(), Mode::Read)
        }

        /// Create the backing file and a [`GzState`] with an arbitrary `mode`
        /// (used to exercise the dispatch's "else" arm with a degenerate mode).
        fn open_with_mode(&self, mode: Mode) -> GzState {
            let file = File::create(&self.path).expect("create temp file");
            GzState::new(file, self.path.to_string_lossy().into_owned(), mode)
        }

        /// Read the raw on-disk bytes (the produced gzip stream).
        fn read_raw(&self) -> Vec<u8> {
            std::fs::read(&self.path).expect("read temp file")
        }
    }

    impl Drop for TempGz {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    /// Decompress a single-member gzip stream with the canonical C-zlib oracle.
    fn gunzip(data: &[u8]) -> Vec<u8> {
        let mut decoder = flate2::read::GzDecoder::new(data);
        let mut out = Vec::new();
        decoder
            .read_to_end(&mut out)
            .expect("flate2 (C zlib) failed to decode the produced gzip stream");
        out
    }

    /// C parity: `if (file == NULL) return Z_STREAM_ERROR;`.
    #[test]
    fn gzclose_none_returns_stream_error() {
        assert_eq!(gzclose(None), Z_STREAM_ERROR);
    }

    /// Write-side dispatch: `gzclose` on a write handle routes to `gzclose_w`,
    /// returns `Z_OK`, finalizes the stream, and leaves a valid gzip file on
    /// disk that the C-zlib oracle decodes back to the exact input.
    #[test]
    fn gzclose_write_dispatches_and_produces_valid_gzip() {
        let g = TempGz::new("write");
        let mut state = g.open_write();
        let payload = b"the quick brown fox jumps over the lazy dog";
        assert_eq!(gzwrite(&mut state, payload), payload.len() as i32);

        // Dispatch through the consuming `gzclose` (not `gzclose_w` directly).
        assert_eq!(gzclose(Some(Box::new(state))), Z_OK);

        // The produced file must be a valid gzip stream per the C-zlib oracle.
        let raw = g.read_raw();
        assert!(
            raw.len() >= 3 && raw[..3] == [0x1f, 0x8b, 0x08],
            "missing gzip magic"
        );
        assert_eq!(gunzip(&raw), payload);
    }

    /// Read-side dispatch: `gzclose` on a read handle routes to `gzclose_r`. A
    /// freshly opened read handle (nothing decompressed yet) closes cleanly with
    /// `Z_OK`.
    #[test]
    fn gzclose_read_dispatches_ok() {
        let g = TempGz::new("read");
        let payload = b"read-side dispatch payload";
        let state = g.open_read_with(payload);

        assert_eq!(gzclose(Some(Box::new(state))), Z_OK);
    }

    /// Dispatch ternary "else" arm: a handle whose mode is neither `Read` nor
    /// `Write` (here the degenerate `Mode::None`) routes to `gzclose_w`, which
    /// rejects a non-write handle with `Z_STREAM_ERROR`. This pins the C
    /// `state->mode == GZ_READ ? gzclose_r : gzclose_w` decision.
    #[test]
    fn gzclose_non_read_mode_routes_to_write_close() {
        let g = TempGz::new("nonemode");
        let state = g.open_with_mode(Mode::None);

        assert_eq!(gzclose(Some(Box::new(state))), Z_STREAM_ERROR);
    }

    /// RAII / no double-finalize: after an explicit `gzclose`, the owned
    /// `Box<GzState>` is dropped inside the function; its `Drop` re-invokes
    /// `finalize()`, which is a no-op under the at-most-once guard. Reaching the
    /// assertion without panicking (and with the expected status) proves
    /// teardown ran exactly once and the handle was consumed.
    #[test]
    fn gzclose_consumes_handle_no_double_finalize_panic() {
        let g = TempGz::new("raii");
        let mut state = g.open_write();
        let _ = gzwrite(&mut state, b"x");

        assert_eq!(gzclose(Some(Box::new(state))), Z_OK);
        // `state` is no longer accessible here — it was moved into `gzclose`.
    }
}
