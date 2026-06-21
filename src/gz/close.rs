//! gzip close dispatcher — the safe-Rust port of zlib's `gzclose.c`.
//!
//! In upstream C, `gzclose.c` is deliberately a *separate* translation unit
//! holding nothing but the top-level [`gzclose`] entry point. The upstream
//! comment explains why: keeping it apart lets the linker pull in only the
//! close path that is actually used, so a program that only ever *reads* need
//! not drag in the write-side teardown (and vice versa). This module preserves
//! that split faithfully — it contains **only** the dispatcher and delegates
//! the real teardown to its siblings:
//!
//! * `gzclose_r` — the read-side close, in
//!   `read.rs` (frees the inflate engine and the read buffers);
//! * [`gzclose_w`] — the write-side close, in
//!   `write.rs` (flushes, finishes the gzip stream, frees the deflate engine
//!   and the write buffers).
//!
//! The bodies of those routines are **not** duplicated here, mirroring the C
//! module boundary one-to-one (AAP §0.3.1).
//!
//! # Behaviour
//!
//! [`gzclose`] inspects the handle's `mode`
//! and routes to the matching close routine, exactly mirroring the C ternary
//! `state->mode == GZ_READ ? gzclose_r(file) : gzclose_w(file)`. A `NULL`
//! handle — here spelled [`None`] — yields [`Z_STREAM_ERROR`], as in C.
//!
//! # Memory-ownership model (AAP §0.6.3)
//!
//! Where the C `gzclose` takes a raw `gzFile` and ultimately `free`s the
//! handle, the idiomatic Rust dispatcher *consumes* an owned `Box<GzState>`
//! (the Rust spelling of `gzFile`). The owned [`File`](std::fs::File), the I/O
//! buffers, and the embedded compression engine are released deterministically
//! when the box is dropped at the end of the call — the RAII replacement for
//! the manual teardown. Because `gzclose_r` /
//! [`gzclose_w`] each call
//! `mark_closed` before returning, the
//! box's [`Drop`] impl observes an already-finalized handle and does nothing —
//! the **double-finalize guard** that guarantees exactly-once teardown whether
//! the caller closes explicitly or simply drops the handle.
//!
//! # Unsafe
//!
//! This file contains **no `unsafe`**. The C-ABI shim that reconstitutes a
//! `Box<GzState>` from the raw `*mut c_void` handle (via `Box::from_raw`) lives
//! in `src/ffi.rs`; by the time control reaches this dispatcher the handle is
//! already a safe, owned `Box`.

use crate::constants::Z_STREAM_ERROR;
use crate::gz::read::gzclose_r;
use crate::gz::state::{GzState, Mode};
use crate::gz::write::gzclose_w;

