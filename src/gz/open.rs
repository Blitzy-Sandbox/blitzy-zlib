//! gzip **open / positioning / status** API — the safe-Rust port of zlib's
//! `gzlib.c` (plus the `gzsetparams` body from `gzwrite.c`).
//!
//! This module is the *policy and parsing* layer of the gzip FILE-I/O subsystem
//! (`crate::gz`). It owns:
//!
//! * **opening** a gzip file — [`gzopen`], [`gzopen64`], [`gzdopen`], and the
//!   shared [`gz_open`] constructor with its character-for-character
//!   mode-string parser;
//! * **buffer sizing** — [`gzbuffer`];
//! * **positioning** — [`gzrewind`], [`gzseek`]/[`gzseek64`],
//!   [`gztell`]/[`gztell64`], [`gzoffset`]/[`gzoffset64`];
//! * **status** — [`gzeof`], [`gzerror`], [`gzclearerr`];
//! * **parameter changes** — [`gzsetparams`] (a thin wrapper that delegates to
//!   [`crate::gz::write::set_params`], whose source is `gzwrite.c` — this is why
//!   `open.rs` lists both `gzlib.c` and `gzwrite.c` as its C sources, AAP
//!   §0.4.1).
//!
//! # What this layer does *not* do
//!
//! `open.rs` never touches the deflate/inflate engine directly. Engine
//! initialization is **lazy**: it happens on first use in
//! [`crate::gz::write::gz_init`] (writing) or [`crate::gz::read::gz_look`]
//! (reading), both of which configure the engine with `windowBits = 31` (the
//! gzip wrapper). `gz_open` only *records* the requested `level` / `strategy` /
//! `mode` / `direct` and opens (or adopts) the OS file. Likewise [`gzseek`]
//! only *computes* the target position and arms a pending skip
//! ([`GzState::skip`]); the actual data skip is performed lazily on the next
//! read/write by [`crate::gz::read::gz_skip`] / [`crate::gz::write::gz_zero`].
//! Preserving this deferral exactly is required for byte-for-byte parity with C
//! zlib (AAP §0.6.1, §0.7.1).
//!
//! # Safety
//!
//! This file contains **no `unsafe`** (AAP §0.6.2 — `unsafe` is confined to
//! `crate::ffi` and `crate::inflate::fast`). Two C idioms that *would* require
//! `unsafe` are deliberately kept out of this safe layer:
//!
//! * **raw-fd `gzdopen`** — turning a C `int` file descriptor into a
//!   [`std::fs::File`] needs `File::from_raw_fd`, which is `unsafe`. That
//!   conversion lives in `crate::ffi`; the FFI shim performs it and then calls
//!   the safe [`gzdopen`]`(File, &str)` here.
//! * **C-string `char *` path/mode marshalling** — also handled in
//!   `crate::ffi`; this layer accepts an `&str` mode and a borrowed
//!   [`std::path::Path`].
//!
//! The OS-level open flags `O_CLOEXEC` / `O_EXCL` / `O_NONBLOCK` are applied
//! entirely through **safe** `std` facilities (see [`gz_open`]).

use crate::constants::*;
use crate::gz::state::{GzState, How, Mode};
use crate::gz::write;
use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom};
use std::path::Path;

/// `O_NONBLOCK` for the current Unix target.
///
/// `std` does not re-export this flag, and the crate intentionally takes **no
/// `libc` dependency** (the shipped library must carry zero C dependencies, AAP
/// §0.5.2), so the small, ABI-frozen constant is named directly here. The value
/// is `0o4000` on Linux/Android and `0x0004` on macOS and every BSD — the only
/// two families that exist across the platforms zlib targets.
#[cfg(all(unix, any(target_os = "linux", target_os = "android")))]
const O_NONBLOCK: i32 = 0o4000;
/// `O_NONBLOCK` for macOS and the BSDs (FreeBSD/OpenBSD/NetBSD/DragonFly), all
/// of which share the value `0x0004`.
#[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
const O_NONBLOCK: i32 = 0x0004;

/// Builds the synthetic identifier C `gzdopen` uses in error messages
/// (`"<fd:N>"`), reading the descriptor number out of the already-open file.
///
/// `AsRawFd::as_raw_fd` is a **safe** borrow of the descriptor (it neither takes
/// ownership nor performs any `unsafe` conversion), so this stays within the
/// no-`unsafe` contract of this module.
#[cfg(unix)]
fn fd_path(file: &File) -> String {
    use std::os::fd::AsRawFd;
    format!("<fd:{}>", file.as_raw_fd())
}

/// Non-Unix fallback for the `gzdopen` error-message identifier: the descriptor
/// number is not portably available, so a generic tag is used.
#[cfg(not(unix))]
fn fd_path(_file: &File) -> String {
    String::from("<fd>")
}

/// The source a gzip handle is opened from — either a filesystem **path**
/// (the [`gzopen`] case) or an already-open **file** (the [`gzdopen`] case).
///
/// C's `gz_open(path, fd, mode)` overloads a single function with a sentinel
/// `fd` (`-1` means "open `path`"). This port models the two cases with an enum
/// instead, which keeps the safe/`unsafe` split clean: the raw-`fd` →
/// [`File`] conversion (`File::from_raw_fd`, `unsafe`) happens in `crate::ffi`,
/// which then hands a ready [`File`] to [`gz_open`] via [`GzSource::File`].
pub(crate) enum GzSource<'a> {
    /// Open the file at this path (the [`gzopen`] / [`gzopen64`] case).
    Path(&'a Path),
    /// Adopt this already-open file (the [`gzdopen`] case). The descriptor's
    /// access mode and any `O_CLOEXEC` / `O_NONBLOCK` flags were established by
    /// the caller (`crate::ffi`) before construction.
    File(File),
}

