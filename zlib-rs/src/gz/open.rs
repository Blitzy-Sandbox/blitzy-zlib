//! gzip file open / buffer / seek / position / error API.
//!
//! This module is the idiomatic-Rust port of **`gzlib.c`** — the functions
//! common to reading and writing gzip files. It hosts the shared constructor
//! [`gz_open`] (plus the `gz_reset` bookkeeping, which lives on
//! [`GzState::reset`]), the public openers [`gzopen`] / [`gzopen64`] /
//! [`gzdopen`], the buffer-sizing [`gzbuffer`], the seek / position family
//! ([`gzrewind`], [`gzseek64`] / [`gzseek`], [`gztell64`] / [`gztell`],
//! [`gzoffset64`] / [`gzoffset`]), and the error API ([`gzerror`] /
//! [`gzclearerr`]). It additionally hosts [`gzsetparams`], which is C-defined in
//! `gzwrite.c` but is grouped here by the module layout.
//!
//! # Availability
//!
//! Compiled only under the `gz-io` Cargo feature (which implies `std` +
//! `gzip`). The feature gate is applied at the `pub mod gz;` declaration site in
//! `gz/mod.rs`, so no inner `#![cfg(...)]` attribute is needed here.
//!
//! # Safety and ownership model
//!
//! The whole crate carries `#![forbid(unsafe_code)]`, so this module contains
//! **zero `unsafe`**. The C representation is re-expressed with ownership:
//!
//! * the C `int fd` and the `open()` / `lseek()` system calls become an owned
//!   [`File`] opened through [`OpenOptions`] and repositioned
//!   through [`Seek`] / [`SeekFrom`];
//! * the opaque `gzFile` pointer (a `gz_state *` in C) becomes an owned
//!   `Box<GzState>` — every public constructor here returns
//!   `Option<Box<GzState>>` (the C `NULL`-on-failure contract maps to
//!   [`None`]), **never** a raw pointer. The raw-pointer `gzFile` ABI is
//!   reconstructed exclusively in the `libz-rs-sys` FFI shim.
//!
//! Because [`GzState`] is a `pub(crate)` type, the entry points here are
//! `pub(crate)` as well: the public `GzFile` handle and the `gz*` C symbols are
//! re-exposed by `gz/mod.rs` and the FFI shim, respectively.

use crate::constants::{Flush, Strategy, Z_DEFAULT_COMPRESSION};
use crate::error::{ReturnCode, ZlibError};
use crate::gz::state::{GZBUFSIZE, GzHow, GzMode, GzState};

use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom};
use std::path::Path;

// ---------------------------------------------------------------------------
// Raw zlib status integers
// ---------------------------------------------------------------------------
//
// The gz error slot (`GzState::err`) and the integer-returning `gz*` functions
// speak in raw zlib `Z_*` status codes. zlib's public header exposes these as
// `#define`s; the safe core models them as the `ReturnCode` / `ZlibError` enums,
// so the exact integers are recovered here from those authoritative `const fn`
// accessors — guaranteeing no value drift from the C ABI.

/// `Z_OK` (`0`) — success / no error.
const Z_OK: i32 = ReturnCode::Ok.as_i32();
/// `Z_STREAM_ERROR` (`-2`) — inconsistent stream state or invalid parameters.
const Z_STREAM_ERROR: i32 = ZlibError::StreamError.as_i32();
/// `Z_MEM_ERROR` (`-4`) — out of memory (reported by [`gzerror`] as a literal).
const Z_MEM_ERROR: i32 = ZlibError::MemError.as_i32();
/// `Z_BUF_ERROR` (`-5`) — soft / retryable condition; tolerated by the seek and
/// rewind guards exactly as in C.
const Z_BUF_ERROR: i32 = ZlibError::BufError.as_i32();

// ---------------------------------------------------------------------------
// Seek origins (`<stdio.h>` `SEEK_SET` / `SEEK_CUR`)
// ---------------------------------------------------------------------------
//
// `gzseek` / `gzseek64` accept the C stdio `whence` integers directly (the FFI
// shim forwards the caller's `int`). Only `SEEK_SET` and `SEEK_CUR` are valid;
// `SEEK_END` is intentionally unsupported, matching C zlib. The literals `0`
// and `1` are fixed by the C standard, so they are defined here without pulling
// in `libc`.

/// `SEEK_SET` (`0`) — seek relative to the start of the stream.
const SEEK_SET: i32 = 0;
/// `SEEK_CUR` (`1`) — seek relative to the current position.
const SEEK_CUR: i32 = 1;

// ---------------------------------------------------------------------------
// gz_open
// ---------------------------------------------------------------------------

/// Where [`gz_open`] obtains its underlying [`File`].
///
/// This distinguishes the two C `gz_open` entry styles without ever exposing a
/// raw file descriptor to the safe core:
///
/// * [`FileSource::Path`] — open the named filesystem path (the `gzopen` case,
///   C `fd == -1`).
/// * [`FileSource::Owned`] — adopt an already-owned [`File`] (the `gzdopen`
///   case). In C this is a raw `int fd`; the `unsafe` `File::from_raw_fd`
///   adoption is performed in the `libz-rs-sys` shim, so this safe-core variant
///   receives the resulting owned handle.
enum FileSource<'a> {
    /// Open the file at this filesystem path.
    Path(&'a Path),
    /// Use this already-owned, already-opened file (the `gzdopen` case).
    Owned(File),
}

