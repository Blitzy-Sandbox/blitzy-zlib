//! Internal state for the buffered gzip file API (`gz*`).
//!
//! This module is a 1:1, idiomatic-Rust port of the C `gz_state` structure and
//! its companion mode / `how` constants from `gzguts.h`, together with the
//! exposed `struct gzFile_s` fields from `zlib.h`. It is the **foundational**
//! module of the `gz` (buffered gzip file I/O) subsystem: the sibling `open`,
//! `read`, `write`, and `close` modules all build on the [`GzState`] type
//! defined here.
//!
//! # Availability
//!
//! Available only under the `gz-io` Cargo feature (which implies `std` +
//! `gzip`). The feature gate is applied at the `pub mod gz;` declaration site in
//! the crate root, so no inner `#![cfg(...)]` attribute is needed here.
//!
//! # Safety and ownership model
//!
//! The whole crate carries `#![forbid(unsafe_code)]`, so this module contains
//! **zero `unsafe`**. The C representation is re-expressed with ownership:
//!
//! * the C `int fd` file descriptor becomes an owned [`File`](std::fs::File)
//!   (which is `Read + Write + Seek`);
//! * the `malloc`/`free`-managed `in` / `out` buffers become owned `Vec<u8>`
//!   buffers (`in_buf` / `out_buf`);
//! * the C `unsigned char *next` cursor into `out` becomes a plain `usize`
//!   index (`next`) — no raw pointers are used;
//! * the embedded `z_stream strm` becomes an owned [`ZStream`]; releasing it
//!   (and the boxed deflate / inflate engine it owns) happens in [`Drop`].
//!
//! # Teardown ownership contract
//!
//! Because Rust's [`Drop`] cannot return a value, the error-returning teardown
//! is split as follows, and the `read` / `write` / `close` modules rely on it:
//!
//! * The canonical `gzclose*` functions in the `close` module perform the
//!   **real**, error-returning teardown. For a write stream, `gzclose_w` first
//!   drives the compression-finish loop (`gz_comp(Z_FINISH)`, which lives in the
//!   `write` module) and then calls [`GzState::finalize`] to record the outcome;
//!   for a read stream, `gzclose_r` simply calls [`GzState::finalize`].
//! * [`GzState::finalize`] performs only the mode-agnostic bookkeeping this
//!   module owns and returns the accumulated zlib error code. It is
//!   **idempotent** (guarded by [`GzMode::None`]): once finalized, a second call
//!   is a no-op. The compression-finish loop is intentionally not duplicated
//!   here — it is owned by the `write` module and orchestrated by the `close`
//!   module — keeping this module decoupled and `unsafe`-free.
//! * [`Drop`] is a best-effort safety net that calls [`GzState::finalize`]
//!   again; after an explicit `gzclose` it is a no-op.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use std::fs::File;

use crate::constants::{Strategy, Z_DEFAULT_COMPRESSION};
use crate::error::{ReturnCode, ZlibError};
use crate::stream::ZStream;

/// Default size, in bytes, of the gzip I/O buffers (`GZBUFSIZE` in `gzguts.h`).
///
/// This is the value [`GzState::want`](GzState) is initialized to; it can be
/// overridden before the first read/write via the `gzbuffer` API. The output
/// buffer is allocated at twice this size when reading (to accommodate the
/// `gzungetc` pushback and leftover copied bytes), and the input buffer at twice
/// this size when writing (for `gzprintf`).
///
/// This is the single canonical definition for the crate; `gz/mod.rs` should
/// re-export it rather than redefine it.
pub const GZBUFSIZE: usize = 8192;

/// Default `memLevel` for the gzip deflate engine (`DEF_MEM_LEVEL` in
/// `gzguts.h`, which is `8` whenever `MAX_MEM_LEVEL >= 8`).
///
/// The gzip write path passes this to `deflateInit2`. It is re-exposed here for
/// the `gz` subsystem and pinned, by the compile-time assertion below, to the
/// crate-wide [`crate::constants::DEF_MEM_LEVEL`] so the two can never drift.
pub const DEF_MEM_LEVEL: i32 = 8;

