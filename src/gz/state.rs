//! Internal gzip file state (`GzState`) and shared sentinels, ported from zlib
//! `gzguts.h`. Backs the `gz*` file-I/O API. RAII `Drop` replaces `gzclose`.
//!
//! This module is the **foundational data model** of the `crate::gz` file-I/O
//! layer: the [`GzState`] handle is threaded through `open.rs`, `read.rs`,
//! `write.rs`, and `close.rs`, exactly as the C `gz_state` structure is threaded
//! through `gzlib.c`/`gzread.c`/`gzwrite.c`/`gzclose.c`. The whole `gz` module
//! is gated by the `gz-io` Cargo feature (which implies `std` + `gzip`), so this
//! file may freely use `std` (`std::fs::File`); the feature gate lives at the
//! `pub mod gz` declaration in `lib.rs`, not on this file.
//!
//! # C `gz_state` → idiomatic Rust
//!
//! The port replaces every manual-memory and raw-pointer idiom of the C struct
//! with an owning, safe Rust equivalent (AAP §0.6.3 memory-ownership model):
//!
//! | C (`gzguts.h`)                       | Rust (`GzState`)                         |
//! |--------------------------------------|------------------------------------------|
//! | `malloc`'d `in`/`out` buffers        | owned [`Vec<u8>`]                        |
//! | raw `int fd`                         | owned [`Option<File>`](std::fs::File)    |
//! | `x.next` raw pointer into `out`      | `next: usize` index into `out_buf`       |
//! | `strm.next_in` raw pointer into `in` | `in_next: usize` index into `in_buf`     |
//! | `x.have` / `avail_in` length fields  | `have: usize` / `in_avail: usize`        |
//! | opaque `z_stream strm` (in place)    | owned [`ZStream`] (engine state inside)  |
//! | manual `gzclose` teardown            | [`Drop`] (deterministic, leak-free)      |
//!
//! Raw pointers become **integer indices plus length counters** into the owned
//! buffers; `read.rs`/`write.rs` rely on this mapping precisely (see the field
//! documentation on [`GzState`]).
//!
//! # Frozen sentinel values
//!
//! The [`Mode`] and [`How`] discriminants and [`GZBUFSIZE`] are pinned to the
//! exact C `#define` values from `gzguts.h`. They are behavioral parity points
//! (AAP §0.6.1, §0.7.1): the mode values double as a lightweight integrity check
//! on a handle and, through the FFI shim, cross the C ABI, so they must never
//! drift from the canonical zlib constants.
//!
//! # Safety
//!
//! This file contains **no `unsafe`** (AAP §0.6.2 — `unsafe` is confined to
//! `crate::ffi` and `crate::inflate::fast`). It performs no C-string/`CStr`
//! marshalling either; that belongs to `crate::ffi`.

use crate::constants::*;
use crate::stream::ZStream;
use std::fs::File;

/// Default I/O buffer size, in bytes (C `GZBUFSIZE`, `gzguts.h`).
///
/// The output buffer is **double** this when reading, and the input buffer is
/// **double** this when writing (this value and twice this value must both fit
/// in the buffer-size type — trivially satisfied here, since the buffers are
/// [`Vec<u8>`] indexed by [`usize`]).
pub const GZBUFSIZE: usize = 8192;

/// The gzip file access mode (C `GZ_NONE`/`GZ_READ`/`GZ_WRITE`/`GZ_APPEND`,
/// `gzguts.h` L159–162).
///
/// The discriminants are the **frozen** zlib sentinel values. Beyond naming the
/// access mode, the unusual constants (`7247`, `31153`) double as a cheap
/// integrity check on a handle that has crossed the C ABI: a pointer that does
/// not carry one of these exact values in its mode field is not a valid
/// `gz_state`. Keeping the values identical to C is therefore a hard
/// requirement (AAP §0.6.1, §0.7.1), enforced by the unit tests in this module.
///
/// # `Append` is transient
///
/// `Append` is only ever observed *during* an open request. The C `gz_open`
/// rewrites `state->mode = GZ_WRITE` immediately after seeking to end-of-file
/// (so `gzoffset()` is correct), and this port does the same in `open.rs`.
/// After a successful open, a handle is therefore always [`Read`](Mode::Read) or
/// [`Write`](Mode::Write); [`Append`](Mode::Append) never persists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum Mode {
    /// `GZ_NONE` (0): no mode assigned yet (a freshly allocated or closed
    /// handle).
    None = 0,
    /// `GZ_APPEND` (1): open for appending. Transient — rewritten to
    /// [`Write`](Mode::Write) once the file is positioned at end-of-file.
    Append = 1,
    /// `GZ_READ` (7247): open for reading (decompression or transparent copy).
    Read = 7247,
    /// `GZ_WRITE` (31153): open for writing (compression).
    Write = 31153,
}