/// Open a gzip file either by path or from an already-owned [`File`] — the
/// shared constructor behind every `gz*` opener (C `gz_open`, `gzlib.c`
/// lines 87-285).
///
/// `path_label` is the human-readable identifier used in error messages: the
/// real path for [`FileSource::Path`], or the `"<fd:N>"` pseudo-path for
/// [`FileSource::Owned`]. `mode` is the zlib mode string (`"rb"`, `"wb9"`,
/// `"ab+"`, …) parsed character-by-character exactly as in C.
///
/// Returns [`None`] (the C `NULL`) for any rejected mode (`'+'`, a transparent
/// read `'T'` forced via the read path, a `'G'` requested while writing, or no
/// `r`/`w`/`a`) or if the file cannot be opened.
fn gz_open(path_label: String, source: FileSource<'_>, mode: &str) -> Option<Box<GzState>> {
    // -- mode defaults, mirroring the field initialization in C `gz_open` --
    let mut mode_kind = GzMode::None;
    let mut level: i32 = Z_DEFAULT_COMPRESSION;
    let mut strategy: i32 = Strategy::Default.as_i32();
    // `direct` tri-state: 0 default, 1 if "T" (transparent), -1 if "G" (gzip
    // only). The last "G" or "T" wins, matching the C switch.
    let mut direct: i32 = 0;
    // "x" -> open exclusively (O_EXCL); maps to `OpenOptions::create_new`.
    let mut exclusive = false;

    // -- interpret the mode string, one character at a time (C lines 113-171) --
    for ch in mode.chars() {
        match ch {
            // Digits set the compression level (last digit wins).
            '0'..='9' => level = (ch as i32) - ('0' as i32),
            'r' => mode_kind = GzMode::Read,
            'w' => mode_kind = GzMode::Write,
            'a' => mode_kind = GzMode::Append,
            // Can't read and write at the same time.
            '+' => return None,
            // Binary is always implied; the flag is accepted and ignored.
            'b' => {}
            // 'e' (O_CLOEXEC) and 'N' (O_NONBLOCK) are recognized for mode-string
            // compatibility but treated as best-effort no-ops in the pure-Rust
            // core: `std` exposes no portable constant for these open flags
            // without `libc` (which is not a `zlib-rs` dependency) or `unsafe`
            // (which is forbidden here). Neither flag affects compression
            // correctness or the gzip wire format. A close-on-exec / non-blocking
            // descriptor, when required, is configured by the caller or the FFI
            // shim before the `File` reaches this safe core.
            'e' | 'N' => {}
            // Exclusive create (only meaningful when writing).
            'x' => exclusive = true,
            'f' => strategy = Strategy::Filtered.as_i32(),
            'h' => strategy = Strategy::HuffmanOnly.as_i32(),
            'R' => strategy = Strategy::Rle.as_i32(),
            'F' => strategy = Strategy::Fixed.as_i32(),
            // Force gzip / disable transparent read.
            'G' => direct = -1,
            // Request transparent (uncompressed) passthrough.
            'T' => direct = 1,
            // Any other character could be treated as an error, but C just
            // ignores it; we match that leniency.
            _ => {}
        }
    }

    // -- must provide an "r", "w", or "a" (C lines 173-177) --
    if mode_kind == GzMode::None {
        return None;
    }

    // -- finalize `direct`: 0, 1 if "T", or -1 if "G" (C lines 179-197) --
    if mode_kind == GzMode::Read {
        if direct == 1 {
            // Can't force a transparent read.
            return None;
        }
        if direct == 0 {
            // Default when reading is auto-detect of gzip vs. transparent --
            // start with a transparent assumption in case of an empty file.
            direct = 1;
        }
    } else if direct == -1 {
        // "G" has no meaning when writing -- disallow it.
        return None;
    }
    // If reading, `direct == 1` for auto-detect, `-1` for gzip only; if writing
    // or appending, `direct == 0` for gzip, `1` for transparent (copy in to out).

    // -- obtain the underlying file (C lines 228-268) --
    let mut file = match source {
        FileSource::Path(path) => {
            let mut options = OpenOptions::new();
            match mode_kind {
                GzMode::Read => {
                    options.read(true);
                }
                GzMode::Write => {
                    options.write(true);
                    if exclusive {
                        // O_WRONLY | O_CREAT | O_EXCL: fail if the file exists.
                        options.create_new(true);
                    } else {
                        // O_WRONLY | O_CREAT | O_TRUNC.
                        options.create(true).truncate(true);
                    }
                }
                GzMode::Append => {
                    // O_WRONLY | O_CREAT | O_APPEND.
                    options.write(true).create(true).append(true);
                }
                // Unreachable: `GzMode::None` was rejected above.
                GzMode::None => return None,
            }
            // On any open failure, return None (the C frees `state` and returns
            // NULL; here the partially built locals are simply dropped).
            options.open(path).ok()?
        }
        // The dopen case: adopt the already-owned handle as-is.
        FileSource::Owned(owned) => owned,
    };

    // -- append special-case (C lines 269-272): position at end so `gzoffset`
    //    is correct, then treat the stream as a plain write to simplify later
    //    checks. --
    if mode_kind == GzMode::Append {
        // A failure here is non-fatal in C (the lseek return is ignored); we
        // mirror that by ignoring a seek error.
        let _ = file.seek(SeekFrom::End(0));
        mode_kind = GzMode::Write;
    }

    // -- save the current position for rewinding, only when reading
    //    (C lines 274-278). On error, default to 0 exactly as C does. --
    let start: i64 = if mode_kind == GzMode::Read {
        // `stream_position()` is the idiomatic spelling of the C
        // `LSEEK(state->fd, 0, SEEK_CUR)` used here to record the read start.
        file.stream_position().map(|p| p as i64).unwrap_or(0)
    } else {
        0
    };

    // -- build the state (`GzState::new` applies the field defaults and runs the
    //    per-mode `gz_reset`), then overwrite the parsed/seeked values. Because
    //    `gz_reset` does not touch `level` / `strategy` / `direct` / `start` /
    //    `want`, assigning them after construction is equivalent to the C order
    //    (which sets them before `gz_reset`). --
    let mut state = GzState::new(file, path_label, mode_kind);
    state.level = level;
    state.strategy = strategy;
    state.direct = direct;
    // Requested buffer size; settable via `gzbuffer` before the first I/O.
    // `GzState::new` already initializes this to `GZBUFSIZE`; restate it for
    // parity with the C `state->want = GZBUFSIZE` initialization.
    state.want = GZBUFSIZE;
    state.start = start;

    Some(Box::new(state))
}

