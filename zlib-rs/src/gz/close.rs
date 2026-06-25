//! gzip file **close** dispatch — the safe-Rust port of the C `gzclose`
//! family (`gzclose`, `gzclose_r`, `gzclose_w`).
//!
//! In the C library these three functions are split across three translation
//! units — `gzclose.c` (the [`gzclose`] dispatcher), `gzread.c` (`gzclose_r`),
//! and `gzwrite.c` (`gzclose_w`) — purely so a static linker can drop the
//! unused read or write half of the library. The safe-Rust `gz` subsystem gates
//! the *entire* layer behind a single `gz-io` Cargo feature, so that linker
//! micro-optimization no longer applies; all three are therefore consolidated
//! here ("design B" of the gz-folder layout), beside the
//! [`read`](crate::gz::read) and [`write`](crate::gz::write) engines whose
//! teardown they drive (hence this module's dependency on both).
//!
//! # Availability & safety
//!
//! Compiled only under the `gz-io` feature (which implies `std` + `gzip`). Like
//! the rest of the core it is built under `#![forbid(unsafe_code)]`, so this
//! file contains **zero** `unsafe`: the C `inflateEnd` / `deflateEnd` /
//! `close(fd)` / `free()` teardown becomes Rust ownership. Dropping the owned
//! [`File`](std::fs::File) closes the descriptor, and dropping the owned
//! [`ZStream`](crate::stream::ZStream) and `Vec<u8>` buffers frees the
//! compression engine and the I/O buffers — augmented, for a write stream, by a
//! final compression-finish step that emits the closing deflate block and the
//! gzip CRC-32/ISIZE trailer.
//!
//! # Ownership & teardown contract
//!
//! The C API hands `gzclose(gzFile)` an opaque pointer, performs teardown,
//! `free`s the state, and returns a status code. The safe core models an open
//! file as an owned `Box<GzState>` (constructed by the [`open`](crate::gz::open)
//! module), so the close functions **take the state by value**
//! (`state: Box<GzState>`): they perform the part of teardown that must
//! *return* an error code — which [`Drop`] cannot — and then let the
//! [`GzState`] fall out of scope, where its field `Drop`s close the file and
//! free the buffers and engine (RAII).
//!
//! Concretely, each function:
//! 1. performs the mode-specific, error-returning teardown —
//!    [`gzclose_w`] finishes the gzip stream via [`GzState::finish_write`];
//!    [`gzclose_r`] harvests any pending `Z_BUF_ERROR`;
//! 2. flushes the file to surface a write/close error as `Z_ERRNO` — the
//!    closest faithful analogue of the C `close(fd) == -1` check, because
//!    Rust's [`File`](std::fs::File) `Drop` deliberately *ignores* close errors;
//! 3. calls [`GzState::finalize`] to mark the stream closed; and
//! 4. returns the accumulated zlib status code, after which the state drops.
//!
//! ## Relationship to `Drop` (design choice)
//!
//! The per-mode close logic lives *here* in `close.rs` (the layout's
//! recommended structure), while [`GzState`]'s own [`Drop`] (in
//! [`state`](crate::gz::state)) is a best-effort safety net that **delegates to
//! the same helper**: for a *write* stream dropped *without* an explicit
//! `gzclose*` it runs [`GzState::finish_write`], so even a forgotten close still
//! yields a complete, valid gzip file — the central promise of the RAII
//! re-architecture. The two paths cannot collide:
//!
//! * [`GzState::finish_write`] is **idempotent** (guarded by its
//!   `reset && have == 0` check), so a second finish emits no second trailer; and
//! * the close functions mark the stream closed via [`GzState::finalize`]
//!   (`mode` → [`GzMode::None`]), so the subsequent implicit `Drop` skips the
//!   finish entirely and `finalize` itself becomes a no-op.
//!
//! Thus an explicit close followed by the implicit `Drop` never double-finishes
//! or double-closes, and a bare drop still finishes exactly once.