impl Mode {
    /// Returns the raw `i32` sentinel for this mode (the C `gz_state.mode`
    /// value), for the FFI shim and any place that must compare against the
    /// canonical zlib `#define`s.
    #[inline]
    #[must_use]
    pub const fn as_i32(self) -> i32 {
        self as i32
    }

    /// Reconstructs a [`Mode`] from its raw `i32` sentinel, or returns [`None`]
    /// if `v` is not one of the four legal mode values.
    ///
    /// This is the inverse of [`as_i32`](Mode::as_i32). The FFI shim reads the
    /// raw integer back out of a C `gz_state` and uses this to validate it, so
    /// an unrecognized value (a corrupt or non-`gz_state` handle) yields
    /// [`None`] rather than an out-of-range enum.
    #[inline]
    #[must_use]
    pub fn from_i32(v: i32) -> Option<Mode> {
        match v {
            0 => Some(Mode::None),
            1 => Some(Mode::Append),
            7247 => Some(Mode::Read),
            31153 => Some(Mode::Write),
            _ => None,
        }
    }
}

/// The read-side substate (C `how`, `gzguts.h` L165–167): what the reader is
/// currently doing with the input stream.
///
/// Set to [`Look`](How::Look) by `gz_reset` at the start of each gzip member;
/// `gz_look` (in `read.rs`) inspects the input and switches to either
/// [`Copy`](How::Copy) (the input is not gzip — pass it through transparently)
/// or [`Gzip`](How::Gzip) (decompress a gzip stream). The discriminants are the
/// frozen zlib sentinel values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum How {
    /// `LOOK` (0): look for a gzip header at the current input position.
    Look = 0,
    /// `COPY` (1): copy input directly to output (the data is not gzip —
    /// transparent pass-through).
    Copy = 1,
    /// `GZIP` (2): decompress a gzip stream.
    Gzip = 2,
}

/// The internal gzip file state — the idiomatic, owning port of the C
/// `gz_state` structure (`gzguts.h` L170–203).
///
/// `GzState` is the single handle that the whole `crate::gz` layer threads
/// through `open`/`read`/`write`/`close`. It owns every resource the gzip file
/// session needs — the OS file, the input/output working buffers, and the
/// compression/decompression engine state — and releases all of them
/// deterministically on [`Drop`] (the RAII replacement for `gzclose`, AAP
/// §0.3.2 / §0.6.3).
///
/// Fields are `pub(crate)` so the sibling `gz` modules can manipulate the state
/// directly (mirroring C's direct struct access through `gz_statep`), while the
/// type stays opaque outside the crate.
///
/// # Cursor-vs-pointer mapping (load-bearing for `read.rs`/`write.rs`)
///
/// C tracks output delivery with a raw pointer `x.next` that walks through the
/// `out` buffer, and input consumption with `strm.next_in` walking through `in`.
/// This port replaces both raw pointers with **`usize` indices** into the owned
/// [`Vec<u8>`] buffers, paired with length counters:
///
/// * Output: [`next`](GzState::next) is the index of the next byte to deliver
///   within [`out_buf`](GzState::out_buf); [`have`](GzState::have) is how many
///   bytes remain available starting at that index. The delivered window is
///   exactly `out_buf[next .. next + have]` (see [`out_slice`](GzState::out_slice)).
/// * Input: [`in_next`](GzState::in_next) is the index of the next unread byte
///   within [`in_buf`](GzState::in_buf); [`in_avail`](GzState::in_avail) is how
///   many valid bytes remain there. (The engine's own `next_out`/`avail_out` are
///   not stored on the stream; `read.rs`/`write.rs` hand the engine a
///   `&mut [u8]` slice of `out_buf` per call.)
pub struct GzState {
    // --- exposed contents (C: `struct gzFile_s x`, the gzgetc() fast path) ---
    /// Number of output bytes available at [`next`](GzState::next) within
    /// [`out_buf`](GzState::out_buf) (C `x.have`).
    pub(crate) have: usize,
    /// Index of the next output byte to deliver, within
    /// [`out_buf`](GzState::out_buf) (C `x.next`, which is a raw pointer; here an
    /// index — see the type-level *cursor-vs-pointer mapping*).
    pub(crate) next: usize,
    /// Current position in the uncompressed data stream (C `x.pos`,
    /// `z_off64_t`). Signed 64-bit so it matches zlib's large-file offset type.
    pub(crate) pos: i64,