// ---------------------------------------------------------------------------
// Public openers
// ---------------------------------------------------------------------------

/// Open the gzip file at `path` with the given zlib `mode` string
/// (C `gzopen`, `gzlib.c` lines 287-290).
///
/// Returns an owned [`GzState`] on success, or [`None`] (the C `NULL`) if the
/// mode string is invalid or the file cannot be opened. See [`gz_open`] for the
/// full mode-string grammar.
pub(crate) fn gzopen(path: &Path, mode: &str) -> Option<Box<GzState>> {
    let label = path.to_string_lossy().into_owned();
    gz_open(label, FileSource::Path(path), mode)
}

/// 64-bit-offset alias of [`gzopen`] (C `gzopen64`, `gzlib.c` lines 292-295).
///
/// In C the `*64` openers exist only because of large-file `off_t` typedef
/// differences. This port uses `i64` offsets uniformly, so `gzopen64` is a
/// true alias of [`gzopen`]; both symbols are provided so the FFI shim can
/// export each one. The shared [`gz_open`] body is invoked identically.
pub(crate) fn gzopen64(path: &Path, mode: &str) -> Option<Box<GzState>> {
    let label = path.to_string_lossy().into_owned();
    gz_open(label, FileSource::Path(path), mode)
}

/// Open a gzip stream over an already-owned [`File`] (C `gzdopen`, `gzlib.c`
/// lines 297-312).
///
/// The C `gzdopen(int fd, mode)` adopts a raw file descriptor and builds a
/// `"<fd:%d>"` identifier for error messages. Adopting a raw fd requires the
/// `unsafe` `File::from_raw_fd`, which **must not** appear in this
/// `#![forbid(unsafe_code)]` core; that adoption is therefore performed in the
/// `libz-rs-sys` FFI shim. This safe-core entry point receives the resulting
/// owned [`File`] plus `fd_label` — the integer used solely to reconstruct the
/// `"<fd:N>"` error identifier.
pub(crate) fn gzdopen(file: File, fd_label: i32, mode: &str) -> Option<Box<GzState>> {
    let label = format!("<fd:{fd_label}>");
    gz_open(label, FileSource::Owned(file), mode)
}

// ---------------------------------------------------------------------------
// gzbuffer
// ---------------------------------------------------------------------------

/// Set the internal buffer size used for subsequent reads or writes
/// (C `gzbuffer`, `gzlib.c` lines 321-343).
///
/// Must be called before any data is read or written. Returns `0` on success
/// and `-1` on failure: when the stream is neither reading nor writing, when
/// the buffers have already been allocated (`size != 0`), or when `size` is so
/// large it could not be doubled without overflow. A `size` below `8` is
/// clamped up to `8` so the flushing logic behaves correctly. Every guard is
/// preserved exactly as in C.
pub(crate) fn gzbuffer(state: &mut GzState, mut size: u32) -> i32 {
    // Check that the stream is open for reading or writing.
    if state.mode != GzMode::Read && state.mode != GzMode::Write {
        return -1;
    }

    // Make sure we haven't already allocated memory.
    if state.size != 0 {
        return -1;
    }

    // Need to be able to double it. `size << 1` is a bit shift (never panics for
    // a shift-by-one) that drops the top bit, exactly like the C unsigned shift;
    // if doubling wraps to a smaller value the request is rejected.
    if (size << 1) < size {
        return -1;
    }

    // A minimum of 8 is needed to behave well with flushing.
    if size < 8 {
        size = 8;
    }

    state.want = size as usize;
    0
}

// ---------------------------------------------------------------------------
// gzrewind
// ---------------------------------------------------------------------------

/// Rewind a read stream back to the start of the gzip data (C `gzrewind`,
/// `gzlib.c` lines 345-364).
///
/// Returns `0` on success and `-1` on failure. The stream must be open for
/// reading and free of any serious error (`Z_OK` or the soft `Z_BUF_ERROR`
/// only). On success the file is repositioned to the saved start offset and the
/// per-operation bookkeeping is reset via [`GzState::reset`] (the C
/// `gz_reset`).
pub(crate) fn gzrewind(state: &mut GzState) -> i32 {
    // Check that we're reading and that there's no (serious) error.
    if state.mode != GzMode::Read || (state.err != Z_OK && state.err != Z_BUF_ERROR) {
        return -1;
    }

    // Back up to the start of the gzip data and start over.
    if state
        .file
        .seek(SeekFrom::Start(state.start as u64))
        .is_err()
    {
        return -1;
    }
    state.reset();
    0
}