use std::io::Write;

use crate::error::{ReturnCode, ZlibError};
use crate::gz::state::{GzMode, GzState};

// ---------------------------------------------------------------------------
// zlib status codes, as `i32`, mirroring the values the C `gzclose*` functions
// pass around. They are recovered from the strongly-typed `ReturnCode` /
// `ZlibError` enums (their authoritative `const fn as_i32()` accessors) so they
// can never drift from the canonical values. Only the codes this module
// actually uses are defined; an unused `const` would trip the crate's
// `-D warnings` dead-code gate.
// ---------------------------------------------------------------------------

/// `Z_OK` (0) — a successful close.
const Z_OK: i32 = ReturnCode::Ok.as_i32();
/// `Z_ERRNO` (-1) — a file/close error (the C `close(fd) == -1` case).
const Z_ERRNO: i32 = ZlibError::ErrNo.as_i32();
/// `Z_STREAM_ERROR` (-2) — the stream is not open in the expected mode.
const Z_STREAM_ERROR: i32 = ZlibError::StreamError.as_i32();
/// `Z_BUF_ERROR` (-5) — a pending soft error (e.g. an unexpected end of file)
/// on a read stream, propagated to the caller by [`gzclose_r`].
const Z_BUF_ERROR: i32 = ZlibError::BufError.as_i32();

/// Finish, flush and close a stream opened for **writing** — a port of the C
/// `gzclose_w` (`gzwrite.c` lines 666-700).
///
/// The stream is finished by [`GzState::finish_write`], which (mirroring the C
/// `gzclose_w` body) first drains any pending forward seek (`gz_zero`) and then
/// runs `gz_comp(Z_FINISH)` to emit the final deflate block and the gzip
/// CRC-32/ISIZE trailer. The deflate engine and the I/O buffers are released
/// when the consumed `state` drops, so the C `deflateEnd` / `free(out)` /
/// `free(in)` teardown (C lines 687-693) collapses into RAII and needs no
/// explicit call here.
///
/// Returns `Z_OK` on success; `Z_STREAM_ERROR` if `state` is not a write
/// stream; the finish error code if compression/flush failed; or `Z_ERRNO` if
/// the final file flush failed.
pub(crate) fn gzclose_w(mut state: Box<GzState>) -> i32 {
    // C lines 676-678: a stream that is not open for writing — including an
    // already-finalized `GzMode::None` — is a misuse; report it without
    // touching the engine.
    if state.mode != GzMode::Write {
        return Z_STREAM_ERROR;
    }

    let mut ret = Z_OK;

    // C lines 680-686: honor a pending forward seek (`gz_zero`) and then emit
    // the closing block + gzip trailer (`gz_comp(Z_FINISH)`). `finish_write`
    // folds both steps together and returns the recorded error code (`Z_OK` on
    // success). In C either failing step sets `ret = state->err`; here a
    // non-`Z_OK` result does the same.
    let finish_code = state.finish_write();
    if finish_code != Z_OK {
        ret = finish_code;
    }

    // C line 694: `gz_error(state, Z_OK, NULL)` — clear the error slot and free
    // any message (the path `String` is freed when `state` drops, C line 695).
    state.clear_error();

    // C lines 696-697: `if (close(state->fd) == -1) ret = Z_ERRNO;`. Rust's
    // `File` `Drop` ignores close errors, so flushing first is the closest
    // faithful approximation — any surfaced `io::Error` becomes `Z_ERRNO`. The
    // descriptor is closed when `state` drops immediately afterward.
    if state.file.flush().is_err() {
        ret = Z_ERRNO;
    }

    // Mark the stream closed so the implicit `Drop` (and its write-finish safety
    // net) is a guaranteed no-op; `state` then drops here, freeing the engine,
    // buffers, path and file (the C `free(state)` at line 698).
    let _ = state.finalize();
    ret
}