// Compile-time invariant: the gz-layer default memLevel must equal the
// crate-wide constant. This assertion also marks `DEF_MEM_LEVEL` as "used", so
// it never trips the `dead_code` lint regardless of which module imports it.
const _: () = assert!(DEF_MEM_LEVEL == crate::constants::DEF_MEM_LEVEL);

// Raw zlib status codes used by the gz error slot (`state->err`). zlib's public
// header exposes these as `Z_*` `#define`s; the safe core models them as the
// `ReturnCode` / `ZlibError` enums, so the exact integers are recovered here
// from those authoritative `const fn` accessors to guarantee no value drift.
const Z_OK: i32 = ReturnCode::Ok.as_i32();
const Z_MEM_ERROR: i32 = ZlibError::MemError.as_i32();
const Z_BUF_ERROR: i32 = ZlibError::BufError.as_i32();

/// gzip stream mode — the safe-Rust replacement for the `GZ_*` mode integers in
/// `gzguts.h`.
///
/// In C the mode is stored as an `int` taking the deliberately odd sentinel
/// values `GZ_READ = 7247` and `GZ_WRITE = 31153` (with `GZ_NONE = 0` and
/// `GZ_APPEND = 1`); those magic numbers double as a crude integrity check on a
/// raw `gzFile` pointer. In safe Rust the type system already guarantees the
/// value is a valid `GzState`, so the sentinels are unnecessary and this is a
/// plain enum. (If the FFI shim ever needs to validate a raw pointer it may
/// reintroduce the numeric tags; that is the shim's concern, not the core's.)
///
/// [`None`](GzMode::None) additionally serves as the "already finalized" marker
/// for [`GzState::finalize`], mirroring the C `GZ_NONE` closed sentinel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GzMode {
    /// No mode — uninitialized or closed (`GZ_NONE`).
    None,
    /// Open for reading / decompression (`GZ_READ`).
    Read,
    /// Open for writing / compression (`GZ_WRITE`).
    Write,
    /// Open for appending (`GZ_APPEND`); the `open` module switches this to
    /// [`Write`](GzMode::Write) once the file has been positioned at its end.
    Append,
}

/// What the read path should currently do with incoming file data — the
/// safe-Rust replacement for the `how` integers in `gzguts.h`
/// (`LOOK = 0`, `COPY = 1`, `GZIP = 2`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GzHow {
    /// Look for a gzip header to decide between transparent copy and
    /// decompression (`LOOK`).
    Look,
    /// Copy input straight through; the data is not a gzip stream (`COPY`).
    Copy,
    /// Decompress an in-progress gzip stream (`GZIP`).
    Gzip,
}

/// Internal state backing a single open gzip file (`gz_state` in `gzguts.h`).
///
/// `GzState` owns everything required to service the buffered `gz*` API for one
/// file: the underlying [`File`](std::fs::File), the input/output buffers, the
/// embedded [`ZStream`], the current position and error slot, and the various
/// read- and write-specific bookkeeping flags. It is an internal type
/// (`pub(crate)`); the public surface (`GzFile` and the `gz*` functions) is
/// exposed by `gz/mod.rs` and the FFI shim.
///
/// Every field corresponds one-to-one with a field of the C `gz_state` struct;
/// the per-field documentation notes the C origin. The three "exposed" fields
/// [`have`](Self::have), [`next`](Self::next), and [`pos`](Self::pos) mirror the
/// `struct gzFile_s` members that the C `gzgetc()` macro reads directly; the FFI
/// shim re-exposes them for its own fast path.
pub(crate) struct GzState {
    // -- exposed contents (the `struct gzFile_s x` portion in C) --
    /// Number of output bytes currently available at [`next`](Self::next)
    /// (C `x.have`).
    pub(crate) have: usize,
    /// Index into [`out_buf`](Self::out_buf) of the next output byte to deliver
    /// (C `x.next`, which is a raw pointer there; here it is a safe `usize`
    /// offset).
    pub(crate) next: usize,
    /// Current position in the uncompressed data stream (C `x.pos`,
    /// a `z_off64_t`).
    pub(crate) pos: i64,