/// Reset the gzip file state at open time and after a rewind — the safe port of
/// the C `gz_reset` (`gzlib.c` L69-84).
///
/// Clears the output window, the seek request, and the error state, resets the
/// uncompressed position to `0`, and — when reading — re-arms the header sniff
/// (`how = Look`, `junk = -1` to mark the first member). When writing, it clears
/// the pending-`deflateReset` flag instead. This matches C field-for-field,
/// including the read/write split and the `in_avail = 0` (C `strm.avail_in = 0`)
/// input-cursor reset. As in C, the output index `next` and input index
/// `in_next` are left untouched (they are don't-cares while `have == 0` /
/// `in_avail == 0` and are re-established by the next fetch).
pub(crate) fn gz_reset(state: &mut GzState) {
    // No output data available yet.
    state.have = 0;
    if state.mode == Mode::Read {
        // For reading: not at EOF, have not read past end, and re-arm the
        // gzip-header sniff at the start of a fresh member.
        state.eof = false;
        state.past = false;
        state.how = How::Look;
        state.junk = -1; // mark the first member
    } else {
        // For writing: no deflateReset pending.
        state.reset = false;
    }
    // No stalled I/O, no pending seek.
    state.again = false;
    state.skip = 0;
    // Clear any error (C `gz_error(state, Z_OK, NULL)`).
    state.clear_error();
    // No uncompressed data produced/consumed yet.
    state.pos = 0;
    state.in_avail = 0; // C `state->strm.avail_in = 0`
}