/// Free and close a stream opened for **reading** — a port of the C
/// `gzclose_r` (`gzread.c` lines 644-668).
///
/// There is no compression to finish: the inflate engine owned by
/// `state.strm` and the `in_buf` / `out_buf` buffers are released purely by
/// RAII when the consumed `state` drops, so the C `inflateEnd` / `free(out)` /
/// `free(in)` teardown (C lines 656-661) needs no explicit call. The one piece
/// of state that must survive into the return value is a pending `Z_BUF_ERROR`
/// (for example an unexpected end of file): it is propagated to the caller,
/// while any other code collapses to `Z_OK` (C line 662).
///
/// Returns `Z_OK` (or the propagated `Z_BUF_ERROR`) on a clean close;
/// `Z_STREAM_ERROR` if `state` is not a read stream; or `Z_ERRNO` if the final
/// file flush failed.
pub(crate) fn gzclose_r(mut state: Box<GzState>) -> i32 {
    // C lines 649-654: reject a stream that is not open for reading (including
    // an already-finalized `GzMode::None`).
    if state.mode != GzMode::Read {
        return Z_STREAM_ERROR;
    }

    // C lines 656-661: free the inflate engine and the I/O buffers. Here that is
    // pure RAII (the boxed inflate state in `state.strm` and the `Vec` buffers
    // drop with `state`), so there is nothing explicit to do.

    // C line 662: capture a pending `Z_BUF_ERROR` *before* clearing the slot;
    // any other code collapses to `Z_OK`.
    let err = if state.err == Z_BUF_ERROR {
        Z_BUF_ERROR
    } else {
        Z_OK
    };

    // C line 663: `gz_error(state, Z_OK, NULL)` — clear the error slot/message
    // (the path `String` is freed when `state` drops, C line 664).
    state.clear_error();

    // C lines 665-667: `ret = close(state->fd); return ret ? Z_ERRNO : err;`.
    // As in `gzclose_w`, flush to approximate close-error detection because
    // `File` `Drop` swallows it; the descriptor closes when `state` drops.
    let ret = if state.file.flush().is_err() {
        Z_ERRNO
    } else {
        err
    };

    // Mark the stream closed so the implicit `Drop` is a no-op, then drop.
    let _ = state.finalize();
    ret
}