    // -- used for both reading and writing --
    /// Current stream mode (C `mode`); see [`GzMode`].
    pub(crate) mode: GzMode,
    /// The underlying OS file, owned (C `int fd`). It is `Read + Write + Seek`;
    /// the `open` module supplies it already positioned, and it is closed when
    /// this `GzState` is dropped.
    pub(crate) file: File,
    /// Path (or `<fd:N>` pseudo-path) used in error messages (C `char *path`).
    pub(crate) path: String,
    /// Allocated buffer size, or `0` when the buffers have not been allocated
    /// yet (C `size`). **`0` is a load-bearing sentinel**: the read/write
    /// initialization paths test `size == 0` to decide whether to allocate.
    pub(crate) size: usize,
    /// Requested buffer size, defaulting to [`GZBUFSIZE`] (C `want`; settable
    /// via `gzbuffer`).
    pub(crate) want: usize,
    /// Input buffer (C `in`; allocated double-sized when writing, for
    /// `gzprintf`). Named `in_buf` because `in` is a Rust keyword.
    pub(crate) in_buf: Vec<u8>,
    /// Output buffer (C `out`; allocated double-sized when reading, for the
    /// `gzungetc` pushback slide and leftover copied bytes). Named `out_buf` to
    /// parallel `in_buf`.
    pub(crate) out_buf: Vec<u8>,
    /// Transparent-vs-gzip tri-state (C `direct`). Its meaning differs by mode:
    /// when **reading** it starts at `1` (auto-detect, assume transparent for an
    /// empty file), is `-1` if the caller forced gzip-only (`'G'`), and becomes
    /// `0` once a real gzip stream is confirmed; when **writing** it is `0` for
    /// gzip compression or `1` for transparent passthrough. The exact `-1/0/1`
    /// transitions, performed in the `open`/`read` modules, are relied upon for
    /// bit-exact behavior, so this is kept as a faithful `i32`.
    pub(crate) direct: i32,

    // -- just for reading --
    /// Concatenated-member / trailing-garbage tracker (C `junk`): `-1` = start
    /// (first member), `1` = junk candidate (scanning for a *subsequent* gzip
    /// member, where trailing garbage is acceptable), `0` = confirmed inside a
    /// real gzip stream. Set by the `open`/`read` modules.
    pub(crate) junk: i32,
    /// What to do with current input (C `how`); see [`GzHow`].
    pub(crate) how: GzHow,
    /// `true` if the last I/O returned `EAGAIN`/`EWOULDBLOCK` (C `again`). With a
    /// blocking [`File`](std::fs::File) this is effectively always `false`, so
    /// the non-blocking branches that read it are unreachable in this port; the
    /// field is retained for fidelity because the read/write error checks (and
    /// [`set_error`](Self::set_error)) reference it.
    pub(crate) again: bool,
    /// File offset at which the gzip data started, used for rewinding
    /// (C `start`, a `z_off64_t`).
    pub(crate) start: i64,
    /// `true` once the end of the input file has been reached (C `eof`).
    pub(crate) eof: bool,
    /// `true` once a read has been requested past the end of the data; this is
    /// what `gzeof` reports (C `past`).
    pub(crate) past: bool,

    // -- just for writing --
    /// Compression level (C `level`); `-1` requests the zlib default.
    pub(crate) level: i32,
    /// Compression strategy as a raw zlib integer (C `strategy`). Kept as `i32`
    /// for parsing fidelity with `gzsetparams`/`gz_open`; it is converted to
    /// [`Strategy`] at the `deflateInit2` call site in the `write` module.
    pub(crate) strategy: i32,
    /// `true` if a `deflateReset` is pending after a `Z_FINISH`, so that a new
    /// gzip member only begins once more data is actually written (C `reset`).
    pub(crate) reset: bool,