    // --- used for both reading and writing ---
    /// The access mode (C `mode`). See [`Mode`].
    pub(crate) mode: Mode,
    /// The owned OS file handle (C `int fd`). [`None`] once the handle has been
    /// explicitly closed, after which [`Drop`] is a no-op for the file.
    pub(crate) file: Option<File>,
    /// The path (or synthetic `<fd:N>` identifier) used in error messages
    /// (C `char *path`).
    pub(crate) path: String,
    /// Allocated buffer size, or `0` if the buffers have not been allocated yet
    /// (C `size`). `size == 0` ⇔ [`in_buf`](GzState::in_buf)/[`out_buf`](GzState::out_buf)
    /// are still empty, exactly like the C `malloc`-on-first-use scheme.
    pub(crate) size: usize,
    /// Requested buffer size (C `want`); defaults to [`GZBUFSIZE`] and is
    /// adjustable via `gzbuffer` before the first I/O.
    pub(crate) want: usize,
    /// Input buffer (C `in`). Owned [`Vec<u8>`] replacing the `malloc`'d block;
    /// sized to `2 * want` when writing. Empty until allocated.
    pub(crate) in_buf: Vec<u8>,
    /// Output buffer (C `out`). Owned [`Vec<u8>`] replacing the `malloc`'d
    /// block; sized to `2 * want` when reading. Empty until allocated.
    pub(crate) out_buf: Vec<u8>,
    /// Transparency tri-state (C `direct`): `0` = processing a gzip stream,
    /// `1` = transparent (raw copy of non-gzip input), `-1` = gzip-only (set by
    /// the `T` mode flag on read; reject non-gzip input). Kept as `i32` to
    /// preserve the exact tri-state comparisons made in `open.rs`/`read.rs`.
    pub(crate) direct: i32,

    // --- just for reading ---
    /// First-member / junk tracking (C `junk`): `-1` = at the start (first
    /// member), `1` = a junk candidate after a member, `0` = inside a gzip
    /// member. Kept as `i32` to preserve the tri-state semantics.
    pub(crate) junk: i32,
    /// The read substate (C `how`). See [`How`].
    pub(crate) how: How,
    /// `true` if the last low-level read returned `EAGAIN`/`EWOULDBLOCK`
    /// (C `again`). When set, an error is *not* treated as fatal by
    /// [`gz_error`](GzState::gz_error) (the I/O may simply be retried).
    pub(crate) again: bool,
    /// Offset in the file where the current gzip data started, for rewinding
    /// (C `start`, `z_off64_t`).
    pub(crate) start: i64,
    /// `true` once end-of-input has been reached (C `eof`).
    pub(crate) eof: bool,
    /// `true` once a read has been requested past the end of the data
    /// (C `past`).
    pub(crate) past: bool,

    // --- just for writing ---
    /// Compression level (C `level`); defaults to [`Z_DEFAULT_COMPRESSION`].
    pub(crate) level: i32,
    /// Compression strategy (C `strategy`); defaults to
    /// [`Z_DEFAULT_STRATEGY`].
    pub(crate) strategy: i32,
    /// `true` if a `deflateReset` is pending after a `Z_FINISH`
    /// (C `reset`) — i.e. a complete gzip member was just flushed and the next
    /// write must start a fresh member.
    pub(crate) reset: bool,

    // --- seek request ---
    /// Amount still to skip to satisfy a pending seek (C `skip`, `z_off64_t`;
    /// already rewound if the seek was backwards).
    pub(crate) skip: i64,

