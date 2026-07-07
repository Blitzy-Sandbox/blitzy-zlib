//! The `gzclose` family of the gzip file-I/O layer.
//!
//! This module is the faithful Rust port of the C `gzclose.c` dispatcher
//! (`gzclose.c` L11-L23) together with the two direction-specific finalizers
//! that live in the read and write translation units of reference zlib:
//!
//! * [`gzclose_r`] &larr; C `gzclose_r` (`gzread.c` L645-L668)
//! * [`gzclose_w`] &larr; C `gzclose_w` (`gzwrite.c` L667-L700)
//! * [`gzclose`]   &larr; C `gzclose`   (`gzclose.c` L11-L23)
//!
//! In reference zlib these three entry points are deliberately split across
//! three source files so that a program linking only the reader or only the
//! writer does not drag in the other half of the library. That link-time
//! concern does not exist for a single Rust crate, so — per the migration's
//! function-family reorganization — all three are consolidated here while
//! preserving the exact C control flow and return-code contract required for
//! API/FFI parity.
//!
//! # Ownership &amp; RAII
//!
//! The C `gzFile` is an opaque pointer that the `gzclose*` functions *free*.
//! In this safe layer the close functions instead **consume an owned handle**,
//! [`Box<GzState>`]. Returning from a close function drops that box, and the
//! [`Drop`] implementation for [`GzState`] performs the resource teardown that
//! C does by hand:
//!
//! * the input/output [`Vec<u8>`](alloc::vec::Vec) buffers free their storage
//!   (C `free(state->in)` / `free(state->out)`);
//! * the embedded stream runs its own `Drop`, which is the
//!   `inflateEnd`/`deflateEnd` equivalent; and
//! * the owned [`File`](std::fs::File) closes the descriptor (C `close(fd)`).
//!
//! Consequently the functions here contribute only the parts that RAII *cannot*
//! express: the read/write **direction dispatch**, the mandatory write-side
//! `Z_FINISH` flush that finalizes the gzip member (the sliding-window `Drop`
//! deliberately does *not* flush, so that write errors remain observable), and
//! the reconstruction of zlib's integer return-code contract.
//!
//! The `#[no_mangle] extern "C"` shim in `src/ffi/gz.rs` bridges the raw
//! `gzFile` pointer to these owned-handle functions: it validates the pointer
//! (rejecting `NULL` with `Z_STREAM_ERROR`, the check that C performs at the top
//! of each function) and reconstructs the `Box` with `Box::from_raw` before
//! calling in. Because that pointer bookkeeping is confined to the FFI layer,
//! this module contains **zero `unsafe`**, enforced by the
//! `#![deny(unsafe_code)]` attribute below.

#![deny(unsafe_code)]

use crate::constants::FlushMode;
use crate::error::ReturnCode;
use crate::gz::state::{GzMode, GzState};
use crate::gz::write::{gz_comp, gz_zero};

/// Best-effort detection of a file-close error, used to reproduce zlib's
/// `ret = close(fd)` errno path in safe Rust.
///
/// Reference zlib inspects the return value of `close(2)` and reports a failure
/// as [`Z_ERRNO`](ReturnCode::ErrNo). Rust's [`File`](std::fs::File) closes its
/// descriptor in `Drop` and **cannot surface a `close(2)` error** to the
/// caller. To approximate that behavior we force any buffered data and metadata
/// to the storage device with [`File::sync_all`](std::fs::File::sync_all)
/// immediately before the handle is dropped; a failure of that sync is the
/// closest safe analogue of a failing `close(2)`.
///
/// Returns `true` if the sync failed (i.e. the caller should report
/// [`ReturnCode::ErrNo`]), and `false` otherwise.
fn sync_failed(file: &GzState) -> bool {
    file.file.sync_all().is_err()
}