    // -- seek request --
    /// Number of uncompressed bytes still to skip to satisfy a pending forward
    /// seek (C `skip`, a `z_off64_t`; the file is already rewound if the seek
    /// was backwards).
    pub(crate) skip: i64,

    // -- error information --
    /// Last zlib status code recorded for this stream (C `err`): `Z_OK` (0),
    /// `Z_ERRNO` (-1), `Z_STREAM_ERROR` (-2), `Z_DATA_ERROR` (-3),
    /// `Z_MEM_ERROR` (-4), or `Z_BUF_ERROR` (-5). This is the number `gzerror`
    /// returns; kept as a raw `i32` for exact error-slot semantics.
    pub(crate) err: i32,
    /// Formatted error message (`"path: msg"`), or `None` when there is no
    /// message (C `char *msg`). For `Z_MEM_ERROR` this stays `None` and
    /// `gzerror` substitutes the literal `"out of memory"`; see
    /// [`set_error`](Self::set_error).
    pub(crate) msg: Option<String>,

    // -- embedded zlib stream --
    /// The in-place zlib stream (C `z_stream strm`, embedded by value rather
    /// than as a pointer). [`GzState`] owns it; the `read` module initializes it
    /// for inflate and the `write` module for deflate, and it is released when
    /// this `GzState` is dropped. This module only constructs and holds it.
    pub(crate) strm: ZStream,
}

impl GzState {
    /// Build a freshly initialized `GzState` for an already-opened file.
    ///
    /// This mirrors the field initialization the C `gz_open` performs before its
    /// per-mode setup: the buffers are marked unallocated ([`size`](Self::size)
    /// `= 0`), [`want`](Self::want) defaults to [`GZBUFSIZE`], the error slot is
    /// clear, and the write parameters take their zlib defaults
    /// ([`level`](Self::level) `= Z_DEFAULT_COMPRESSION`,
    /// [`strategy`](Self::strategy) `= Z_DEFAULT_STRATEGY`). The embedded
    /// [`ZStream`] is created with [`ZStream::new`]. [`reset`](Self::reset) is
    /// then invoked to apply the per-mode bookkeeping defaults.
    ///
    /// The caller (the `open` module) is responsible for everything `gz_open`
    /// does *around* this — parsing the mode characters, finalizing
    /// [`level`](Self::level) / [`strategy`](Self::strategy) /
    /// [`direct`](Self::direct), opening/positioning the file, converting an
    /// append mode to [`GzMode::Write`], and capturing [`start`](Self::start)
    /// for read streams — none of which [`reset`](Self::reset) disturbs.
    pub(crate) fn new(file: File, path: String, mode: GzMode) -> Self {
        let mut state = GzState {
            // exposed contents
            have: 0,
            next: 0,
            pos: 0,
            // both reading and writing
            mode,
            file,
            path,
            size: 0,
            want: GZBUFSIZE,
            in_buf: Vec::new(),
            out_buf: Vec::new(),
            direct: 0,
            // just for reading (finalized by `reset` for read mode)
            junk: 0,
            how: GzHow::Look,
            again: false,
            start: 0,
            eof: false,
            past: false,
            // just for writing
            level: Z_DEFAULT_COMPRESSION,
            strategy: Strategy::Default.as_i32(),
            reset: false,
            // seek request
            skip: 0,
            // error information
            err: Z_OK,
            msg: None,
            // embedded zlib stream
            strm: ZStream::new(),
        };
        state.reset();
        state
    }