    // --- error information ---
    /// The last error code (C `err`). Holds a raw `Z_*` value (e.g.
    /// [`Z_OK`], [`Z_BUF_ERROR`], [`Z_DATA_ERROR`], [`Z_MEM_ERROR`]) so that
    /// `gzerror`/`gzclose_*` return values are bit-exact with C; conversion to
    /// [`ReturnCode`](crate::error::ReturnCode)/[`ZlibError`](crate::error::ZlibError)
    /// is for ergonomics only.
    pub(crate) err: i32,
    /// The last formatted error message (C `char *msg`), as
    /// `"<path>: <message>"`. [`None`] when there is no message — and, matching
    /// C, deliberately left [`None`] for [`Z_MEM_ERROR`], which `gzerror`
    /// reports with the literal `"out of memory"`.
    pub(crate) msg: Option<String>,

    // --- zlib inflate or deflate stream (C: `z_stream strm`, in place) ---
    /// The owned compression/decompression engine (C `z_stream strm`). The
    /// boxed engine state inside [`ZStream`] is freed automatically when this
    /// `GzState` is dropped — the idiomatic replacement for
    /// `deflateEnd`/`inflateEnd`.
    pub(crate) strm: ZStream,
    /// Index into [`in_buf`](GzState::in_buf) of the next unread input byte
    /// (the engine's C `strm.next_in`, expressed as an index — see the
    /// type-level *cursor-vs-pointer mapping*).
    pub(crate) in_next: usize,
    /// Number of valid input bytes remaining at [`in_next`](GzState::in_next)
    /// (the engine's C `strm.avail_in`).
    pub(crate) in_avail: usize,

    // --- finalization guard (Rust-only; supports the RAII contract) ---
    /// `true` once the handle has been finalized by an explicit
    /// `close()`/`finish()` *or* by [`Drop`]. Guarantees that finalization runs
    /// **at most once** (no double-finalize): an explicit close marks this, and
    /// [`Drop`] then becomes a no-op. There is no C counterpart — C relies on
    /// the caller not to `gzclose` twice; the Rust contract makes that safe by
    /// construction.
    pub(crate) finalized: bool,
}

impl GzState {
    /// Creates a new gzip file state for `file`, identified by `path`, opened in
    /// `mode`, with all fields set to the C `gz_open` post-allocation defaults
    /// combined with the `gz_reset` initial values (`gzlib.c`).
    ///
    /// Specifically (matching `gz_open` + `gz_reset`):
    ///
    /// * buffers unallocated: [`size`](GzState::size) `= 0`,
    ///   [`in_buf`](GzState::in_buf)/[`out_buf`](GzState::out_buf) empty;
    /// * [`want`](GzState::want) `= GZBUFSIZE`;
    /// * [`level`](GzState::level) `= Z_DEFAULT_COMPRESSION`,
    ///   [`strategy`](GzState::strategy) `= Z_DEFAULT_STRATEGY`;
    /// * [`direct`](GzState::direct) `= 0`, [`junk`](GzState::junk) `= -1`
    ///   (first member), [`how`](GzState::how) `= How::Look`;
    /// * no error: [`err`](GzState::err) `= Z_OK`, [`msg`](GzState::msg)
    ///   `= None`;
    /// * all cursors/counters zero, all read/write flags clear, a fresh
    ///   [`ZStream`], and not yet [`finalized`](GzState::finalized).
    ///
    /// The mode-specific reset performed by `gz_reset` (clearing `eof`/`past`
    /// for reading, `reset` for writing) is folded into these defaults; the
    /// caller in `open.rs` applies any further mode adjustments (for example
    /// rewriting [`Mode::Append`] to [`Mode::Write`]).
    pub(crate) fn new(file: File, path: String, mode: Mode) -> GzState {
        GzState {
            // exposed contents
            have: 0,
            next: 0,
            pos: 0,
            // both reading and writing
            mode,
            file: Some(file),
            path,
            size: 0,
            want: GZBUFSIZE,
            in_buf: Vec::new(),
            out_buf: Vec::new(),
            direct: 0,
            // reading
            junk: -1,
            how: How::Look,
            again: false,
            start: 0,
            eof: false,
            past: false,
            // writing
            level: Z_DEFAULT_COMPRESSION,
            strategy: Z_DEFAULT_STRATEGY,
            reset: false,
            // seek request
            skip: 0,
            // error information
            err: Z_OK,
            msg: None,
            // engine + input cursor bookkeeping
            strm: ZStream::new(),
            in_next: 0,
            in_avail: 0,
            // finalization guard
            finalized: false,
        }
    }