/// Finalizes and closes a gzip file opened for **writing** — port of C
/// `gzclose_w` (`gzwrite.c` L667-L700).
///
/// This is the sole place a write stream is finalized. It flushes any buffered
/// input with `Z_FINISH`, emitting the final DEFLATE block and the gzip trailer
/// (the CRC-32 and ISIZE fields), then releases the handle via [`Drop`].
///
/// The handle is consumed by value: on return the [`Box<GzState>`] is dropped,
/// which frees the I/O buffers, ends the deflate stream, and closes the file
/// descriptor (the RAII replacement for the C `deflateEnd`/`free`/`close`
/// sequence).
///
/// # Return value
///
/// The raw zlib integer return code (see [`ReturnCode::as_c_int`]), following
/// the C precedence exactly:
///
/// * [`Z_STREAM_ERROR`](ReturnCode::StreamError) if the handle was not opened
///   for writing;
/// * otherwise [`Z_OK`](ReturnCode::Ok) on success, or the stream's recorded
///   error code if the pending-seek zero-fill or the `Z_FINISH` flush failed,
///   or [`Z_ERRNO`](ReturnCode::ErrNo) if the final file sync failed (a later
///   error in this sequence overrides an earlier one, matching C).
pub fn gzclose_w(mut file: Box<GzState>) -> i32 {
    // C L676-L677: reject a handle that is not open for writing.
    if file.mode != GzMode::Write {
        return ReturnCode::StreamError.as_c_int();
    }

    // C L671: the running result, defaulting to success.
    let mut ret = ReturnCode::Ok;

    // C L682-L683: honor a pending forward seek by compressing the requested
    // run of zero bytes before finishing. `gz_comp(Z_FINISH)` alone does not
    // drain a pending `skip`, so this step is required for byte-for-byte parity
    // with a zlib stream produced after `gzseek`. `gz_zero` records its own
    // error into `state.err`, which we then surface.
    if file.skip != 0 && gz_zero(&mut file).is_err() {
        ret = file.err;
    }

    // C L685-L686: the mandatory finalize flush. `gz_comp` sets `state.err` on
    // failure, so reading `file.err` here mirrors the C `ret = state->err`.
    if gz_comp(&mut file, FlushMode::Finish).is_err() {
        ret = file.err;
    }

    // C L695-L696: `if (close(fd) == -1) ret = Z_ERRNO;`. Approximated by
    // syncing before the descriptor is dropped (see [`sync_failed`]). This runs
    // last, so a close-time failure overrides an earlier flush error.
    if sync_failed(&file) {
        ret = ReturnCode::ErrNo;
    }

    // C L698: `free(state)`. Dropping the box frees the buffers, ends the
    // deflate stream, and closes the descriptor via RAII.
    drop(file);

    ret.as_c_int()
}

/// Closes a gzip file opened for **reading** — port of C `gzclose_r`
/// (`gzread.c` L645-L668).
///
/// The decompressor holds no unflushed output, so — unlike the write path —
/// there is nothing to finalize; the entire resource release (the C
/// `inflateEnd`/`free`/`close`) is performed by [`Drop`] when the consumed
/// [`Box<GzState>`] goes out of scope.
///
/// # Return value
///
/// The raw zlib integer return code (see [`ReturnCode::as_c_int`]):
///
/// * [`Z_STREAM_ERROR`](ReturnCode::StreamError) if the handle was not opened
///   for reading;
/// * [`Z_BUF_ERROR`](ReturnCode::BufError) if the stream's last recorded error
///   was a buffer error (C preserves this one code across close), otherwise
///   [`Z_OK`](ReturnCode::Ok);
/// * or [`Z_ERRNO`](ReturnCode::ErrNo) if the final file sync failed, which
///   overrides the status above (C `return ret ? Z_ERRNO : err;`).
pub fn gzclose_r(file: Box<GzState>) -> i32 {
    // C L650-L651: reject a handle that is not open for reading.
    if file.mode != GzMode::Read {
        return ReturnCode::StreamError.as_c_int();
    }

    // C L662: `err = state->err == Z_BUF_ERROR ? Z_BUF_ERROR : Z_OK;`.
    let status = if file.err == ReturnCode::BufError {
        ReturnCode::BufError
    } else {
        ReturnCode::Ok
    };

    // C L665: `ret = close(state->fd);`. Capture the best-effort close result
    // before the descriptor is dropped (see [`sync_failed`]).
    let close_failed = sync_failed(&file);

    // C L666: `free(state);`. RAII: end the inflate stream, free buffers, close
    // the descriptor.
    drop(file);

    // C L667: `return ret ? Z_ERRNO : err;`.
    if close_failed {
        ReturnCode::ErrNo.as_c_int()
    } else {
        status.as_c_int()
    }
}