// ---------------------------------------------------------------------------
// gzseek64 / gzseek
// ---------------------------------------------------------------------------

/// Set the stream position for the next read or write (C `gzseek64`, `gzlib.c`
/// lines 366-435).
///
/// `whence` is the C stdio origin: only [`SEEK_SET`] and [`SEEK_CUR`] are
/// supported (no `SEEK_END`). Returns the resulting uncompressed offset, or
/// `-1` on error.
///
/// The algorithm reproduces C exactly:
///
/// 1. validate the mode, error slot, and `whence`;
/// 2. normalize the request to a `SEEK_CUR`-relative `offset`;
/// 3. **raw-area fast path** — when reading a transparently-copied
///    ([`GzHow::Copy`]) stream and the target is at or after the current
///    position, seek the file directly;
/// 4. **back-seek** — a negative net offset (reading only) rewinds and converts
///    the request to an absolute forward skip;
/// 5. consume any already-buffered output toward the offset;
/// 6. record the residual as a deferred `skip`, satisfied later by the read /
///    write layers.
pub(crate) fn gzseek64(state: &mut GzState, mut offset: i64, whence: i32) -> i64 {
    // Check the stream mode.
    if state.mode != GzMode::Read && state.mode != GzMode::Write {
        return -1;
    }

    // Check that there's no (serious) error.
    if state.err != Z_OK && state.err != Z_BUF_ERROR {
        return -1;
    }

    // Can only seek from the start or relative to the current position.
    if whence != SEEK_SET && whence != SEEK_CUR {
        return -1;
    }

    // Normalize offset to a SEEK_CUR specification.
    if whence == SEEK_SET {
        offset -= state.pos;
    } else {
        // Fold any pending (not-yet-applied) skip into the request, unless a
        // read has already gone past the end of the data.
        offset += if state.past { 0 } else { state.skip };
        state.skip = 0;
    }

    // If within the raw area while reading, just go there.
    if state.mode == GzMode::Read && state.how == GzHow::Copy && state.pos + offset >= 0 {
        // Seek past the bytes still buffered in `out` so the file lands at the
        // requested logical position.
        if state
            .file
            .seek(SeekFrom::Current(offset - state.have as i64))
            .is_err()
        {
            return -1;
        }
        state.have = 0;
        state.eof = false;
        state.past = false;
        state.skip = 0;
        state.set_error(Z_OK, None);
        // The C `state->strm.avail_in = 0` has no counterpart here: the embedded
        // `ZStream` retains no input between calls (per-call input is a borrowed
        // slice owned by the read layer), so there is nothing to clear.
        state.pos += offset;
        return state.pos;
    }

    // Calculate the skip amount, rewinding if needed for a back seek when
    // reading.
    if offset < 0 {
        if state.mode != GzMode::Read {
            // Writing -- can't go backwards.
            return -1;
        }
        offset += state.pos;
        if offset < 0 {
            // Before the start of the file.
            return -1;
        }
        if gzrewind(state) == -1 {
            return -1;
        }
        // After rewinding, `state.pos` is 0 and `offset` is the absolute target.
    }

    // If reading, skip what's already in the output buffer (one fewer `gzgetc`
    // check later). `offset` is non-negative here, so the C `GT_OFF` /
    // unsigned-vs-signed guard collapses to a plain minimum.
    if state.mode == GzMode::Read {
        let n = core::cmp::min(state.have as i64, offset);
        state.have -= n as usize;
        state.next += n as usize;
        state.pos += n;
        offset -= n;
    }

    // Request the residual skip (if any), to be satisfied by the read layer's
    // `gz_skip` or the write layer's `gz_zero`.
    state.skip = offset;
    state.pos + offset
}

/// Set the stream position using the platform `z_off_t` width (C `gzseek`,
/// `gzlib.c` lines 437-443).
///
/// Delegates to [`gzseek64`]. In C this clamps the 64-bit result back to the
/// (possibly narrower) `z_off_t`, returning `-1` if it does not fit. That
/// width-clamp depends on the C `z_off_t` typedef, which is reconstructed in the
/// FFI shim; the safe core therefore returns the full `i64` result and lets the
/// shim perform the final `ret == (z_off_t)ret ? ret : -1` narrowing. Both
/// functions are provided so the shim can export each symbol.
pub(crate) fn gzseek(state: &mut GzState, offset: i64, whence: i32) -> i64 {
    gzseek64(state, offset, whence)
}

// ---------------------------------------------------------------------------
// gztell64 / gztell, gzoffset64 / gzoffset
// ---------------------------------------------------------------------------

/// Return the current uncompressed position in the stream (C `gztell64`,
/// `gzlib.c` lines 445-458).
///
/// Returns `-1` if the stream is neither reading nor writing. Any pending
/// (not-yet-applied) forward `skip` is included so the reported position
/// reflects where the caller has logically seeked to, unless a read has already
/// gone past the end of the data.
pub(crate) fn gztell64(state: &GzState) -> i64 {
    if state.mode != GzMode::Read && state.mode != GzMode::Write {
        return -1;
    }
    state.pos + if state.past { 0 } else { state.skip }
}

/// Platform `z_off_t`-width form of [`gztell64`] (C `gztell`, `gzlib.c`
/// lines 460-466).
///
/// Delegates to [`gztell64`]; the FFI shim performs the final `z_off_t`
/// width-clamp, as described on [`gzseek`].
pub(crate) fn gztell(state: &GzState) -> i64 {
    gztell64(state)
}