    /// Returns `true` if this handle is open for reading
    /// ([`Mode::Read`](Mode::Read)).
    #[inline]
    #[must_use]
    pub(crate) fn is_reading(&self) -> bool {
        self.mode == Mode::Read
    }

    /// Returns `true` if this handle is open for writing
    /// ([`Mode::Write`](Mode::Write)).
    ///
    /// Note that a handle opened in [`Mode::Append`](Mode::Append) is rewritten
    /// to [`Mode::Write`](Mode::Write) during open, so append streams report
    /// `true` here too.
    #[inline]
    #[must_use]
    pub(crate) fn is_writing(&self) -> bool {
        self.mode == Mode::Write
    }

    /// Returns the slice of decompressed output currently available to the
    /// reader: `out_buf[next .. next + have]`.
    ///
    /// This is the safe-Rust expression of the C `gzgetc()` fast path, which
    /// reads through the raw `x.next` pointer for `x.have` bytes. The slice is
    /// empty when [`have`](GzState::have) is `0`.
    ///
    /// # Panics
    ///
    /// Panics if `next + have` exceeds `out_buf.len()`. That can only happen if
    /// a caller corrupts the cursor invariant maintained by `read.rs`
    /// (`next + have <= out_buf.len()` always holds for a well-formed handle),
    /// so it indicates a bug rather than bad input.
    #[inline]
    #[must_use]
    pub(crate) fn out_slice(&self) -> &[u8] {
        &self.out_buf[self.next..self.next + self.have]
    }

    /// Portable maximum value of a C `int`, as an unsigned (C `gz_intmax`,
    /// `gzlib.c`).
    ///
    /// C uses this to guard unsigned-vs-`z_off64_t` comparisons (the `GT_OFF`
    /// macro) when `int` and `z_off64_t` happen to be the same width. This port
    /// represents positions and seek amounts with `i64` (`z_off64_t`) and
    /// lengths with `usize` throughout, so those `GT_OFF`-style overflow guards
    /// largely collapse; this helper is retained for the FFI `gzseek` path,
    /// which still reports offsets through the C `int`/`z_off_t` surface.
    #[inline]
    #[must_use]
    pub(crate) const fn gz_intmax() -> u32 {
        i32::MAX as u32
    }

    /// Records an error on the handle, mirroring the C `gz_error` (`gzlib.c`
    /// L555–590) behavior exactly.
    ///
    /// Steps, in C order:
    ///
    /// 1. Clear any previous message ([`msg`](GzState::msg) `= None`). In C this
    ///    frees the prior `malloc`'d string (unless it was the static
    ///    out-of-memory message); here dropping the [`Option<String>`] does the
    ///    equivalent automatically, so the C `err != Z_MEM_ERROR` free guard is
    ///    unnecessary.
    /// 2. If the error is **fatal** — `err` is neither [`Z_OK`] nor
    ///    [`Z_BUF_ERROR`] **and** [`again`](GzState::again) is `false` — set
    ///    [`have`](GzState::have) to `0` so the fast `gzgetc` path fails and
    ///    forces a re-check.
    /// 3. Store `err` in [`err`](GzState::err).
    /// 4. If `msg` is [`None`], stop (code recorded, no message).
    /// 5. If `err == Z_MEM_ERROR`, stop **without** building a message: out of
    ///    memory is the one case where C does not allocate, and `gzerror`
    ///    special-cases it to return the literal `"out of memory"`. Leaving
    ///    [`msg`](GzState::msg) as [`None`] reproduces that.
    /// 6. Otherwise format and store `"<path>: <message>"`.
    pub(crate) fn gz_error(&mut self, err: i32, msg: Option<&str>) {
        // (1) free/clear any previous message.
        self.msg = None;

        // (2) if fatal, zero `have` so the gzgetc() fast path fails.
        if err != Z_OK && err != Z_BUF_ERROR && !self.again {
            self.have = 0;
        }

        // (3) set the error code.
        self.err = err;

        // (4) no message requested -> done.
        let Some(msg) = msg else {
            return;
        };

        // (5) out-of-memory: do not allocate; gzerror() returns the literal
        // "out of memory" for Z_MEM_ERROR, so leave `msg` as None.
        if err == Z_MEM_ERROR {
            return;
        }

        // (6) construct "<path>: <message>".
        self.msg = Some(format!("{}: {}", self.path, msg));
    }