/// Closes a gzip file, dispatching to the reader or writer finalizer — port of
/// C `gzclose` (`gzclose.c` L11-L23).
///
/// The C function first rejects a `NULL` handle with
/// [`Z_STREAM_ERROR`](ReturnCode::StreamError); that check is performed at the
/// FFI boundary (`src/ffi/gz.rs`), where the raw `gzFile` pointer is validated
/// before the owning [`Box<GzState>`] is reconstructed. By the time control
/// reaches this function the handle is therefore known to be non-null.
///
/// Dispatch mirrors C exactly: a read handle goes to [`gzclose_r`] and every
/// other mode goes to [`gzclose_w`]. [`GzMode::Append`] is normalized to
/// [`GzMode::Write`] at open time, so in practice the fall-through arm sees only
/// write handles; routing any non-read mode to [`gzclose_w`] preserves the C
/// `state->mode == GZ_READ ? gzclose_r(file) : gzclose_w(file)` ternary.
///
/// # Return value
///
/// The raw zlib integer return code produced by the selected finalizer.
pub fn gzclose(file: Box<GzState>) -> i32 {
    match file.mode {
        GzMode::Read => gzclose_r(file),
        _ => gzclose_w(file),
    }
}

#[cfg(test)]
mod tests {
    //! Unit tests for the close family: direction dispatch, the C return-code
    //! contract, and — for the write path — that the mandatory `Z_FINISH` flush
    //! produces a complete, independently decodable gzip member.
    //!
    //! Write-path tests finalize a stream and then decompress the resulting file
    //! with the reference `flate2` decoder (its pure-Rust `miniz_oxide` backend),
    //! proving that valid gzip (RFC 1952) framing — header, DEFLATE body, and
    //! CRC-32/ISIZE trailer — is emitted. All buffer handling here is safe Rust;
    //! there is **zero `unsafe`** in this module.

    use super::*;