/// Open a gzip file by path or adopt an existing file — the safe-Rust port of
/// the shared C `gz_open` constructor (`gzlib.c` L86-285).
///
/// Returns the owning [`GzState`] handle (boxed, the idiomatic `gzFile`) on
/// success, or [`None`] on any failure — exactly mirroring C's `NULL` return
/// (which there is accompanied by freeing the partially built state; here the
/// state is simply never constructed, or is dropped on the early return).
///
/// # Mode-string parsing (frozen contract — `gzlib.c` L108-171)
///
/// Each character of `mode` is interpreted *exactly* as C does:
///
/// | Char(s)  | Effect                                                        |
/// |----------|---------------------------------------------------------------|
/// | `'0'..='9'` | set the compression `level` to that digit                 |
/// | `'r'`    | open for reading                                              |
/// | `'w'`    | open for writing                                              |
/// | `'a'`    | open for appending                                            |
/// | `'+'`    | **error** — cannot read and write at once (returns [`None`])  |
/// | `'b'`    | binary — ignored (binary is always implied)                   |
/// | `'e'`    | `O_CLOEXEC` — honored inherently (see below)                  |
/// | `'x'`    | `O_EXCL` — fail if the file already exists                    |
/// | `'f'`    | strategy [`Z_FILTERED`]                                       |
/// | `'h'`    | strategy [`Z_HUFFMAN_ONLY`]                                   |
/// | `'R'`    | strategy [`Z_RLE`]                                            |
/// | `'F'`    | strategy [`Z_FIXED`]                                          |
/// | `'G'`    | `direct = -1` — gzip-only on read (disable transparent read)  |
/// | `'N'`    | `O_NONBLOCK`                                                  |
/// | `'T'`    | `direct = 1` — write transparently (store, no compression)    |
/// | other    | ignored                                                       |
///
/// After parsing, the same validation C performs is applied (`gzlib.c`
/// L173-195): a mode (`r`/`w`/`a`) is mandatory; a forced-transparent read
/// (`'T'`) is rejected; a plain read defaults `direct` to `1` (auto-detect
/// gzip-vs-transparent); and `'G'` on a write stream is rejected.
///
/// # OS open flags without a `libc` dependency
///
/// * **`O_EXCL`** (`'x'`) → [`OpenOptions::create_new`] (which is exactly
///   `O_CREAT | O_EXCL`).
/// * **`O_CLOEXEC`** (`'e'`) → honored *inherently*: Rust's `std` opens every
///   file close-on-exec by default, so a handle is close-on-exec whether or not
///   `'e'` was supplied. (This is a safe, secure superset of C's behavior; the
///   only divergence is that C leaves the descriptor inheritable when `'e'` is
///   absent.)
/// * **`O_NONBLOCK`** (`'N'`) → applied via the safe
///   [`std::os::unix::fs::OpenOptionsExt::custom_flags`] on Unix. On non-Unix
///   targets the flag is recorded but not forced (it is a no-op for regular
///   files anyway). For the raw-`fd` [`gzdopen`] path, where these flags matter
///   for pipes/FIFOs, `crate::ffi` applies them via `fcntl` before handing over
///   the [`File`].
///
/// # Append
///
/// An append open is positioned at end-of-file (so [`gzoffset`] is correct) and
/// then rewritten to [`Mode::Write`], exactly as C does
/// (`if (mode == GZ_APPEND) { LSEEK(SEEK_END); mode = GZ_WRITE; }`).
pub(crate) fn gz_open(source: GzSource<'_>, mode: &str) -> Option<Box<GzState>> {
    // -- interpret the mode string (gzlib.c L108-171) --
    // Defaults match C `gz_open`'s post-allocation initialization.
    let mut parsed_mode = Mode::None;
    let mut level: i32 = Z_DEFAULT_COMPRESSION;
    let mut strategy: i32 = Z_DEFAULT_STRATEGY;
    // `direct` tri-state: 0 = gzip, 1 = transparent ('T'), -1 = gzip-only ('G').
    let mut direct: i32 = 0;
    // OS-level open intentions tracked while parsing.
    let mut exclusive = false; // 'x' -> O_EXCL
    let mut nonblock = false; // 'N' -> O_NONBLOCK
    // 'e' (O_CLOEXEC) needs no tracking: std opens files close-on-exec already.

    for ch in mode.chars() {
        match ch {
            '0'..='9' => level = (ch as u8 - b'0') as i32,
            'r' => parsed_mode = Mode::Read,
            'w' => parsed_mode = Mode::Write,
            'a' => parsed_mode = Mode::Append,
            // Cannot read and write at the same time -> C frees state, NULLs.
            '+' => return None,
            'b' => {} // binary is always implied -- ignore
            'e' => {} // O_CLOEXEC -- std default; nothing to do
            'x' => exclusive = true,
            'f' => strategy = Z_FILTERED,
            'h' => strategy = Z_HUFFMAN_ONLY,
            'R' => strategy = Z_RLE,
            'F' => strategy = Z_FIXED,
            'G' => direct = -1,
            'N' => nonblock = true,
            'T' => direct = 1,
            _ => {} // unknown -- ignore (C `default: ;`)
        }
    }

    // -- must provide an "r", "w", or "a" (gzlib.c L173-177) --
    if parsed_mode == Mode::None {
        return None;
    }

    // -- resolve the `direct` tri-state (gzlib.c L179-195) --
    if parsed_mode == Mode::Read {
        if direct == 1 {
            // 'T' on a read stream: cannot force a transparent read.
            return None;
        }
        if direct == 0 {
            // Default when reading: auto-detect gzip vs. transparent (start
            // transparent so an empty file behaves), re-evaluated by gz_look.
            direct = 1;
        }
        // direct == -1 ('G', gzip-only) is kept as-is.
    } else if direct == -1 {
        // 'G' has no meaning when writing -- disallow it.
        return None;
    }

    // -- open (gzopen) or adopt (gzdopen) the file; build the error-path name --
    let (file, path) = match source {
        GzSource::Path(p) => {
            let mut opts = OpenOptions::new();
            match parsed_mode {
                Mode::Read => {
                    opts.read(true); // O_RDONLY
                }
                Mode::Write => {
                    opts.write(true);
                    if exclusive {
                        // O_CREAT | O_EXCL (fail if the file already exists).
                        opts.create_new(true);
                    } else {
                        // O_CREAT | O_TRUNC.
                        opts.create(true).truncate(true);
                    }
                }
                Mode::Append => {
                    // O_CREAT | O_APPEND; positioned to end below.
                    opts.append(true).create(true);
                }
                // Unreachable: Mode::None was rejected above.
                Mode::None => return None,
            }
            // Apply O_NONBLOCK via the safe custom_flags extension on Unix.
            #[cfg(unix)]
            {
                if nonblock {
                    use std::os::unix::fs::OpenOptionsExt;
                    opts.custom_flags(O_NONBLOCK);
                }
            }
            #[cfg(not(unix))]
            {
                let _ = nonblock; // recorded but not forced on non-Unix
            }
            // On open failure, C frees the state and returns NULL; here we just
            // return None (no state was built yet).
            let file = opts.open(p).ok()?;
            (file, p.to_string_lossy().into_owned())
        }
        GzSource::File(f) => {
            // gzdopen: the descriptor is already open with its access mode and
            // any O_CLOEXEC/O_NONBLOCK flags established by crate::ffi.
            let path = fd_path(&f);
            (f, path)
        }
    };

    // -- construct the owning handle with the parsed parameters --
    // GzState::new applies the C `gz_open` post-allocation defaults
    // (want = GZBUFSIZE, size = 0, err = Z_OK, msg = None, ...); we layer the
    // mode-derived level/strategy/direct on top.
    let mut state = GzState::new(file, path, parsed_mode);
    state.level = level;
    state.strategy = strategy;
    state.direct = direct;

    // -- append: seek to end so gzoffset() is correct, then treat as write --
    if parsed_mode == Mode::Append {
        if let Some(file) = state.file.as_mut() {
            // Best-effort, matching C `LSEEK(fd, 0, SEEK_END)`; an error here is
            // non-fatal (O_APPEND still directs writes to end-of-file).
            let _ = file.seek(SeekFrom::End(0));
        }
        state.mode = Mode::Write; // simplify later checks (C L271)
    }

    // -- reading: remember the start position for rewinding (gzlib.c L274-278) --
    if state.mode == Mode::Read {
        state.start = match state.file.as_mut() {
            // C: start = LSEEK(fd, 0, SEEK_CUR); if (start == -1) start = 0;
            Some(file) => file.stream_position().map(|p| p as i64).unwrap_or(0),
            None => 0,
        };
    }

    // -- initialize the read/write working state (C `gz_reset`) --
    gz_reset(&mut state);

    Some(Box::new(state))
}

// ===========================================================================
// Public open functions (gzlib.c L287-312)
// ===========================================================================

/// Open the gzip file at `path` with the given `mode` — the safe-Rust port of
/// C `gzopen` (`gzlib.c` L287-290).
///
/// `mode` follows the zlib convention (e.g. `"rb"`, `"wb9"`, `"wb1f"`,
/// `"ab"`); see [`gz_open`] for the full character table. Returns the gzip
/// handle, or [`None`] if `mode` is invalid or the file cannot be opened.
#[must_use]
pub fn gzopen(path: &Path, mode: &str) -> Option<Box<GzState>> {
    gz_open(GzSource::Path(path), mode)
}

/// Open the gzip file at `path` — the safe-Rust port of C `gzopen64`
/// (`gzlib.c` L292-295).
///
/// This port represents all file positions and seek amounts with `i64`
/// ([`z_off64_t`](crate::constants)) throughout, so the 32-bit `gzopen` and the
/// 64-bit `gzopen64` are **identical**; this function exists for C API parity
/// and simply forwards to the same [`gz_open`] constructor.
#[must_use]
pub fn gzopen64(path: &Path, mode: &str) -> Option<Box<GzState>> {
    gz_open(GzSource::Path(path), mode)
}