/// Return the current compressed (on-disk) offset of the stream (C
/// `gzoffset64`, `gzlib.c` lines 468-487).
///
/// Returns `-1` if the stream is neither reading nor writing or if the file
/// position cannot be queried.
///
/// In C, when reading, the count of bytes buffered-but-not-yet-consumed by the
/// decompressor (`state->strm.avail_in`) is subtracted so the result reflects
/// the logical compressed position. This port tracks that same count in
/// [`GzState::in_have`] — the safe-core [`ZStream`] retains no input between
/// calls, so the gz layer owns the pending-compressed-input cursor — and
/// subtracts it in read mode exactly as C does. (Until the read layer is wired
/// in a later checkpoint, [`in_have`](GzState::in_have) stays `0` in read mode,
/// so the subtraction is currently a no-op; it becomes load-bearing once reads
/// buffer compressed input.)
pub(crate) fn gzoffset64(state: &mut GzState) -> i64 {
    if state.mode != GzMode::Read && state.mode != GzMode::Write {
        return -1;
    }

    // Compute the effective offset in the file (the current OS position).
    // `stream_position()` is the idiomatic spelling of C's
    // `LSEEK(state->fd, 0, SEEK_CUR)`.
    let mut offset = match state.file.stream_position() {
        Ok(offset) => offset as i64,
        Err(_) => return -1,
    };

    // When reading, discount the compressed input read from the file into the
    // gz buffer but not yet consumed by the decompressor, so the result is the
    // logical compressed position. C: `if (state->mode == GZ_READ) offset -=
    // state->strm.avail_in;`.
    if state.mode == GzMode::Read {
        offset -= state.in_have as i64;
    }
    offset
}

/// Platform `z_off_t`-width form of [`gzoffset64`] (C `gzoffset`, `gzlib.c`
/// lines 489-495).
///
/// Delegates to [`gzoffset64`]; the FFI shim performs the final `z_off_t`
/// width-clamp, as described on [`gzseek`].
pub(crate) fn gzoffset(state: &mut GzState) -> i64 {
    gzoffset64(state)
}

// NOTE: `gzeof` (C `gzlib.c` lines 497-510) is intentionally **not** implemented
// here. By the module layout it belongs to `gz/read.rs`, which owns the read
// state (`past`) that `gzeof` reports. See that module for the implementation.

// ---------------------------------------------------------------------------
// gzerror / gzclearerr
// ---------------------------------------------------------------------------

/// Return the stream's last error code and message (C `gzerror`, `gzlib.c`
/// lines 512-528).
///
/// Writes the raw zlib error code into `errnum` and returns the associated
/// message, mirroring C exactly:
///
/// * if the stream is neither reading nor writing, returns [`None`] (the C
///   `NULL`) and leaves `errnum` unchanged;
/// * for `Z_MEM_ERROR` the literal `"out of memory"` is returned (no allocation
///   is performed on the out-of-memory path);
/// * otherwise the stored `"path: msg"` message is returned, or the empty
///   string `""` when there is no message.
///
/// The returned `&str` borrows from `state`; the FFI shim adapts it to the C
/// `const char *gzerror(gzFile, int *)` signature.
pub(crate) fn gzerror<'a>(state: &'a GzState, errnum: &mut i32) -> Option<&'a str> {
    // Check that the stream is open for reading or writing.
    if state.mode != GzMode::Read && state.mode != GzMode::Write {
        return None;
    }

    // Return the error code.
    *errnum = state.err;

    // Return the error information.
    if state.err == Z_MEM_ERROR {
        Some("out of memory")
    } else {
        // The empty string when there is no stored message, matching C.
        Some(state.msg.as_deref().unwrap_or(""))
    }
}

/// Clear the stream's error and end-of-file indicators (C `gzclearerr`,
/// `gzlib.c` lines 530-547).
///
/// A no-op when the stream is neither reading nor writing. For a read stream
/// the EOF / past-end flags are also cleared. The error slot is reset via
/// [`GzState::set_error`] (the C `gz_error(state, Z_OK, NULL)`).
pub(crate) fn gzclearerr(state: &mut GzState) {
    // Check that the stream is open for reading or writing.
    if state.mode != GzMode::Read && state.mode != GzMode::Write {
        return;
    }

    // Clear the end-of-file state when reading.
    if state.mode == GzMode::Read {
        state.eof = false;
        state.past = false;
    }

    // Clear the error.
    state.set_error(Z_OK, None);
}

// ---------------------------------------------------------------------------
// gzsetparams
// ---------------------------------------------------------------------------