    use crate::gz::state::How;
    use crate::gz::write::gz_write;
    use crate::stream::ZStream;
    use std::fs::File;
    use std::io::Read;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU32, Ordering};

    use flate2::read::GzDecoder;

    /// Generates a unique, process- and counter-tagged temporary file path. The
    /// `blitzy_adhoc_test_` prefix keeps these files out of any commit and makes
    /// them trivial to clean up.
    fn temp_path(tag: &str) -> PathBuf {
        static CTR: AtomicU32 = AtomicU32::new(0);
        let n = CTR.fetch_add(1, Ordering::Relaxed);
        let mut p = std::env::temp_dir();
        p.push(format!(
            "blitzy_adhoc_test_gzclose_{tag}_{}_{n}.gz",
            std::process::id()
        ));
        p
    }

    /// Builds a fresh write-mode [`GzState`] backed by a real, truncated temp
    /// file. `size` is left `0` so the finalize path lazily allocates buffers
    /// and initializes the deflate stream, exactly as a real `gzopen` would.
    fn write_state(path: &Path) -> Box<GzState> {
        let file = File::create(path).expect("create writable temp file");
        Box::new(GzState {
            have: 0,
            next: 0,
            pos: 0,
            mode: GzMode::Write,
            file,
            path: path.display().to_string(),
            size: 0,
            want: 8192,
            in_buf: Vec::new(),
            out_buf: Vec::new(),
            direct: 0,
            how: How::Look,
            junk: 0,
            again: false,
            in_next: 0,
            in_avail: 0,
            start: 0,
            eof: false,
            past: false,
            level: 6,
            strategy: 0,
            reset: false,
            skip: 0,
            err: ReturnCode::Ok,
            msg: None,
            strm: ZStream::new(),
        })
    }

    /// Builds a read-mode [`GzState`] backed by a real (materialized) temp file
    /// opened read-only, with the given last-recorded error code so the status
    /// mapping can be exercised.
    fn read_state(path: &Path, err: ReturnCode) -> Box<GzState> {
        File::create(path).expect("materialize temp file");
        let file = File::open(path).expect("open readable temp file");
        Box::new(GzState {
            have: 0,
            next: 0,
            pos: 0,
            mode: GzMode::Read,
            file,
            path: path.display().to_string(),
            size: 0,
            want: 8192,
            in_buf: Vec::new(),
            out_buf: Vec::new(),
            direct: 0,
            how: How::Look,
            junk: 0,
            again: false,
            in_next: 0,
            in_avail: 0,
            start: 0,
            eof: false,
            past: false,
            level: 6,
            strategy: 0,
            reset: false,
            skip: 0,
            err,
            msg: None,
            strm: ZStream::new(),
        })
    }

    /// Reads the whole file at `path`, then removes it, returning the bytes.
    fn read_and_remove(path: &Path) -> Vec<u8> {
        let bytes = std::fs::read(path).expect("read compressed output");
        let _ = std::fs::remove_file(path);
        bytes
    }

    /// Decompresses a complete gzip member with the reference `flate2` decoder.
    fn gunzip(compressed: &[u8]) -> Vec<u8> {
        let mut decoder = GzDecoder::new(compressed);
        let mut out = Vec::new();
        decoder
            .read_to_end(&mut out)
            .expect("output is a valid gzip member");
        out
    }

    #[test]
    fn gzclose_w_finalizes_empty_member() {
        // Closing a write handle that received no data must still emit a valid,
        // empty gzip member (header + empty block + zeroed trailer).
        let path = temp_path("empty");
        let ret = gzclose_w(write_state(&path));
        assert_eq!(
            ret,
            ReturnCode::Ok.as_c_int(),
            "an empty finalize returns Z_OK"
        );
        let bytes = read_and_remove(&path);
        assert_eq!(
            gunzip(&bytes),
            Vec::<u8>::new(),
            "the empty gzip member decodes to no bytes"
        );
    }

    #[test]
    fn gzclose_finalizes_written_data_via_dispatch() {
        // `gzclose` on a write handle must dispatch to `gzclose_w`, whose
        // `Z_FINISH` flush yields a member decodable by an independent decoder.
        const DATA: &[u8] = b"The quick brown fox jumps over the lazy dog.\n";
        let path = temp_path("data");
        let mut file = write_state(&path);
        let n = gz_write(&mut file, DATA);
        assert_eq!(n, DATA.len(), "all input bytes are accepted");

        let ret = gzclose(file);
        assert_eq!(
            ret,
            ReturnCode::Ok.as_c_int(),
            "finalize through gzclose returns Z_OK"
        );

        let bytes = read_and_remove(&path);
        assert_eq!(gunzip(&bytes), DATA, "the data round-trips exactly");
    }

    #[test]
    fn gzclose_r_returns_ok_on_clean_read() {
        let path = temp_path("read_ok");
        let ret = gzclose_r(read_state(&path, ReturnCode::Ok));
        assert_eq!(ret, ReturnCode::Ok.as_c_int());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn gzclose_r_preserves_buf_error() {
        // C L662: a lingering Z_BUF_ERROR is the one code preserved across close.
        let path = temp_path("read_buf");
        let ret = gzclose_r(read_state(&path, ReturnCode::BufError));
        assert_eq!(ret, ReturnCode::BufError.as_c_int());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn gzclose_r_maps_other_errors_to_ok() {
        // C L662: any non-buffer error collapses to Z_OK at close time.
        let path = temp_path("read_data_err");
        let ret = gzclose_r(read_state(&path, ReturnCode::DataError));
        assert_eq!(ret, ReturnCode::Ok.as_c_int());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn gzclose_r_rejects_a_write_handle() {
        let path = temp_path("wrongdir_r");
        let ret = gzclose_r(write_state(&path));
        assert_eq!(ret, ReturnCode::StreamError.as_c_int());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn gzclose_w_rejects_a_read_handle() {
        let path = temp_path("wrongdir_w");
        let ret = gzclose_w(read_state(&path, ReturnCode::Ok));
        assert_eq!(ret, ReturnCode::StreamError.as_c_int());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn gzclose_dispatches_a_read_handle_to_the_reader() {
        // Routing proof: a read handle carrying Z_BUF_ERROR must come back as
        // Z_BUF_ERROR (the reader's contract); had it gone to `gzclose_w` the
        // wrong-direction check would have produced Z_STREAM_ERROR instead.
        let path = temp_path("dispatch_read");
        let ret = gzclose(read_state(&path, ReturnCode::BufError));
        assert_eq!(ret, ReturnCode::BufError.as_c_int());
        let _ = std::fs::remove_file(&path);
    }
}