    /// Reset the per-operation bookkeeping to its initial state — an exact port
    /// of the C `gz_reset` (`gzlib.c`).
    ///
    /// Invoked on a freshly opened file and on `gzrewind`. It clears the
    /// available-output count and position and the stalled-I/O and skip flags,
    /// and applies the mode-specific defaults: for a read stream it clears the
    /// EOF/past flags, sets [`how`](Self::how) to [`GzHow::Look`] and
    /// [`junk`](Self::junk) to `-1` (mark first member); for a write stream it
    /// clears the pending-[`reset`](Self::reset) flag.
    ///
    /// It does **not** reallocate buffers or (re)initialize the deflate/inflate
    /// engine — that is done separately by the read/write initialization paths,
    /// exactly as in C where `gz_reset` is paired with `inflateReset` /
    /// `deflateReset`.
    ///
    /// The C line `state->strm.avail_in = 0` ("no input data yet") has no
    /// counterpart here: in this design the embedded [`ZStream`] retains no input
    /// between calls (per-call input is a borrowed slice), so "no pending input"
    /// is the natural post-reset state and there is nothing to clear on the
    /// stream.
    pub(crate) fn reset(&mut self) {
        self.have = 0; // no output data available
        if self.mode == GzMode::Read {
            self.eof = false; // not at end of file
            self.past = false; // have not read past end yet
            self.how = GzHow::Look; // look for gzip header
            self.junk = -1; // mark first member
        } else {
            self.reset = false; // no deflateReset pending
        }
        self.again = false; // no stalled i/o yet
        self.skip = 0; // no seek request pending
        self.clear_error(); // clear error (err = Z_OK, msg = None)
        self.pos = 0; // no uncompressed data yet
        // `self.strm`: no retained input to clear (see the doc comment above).
    }

    /// Return the slice of decompressed/copied output bytes currently available
    /// to the caller: `out_buf[next .. next + have]`.
    ///
    /// The read path (`gzread`/`gzgets`) copies from this slice and then calls
    /// [`consume_output`](Self::consume_output) to advance the cursor. When no
    /// output is buffered ([`have`](Self::have) `== 0`) this is an empty slice.
    pub(crate) fn output(&self) -> &[u8] {
        &self.out_buf[self.next..self.next + self.have]
    }

    /// Advance the output cursor after `n` bytes have been delivered to the
    /// caller: `next += n; have -= n; pos += n`.
    ///
    /// Mirrors the C `x.next += n; x.have -= n; x.pos += n` pattern. `n` must not
    /// exceed [`have`](Self::have); this precondition is checked with a
    /// `debug_assert!`.
    pub(crate) fn consume_output(&mut self, n: usize) {
        debug_assert!(
            n <= self.have,
            "consume_output: n = {n} exceeds available output have = {}",
            self.have
        );
        self.next += n;
        self.have -= n;
        self.pos += n as i64;
    }

    /// Fast single-byte read from the output buffer, backing the `gzgetc` fast
    /// path.
    ///
    /// If a buffered byte is available it is returned and the cursor is advanced
    /// (`have -= 1; next += 1; pos += 1`), mirroring the C `gzgetc()` macro;
    /// otherwise [`None`] is returned and the caller falls back to the full
    /// `gz_read` path.
    pub(crate) fn pop_byte(&mut self) -> Option<u8> {
        if self.have == 0 {
            return None;
        }
        let byte = self.out_buf[self.next];
        self.next += 1;
        self.have -= 1;
        self.pos += 1;
        Some(byte)
    }

    /// Set the stream's error slot — an exact port of the C `gz_error`
    /// (`gzlib.c`).
    ///
    /// The semantics, preserved precisely because `gzerror` / `gzclearerr` and
    /// the deferred-error behavior depend on them, are:
    ///
    /// 1. any previously stored message is dropped;
    /// 2. if the error is **fatal** — `err` is neither `Z_OK` nor `Z_BUF_ERROR`
    ///    and [`again`](Self::again) is `false` — [`have`](Self::have) is forced
    ///    to `0` so the `gzgetc` fast path falls through to the function and no
    ///    stale buffered output is reported;
    /// 3. the [`err`](Self::err) code is stored;
    /// 4. if `msg` is `None`, no message is built;
    /// 5. for `Z_MEM_ERROR` no message is constructed (the slot stays `None`;
    ///    `gzerror` substitutes the literal `"out of memory"`), avoiding an
    ///    allocation on the out-of-memory path;
    /// 6. otherwise the message becomes `"{path}: {msg}"`, matching the C
    ///    format.
    pub(crate) fn set_error(&mut self, err: i32, msg: Option<&str>) {
        // 1. drop any previous message.
        self.msg = None;

        // 2. if fatal, clear the available output so the gzgetc macro fails.
        if err != Z_OK && err != Z_BUF_ERROR && !self.again {
            self.have = 0;
        }

        // 3. record the error code.
        self.err = err;

        // 4. no message requested.
        let Some(text) = msg else {
            return;
        };

        // 5. out-of-memory: do not allocate; gzerror returns a literal instead.
        if err == Z_MEM_ERROR {
            return;
        }

        // 6. build the "path: msg" message.
        self.msg = Some(format!("{}: {}", self.path, text));
    }