/// Adopt an already-open `file` as a gzip handle — the safe-Rust port of C
/// `gzdopen` (`gzlib.c` L297-312).
///
/// C's `gzdopen` takes a raw `int` file descriptor. Because turning a raw
/// descriptor into a [`File`] requires `File::from_raw_fd` (which is `unsafe`),
/// that conversion lives in `crate::ffi`; the FFI `gzdopen` shim performs it and
/// then calls this safe entry point with the resulting [`File`]. The handle's
/// access mode must be compatible with `mode`.
#[must_use]
pub fn gzdopen(file: File, mode: &str) -> Option<Box<GzState>> {
    gz_open(GzSource::File(file), mode)
}

// ===========================================================================
// gzbuffer (gzlib.c L321-343)
// ===========================================================================

/// Set the internal buffer size for subsequent I/O — the safe-Rust port of C
/// `gzbuffer` (`gzlib.c` L321-343).
///
/// Must be called **before** the first read or write (i.e. before the buffers
/// are allocated). Returns `0` on success, or `-1` if the handle is invalid, the
/// buffers are already allocated ([`size`](GzState::size) `!= 0`), or `size`
/// would overflow when doubled. A `size` below `8` is rounded up to `8` so the
/// flushing logic behaves; the accepted value is stored in
/// [`want`](GzState::want).
pub fn gzbuffer(state: &mut GzState, size: u32) -> i32 {
    // Integrity check: must be a read or write handle.
    if state.mode != Mode::Read && state.mode != Mode::Write {
        return -1;
    }
    // The buffers must not have been allocated yet.
    if state.size != 0 {
        return -1;
    }
    // Overflow guard: the buffers may be doubled, so reject any size whose
    // double would overflow a u32 (C `if ((size << 1) < size) return -1;`).
    if size > u32::MAX >> 1 {
        return -1;
    }
    // Round up to the minimum that behaves well with flushing.
    let size = if size < 8 { 8 } else { size };
    state.want = size as usize;
    0
}

// ===========================================================================
// gzrewind (gzlib.c L345-364)
// ===========================================================================

/// Rewind a gzip file open for reading back to its start — the safe-Rust port
/// of C `gzrewind` (`gzlib.c` L345-364).
///
/// Valid only for a read handle with no fatal error (only [`Z_OK`] or
/// [`Z_BUF_ERROR`] are tolerated). Seeks the underlying file back to the saved
/// [`start`](GzState::start) position and re-initializes the read state via
/// [`gz_reset`] (which re-arms the gzip-header sniff). Returns `0` on success or
/// `-1` on failure.
pub fn gzrewind(state: &mut GzState) -> i32 {
    // Must be reading, and there must be no serious error.
    if state.mode != Mode::Read || (state.err != Z_OK && state.err != Z_BUF_ERROR) {
        return -1;
    }
    // Back up to where the gzip data started.
    let start = state.start;
    match state.file.as_mut() {
        Some(file) => {
            if file.seek(SeekFrom::Start(start as u64)).is_err() {
                return -1;
            }
        }
        None => return -1,
    }
    // Start over.
    gz_reset(state);
    0
}

// ===========================================================================
// gzseek / gzseek64 (gzlib.c L366-443)
// ===========================================================================