    /// Clears the error state, equivalent to the frequent C idiom
    /// `gz_error(state, Z_OK, NULL)`.
    ///
    /// Resets [`err`](GzState::err) to [`Z_OK`] and [`msg`](GzState::msg) to
    /// [`None`]. Because [`Z_OK`] is not fatal, [`have`](GzState::have) is left
    /// untouched.
    #[inline]
    pub(crate) fn clear_error(&mut self) {
        self.gz_error(Z_OK, None);
    }

    /// Performs idempotent finalization of the handle and reports whether this
    /// call did the work.
    ///
    /// Returns `true` the first time it runs (marking the handle
    /// [`finalized`](GzState::finalized)), and `false` on every subsequent call
    /// — the guarantee that finalization happens **at most once** (no
    /// double-finalize, AAP §0.6.3 / §0.3.2).
    ///
    /// This method only flips the guard flag; it intentionally performs no I/O.
    /// The observable teardown is split by responsibility:
    ///
    /// * **Resource release** (closing the file, freeing the engine state and
    ///   the working buffers) happens automatically when the owned fields are
    ///   dropped — see [`Drop`].
    /// * **The write-side flush** (emitting the trailing deflate data, the gzip
    ///   trailer, and `deflateEnd`) is the job of the explicit
    ///   `close()`/`finish()` entry points in `crate::gz::write`/`crate::gz::close`,
    ///   which return a status the caller can observe. Those entry points call
    ///   `finalize()` after a successful flush so that the subsequent [`Drop`]
    ///   becomes a no-op.
    ///
    /// Splitting it this way keeps the flush — which can fail with an I/O error
    /// — on a path that returns a result, while [`Drop`] remains an
    /// infallible, panic-free safety net. It also keeps this file within its
    /// dependency boundary: the flush logic lives in sibling `gz` modules, and
    /// `GzState` does not reach into them.
    pub(crate) fn finalize(&mut self) -> bool {
        if self.finalized {
            return false;
        }
        self.finalized = true;
        true
    }
}