    /// Clear the error slot, equivalent to `set_error(Z_OK, None)`.
    ///
    /// Resets [`err`](Self::err) to `Z_OK` and drops any stored message; because
    /// `Z_OK` is non-fatal this leaves [`have`](Self::have) untouched.
    pub(crate) fn clear_error(&mut self) {
        self.set_error(Z_OK, None);
    }

    /// Perform the mode-agnostic teardown bookkeeping and return the accumulated
    /// zlib error code.
    ///
    /// This is the shared helper behind the `gzclose*` family and [`Drop`]. As
    /// described in the module-level "Teardown ownership contract", the `close`
    /// module drives the real teardown — for a write stream it runs the
    /// `gz_comp(Z_FINISH)` compression-finish loop (in the `write` module)
    /// *before* calling this — and this method only records that the stream is
    /// closed and reports the error code. The boxed deflate/inflate engine held
    /// by [`strm`](Self::strm) is freed when the `GzState` is dropped.
    ///
    /// The method is **idempotent**: it marks the stream closed by setting
    /// [`mode`](Self::mode) to [`GzMode::None`], so a second call (notably the
    /// one from [`Drop`] after an explicit `gzclose`) is a no-op that simply
    /// returns the stored error code.
    pub(crate) fn finalize(&mut self) -> i32 {
        if self.mode == GzMode::None {
            // Already finalized — nothing more to do.
            return self.err;
        }
        // Mark the stream closed. The compression-finish loop (write streams)
        // and engine teardown are handled by the close/write modules and by
        // dropping `self`, respectively; see the module documentation.
        self.mode = GzMode::None;
        self.err
    }
}