/// Set the starting position for the next read or write — the safe-Rust port
/// of C `gzseek64` (`gzlib.c` L366-435).
///
/// Only [`SEEK_SET`] (absolute) and [`SEEK_CUR`] (relative to the current
/// position) are supported; [`SEEK_END`] returns `-1`, matching C. The returned
/// value is the resulting uncompressed offset, or `-1` on error.
///
/// # Lazy skip (parity-critical)
///
/// A forward seek does **not** perform any I/O here. It records the remaining
/// distance in [`skip`](GzState::skip); the actual work happens on the next
/// operation — [`crate::gz::read::gz_skip`] discards that many decompressed
/// bytes when reading, and [`crate::gz::write::gz_zero`] writes that many zero
/// bytes when writing. Three cases are handled directly, exactly as C does:
///
/// * **Transparent read fast path** — when reading a non-gzip stream
///   ([`How::Copy`]), the seek is satisfied immediately with a single
///   underlying file seek.
/// * **Backward read seek** — rewinds via [`gzrewind`] and then skips forward.
/// * **Backward write seek** — rejected (`-1`): a write stream cannot move
///   backwards.
///
/// The 32-bit C `gzseek` and 64-bit `gzseek64` are unified here (this port uses
/// `i64` positions throughout); the 32-bit `z_off_t` truncation C performs lives
/// in the `crate::ffi` shim, not in this idiomatic layer. [`gzseek64`] forwards
/// to this function.
pub fn gzseek(state: &mut GzState, offset: i64, whence: i32) -> i64 {
    // Integrity check: must be a read or write handle.
    if state.mode != Mode::Read && state.mode != Mode::Write {
        return -1;
    }
    // No serious error pending.
    if state.err != Z_OK && state.err != Z_BUF_ERROR {
        return -1;
    }
    // Can only seek from the start or relative to the current position.
    if whence != SEEK_SET && whence != SEEK_CUR {
        return -1;
    }

    // Normalize `offset` to a SEEK_CUR specification (relative to `pos`).
    let mut offset = offset;
    if whence == SEEK_SET {
        offset -= state.pos;
    } else {
        // Fold any pending (not-yet-applied) skip into the relative offset.
        offset += if state.past { 0 } else { state.skip };
        state.skip = 0;
    }

    // If within the raw (transparent) area while reading, just go there with a
    // single underlying file seek (gzlib.c L395-409).
    if state.mode == Mode::Read && state.how == How::Copy && state.pos + offset >= 0 {
        // Seek relative to the current file position, accounting for bytes
        // already buffered as output (C `offset - x.have`).
        let relative = offset - state.have as i64;
        match state.file.as_mut() {
            Some(file) => {
                if file.seek(SeekFrom::Current(relative)).is_err() {
                    return -1;
                }
            }
            None => return -1,
        }
        state.have = 0;
        state.eof = false;
        state.past = false;
        state.skip = 0;
        state.clear_error();
        state.in_avail = 0; // C `state->strm.avail_in = 0`
        state.pos += offset;
        return state.pos;
    }

    // Backward seek: rewind first (read only), then skip forward
    // (gzlib.c L411-420).
    if offset < 0 {
        if state.mode != Mode::Read {
            // Writing -- cannot go backwards.
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
    }

    // If reading, consume what is already in the output buffer first, so the
    // gzgetc() fast path needs one less check (gzlib.c L422-430).
    //
    // The C `GT_OFF(x.have)` guard (which fires only when `int` and `z_off64_t`
    // are the same width and `have` exceeds the max offset) is always false in
    // this port: `have` is a small `usize` (bounded by the buffer size) and
    // `offset >= 0` here, so it always fits in `i64`. The selection therefore
    // reduces to `n = min(have, offset)`.
    if state.mode == Mode::Read {
        let n = if (state.have as i64) > offset {
            offset as usize
        } else {
            state.have
        };
        state.have -= n;
        state.next += n;
        state.pos += n as i64;
        offset -= n as i64;
    }

    // Request the (possibly zero) remaining skip; the next read/write applies
    // it lazily.
    state.skip = offset;
    state.pos + offset
}

/// Set the starting position for the next read or write — the safe-Rust port
/// of C `gzseek` (`gzlib.c` L437-443).
///
/// Identical to [`gzseek`] in this port (see that function's note on the
/// 32-/64-bit unification); provided for C API parity.
pub fn gzseek64(state: &mut GzState, offset: i64, whence: i32) -> i64 {
    gzseek(state, offset, whence)
}

// ===========================================================================
// Position / status accessors (gzlib.c L445-547)
// ===========================================================================

/// Return the current uncompressed position — the safe-Rust port of C
/// `gztell64` (`gzlib.c` L445-458) and `gztell` (`gzlib.c` L460-466).
///
/// Includes any pending (not-yet-applied) seek: `pos + skip`, unless a read past
/// end already occurred (in which case the pending skip is not counted), exactly
/// as C does (`state->x.pos + (state->past ? 0 : state->skip)`). Returns `-1`
/// for an invalid handle. The 32-/64-bit variants are unified in this port.
#[must_use]
pub fn gztell(state: &GzState) -> i64 {
    if state.mode != Mode::Read && state.mode != Mode::Write {
        return -1;
    }
    state.pos + if state.past { 0 } else { state.skip }
}

/// Return the current uncompressed position — the safe-Rust port of C
/// `gztell64`. Identical to [`gztell`] in this port; provided for C API parity.
#[must_use]
pub fn gztell64(state: &GzState) -> i64 {
    gztell(state)
}

/// Return the current *compressed* file offset — the safe-Rust port of C
/// `gzoffset64` (`gzlib.c` L468-487) and `gzoffset` (`gzlib.c` L489-495).
///
/// This is the position within the underlying file (C `LSEEK(fd, 0, SEEK_CUR)`),
/// minus, when reading, any input that has been buffered but not yet consumed
/// ([`in_avail`](GzState::in_avail), the port of C `strm.avail_in`) so the
/// reported offset reflects what has actually been processed. Returns `-1` for
/// an invalid handle or on a seek error. The 32-/64-bit variants are unified.
pub fn gzoffset(state: &mut GzState) -> i64 {
    if state.mode != Mode::Read && state.mode != Mode::Write {
        return -1;
    }
    // Effective offset in the file.
    let mut offset = match state.file.as_mut() {
        Some(file) => match file.stream_position() {
            Ok(pos) => pos as i64,
            Err(_) => return -1,
        },
        None => return -1,
    };
    if state.mode == Mode::Read {
        // Do not count buffered-but-unconsumed input.
        offset -= state.in_avail as i64;
    }
    offset
}

/// Return the current compressed file offset — the safe-Rust port of C
/// `gzoffset64`. Identical to [`gzoffset`] in this port; provided for C API
/// parity.
pub fn gzoffset64(state: &mut GzState) -> i64 {
    gzoffset(state)
}

/// Report whether a read has been attempted **past** the end of the data — the
/// safe-Rust port of C `gzeof` (`gzlib.c` L497-510).
///
/// Returns `true` only once an attempt to read beyond the end of the
/// uncompressed data has been made — *not* merely upon reaching the last byte
/// (this is the classic zlib `past`-vs-EOF subtlety: C returns
/// `state->past`, not `state->eof`). Always `false` for a write handle, and
/// `false` for an invalid handle. The `crate::ffi` shim maps this to the C
/// `int` (`0`/`1`).
#[must_use]
pub fn gzeof(state: &GzState) -> bool {
    if state.mode != Mode::Read && state.mode != Mode::Write {
        return false;
    }
    state.mode == Mode::Read && state.past
}

/// Return the last error code and message recorded on the handle — the
/// safe-Rust port of C `gzerror` (`gzlib.c` L512-528).
///
/// Returns the raw `Z_*` error code together with its formatted
/// `"<path>: <message>"` text (or [`None`] when there is no message — the
/// idiomatic equivalent of C's empty string). For [`Z_MEM_ERROR`] the literal
/// `"out of memory"` is returned without consulting the (deliberately unset)
/// stored message, matching C exactly.
///
/// **Error-disclosure policy (two surfaces):** this safe-Rust entry point
/// returns the detailed `"<path>: <detail>"` message, which is idiomatic for an
/// in-process Rust API (mirroring how `std::io::Error` embeds the offending
/// path for its own caller). The FFI-visible `gzerror` shim does **not** expose
/// it: it writes the exact code through the C `int *errnum` out-parameter and
/// returns the fixed `'static` text for that code, so no path/OS/inflate detail
/// crosses the C ABI. That satisfies the CP2 "fixed `z_errmsg` strings only"
/// requirement at the external surface while the error *code* stays
/// bit-identical to C. See the full rationale on `GzState::gz_error` and the
/// module-level `gzerror` divergence note in `crate::ffi`.
#[must_use]
pub fn gzerror(state: &GzState) -> (i32, Option<&str>) {
    // Integrity check (C returns NULL here; unreachable for a well-formed
    // handle, whose mode is always Read or Write after a successful open).
    if state.mode != Mode::Read && state.mode != Mode::Write {
        return (state.err, None);
    }
    // Out of memory: report the literal string (C special-case).
    if state.err == Z_MEM_ERROR {
        return (Z_MEM_ERROR, Some("out of memory"));
    }
    (state.err, state.msg.as_deref())
}

/// Clear the error and end-of-file state of a handle — the safe-Rust port of C
/// `gzclearerr` (`gzlib.c` L530-547).
///
/// For a read handle this also clears [`eof`](GzState::eof) and
/// [`past`](GzState::past); for either mode it resets the error to [`Z_OK`] with
/// no message. Matching C, the clear is *unconditional* (there is no
/// error-code-dependent guard).
pub fn gzclearerr(state: &mut GzState) {
    if state.mode != Mode::Read && state.mode != Mode::Write {
        return;
    }
    if state.mode == Mode::Read {
        state.eof = false;
        state.past = false;
    }
    state.clear_error();
}

// ===========================================================================
// gzsetparams (entry point here; body in gzwrite.c -> crate::gz::write)
// ===========================================================================

/// Change the compression level and strategy for subsequent input — the entry
/// point for C `gzsetparams`.
///
/// The C `gzsetparams` lives in `gzwrite.c`, and its deflate-driving body is
/// ported in [`crate::gz::write::set_params`] (so the engine logic is not
/// duplicated). This function is the thin wrapper the AAP places in `open.rs`
/// (which is why `open.rs` lists `gzwrite.c` among its sources, AAP §0.4.1): it
/// forwards directly. `set_params` performs all the validation (write-mode,
/// non-transparent, no serious error), the no-change fast path, the pending-seek
/// flush, and the `deflateParams` call. Returns [`Z_OK`] on success or a `Z_*`
/// error code.
pub fn gzsetparams(state: &mut GzState, level: i32, strategy: i32) -> i32 {
    write::set_params(state, level, strategy)
}

#[cfg(test)]
mod tests {
    //! Unit + integration tests for the gzip open/seek/status API.
    //!
    //! Mode parsing, `gzbuffer`, `gz_reset`, and the status accessors are tested
    //! directly against freshly opened handles. Positioning is validated
    //! end-to-end: gzip members are produced with `flate2` (the dev-only oracle
    //! that links canonical C zlib), opened with [`gzopen`], seeked, and read
    //! back through the sibling [`crate::gz::read`] path — proving that the
    //! lazy-skip handoff (`gzseek` arms [`GzState::skip`]; `gz_read` applies it)
    //! lands at the right uncompressed offset.

    use super::*;
    use std::io::Write as _;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// A unique temp path (process id + monotonic counter) so parallel test
    /// runs do not collide, without pulling in any extra crate.
    fn unique_temp_path(tag: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut p = std::env::temp_dir();
        p.push(format!(
            "zlib_rs_gzopen_test_{}_{}_{}.bin",
            tag,
            std::process::id(),
            n
        ));
        p
    }

    /// Creates a fresh empty temp file and returns its path (so a read-mode
    /// `gzopen`, which uses `O_RDONLY`, finds something to open).
    fn make_empty_file(tag: &str) -> PathBuf {
        let path = unique_temp_path(tag);
        File::create(&path).expect("create temp file");
        path
    }

    /// gzip-compresses `data` with `flate2` (canonical C zlib) at the default
    /// level, producing a standard RFC 1952 gzip member.
    fn gzip(data: &[u8]) -> Vec<u8> {
        use flate2::Compression;
        use flate2::write::GzEncoder;
        let mut enc = GzEncoder::new(Vec::new(), Compression::default());
        enc.write_all(data).expect("gz encode write");
        enc.finish().expect("gz encode finish")
    }

    // ---- mode-string parsing ------------------------------------------------

    #[test]
    fn rb_is_read_with_autodetect_default() {
        let path = make_empty_file("rb");
        let st = gzopen(&path, "rb").expect("gzopen rb");
        assert_eq!(st.mode, Mode::Read);
        // A plain read defaults `direct` to 1 (auto-detect gzip vs transparent).
        assert_eq!(st.direct, 1);
        assert_eq!(st.level, Z_DEFAULT_COMPRESSION);
        assert_eq!(st.strategy, Z_DEFAULT_STRATEGY);
        drop(st);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn wb9_is_write_level_9() {
        let path = unique_temp_path("wb9");
        assert!(!path.exists());
        let st = gzopen(&path, "wb9").expect("gzopen wb9");
        assert_eq!(st.mode, Mode::Write);
        assert_eq!(st.level, 9);
        // The file is created by the write open.
        assert!(path.exists());
        drop(st);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn wb1f_is_write_level_1_filtered() {
        let path = unique_temp_path("wb1f");
        let st = gzopen(&path, "wb1f").expect("gzopen wb1f");
        assert_eq!(st.mode, Mode::Write);
        assert_eq!(st.level, 1);
        assert_eq!(st.strategy, Z_FILTERED);
        drop(st);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn wbr_is_rle_strategy() {
        let path = unique_temp_path("wbR");
        let st = gzopen(&path, "wbR").expect("gzopen wbR");
        assert_eq!(st.mode, Mode::Write);
        assert_eq!(st.strategy, Z_RLE);
        drop(st);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn wbt_sets_transparent_write_direct() {
        let path = unique_temp_path("wbT");
        let st = gzopen(&path, "wbT").expect("gzopen wbT");
        assert_eq!(st.mode, Mode::Write);
        assert_eq!(st.direct, 1); // 'T' -> transparent (store, no compression)
        drop(st);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn rbg_sets_gzip_only_direct() {
        let path = make_empty_file("rbG");
        let st = gzopen(&path, "rbG").expect("gzopen rbG");
        assert_eq!(st.mode, Mode::Read);
        assert_eq!(st.direct, -1); // 'G' -> gzip-only (no transparent read)
        drop(st);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn append_becomes_write_positioned_at_end() {
        let path = unique_temp_path("append");
        std::fs::write(&path, b"existing-content").expect("seed file");
        let mut st = gzopen(&path, "ab").expect("gzopen ab");
        // Append is rewritten to Write after seeking to end (C parity).
        assert_eq!(st.mode, Mode::Write);
        // The underlying file is positioned at end-of-file (16 bytes).
        let off = gzoffset(&mut st);
        assert_eq!(off, 16);
        drop(st);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn invalid_modes_return_none() {
        let path = make_empty_file("invalid");
        // '+' cannot read and write at once.
        assert!(gzopen(&path, "rb+").is_none());
        assert!(gzopen(&path, "wb+").is_none());
        // 'T' cannot force a transparent read.
        assert!(gzopen(&path, "rbT").is_none());
        // 'G' has no meaning when writing.
        assert!(gzopen(&path, "wbG").is_none());
        // No r/w/a provided.
        assert!(gzopen(&path, "b").is_none());
        assert!(gzopen(&path, "").is_none());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn last_digit_and_last_g_or_t_win() {
        // Multiple digits: the last one wins (each overwrites `level`).
        let path = unique_temp_path("digits");
        let st = gzopen(&path, "wb12").expect("gzopen wb12");
        assert_eq!(st.level, 2);
        drop(st);
        let _ = std::fs::remove_file(&path);
    }

    // ---- gzdopen ------------------------------------------------------------

    #[test]
    fn gzdopen_adopts_an_open_file() {
        let path = unique_temp_path("dopen");
        std::fs::write(&path, gzip(b"dopen payload")).expect("seed gzip file");
        let file = File::open(&path).expect("open for dopen");
        let st = gzdopen(file, "rb").expect("gzdopen rb");
        assert_eq!(st.mode, Mode::Read);
        // The synthetic identifier C uses for fd-based handles.
        assert!(st.path.starts_with("<fd:") || st.path == "<fd>");
        drop(st);
        let _ = std::fs::remove_file(&path);
    }

    // ---- gzbuffer -----------------------------------------------------------

    #[test]
    fn gzbuffer_sets_want_rounds_min_and_guards_overflow() {
        let path = unique_temp_path("buffer");
        let mut st = gzopen(&path, "wb").expect("gzopen wb");

        // Below the minimum of 8 rounds up.
        assert_eq!(gzbuffer(&mut st, 4), 0);
        assert_eq!(st.want, 8);

        // A normal value is stored verbatim.
        assert_eq!(gzbuffer(&mut st, 4096), 0);
        assert_eq!(st.want, 4096);

        // Overflow guard: a size whose double overflows u32 is rejected.
        assert_eq!(gzbuffer(&mut st, u32::MAX), -1);
        assert_eq!(gzbuffer(&mut st, 0x8000_0000), -1);

        // Once the buffers are allocated (size != 0), further calls are rejected.
        st.size = 100;
        assert_eq!(gzbuffer(&mut st, 64), -1);

        drop(st);
        let _ = std::fs::remove_file(&path);
    }

    // ---- gz_reset -----------------------------------------------------------

    #[test]
    fn gz_reset_clears_read_state() {
        let path = make_empty_file("reset");
        let mut st = gzopen(&path, "rb").expect("gzopen rb");

        // Dirty every field gz_reset is responsible for.
        st.have = 5;
        st.pos = 99;
        st.skip = 10;
        st.eof = true;
        st.past = true;
        st.how = How::Gzip;
        st.junk = 0;
        st.again = true;
        st.in_avail = 7;
        st.gz_error(Z_DATA_ERROR, Some("dirty"));

        gz_reset(&mut st);

        assert_eq!(st.have, 0);
        assert_eq!(st.pos, 0);
        assert_eq!(st.skip, 0);
        assert!(!st.eof);
        assert!(!st.past);
        assert_eq!(st.how, How::Look);
        assert_eq!(st.junk, -1);
        assert!(!st.again);
        assert_eq!(st.in_avail, 0);
        assert_eq!(st.err, Z_OK);
        assert_eq!(st.msg, None);

        drop(st);
        let _ = std::fs::remove_file(&path);
    }

    // ---- status accessors ---------------------------------------------------

    #[test]
    fn fresh_read_handle_status_is_clean() {
        let path = make_empty_file("status");
        let st = gzopen(&path, "rb").expect("gzopen rb");
        assert_eq!(gztell(&st), 0);
        assert_eq!(gztell64(&st), 0);
        assert!(!gzeof(&st));
        let (err, msg) = gzerror(&st);
        assert_eq!(err, Z_OK);
        assert_eq!(msg, None);
        drop(st);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn gzeof_returns_past_not_eof() {
        // Read handle: gzeof mirrors `past`, not `eof`.
        let path = make_empty_file("eof_read");
        let mut st = gzopen(&path, "rb").expect("gzopen rb");
        st.eof = true; // at EOF...
        assert!(!gzeof(&st)); // ...but no read PAST end yet -> false
        st.past = true;
        assert!(gzeof(&st)); // a read past end occurred -> true
        drop(st);
        let _ = std::fs::remove_file(&path);

        // Write handle: gzeof is always false, even if `past` is set.
        let path2 = unique_temp_path("eof_write");
        let mut stw = gzopen(&path2, "wb").expect("gzopen wb");
        stw.past = true;
        assert!(!gzeof(&stw));
        drop(stw);
        let _ = std::fs::remove_file(&path2);
    }

    #[test]
    fn gzerror_formats_message_and_clears() {
        let path = make_empty_file("error");
        let mut st = gzopen(&path, "rb").expect("gzopen rb");

        st.gz_error(Z_DATA_ERROR, Some("invalid block"));
        let (code, msg) = gzerror(&st);
        assert_eq!(code, Z_DATA_ERROR);
        assert!(msg.expect("message present").ends_with(": invalid block"));

        // gzclearerr resets the error and the eof/past flags for a read handle.
        st.eof = true;
        st.past = true;
        gzclearerr(&mut st);
        let (code2, msg2) = gzerror(&st);
        assert_eq!(code2, Z_OK);
        assert_eq!(msg2, None);
        assert!(!st.eof);
        assert!(!st.past);

        drop(st);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn gzerror_reports_out_of_memory_literal() {
        let path = make_empty_file("mem");
        let mut st = gzopen(&path, "rb").expect("gzopen rb");
        // Z_MEM_ERROR does not store a message; gzerror returns the literal.
        st.gz_error(Z_MEM_ERROR, Some("this text is ignored"));
        let (code, msg) = gzerror(&st);
        assert_eq!(code, Z_MEM_ERROR);
        assert_eq!(msg, Some("out of memory"));
        drop(st);
        let _ = std::fs::remove_file(&path);
    }

    // ---- gzseek skip arming (no I/O) ---------------------------------------

    #[test]
    fn gzseek_rejects_seek_end_and_backward_write() {
        let path = unique_temp_path("seek_guard");
        let mut st = gzopen(&path, "wb").expect("gzopen wb");
        // SEEK_END is unsupported.
        assert_eq!(gzseek(&mut st, 0, SEEK_END), -1);
        // A write stream cannot seek backwards (absolute target < 0).
        assert_eq!(gzseek(&mut st, -5, SEEK_SET), -1);
        drop(st);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn gzseek_set_forward_arms_skip_and_updates_tell() {
        let path = unique_temp_path("seek_arm");
        let mut st = gzopen(&path, "wb").expect("gzopen wb");
        // Absolute forward seek to 100 from pos 0: arms skip, returns 100.
        assert_eq!(gzseek(&mut st, 100, SEEK_SET), 100);
        assert_eq!(st.skip, 100);
        // gztell counts the pending skip.
        assert_eq!(gztell(&st), 100);
        // A relative seek folds the pending skip in: +50 -> total 150.
        assert_eq!(gzseek(&mut st, 50, SEEK_CUR), 150);
        assert_eq!(st.skip, 150);
        assert_eq!(gztell(&st), 150);
        drop(st);
        let _ = std::fs::remove_file(&path);
    }

    // ---- positioning round-trips (integration with read path) --------------

    #[test]
    fn seek_set_then_read_returns_bytes_from_offset() {
        let data = b"0123456789ABCDEFGHIJ".repeat(50); // 1000 bytes
        let path = unique_temp_path("seek_read");
        std::fs::write(&path, gzip(&data)).expect("seed gzip file");

        let mut st = gzopen(&path, "rb").expect("gzopen rb");
        let k: i64 = 123;
        assert_eq!(gzseek(&mut st, k, SEEK_SET), k);
        assert_eq!(gztell(&st), k);

        let mut buf = [0u8; 10];
        let n = crate::gz::read::gzread(&mut st, &mut buf);
        assert_eq!(n, 10);
        let off = k as usize;
        assert_eq!(&buf[..], &data[off..off + 10]);
        // After consuming 10 bytes, the position has advanced.
        assert_eq!(gztell(&st), k + 10);

        drop(st);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn gzrewind_returns_to_start() {
        let data = b"abcdefghij".repeat(20); // 200 bytes
        let path = unique_temp_path("rewind");
        std::fs::write(&path, gzip(&data)).expect("seed gzip file");

        let mut st = gzopen(&path, "rb").expect("gzopen rb");
        let mut buf = [0u8; 50];
        assert_eq!(crate::gz::read::gzread(&mut st, &mut buf), 50);
        assert_eq!(gztell(&st), 50);

        assert_eq!(gzrewind(&mut st), 0);
        assert_eq!(gztell(&st), 0);

        let mut buf2 = [0u8; 10];
        assert_eq!(crate::gz::read::gzread(&mut st, &mut buf2), 10);
        assert_eq!(&buf2[..], &data[..10]);

        drop(st);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn gzeof_true_only_after_reading_past_end() {
        let data = b"hello world";
        let path = unique_temp_path("eof_past");
        std::fs::write(&path, gzip(data)).expect("seed gzip file");

        let mut st = gzopen(&path, "rb").expect("gzopen rb");
        let mut buf = [0u8; 11];
        assert_eq!(crate::gz::read::gzread(&mut st, &mut buf), 11);
        assert_eq!(&buf[..], data);
        // At exact EOF, gzeof is still false (no read past end yet).
        assert!(!gzeof(&st));

        // Reading again returns 0 and trips `past`.
        let mut buf2 = [0u8; 4];
        assert_eq!(crate::gz::read::gzread(&mut st, &mut buf2), 0);
        assert!(gzeof(&st));

        drop(st);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn gzrewind_rejected_for_write_handle() {
        let path = unique_temp_path("rewind_write");
        let mut st = gzopen(&path, "wb").expect("gzopen wb");
        assert_eq!(gzrewind(&mut st), -1); // only valid for reading
        drop(st);
        let _ = std::fs::remove_file(&path);
    }
}