/// RAII teardown — the deterministic, leak-free replacement for `gzclose`
/// (AAP §0.3.2, §0.6.3).
///
/// Dropping a `GzState` always releases every resource it owns and never
/// unwinds:
///
/// * the engine state inside [`strm`](GzState::strm) is freed when the
///   [`ZStream`] is dropped (the `inflateEnd`/`deflateEnd` equivalent — no
///   explicit action needed here);
/// * the OS file in [`file`](GzState::file) is closed when the
///   [`Option<File>`](std::fs::File) is dropped;
/// * the [`in_buf`](GzState::in_buf)/[`out_buf`](GzState::out_buf) buffers are
///   freed when their [`Vec<u8>`] are dropped.
///
/// All of that happens automatically *after* this method returns, when the
/// struct's fields are dropped in turn; this impl therefore only needs to run
/// the at-most-once finalization guard.
///
/// # The flush contract
///
/// `Drop` is a **safety net**, not the primary close path. For a write handle,
/// the trailing compressed data and the gzip trailer are flushed by the
/// explicit `close()`/`finish()` functions in `crate::gz::write` /
/// `crate::gz::close`, which return a status so the caller can observe an I/O
/// error on close (this mirrors `flate2`'s `GzEncoder::finish` + `Drop`). Those
/// functions call [`finalize`](GzState::finalize) once their flush succeeds, so
/// reaching `Drop` afterward is a no-op. A handle dropped *without* an explicit
/// close is still torn down without leaking; callers that need to observe close
/// errors (or guarantee the final bytes are flushed) must call the explicit
/// close path.
///
/// `Drop` cannot return a `Result`, so any work it performs must be infallible;
/// keeping the fallible flush on the explicit path is what lets this impl stay
/// panic-free.
impl Drop for GzState {
    fn drop(&mut self) {
        // Run the at-most-once guard. If an explicit close()/finish() already
        // finalized the handle, this returns false and we do nothing further.
        // Either way, the owned fields (strm, file, in_buf, out_buf) are dropped
        // automatically once this method returns, releasing all resources.
        let _ = self.finalize();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Builds a unique temporary path under the system temp dir without relying
    /// on any external crate (process id + a monotonically increasing counter
    /// keep concurrent test runs from colliding).
    fn unique_temp_path(tag: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut p = std::env::temp_dir();
        p.push(format!(
            "zlib_rs_gzstate_test_{}_{}_{}.bin",
            tag,
            std::process::id(),
            n
        ));
        p
    }

    /// Creates a fresh temporary file and returns it together with its path (as
    /// the `String` that `GzState` stores) and the `PathBuf` for cleanup.
    fn temp_file(tag: &str) -> (File, String, PathBuf) {
        let path = unique_temp_path(tag);
        let file = File::create(&path).expect("create temp file for test");
        let path_str = path.to_string_lossy().into_owned();
        (file, path_str, path)
    }

    #[test]
    fn mode_sentinels_are_frozen() {
        // The discriminants must equal the C gzguts.h #defines exactly.
        assert_eq!(Mode::None as i32, 0);
        assert_eq!(Mode::Append as i32, 1);
        assert_eq!(Mode::Read as i32, 7247);
        assert_eq!(Mode::Write as i32, 31153);

        // as_i32() agrees with the cast.
        assert_eq!(Mode::None.as_i32(), 0);
        assert_eq!(Mode::Append.as_i32(), 1);
        assert_eq!(Mode::Read.as_i32(), 7247);
        assert_eq!(Mode::Write.as_i32(), 31153);

        // from_i32() is the exact inverse, and rejects unknown values.
        assert_eq!(Mode::from_i32(0), Some(Mode::None));
        assert_eq!(Mode::from_i32(1), Some(Mode::Append));
        assert_eq!(Mode::from_i32(7247), Some(Mode::Read));
        assert_eq!(Mode::from_i32(31153), Some(Mode::Write));
        assert_eq!(Mode::from_i32(2), None);
        assert_eq!(Mode::from_i32(-1), None);
        assert_eq!(Mode::from_i32(i32::MAX), None);
    }

    #[test]
    fn how_sentinels_are_frozen() {
        assert_eq!(How::Look as i32, 0);
        assert_eq!(How::Copy as i32, 1);
        assert_eq!(How::Gzip as i32, 2);
    }

    #[test]
    fn gzbufsize_is_8192() {
        assert_eq!(GZBUFSIZE, 8192);
    }

    #[test]
    fn new_sets_open_defaults() {
        let (file, path_str, path) = temp_file("defaults");
        let st = GzState::new(file, path_str.clone(), Mode::Write);

        // exposed contents
        assert_eq!(st.have, 0);
        assert_eq!(st.next, 0);
        assert_eq!(st.pos, 0);
        // both reading and writing
        assert_eq!(st.mode, Mode::Write);
        assert!(st.file.is_some());
        assert_eq!(st.path, path_str);
        assert_eq!(st.size, 0);
        assert_eq!(st.want, GZBUFSIZE);
        assert!(st.in_buf.is_empty());
        assert!(st.out_buf.is_empty());
        assert_eq!(st.direct, 0);
        // reading
        assert_eq!(st.junk, -1);
        assert_eq!(st.how, How::Look);
        assert!(!st.again);
        assert_eq!(st.start, 0);
        assert!(!st.eof);
        assert!(!st.past);
        // writing
        assert_eq!(st.level, Z_DEFAULT_COMPRESSION);
        assert_eq!(st.strategy, Z_DEFAULT_STRATEGY);
        assert!(!st.reset);
        // seek request
        assert_eq!(st.skip, 0);
        // error information
        assert_eq!(st.err, Z_OK);
        assert_eq!(st.msg, None);
        // engine + input cursor bookkeeping
        assert!(!st.strm.is_initialized());
        assert_eq!(st.in_next, 0);
        assert_eq!(st.in_avail, 0);
        // finalization guard
        assert!(!st.finalized);

        // mode helpers
        assert!(st.is_writing());
        assert!(!st.is_reading());

        drop(st);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn new_read_mode_reports_reading() {
        let (file, path_str, path) = temp_file("read");
        let st = GzState::new(file, path_str, Mode::Read);
        assert!(st.is_reading());
        assert!(!st.is_writing());
        drop(st);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn gz_error_fatal_zeroes_have_and_formats_message() {
        let (file, path_str, path) = temp_file("fatal");
        let mut st = GzState::new(file, path_str.clone(), Mode::Read);

        // Pretend some output was buffered; a fatal error must zero it so the
        // gzgetc() fast path fails.
        st.have = 5;
        st.again = false;
        st.gz_error(Z_DATA_ERROR, Some("invalid block type"));

        assert_eq!(st.err, Z_DATA_ERROR);
        assert_eq!(st.have, 0);
        assert_eq!(st.msg, Some(format!("{path_str}: invalid block type")));

        drop(st);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn gz_error_buf_error_keeps_have() {
        let (file, path_str, path) = temp_file("buferr");
        let mut st = GzState::new(file, path_str, Mode::Read);

        st.have = 7;
        // Z_BUF_ERROR is non-fatal: `have` must be left untouched, and a None
        // message clears any previous text.
        st.gz_error(Z_BUF_ERROR, None);

        assert_eq!(st.err, Z_BUF_ERROR);
        assert_eq!(st.have, 7);
        assert_eq!(st.msg, None);

        drop(st);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn gz_error_again_is_not_fatal() {
        let (file, path_str, path) = temp_file("again");
        let mut st = GzState::new(file, path_str.clone(), Mode::Read);

        st.have = 9;
        // An error code that would normally be fatal is treated as non-fatal
        // while `again` (EAGAIN/EWOULDBLOCK) is set, so `have` is preserved.
        st.again = true;
        st.gz_error(Z_ERRNO, Some("would block"));

        assert_eq!(st.err, Z_ERRNO);
        assert_eq!(st.have, 9);
        assert_eq!(st.msg, Some(format!("{path_str}: would block")));

        drop(st);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn gz_error_mem_error_leaves_message_none() {
        let (file, path_str, path) = temp_file("memerr");
        let mut st = GzState::new(file, path_str, Mode::Read);

        // Z_MEM_ERROR must NOT allocate a message; gzerror() reports the literal
        // "out of memory" for this code, so `msg` stays None even though a text
        // was supplied.
        st.gz_error(Z_MEM_ERROR, Some("this text is ignored"));

        assert_eq!(st.err, Z_MEM_ERROR);
        assert_eq!(st.msg, None);

        drop(st);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn clear_error_resets_code_and_message() {
        let (file, path_str, path) = temp_file("clear");
        let mut st = GzState::new(file, path_str, Mode::Read);

        st.gz_error(Z_DATA_ERROR, Some("boom"));
        assert_eq!(st.err, Z_DATA_ERROR);
        assert!(st.msg.is_some());

        st.clear_error();
        assert_eq!(st.err, Z_OK);
        assert_eq!(st.msg, None);

        drop(st);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn out_slice_returns_available_window() {
        let (file, path_str, path) = temp_file("outslice");
        let mut st = GzState::new(file, path_str, Mode::Read);

        st.out_buf = vec![10, 20, 30, 40, 50];
        st.next = 1;
        st.have = 3;
        assert_eq!(st.out_slice(), &[20u8, 30, 40][..]);

        // With nothing available, the window is empty.
        st.have = 0;
        assert!(st.out_slice().is_empty());

        drop(st);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn gz_intmax_is_i32_max() {
        assert_eq!(GzState::gz_intmax(), i32::MAX as u32);
        assert_eq!(GzState::gz_intmax(), 2_147_483_647);
    }

    #[test]
    fn finalize_is_idempotent_no_double_finalize() {
        let (file, path_str, path) = temp_file("finalize");
        let mut st = GzState::new(file, path_str, Mode::Write);

        assert!(!st.finalized);
        // First finalize does the work...
        assert!(st.finalize());
        assert!(st.finalized);
        // ...and every later call is a no-op (this boolean acts as the
        // "counter" that proves finalization runs at most once).
        assert!(!st.finalize());
        assert!(!st.finalize());
        assert!(st.finalized);

        // Dropping an already-finalized handle must be a no-op and must not
        // panic.
        drop(st);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn drop_without_explicit_close_is_safe() {
        let (file, path_str, path) = temp_file("dropsafe");
        // A handle dropped without an explicit close() must tear down cleanly
        // (no leak, no panic). The owned File/Vec/ZStream release their
        // resources via their own Drop impls.
        {
            let st = GzState::new(file, path_str, Mode::Write);
            assert!(!st.finalized);
        } // <- implicit Drop here

        let _ = std::fs::remove_file(&path);
    }
}