/// Change the compression level and strategy for subsequent writes (C
/// `gzsetparams`, `gzwrite.c` lines 612-664).
///
/// Defined in `gzwrite.c` in C but grouped here by the module layout. Returns
/// `Z_OK` on success, or `Z_STREAM_ERROR` if the stream is not a (non-direct)
/// write stream or carries a serious error. A request that matches the current
/// parameters is a no-op that returns `Z_OK`.
///
/// # Cross-module contract
///
/// This function orchestrates two `pub(crate)` helpers owned by the write layer
/// (`gz/write.rs`) plus the deflate engine entry point, all part of the same
/// `zlib-rs` crate:
///
/// * `GzState::gz_zero(&mut self) -> i32` — satisfies a pending forward `skip`
///   by compressing zero bytes; returns `-1` on error (the C `gz_zero`).
/// * `GzState::gz_comp(&mut self, flush: Flush) -> i32` — drives the
///   compressor; returns `-1` on error (the C `gz_comp`). It is contracted to
///   succeed as a no-op when there is no pending input, which lets this code
///   call it unconditionally: the safe-core [`ZStream`] does not expose the C
///   `strm->avail_in` pending-input count that C tests before the call.
/// * [`crate::deflate::deflate_params`] — the deflate engine's parameter-change
///   routine. Its return code is intentionally ignored, exactly as C ignores
///   the `deflateParams` return here; any bytes it emits while flushing the
///   current block under the *old* parameters are appended to the output buffer
///   (advancing [`GzState::out_pos`]) for the next `gz_comp` to drain.
pub(crate) fn gzsetparams(state: &mut GzState, level: i32, strategy: i32) -> i32 {
    // Check that we're compressing and that there's no (serious) error. A
    // non-zero `direct` means transparent passthrough, for which deflate
    // parameters are meaningless.
    if state.mode != GzMode::Write || (state.err != Z_OK && !state.again) || state.direct != 0 {
        return Z_STREAM_ERROR;
    }
    state.set_error(Z_OK, None);

    // If no change is requested, then do nothing.
    if level == state.level && strategy == state.strategy {
        return Z_OK;
    }

    // Check for a pending seek request and satisfy it first.
    if state.skip != 0 && state.gz_zero() == -1 {
        return state.err;
    }

    // Change compression parameters for subsequent input.
    if state.size != 0 {
        // Flush previous input with the previous parameters before changing.
        // C guards this with `strm->avail_in`; here `gz_comp` no-ops when there
        // is nothing pending (see the cross-module contract above), so it is
        // safe to call unconditionally and treat `-1` as a real error.
        if state.gz_comp(Flush::Block) == -1 {
            return state.err;
        }
        // Apply the new parameters to the engine. As in C, the return code is
        // not inspected. The safe-core `deflate_params` takes explicit
        // input/output slices (the streaming core retains no buffers between
        // calls): input is empty (only parameters change, no new data), and any
        // bytes emitted while flushing the current block under the old
        // parameters are written into the free tail of the output buffer and
        // accounted for by advancing `out_pos` so the next `gz_comp` drains
        // them. The preceding `gz_comp(Z_BLOCK)` already flushed to a block
        // boundary, so this residual is small and always fits.
        let out_start = state.out_pos;
        let (_ret, _consumed, produced) = crate::deflate::deflate_params(
            &mut state.strm,
            level,
            Strategy::try_from_i32(strategy).unwrap_or(Strategy::Default),
            &[],
            &mut state.out_buf[out_start..state.size],
        );
        state.out_pos += produced;
    }

    state.level = level;
    state.strategy = strategy;
    Z_OK
}