/// Close a gzip file, routing to the read- or write-side teardown by mode.
///
/// This is the safe-Rust port of zlib's `gzclose()` (`gzclose.c` L11-23) — the
/// single top-level close entry point that dispatches on the handle's
/// `mode`:
///
/// * [`Mode::Read`] → `gzclose_r` (tears down the
///   inflate engine and frees the read buffers);
/// * anything else ([`Mode::Write`] / [`Mode::Append`] / [`Mode::None`]) →
///   [`gzclose_w`] (flushes and finishes the gzip
///   stream, then frees the deflate engine and write buffers).
///
/// This binary split mirrors the C ternary exactly:
/// `state->mode == GZ_READ ? gzclose_r(file) : gzclose_w(file)`.
///
/// # Ownership
///
/// The handle is passed **by value** as an `Option<Box<GzState>>` (a `gzFile`
/// is a `Box<GzState>`), so this function takes ownership and the box is
/// released before it returns — the RAII analogue of the C `free(state)`.
/// Note a deliberate structural difference from C: in C the delegated
/// `gzclose_r` / `gzclose_w` free the handle themselves, whereas here they only
/// borrow `&mut GzState` and finalize it; the actual release happens when this
/// dispatcher drops the owning box. Because the delegated routine marks the
/// state closed first, that drop is an inert no-op (the double-finalize guard),
/// so teardown runs exactly once.
///
/// # Returns
///
/// * [`Z_STREAM_ERROR`] if `file` is [`None`] (the C `file == NULL` case).
/// * Otherwise the status code produced by
///   `gzclose_r` /
///   [`gzclose_w`] — [`Z_OK`](crate::constants::Z_OK)
///   on success, or a latched `Z_*` error such as
///   [`Z_BUF_ERROR`](crate::constants::Z_BUF_ERROR) /
///   [`Z_ERRNO`](crate::constants::Z_ERRNO).
///
/// # Examples
///
/// ```ignore
/// // `file` is an owned gzip handle obtained from the open path.
/// let rc = gzclose(Some(file));
/// assert_eq!(rc, zlib_rs::Z_OK);
/// // `file` has been consumed — using it again is a compile-time error.
/// ```
pub fn gzclose(file: Option<Box<GzState>>) -> i32 {
    // C: `if (file == NULL) return Z_STREAM_ERROR;`
    //
    // A `None` handle is the safe-Rust spelling of the C `NULL` check: there is
    // nothing to close, so report the identical error code C does.
    let Some(mut state) = file else {
        return Z_STREAM_ERROR;
    };

    // C: `return state->mode == GZ_READ ? gzclose_r(file) : gzclose_w(file);`
    //
    // Route to the read- or write-side teardown. Each routine performs the full
    // close (engine teardown, buffer release, fd close) and then calls
    // `mark_closed()`, so the owning box's `Drop` becomes an inert no-op.
    let rc = if state.mode == Mode::Read {
        gzclose_r(&mut state)
    } else {
        gzclose_w(&mut state)
    };

    // Release the handle (RAII): dropping the `Box<GzState>` frees the owned
    // `File` and buffers — the analogue of the C `free(state)`. The dispatched
    // close routine already finalized the stream and called `mark_closed()`, so
    // this `Drop` neither re-finalizes nor errors (the double-finalize guard);
    // teardown therefore happens exactly once.
    drop(state);

    rc
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::gzclose;
    use crate::constants::{Z_OK, Z_STREAM_ERROR};
    use crate::gz::state::{GzState, Mode};
    use crate::gz::write::gzwrite;
    use std::fs::File;
    use std::io::Read;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    /// A process-unique temp path so parallel test threads never collide.
    fn temp_path(tag: &str) -> PathBuf {
        static CTR: AtomicU64 = AtomicU64::new(0);
        let n = CTR.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        std::env::temp_dir().join(format!("blitzy_gzclose_ut_{tag}_{pid}_{n}.gz"))
    }

    /// Open a real file at `path` and wrap it in a fresh `GzState` of `mode`,
    /// boxed so it matches the owned-handle (`gzFile`) shape `gzclose` consumes.
    fn boxed_state(path: &Path, mode: Mode) -> Box<GzState> {
        let file = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)
            .expect("open temp file");
        Box::new(GzState::new(
            file,
            path.to_string_lossy().into_owned(),
            mode,
        ))
    }

    /// Decode a single-member gzip file via the flate2 (C zlib) oracle.
    fn decode_gzip(path: &Path) -> Vec<u8> {
        let data = std::fs::read(path).expect("read gz file");
        let mut dec = flate2::read::GzDecoder::new(&data[..]);
        let mut out = Vec::new();
        dec.read_to_end(&mut out).expect("flate2 decode");
        out
    }

    /// C parity: a `NULL` handle (`None`) must report `Z_STREAM_ERROR`.
    #[test]
    fn gzclose_none_returns_stream_error() {
        assert_eq!(gzclose(None), Z_STREAM_ERROR);
    }

    /// Read-side dispatch: a read-mode handle must route to `gzclose_r`, which
    /// succeeds (`Z_OK`) on a freshly opened stream. Crucially, had the
    /// dispatcher wrongly routed to `gzclose_w`, the non-write mode would yield
    /// `Z_STREAM_ERROR` — so observing `Z_OK` here proves correct read-side
    /// routing (the `mode == Read` arm of the C ternary).
    #[test]
    fn gzclose_read_mode_dispatches_to_gzclose_r() {
        let p = temp_path("read");
        let handle = boxed_state(&p, Mode::Read);
        assert_eq!(gzclose(Some(handle)), Z_OK);
        let _ = std::fs::remove_file(&p);
    }

    /// Write-side dispatch + finalize: a write-mode handle must route to
    /// `gzclose_w`, which finishes the gzip stream and returns `Z_OK`. The file
    /// on disk must then be a valid gzip stream decoding back to exactly the
    /// bytes written — proving (1) the write-side routing (the `else` arm of the
    /// C ternary), (2) the finalize that emits the gzip trailer, and (3) the
    /// absence of double-finalize corruption: the consumed box's `Drop` is an
    /// inert no-op after `mark_closed`, so no second trailer is appended.
    #[test]
    fn gzclose_write_mode_dispatches_and_produces_valid_gzip() {
        let p = temp_path("write");
        let payload: Vec<u8> = b"close.rs dispatcher write-side round trip! ".repeat(64);

        let mut handle = boxed_state(&p, Mode::Write);
        let written = gzwrite(&mut handle, &payload);
        assert_eq!(
            written as usize,
            payload.len(),
            "gzwrite should report full length"
        );

        assert_eq!(
            gzclose(Some(handle)),
            Z_OK,
            "write-side close should succeed"
        );
        assert_eq!(decode_gzip(&p), payload, "decoded gzip must equal input");

        let _ = std::fs::remove_file(&p);
    }
}