/// Close a gzip file, dispatching by mode — a port of the C `gzclose`
/// (`gzclose.c` lines 11-23).
///
/// A read stream routes to [`gzclose_r`]; every other mode routes to
/// [`gzclose_w`]. This faithfully reproduces the C dispatch
/// `state->mode == GZ_READ ? gzclose_r(file) : gzclose_w(file)`:
///
/// * `GZ_APPEND` is converted to `GZ_WRITE` in `gz_open`, so an append stream
///   is already a [`GzMode::Write`] here and finishes correctly; and
/// * an already-closed [`GzMode::None`] falls through to [`gzclose_w`], which
///   returns `Z_STREAM_ERROR` for the wrong mode — exactly as the C code would.
///
/// The C NULL guard (`if (file == NULL) return Z_STREAM_ERROR;`) is intentionally
/// omitted: a `Box<GzState>` is non-null by construction, so translating a
/// NULL / `None` handle to `Z_STREAM_ERROR` is the concern of the FFI shim and
/// the `mod.rs` public surface, not the safe core. The C `#ifndef NO_GZCOMPRESS`
/// fallback (which would route everything to `gzclose_r` in a decompress-only
/// build) is likewise unnecessary: the `gz-io` feature always includes write
/// support, so the full dispatch is unconditionally compiled.
pub(crate) fn gzclose(state: Box<GzState>) -> i32 {
    match state.mode {
        GzMode::Read => gzclose_r(state),
        _ => gzclose_w(state),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gz::{open, read, write};
    use alloc::string::String;
    use alloc::vec::Vec;
    use std::fs::File;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    /// A unique temp-file path that deletes its backing file on drop, keeping
    /// the test tree clean even if a test panics.
    ///
    /// The *file* (not merely a handle) must outlive a write-close / read-open
    /// pair, so the cleanup is owned by this path holder rather than by any
    /// individual [`GzState`].
    struct TempPath(PathBuf);

    impl TempPath {
        fn new(tag: &str) -> Self {
            let mut p = std::env::temp_dir();
            let pid = std::process::id();
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            p.push(format!("zlibrs_gz_close_test_{tag}_{pid}_{n}.gz"));
            // Start from a clean slate in case a previous run left the path.
            let _ = std::fs::remove_file(&p);
            TempPath(p)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempPath {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    /// Open `path` for compressed writing (`"wb"`).
    fn open_write(path: &Path) -> Box<GzState> {
        open::gzopen(path, "wb").expect("open temp file for write")
    }

    /// Open `path` for reading (`"rb"`).
    fn open_read(path: &Path) -> Box<GzState> {
        open::gzopen(path, "rb").expect("open temp file for read")
    }

    /// Drain a read stream to EOF, returning every decompressed byte.
    fn read_all(state: &mut GzState) -> Vec<u8> {
        let mut out = Vec::new();
        let mut buf = [0u8; 64];
        loop {
            let n = read::gzread(state, &mut buf);
            assert!(n >= 0, "gzread reported an error ({n})");
            if n == 0 {
                break;
            }
            out.extend_from_slice(&buf[..n as usize]);
        }
        out
    }

    /// Build a bare read-mode state over `/dev/null` with a preset error code,
    /// for the mode / error-propagation checks that need no real gzip data.
    /// `/dev/null` is always available on the Unix CI hosts this crate targets.
    fn read_state_with_err(err: i32) -> Box<GzState> {
        let file = File::open("/dev/null").expect("open /dev/null");
        let mut state = GzState::new(file, String::from("/dev/null"), GzMode::Read);
        state.err = err;
        Box::new(state)
    }

    // -- mode rejection (the C `mode != GZ_*` guards) -----------------------

    #[test]
    fn gzclose_w_rejects_a_read_stream() {
        let state = read_state_with_err(Z_OK);
        assert_eq!(gzclose_w(state), Z_STREAM_ERROR);
    }

    #[test]
    fn gzclose_r_rejects_a_write_stream() {
        let tmp = TempPath::new("r_rejects_w");
        let state = open_write(tmp.path());
        assert_eq!(gzclose_r(state), Z_STREAM_ERROR);
    }

    // -- gzclose_r error harvesting (C line 662) ----------------------------

    #[test]
    fn gzclose_r_propagates_pending_buf_error() {
        let state = read_state_with_err(Z_BUF_ERROR);
        assert_eq!(gzclose_r(state), Z_BUF_ERROR);
    }

    #[test]
    fn gzclose_r_clean_stream_returns_ok() {
        let state = read_state_with_err(Z_OK);
        assert_eq!(gzclose_r(state), Z_OK);
    }

    #[test]
    fn gzclose_r_non_buf_error_collapses_to_ok() {
        // Only `Z_BUF_ERROR` is propagated; a hard error (e.g. `Z_DATA_ERROR`,
        // -3) collapses to `Z_OK`, matching the C ternary exactly.
        let state = read_state_with_err(ZlibError::DataError.as_i32());
        assert_eq!(gzclose_r(state), Z_OK);
    }

    // -- gzclose dispatch (C line 19) ---------------------------------------

    #[test]
    fn gzclose_routes_write_mode_to_gzclose_w() {
        let tmp = TempPath::new("dispatch_w");
        let state = open_write(tmp.path());
        assert_eq!(state.mode, GzMode::Write);
        assert_eq!(gzclose(state), Z_OK);
    }

    #[test]
    fn gzclose_routes_read_mode_to_gzclose_r() {
        let tmp = TempPath::new("dispatch_r");
        // Produce a valid (empty) gzip file first so the read open succeeds.
        assert_eq!(gzclose(open_write(tmp.path())), Z_OK);
        let state = open_read(tmp.path());
        assert_eq!(state.mode, GzMode::Read);
        assert_eq!(gzclose(state), Z_OK);
    }

    #[test]
    fn gzclose_on_finalized_stream_is_stream_error() {
        // A finalized stream has `mode == None`; dispatch routes it to
        // `gzclose_w`, which rejects the non-write mode with `Z_STREAM_ERROR`
        // — exactly as the C dispatch would for a `GZ_NONE` handle.
        let mut state = read_state_with_err(Z_OK);
        let _ = state.finalize();
        assert_eq!(state.mode, GzMode::None);
        assert_eq!(gzclose(state), Z_STREAM_ERROR);
    }

    // -- round trip via explicit close --------------------------------------

    #[test]
    fn write_close_then_read_close_round_trip() {
        let tmp = TempPath::new("round_trip");
        let data: &[u8] =
            b"The quick brown fox jumps over the lazy dog. \x00\x01\x02\xff and then some.";

        let mut wstate = open_write(tmp.path());
        assert_eq!(write::gzwrite(&mut wstate, data) as usize, data.len());
        assert_eq!(gzclose(wstate), Z_OK, "write close must succeed");

        let mut rstate = open_read(tmp.path());
        let got = read_all(&mut rstate);
        assert_eq!(gzclose(rstate), Z_OK, "read close must succeed");
        assert_eq!(got, data, "round-tripped data must match the original");
    }

    // -- RAII safety net: a dropped (un-closed) writer still finishes --------

    #[test]
    fn drop_without_close_still_finishes_the_stream() {
        let tmp = TempPath::new("drop_finishes");
        let data: &[u8] = b"data written but never explicitly closed";

        {
            let mut wstate = open_write(tmp.path());
            assert_eq!(write::gzwrite(&mut wstate, data) as usize, data.len());
            // Intentionally NO `gzclose`: `wstate` drops at the end of this
            // scope. `GzState`'s `Drop` runs `finish_write`, so the closing
            // block + gzip CRC-32/ISIZE trailer are still emitted and the file
            // is complete (the RAII safety net).
        }

        let mut rstate = open_read(tmp.path());
        let got = read_all(&mut rstate);
        assert_eq!(gzclose(rstate), Z_OK);
        assert_eq!(
            got, data,
            "a dropped writer must still produce a complete gzip file (RAII)"
        );
    }

    // -- idempotency: explicit close and bare drop yield identical files -----

    #[test]
    fn explicit_close_and_bare_drop_produce_identical_files() {
        // The gzip header is deterministic (MTIME = 0) and both writers use the
        // same default level/strategy, so a single finish from either path
        // produces byte-identical output. A double-finish (missing idempotency)
        // would append a second trailer and break this equality.
        let data: &[u8] = b"identical-output payload for the idempotency check";

        // (a) explicit gzclose.
        let tmp_a = TempPath::new("idem_close");
        let mut sa = open_write(tmp_a.path());
        assert_eq!(write::gzwrite(&mut sa, data) as usize, data.len());
        assert_eq!(gzclose(sa), Z_OK);
        let bytes_a = std::fs::read(tmp_a.path()).expect("read file a");

        // (b) bare drop (no explicit close).
        let tmp_b = TempPath::new("idem_drop");
        {
            let mut sb = open_write(tmp_b.path());
            assert_eq!(write::gzwrite(&mut sb, data) as usize, data.len());
        }
        let bytes_b = std::fs::read(tmp_b.path()).expect("read file b");

        assert_eq!(
            bytes_a, bytes_b,
            "explicit close and bare drop must emit exactly one identical gzip stream"
        );
        assert!(!bytes_a.is_empty(), "the gzip stream must be non-empty");
    }
}