// ---------------------------------------------------------------------------
// Internal-helper substitutions (C `gz_error` / `gz_intmax`)
// ---------------------------------------------------------------------------
//
// * The C `gz_error` (`gzlib.c` lines 549-590) is implemented as
//   [`GzState::set_error`] in `gz/state.rs`; every error-slot update in this
//   module calls `state.set_error(...)` rather than reimplementing it.
//
// * The C `gz_intmax` (`gzlib.c` lines 592-609) merely returns `INT_MAX`
//   portably. Its only use is the `GT_OFF` guard inside `gzseek64`, which
//   compares an unsigned buffer count against a signed `z_off64_t`. This port
//   uses `i64` offsets and a `usize` count, so that comparison collapses to a
//   plain [`core::cmp::min`] (see [`gzseek64`]) and no `gz_intmax` /
//   `i32::MAX` constant is required.

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// Build a unique temporary path so parallel test threads never collide.
    /// The file (if created) is removed by [`TempPath`]'s `Drop`.
    fn unique_temp_path(tag: &str) -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!("zlibrs_gz_open_test_{tag}_{pid}_{n}_{nanos}.gz"))
    }

    /// RAII wrapper that deletes its backing file on drop, keeping the test
    /// temp directory clean even when a test fails.
    struct TempPath(PathBuf);

    impl TempPath {
        fn new(tag: &str) -> Self {
            Self(unique_temp_path(tag))
        }
        fn path(&self) -> &Path {
            &self.0
        }
        /// Create the file on disk (empty) so a read-mode open can succeed.
        fn create_empty(&self) {
            fs::write(&self.0, b"").expect("create temp file");
        }
    }

    impl Drop for TempPath {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }

    #[test]
    fn open_write_parses_level_strategy_and_direct() {
        let tmp = TempPath::new("wb9");
        let state = gzopen(tmp.path(), "wb9").expect("open for write");
        assert_eq!(state.mode, GzMode::Write);
        assert_eq!(state.level, 9);
        // No strategy character -> default strategy.
        assert_eq!(state.strategy, Strategy::Default.as_i32());
        // Writing without "T" -> gzip compression (direct == 0).
        assert_eq!(state.direct, 0);
        // `want` defaults to GZBUFSIZE; buffers are not yet allocated.
        assert_eq!(state.want, GZBUFSIZE);
        assert_eq!(state.size, 0);
    }

    #[test]
    fn open_write_parses_each_strategy_char() {
        for (mode, expected) in [
            ("wbf", Strategy::Filtered),
            ("wbh", Strategy::HuffmanOnly),
            ("wbR", Strategy::Rle),
            ("wbF", Strategy::Fixed),
        ] {
            let tmp = TempPath::new("strat");
            let state = gzopen(tmp.path(), mode).expect("open for write");
            assert_eq!(
                state.strategy,
                expected.as_i32(),
                "strategy mismatch for mode {mode:?}"
            );
        }
    }

    #[test]
    fn open_write_transparent_with_t() {
        let tmp = TempPath::new("wT");
        let state = gzopen(tmp.path(), "wT").expect("open for transparent write");
        assert_eq!(state.mode, GzMode::Write);
        // "T" while writing requests transparent passthrough.
        assert_eq!(state.direct, 1);
    }

    #[test]
    fn open_read_defaults_to_autodetect_direct() {
        let tmp = TempPath::new("r");
        tmp.create_empty();
        let state = gzopen(tmp.path(), "r").expect("open for read");
        assert_eq!(state.mode, GzMode::Read);
        // Reading defaults to auto-detect: direct finalized from 0 to 1.
        assert_eq!(state.direct, 1);
        // gz_reset read-mode defaults applied through GzState::new.
        assert_eq!(state.how, GzHow::Look);
        assert_eq!(state.junk, -1);
        assert_eq!(state.start, 0);
    }

    #[test]
    fn open_read_gzip_only_keeps_direct_negative() {
        let tmp = TempPath::new("rG");
        tmp.create_empty();
        let state = gzopen(tmp.path(), "rG").expect("open for gzip-only read");
        assert_eq!(state.mode, GzMode::Read);
        // "G" forces gzip-only: direct stays -1 (not finalized to 1).
        assert_eq!(state.direct, -1);
    }

    #[test]
    fn open_rejects_forced_transparent_read() {
        let tmp = TempPath::new("rT");
        tmp.create_empty();
        // "T" (transparent) cannot be forced on a read stream.
        assert!(gzopen(tmp.path(), "rT").is_none());
    }

    #[test]
    fn open_rejects_gzip_flag_when_writing() {
        let tmp = TempPath::new("wG");
        // "G" has no meaning when writing.
        assert!(gzopen(tmp.path(), "wG").is_none());
    }

    #[test]
    fn open_rejects_read_write_plus() {
        let tmp = TempPath::new("rb_plus");
        tmp.create_empty();
        // "+" (simultaneous read and write) is rejected.
        assert!(gzopen(tmp.path(), "rb+").is_none());
    }

    #[test]
    fn open_rejects_missing_rwa() {
        let tmp = TempPath::new("norwa");
        // No r/w/a character -> None, regardless of other flags/digits.
        assert!(gzopen(tmp.path(), "").is_none());
        assert!(gzopen(tmp.path(), "b9").is_none());
    }

    #[test]
    fn open_read_missing_file_returns_none() {
        let tmp = TempPath::new("missing");
        // The file does not exist on disk; opening it for read must fail.
        assert!(gzopen(tmp.path(), "r").is_none());
    }

    #[test]
    fn gzopen64_is_an_alias() {
        let tmp = TempPath::new("open64");
        let state = gzopen64(tmp.path(), "wb1").expect("open64 for write");
        assert_eq!(state.mode, GzMode::Write);
        assert_eq!(state.level, 1);
    }

    #[test]
    fn gzdopen_adopts_owned_file_and_labels_fd() {
        let tmp = TempPath::new("dopen");
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(tmp.path())
            .expect("open owned file");
        let state = gzdopen(file, 7, "wb").expect("dopen owned file");
        assert_eq!(state.mode, GzMode::Write);
        // The fd label is used only for error messages.
        assert_eq!(state.path, "<fd:7>");
    }

    #[test]
    fn gzbuffer_clamps_small_sizes_to_eight() {
        let tmp = TempPath::new("buf_clamp");
        let mut state = gzopen(tmp.path(), "wb").expect("open for write");
        // Sizes below 8 (including 0) are clamped up to 8, not rejected.
        assert_eq!(gzbuffer(&mut state, 0), 0);
        assert_eq!(state.want, 8);
    }

    #[test]
    fn gzbuffer_accepts_normal_size() {
        let tmp = TempPath::new("buf_ok");
        let mut state = gzopen(tmp.path(), "wb").expect("open for write");
        assert_eq!(gzbuffer(&mut state, 65536), 0);
        assert_eq!(state.want, 65536);
    }

    #[test]
    fn gzbuffer_rejects_after_allocation() {
        let tmp = TempPath::new("buf_alloc");
        let mut state = gzopen(tmp.path(), "wb").expect("open for write");
        // Simulate buffers already allocated.
        state.size = 100;
        assert_eq!(gzbuffer(&mut state, 4096), -1);
        // This state is deliberately inconsistent (`size` is set without any
        // matching buffer allocation) purely to exercise gzbuffer's
        // post-allocation rejection. Finalize it before it drops so the RAII
        // safety net in `GzState::Drop` — which finishes a *write* stream by
        // running `finish_write` — does not operate on this artificial state
        // (which `gz_open` would never produce).
        let _ = state.finalize();
    }

    #[test]
    fn gzbuffer_rejects_doubling_overflow() {
        let tmp = TempPath::new("buf_overflow");
        let mut state = gzopen(tmp.path(), "wb").expect("open for write");
        // A size whose top bit is set cannot be doubled without overflow.
        assert_eq!(gzbuffer(&mut state, 0x8000_0000), -1);
    }

    #[test]
    fn gzbuffer_rejects_wrong_mode() {
        let tmp = TempPath::new("buf_mode");
        let mut state = gzopen(tmp.path(), "wb").expect("open for write");
        // Finalizing sets mode to None; gzbuffer then rejects.
        let _ = state.finalize();
        assert_eq!(gzbuffer(&mut state, 4096), -1);
    }

    #[test]
    fn seek_set_forward_records_skip_and_tell_reflects_it() {
        let tmp = TempPath::new("seek_fwd");
        tmp.create_empty();
        let mut state = gzopen(tmp.path(), "r").expect("open for read");
        // Fresh read stream: how == Look (not Copy), so the request becomes a
        // deferred forward skip rather than a direct file seek.
        assert_eq!(gzseek64(&mut state, 100, SEEK_SET), 100);
        assert_eq!(state.skip, 100);
        // gztell64 includes the pending skip.
        assert_eq!(gztell64(&state), 100);
        assert_eq!(gztell(&state), 100);
    }

    #[test]
    fn seek_rejects_unsupported_whence() {
        let tmp = TempPath::new("seek_whence");
        tmp.create_empty();
        let mut state = gzopen(tmp.path(), "r").expect("open for read");
        // SEEK_END (2) is not supported.
        assert_eq!(gzseek64(&mut state, 0, 2), -1);
        assert_eq!(gzseek(&mut state, 0, 2), -1);
    }

    #[test]
    fn seek_rejects_before_start_of_file() {
        let tmp = TempPath::new("seek_neg");
        tmp.create_empty();
        let mut state = gzopen(tmp.path(), "r").expect("open for read");
        // A negative net offset that lands before position 0 is rejected.
        assert_eq!(gzseek64(&mut state, -10, SEEK_CUR), -1);
    }

    #[test]
    fn seek_backwards_when_writing_is_rejected() {
        let tmp = TempPath::new("seek_write_back");
        let mut state = gzopen(tmp.path(), "wb").expect("open for write");
        // Writing cannot seek backwards.
        assert_eq!(gzseek64(&mut state, -1, SEEK_CUR), -1);
    }

    #[test]
    fn rewind_clears_pending_skip() {
        let tmp = TempPath::new("rewind");
        tmp.create_empty();
        let mut state = gzopen(tmp.path(), "r").expect("open for read");
        assert_eq!(gzseek64(&mut state, 50, SEEK_SET), 50);
        assert_eq!(state.skip, 50);
        assert_eq!(gzrewind(&mut state), 0);
        // reset() clears the deferred skip and position.
        assert_eq!(state.skip, 0);
        assert_eq!(state.pos, 0);
        assert_eq!(gztell64(&state), 0);
    }

    #[test]
    fn rewind_rejects_write_mode() {
        let tmp = TempPath::new("rewind_write");
        let mut state = gzopen(tmp.path(), "wb").expect("open for write");
        // Rewind is only valid for read streams.
        assert_eq!(gzrewind(&mut state), -1);
    }

    #[test]
    fn tell_and_offset_reject_finalized_stream() {
        let tmp = TempPath::new("tell_mode");
        let mut state = gzopen(tmp.path(), "wb").expect("open for write");
        let _ = state.finalize();
        assert_eq!(gztell64(&state), -1);
        assert_eq!(gzoffset64(&mut state), -1);
    }

    #[test]
    fn offset_returns_file_position_for_write() {
        let tmp = TempPath::new("offset");
        let mut state = gzopen(tmp.path(), "wb").expect("open for write");
        // A freshly created/truncated write file is positioned at 0.
        assert_eq!(gzoffset64(&mut state), 0);
        assert_eq!(gzoffset(&mut state), 0);
    }

    #[test]
    fn gzerror_reports_code_and_message() {
        let tmp = TempPath::new("err");
        tmp.create_empty();
        let mut state = gzopen(tmp.path(), "r").expect("open for read");
        // No error yet: empty message, Z_OK code.
        let mut errnum = 12345;
        assert_eq!(gzerror(&state, &mut errnum), Some(""));
        assert_eq!(errnum, Z_OK);

        // A data error stores a "path: msg" message.
        state.set_error(ZlibError::DataError.as_i32(), Some("bad data"));
        let mut errnum = 0;
        let msg = gzerror(&state, &mut errnum).expect("message");
        assert_eq!(errnum, ZlibError::DataError.as_i32());
        assert!(msg.ends_with("bad data"), "unexpected message: {msg:?}");
    }

    #[test]
    fn gzerror_mem_error_returns_literal() {
        let tmp = TempPath::new("err_mem");
        tmp.create_empty();
        let mut state = gzopen(tmp.path(), "r").expect("open for read");
        state.set_error(Z_MEM_ERROR, Some("ignored"));
        let mut errnum = 0;
        assert_eq!(gzerror(&state, &mut errnum), Some("out of memory"));
        assert_eq!(errnum, Z_MEM_ERROR);
    }

    #[test]
    fn gzclearerr_clears_error_and_eof() {
        let tmp = TempPath::new("clearerr");
        tmp.create_empty();
        let mut state = gzopen(tmp.path(), "r").expect("open for read");
        state.set_error(ZlibError::DataError.as_i32(), Some("boom"));
        state.eof = true;
        state.past = true;
        gzclearerr(&mut state);
        assert_eq!(state.err, Z_OK);
        assert!(!state.eof);
        assert!(!state.past);
    }

    #[test]
    fn gzerror_rejects_finalized_stream() {
        let tmp = TempPath::new("err_mode");
        let mut state = gzopen(tmp.path(), "wb").expect("open for write");
        let _ = state.finalize();
        let mut errnum = 999;
        // A finalized (mode None) stream returns None and leaves errnum intact.
        assert_eq!(gzerror(&state, &mut errnum), None);
        assert_eq!(errnum, 999);
    }
}