impl Drop for GzState {
    /// Best-effort safety net: finalize the stream on scope exit.
    ///
    /// After an explicit `gzclose` the stream is already finalized (its
    /// [`mode`](Self::mode) is [`GzMode::None`]), so this call is a no-op. The
    /// owned [`File`](std::fs::File) and [`ZStream`] (and thus the boxed
    /// deflate/inflate engine) are released by their own `Drop` implementations
    /// immediately afterward. The returned error code is intentionally ignored
    /// because `Drop` cannot propagate it — callers that need it must use
    /// `gzclose`.
    fn drop(&mut self) {
        let _ = self.finalize();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Open a throwaway file handle for tests. The tests never perform real I/O
    /// through it; they only need a valid owned [`File`] to populate
    /// [`GzState::file`]. `/dev/null` is always available on the Unix CI hosts
    /// this crate is tested on.
    fn dummy_file() -> File {
        File::open("/dev/null").expect("open /dev/null for test")
    }

    #[test]
    fn new_read_applies_reset_defaults() {
        let state = GzState::new(dummy_file(), String::from("test.gz"), GzMode::Read);
        // Pre-parse defaults from gz_open.
        assert_eq!(state.mode, GzMode::Read);
        assert_eq!(state.size, 0); // buffers unallocated
        assert_eq!(state.want, GZBUFSIZE);
        assert_eq!(state.level, Z_DEFAULT_COMPRESSION);
        assert_eq!(state.strategy, Strategy::Default.as_i32());
        assert!(state.in_buf.is_empty());
        assert!(state.out_buf.is_empty());
        // gz_reset read-mode defaults.
        assert_eq!(state.how, GzHow::Look);
        assert_eq!(state.junk, -1);
        assert!(!state.eof);
        assert!(!state.past);
        assert!(!state.again);
        assert_eq!(state.skip, 0);
        assert_eq!(state.pos, 0);
        assert_eq!(state.have, 0);
        // Cleared error slot.
        assert_eq!(state.err, Z_OK);
        assert!(state.msg.is_none());
    }

    #[test]
    fn new_write_applies_reset_defaults() {
        let state = GzState::new(dummy_file(), String::from("out.gz"), GzMode::Write);
        assert_eq!(state.mode, GzMode::Write);
        // gz_reset write-mode default: no deflateReset pending.
        assert!(!state.reset);
        assert_eq!(state.err, Z_OK);
        assert!(state.msg.is_none());
    }

    #[test]
    fn set_error_data_error_builds_message_and_clears_have() {
        let mut state = GzState::new(dummy_file(), String::from("file.gz"), GzMode::Read);
        // Simulate buffered output that a fatal error must invalidate.
        state.have = 5;
        let code = ZlibError::DataError.as_i32();
        state.set_error(code, Some("bad data"));
        assert_eq!(state.err, code);
        assert_eq!(state.msg.as_deref(), Some("file.gz: bad data"));
        // A fatal error clears the available output.
        assert_eq!(state.have, 0);
    }

    #[test]
    fn set_error_mem_error_stores_no_message() {
        let mut state = GzState::new(dummy_file(), String::from("file.gz"), GzMode::Read);
        state.set_error(Z_MEM_ERROR, Some("ignored"));
        assert_eq!(state.err, Z_MEM_ERROR);
        // Z_MEM_ERROR never allocates a message; gzerror returns a literal.
        assert!(state.msg.is_none());
    }

    #[test]
    fn buf_error_is_not_fatal_and_preserves_have() {
        let mut state = GzState::new(dummy_file(), String::from("file.gz"), GzMode::Read);
        state.have = 3;
        state.set_error(Z_BUF_ERROR, None);
        // Z_BUF_ERROR is a soft error: available output is preserved.
        assert_eq!(state.have, 3);
        assert_eq!(state.err, Z_BUF_ERROR);
    }

    #[test]
    fn output_consume_and_pop_byte_round_trip() {
        let mut state = GzState::new(dummy_file(), String::from("file.gz"), GzMode::Read);
        state.out_buf = Vec::from([10u8, 20, 30, 40]);
        state.next = 0;
        state.have = 4;
        assert_eq!(state.output(), &[10, 20, 30, 40]);
        // Pop one byte via the fast path.
        assert_eq!(state.pop_byte(), Some(10));
        assert_eq!(state.have, 3);
        assert_eq!(state.next, 1);
        assert_eq!(state.pos, 1);
        assert_eq!(state.output(), &[20, 30, 40]);
        // Consume two more.
        state.consume_output(2);
        assert_eq!(state.output(), &[40]);
        assert_eq!(state.pos, 3);
        // Drain the last byte, then pop on empty returns None.
        assert_eq!(state.pop_byte(), Some(40));
        assert_eq!(state.have, 0);
        assert_eq!(state.pop_byte(), None);
    }

    #[test]
    fn finalize_is_idempotent() {
        let mut state = GzState::new(dummy_file(), String::from("file.gz"), GzMode::Write);
        assert_eq!(state.finalize(), Z_OK);
        assert_eq!(state.mode, GzMode::None);
        // A second finalize is a no-op returning the stored code.
        assert_eq!(state.finalize(), Z_OK);
        assert_eq!(state.mode, GzMode::None);
    }

    #[test]
    fn reset_clears_message_and_error() {
        let mut state = GzState::new(dummy_file(), String::from("file.gz"), GzMode::Read);
        state.set_error(ZlibError::DataError.as_i32(), Some("boom"));
        assert!(state.msg.is_some());
        state.reset();
        assert_eq!(state.err, Z_OK);
        assert!(state.msg.is_none());
        // Read-mode defaults are re-applied.
        assert_eq!(state.junk, -1);
        assert_eq!(state.how, GzHow::Look);
    }
}
