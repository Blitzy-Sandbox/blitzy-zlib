//! gzip **open / positioning / status** API, ported from zlib `gzlib.c`
//! (plus the `gzsetparams` body, which is delegated to [`crate::gz::write`]
//! because its implementation lives in `gzwrite.c`).
//!
//! This module is the *policy and parsing* layer of the gzip file interface.
//! It owns:
//!
//! * the shared `gz_open` constructor with its **mode-string parser**
//!   (`gzopen` / `gzopen64` / `gzdopen`);
//! * buffer sizing ([`gzbuffer`]);
//! * positioning ([`gzrewind`], [`gzseek`] / [`gzseek64`], [`gztell`] /
//!   [`gztell64`], [`gzoffset`] / [`gzoffset64`]);
//! * status ([`gzeof`], [`gzerror`], [`gzclearerr`]);
//! * the [`gzsetparams`] wrapper.
//!
//! # What this module does NOT do
//!
//! It never touches the DEFLATE / INFLATE engine directly — engine
//! initialisation happens **lazily** in [`crate::gz::write`]'s `gz_init`
//! (`deflateInit2` with `windowBits = 31`, the gzip wrapper) and
//! [`crate::gz::read`]'s `gz_look` (`inflateInit2`). `open.rs` only records the
//! requested `level` / `strategy` / `mode` / `direct` and opens the underlying
//! [`File`], exactly as C `gz_open` (`gzlib.c`) only sets those fields and calls
//! `open()`.
//!
//! Likewise, [`gzseek`] does **not** perform any data movement: it computes the
//! target position and records a pending forward skip in
//! [`GzState::skip`](crate::gz::state). The actual skip is performed lazily by
//! the next read/write through [`crate::gz::read`]'s `gz_skip` (when reading) or
//! [`crate::gz::write`]'s `gz_zero` (when writing), which both test
//! `state.skip != 0`. This deferral is reproduced verbatim from C
//! (`gzread.c` / `gzwrite.c`).
//!
//! # Mode-string contract (frozen — AAP §0.6.1, §0.7.1)
//!
//! Every character of the mode string is significant and is parsed exactly as
//! C `gz_open` (`gzlib.c` L113-171) does — see `gz_open` for the full table.
//!
//! # No `unsafe`
//!
//! This file contains **no `unsafe`** (AAP §0.6.2 confines `unsafe` to
//! `src/ffi.rs` and `src/inflate/fast.rs`). The raw-file-descriptor `gzdopen`
//! path (`File::from_raw_fd`) and the C-string `char*` marshalling of the
//! `path` / `mode` arguments are the FFI layer's concern: `ffi.rs` performs the
//! `unsafe { File::from_raw_fd(fd) }` and then calls the safe [`gzdopen`] here.
//! The OS-level open flags `O_EXCL` / `O_NONBLOCK` are applied through the
//! **safe** [`OpenOptions`] / [`OpenOptionsExt::custom_flags`](std::os::unix::fs::OpenOptionsExt::custom_flags) surface, and
//! `O_CLOEXEC` ('e') is the Rust standard-library default for every opened file.
//!
//! # Feature gating
//!
//! The whole `gz` folder is gated by the `gz-io` Cargo feature (which implies
//! `std` + `gzip`) at the `pub mod gz` site in `lib.rs`, so this module uses
//! `std` freely with no per-file `#![cfg(...)]` guard.

use std::fs::{File, OpenOptions};
use std::io::{self, Seek, SeekFrom};
use std::path::Path;

use crate::constants::{
    SEEK_CUR, SEEK_SET, Z_BUF_ERROR, Z_DEFAULT_COMPRESSION, Z_DEFAULT_STRATEGY, Z_FILTERED,
    Z_FIXED, Z_HUFFMAN_ONLY, Z_MEM_ERROR, Z_OK, Z_RLE,
};
use crate::gz::read::GzReader;
use crate::gz::state::{GzState, How, Mode};
use crate::gz::write;

// ===========================================================================
// O_NONBLOCK — platform flag for the 'N' mode character (gzlib.c L159-163)
// ===========================================================================
//
// The C `gz_open` honours 'N' by OR-ing `O_NONBLOCK` into the `open()` flags.
// The crate intentionally takes **no `libc` dependency** (its only runtime deps
// are `crc32fast` and `cfg-if`), so the platform value is provided here as a
// `const` selected by target OS. `O_NONBLOCK` is stable and well-known on the
// mainstream Unix targets; on an unrecognised Unix we fall back to `0`, which
// makes honouring 'N' a no-op there (best effort, exactly the spirit of the C
// `#ifdef O_NONBLOCK` guard, which also compiles 'N' out where the flag is
// unavailable). 'N' on a *regular file* has no effect on read/write semantics
// anyway — it matters only for FIFOs / devices — so a no-op fallback is benign.
//
// 'e' (`O_CLOEXEC`) needs no analogous constant: Rust's standard library opens
// every file close-on-exec by default, so the 'e' request is already satisfied.

#[cfg(unix)]
fn nonblock_flag() -> i32 {
    cfg_if::cfg_if! {
        if #[cfg(any(target_os = "linux", target_os = "android"))] {
            0o4000
        } else if #[cfg(any(
            target_os = "macos",
            target_os = "ios",
            target_os = "tvos",
            target_os = "watchos",
            target_os = "freebsd",
            target_os = "dragonfly",
            target_os = "netbsd",
            target_os = "openbsd",
        ))] {
            0x0004
        } else {
            0
        }
    }
}

// ===========================================================================
// GzSource — the safe input to `gz_open` (decouples raw-fd `unsafe` into ffi.rs)
// ===========================================================================

/// The source a gzip stream is opened from — the safe twin of the C
/// `gz_open(path, fd, mode)` overload.
///
/// C's `gz_open` accepts *either* a path (`gzopen`, `fd == -1`) *or* a raw file
/// descriptor (`gzdopen`). Adopting a raw `fd` in Rust requires
/// `File::from_raw_fd`, which is `unsafe`; that call therefore lives in
/// `src/ffi.rs` (the designated `unsafe` boundary), which wraps the descriptor
/// into an owned [`File`] and hands it here as [`GzSource::File`]. The safe core
/// thus never performs an `unsafe` operation.
pub enum GzSource<'a> {
    /// Open the named path (the `gzopen` / `gzopen64` case; C `fd == -1`).
    Path(&'a Path),
    /// Adopt an already-open [`File`] (the `gzdopen` case). The FFI layer builds
    /// this from a raw descriptor via `File::from_raw_fd`.
    File(File),
}

// ===========================================================================
// gz_reset — reset gzip file state (port of `gz_reset`, gzlib.c L68-84)
// ===========================================================================

/// Reset the internal stream state to its freshly-opened condition — port of C
/// `gz_reset` (`gzlib.c` L68-84). Called by [`gz_open`] at open time and by
/// [`gzrewind`] / the in-file [`gzseek`] fast path after repositioning.
///
/// Mirrors the C source field-for-field:
///
/// * `x.have = 0` — no buffered output is available;
/// * **reading** resets `eof` / `past` to `false`, sets `how = LOOK` (so the
///   next read re-sniffs for a gzip header) and `junk = -1` (the
///   "first member" marker);
/// * **writing** clears the pending-`deflateReset` flag (`reset = false`);
/// * `again = false`, `skip = 0` (no stalled I/O, no pending seek);
/// * the error state is cleared (`gz_error(Z_OK, NULL)`);
/// * `x.pos = 0` and the buffered input is dropped (`strm.avail_in = 0`, which
///   in this port is the [`in_avail`](crate::gz::state) cursor — the input
///   index [`in_next`](crate::gz::state) is reset alongside it, matching the way
///   C `gz_avail` resets `next_in` to the start of the buffer whenever
///   `avail_in == 0`).
pub(crate) fn gz_reset(state: &mut GzState) {
    state.have = 0; // no output data available
    if state.mode == Mode::Read {
        // for reading ...
        state.eof = false; // not at end of file
        state.past = false; // have not read past end yet
        state.how = How::Look; // look for gzip header
        state.junk = -1; // mark first member
    } else {
        // for writing ...
        state.reset = false; // no deflateReset pending
    }
    state.again = false; // no stalled i/o yet
    state.skip = 0; // no seek request pending
    state.clear_error(); // clear error (gz_error(state, Z_OK, NULL))
    state.pos = 0; // no uncompressed data yet
    state.in_avail = 0; // no input data yet (C strm.avail_in = 0)
    state.in_next = 0; // input cursor back to the start of the buffer
}

// ===========================================================================
// gz_open — the shared constructor (port of `gz_open`, gzlib.c L86-285)
// ===========================================================================

/// Open a gzip stream from a path or an already-open file — port of C
/// `gz_open` (`gzlib.c` L86-285), the shared constructor behind `gzopen`,
/// `gzopen64`, and `gzdopen`.
///
/// Returns the owned handle (`Box<GzState>`, the safe equivalent of the C
/// `gzFile`) on success, or `None` on any failure — mirroring C, which frees
/// the partially-built `gz_state` and returns `NULL` for every error path.
///
/// # Mode-string parsing (frozen — gzlib.c L108-171)
///
/// Each character of `mode` is parsed exactly as C does; the table is a
/// behavioural contract (AAP §0.6.1):
///
/// | Char        | Effect                                                        |
/// |-------------|---------------------------------------------------------------|
/// | `'0'..='9'` | set compression `level` to the digit                          |
/// | `'r'`       | `mode = Read`                                                 |
/// | `'w'`       | `mode = Write`                                                |
/// | `'a'`       | `mode = Append` (becomes `Write` after seeking to end)        |
/// | `'+'`       | **error** — can't read and write at once → `None`             |
/// | `'b'`       | ignored — binary is always implied                            |
/// | `'e'`       | `O_CLOEXEC` — already the Rust default for opened files        |
/// | `'x'`       | `O_EXCL` — fail if the file already exists (write/append)      |
/// | `'f'`       | `strategy = Z_FILTERED`                                        |
/// | `'h'`       | `strategy = Z_HUFFMAN_ONLY`                                    |
/// | `'R'`       | `strategy = Z_RLE`                                             |
/// | `'F'`       | `strategy = Z_FIXED`                                           |
/// | `'G'`       | `direct = -1` (gzip-only; transparent read disabled)          |
/// | `'N'`       | `O_NONBLOCK`                                                  |
/// | `'T'`       | `direct = 1` (write transparently — store, no compression)    |
/// | any other   | ignored (C `default: ;`)                                      |
///
/// # Post-parse validation (gzlib.c L173-198)
///
/// * no `r`/`w`/`a` given (`mode == None`) → `None`;
/// * reading with `'T'` (`direct == 1`) → `None` (can't force a transparent
///   read); reading with neither `'G'` nor `'T'` (`direct == 0`) defaults to
///   `direct = 1` (auto-detect, transparent assumption for an empty file);
///   reading with `'G'` keeps `direct == -1`;
/// * writing/appending with `'G'` (`direct == -1`) → `None`.
///
/// The `windowBits = 31` gzip-wrapper selection is **not** applied here; it is
/// applied lazily by the write/read engine initialisation (see the
/// [module documentation](self)). This function only records `level` /
/// `strategy` / `mode` / `direct` and opens the file.
/// The result of parsing and validating a gz mode string — the `level` /
/// `strategy` / `mode` / `direct` selections plus the `O_EXCL` / `O_NONBLOCK`
/// open-flag requests. Produced by [`parse_mode`] and consumed by [`gz_open`].
///
/// Extracting this from [`gz_open`] lets the FFI layer validate the mode string
/// **before** it adopts a caller-supplied file descriptor (see
/// [`mode_is_valid`]): C `gz_open` never closes a provided `fd` on a parse
/// failure, so the descriptor must not be wrapped in an owning [`File`] until
/// the mode is known to be valid.
struct ParsedMode {
    want_mode: Mode,
    level: i32,
    strategy: i32,
    direct: i32,
    exclusive: bool,
    nonblock: bool,
}

/// Parse and validate a gz mode string, returning `None` for any rejected
/// combination exactly as C `gz_open` (`gzlib.c` L108-198) does. This is the
/// single source of truth for the mode contract documented on [`gz_open`].
///
/// Returning `None` here corresponds to every C `free(state); return NULL;`
/// parse-failure path; crucially it performs **no I/O and adopts no
/// descriptor**, so a caller can probe mode validity without side effects.
fn parse_mode(mode: &str) -> Option<ParsedMode> {
    // --- interpret mode (gzlib.c L108-171) ---
    let mut want_mode = Mode::None;
    let mut level = Z_DEFAULT_COMPRESSION;
    let mut strategy = Z_DEFAULT_STRATEGY;
    let mut direct: i32 = 0;
    let mut exclusive = false; // 'x' -> O_EXCL
    let mut nonblock = false; // 'N' -> O_NONBLOCK

    // C iterates raw bytes; the mode characters are ASCII, so byte iteration is
    // an exact match and ignores any stray non-ASCII bytes (C `default: ;`).
    for &b in mode.as_bytes() {
        if b.is_ascii_digit() {
            level = i32::from(b - b'0');
        } else {
            match b {
                b'r' => want_mode = Mode::Read,
                b'w' => want_mode = Mode::Write,
                b'a' => want_mode = Mode::Append,
                // '+' : can't read and write at the same time (C frees + NULL).
                b'+' => return None,
                // 'b' : ignore -- binary is requested anyway.
                b'b' => {}
                // 'e' : O_CLOEXEC. Rust's std opens every file close-on-exec by
                // default, so this request is already satisfied -- nothing to do.
                b'e' => {}
                // 'x' : O_EXCL -- fail if the file already exists.
                b'x' => exclusive = true,
                b'f' => strategy = Z_FILTERED,
                b'h' => strategy = Z_HUFFMAN_ONLY,
                b'R' => strategy = Z_RLE,
                b'F' => strategy = Z_FIXED,
                // 'G' : gzip-only read (disable transparent fallback).
                b'G' => direct = -1,
                // 'N' : O_NONBLOCK.
                b'N' => nonblock = true,
                // 'T' : write transparently (store, no compression).
                b'T' => direct = 1,
                // could consider any other char an error, but just ignore it.
                _ => {}
            }
        }
    }

    // --- must provide an "r", "w", or "a" (gzlib.c L173-177) ---
    if want_mode == Mode::None {
        return None;
    }

    // --- direct is 0, 1 if "T", or -1 if "G" (last "G"/"T" wins); validate the
    //     read/write combinations (gzlib.c L179-198) ---
    if want_mode == Mode::Read {
        if direct == 1 {
            // can't force a transparent read
            return None;
        }
        if direct == 0 {
            // default when reading is auto-detect of gzip vs. transparent --
            // start with a transparent assumption in case of an empty file
            direct = 1;
        }
        // direct == -1 ("G") is kept: gzip-only.
    } else if direct == -1 {
        // "G" has no meaning when writing -- disallow it
        return None;
    }

    Some(ParsedMode {
        want_mode,
        level,
        strategy,
        direct,
        exclusive,
        nonblock,
    })
}

/// Returns `true` if `mode` is a valid gz mode string.
///
/// The FFI `gzdopen` shim calls this **before** `File::from_raw_fd` so that an
/// invalid mode is rejected without ever adopting (and therefore without ever
/// closing) the caller's descriptor — matching C `gz_open`, which never closes
/// a provided `fd` on a mode-parse failure. Pure-Rust callers do not need it
/// because they hand `gz_open` an owned [`File`]/path directly.
#[must_use]
pub fn mode_is_valid(mode: &str) -> bool {
    parse_mode(mode).is_some()
}

#[must_use]
pub(crate) fn gz_open(source: GzSource<'_>, mode: &str) -> Option<Box<GzState>> {
    // Parse and validate the mode string up front (the single source of truth
    // is `parse_mode`). On any rejected combination this returns `None` having
    // performed no I/O and — critically — without consuming `source`, so a
    // caller-supplied descriptor inside `source` is never adopted/closed on a
    // parse failure (matching C `gz_open`'s `free(state); return NULL;`).
    let ParsedMode {
        want_mode,
        level,
        strategy,
        direct,
        exclusive,
        nonblock,
    } = parse_mode(mode)?;

    // --- open the file (or adopt the supplied one) and record its identifier
    //     for error messages (gzlib.c L199-268) ---
    let (mut file, path) = match source {
        GzSource::Path(p) => {
            // C: open failure frees the state and returns NULL (the error is
            // unobservable through the returned handle), so we simply return
            // None without constructing a state to hold it.
            let opened = open_path(p, want_mode, exclusive, nonblock).ok()?;
            (opened, p.to_string_lossy().into_owned())
        }
        // gzdopen: adopt the already-open File. The O_EXCL / O_NONBLOCK request
        // bits only affect a fresh open(), so they do not apply to an adopted
        // descriptor (matching the practical effect: the descriptor is already
        // open). The FFI layer may set those on the fd before handing it over.
        GzSource::File(f) => {
            let id = fd_identifier(&f);
            (f, id)
        }
    };

    // --- append: seek to end so gzoffset() is correct, then simplify later
    //     checks by becoming a plain write stream (gzlib.c L269-272) ---
    let final_mode = if want_mode == Mode::Append {
        // C ignores the LSEEK result here; so do we (best effort).
        let _ = file.seek(SeekFrom::End(0));
        Mode::Write
    } else {
        want_mode
    };

    // --- build the state and record the parsed parameters ---
    let mut state = GzState::new(file, path, final_mode);
    state.level = level;
    state.strategy = strategy;
    state.direct = direct;

    // --- save the current position for rewinding (only if reading)
    //     (gzlib.c L274-278): C uses LSEEK(fd, 0, SEEK_CUR), falling back to 0
    //     on error. ---
    if final_mode == Mode::Read {
        let start = state
            .file
            .as_mut()
            .and_then(|f| f.stream_position().ok())
            .map_or(0, |p| p as i64);
        state.start = start;
    }

    // --- initialize stream (gzlib.c L280-281) ---
    gz_reset(&mut state);

    // --- return stream (gzlib.c L283-284) ---
    Some(Box::new(state))
}

// ===========================================================================
// Private file-open helpers (the safe `OpenOptions` translation of the C
// `open()`-flag computation, gzlib.c L228-263).
// ===========================================================================

/// Build the [`OpenOptions`] for `mode` and open `path` — the safe translation
/// of C `gz_open`'s `oflag` computation (`gzlib.c` L228-248).
///
/// * read  → `O_RDONLY`                              → `.read(true)`
/// * write → `O_WRONLY | O_CREAT | O_TRUNC`          → `.write(true).create(true).truncate(true)`
/// * append→ `O_WRONLY | O_CREAT | O_APPEND`         → `.append(true).create(true)`
/// * `'x'` → adds `O_EXCL`                           → `.create_new(true)` (replaces create+truncate)
/// * `'N'` → adds `O_NONBLOCK`                        → [`apply_nonblock`] (`custom_flags`, unix)
fn open_path(path: &Path, mode: Mode, exclusive: bool, nonblock: bool) -> io::Result<File> {
    let mut opts = OpenOptions::new();
    match mode {
        Mode::Read => {
            opts.read(true);
        }
        Mode::Write => {
            opts.write(true);
            if exclusive {
                // O_CREAT | O_EXCL: create, failing if it already exists. A new
                // file is empty, so O_TRUNC is moot (and std forbids combining
                // create_new with truncate).
                opts.create_new(true);
            } else {
                opts.create(true).truncate(true);
            }
        }
        Mode::Append => {
            opts.append(true);
            if exclusive {
                opts.create_new(true);
            } else {
                opts.create(true);
            }
        }
        // gz_open never calls this with Mode::None (rejected during validation).
        Mode::None => unreachable!("open_path called with Mode::None"),
    }
    apply_nonblock(&mut opts, nonblock);
    opts.open(path)
}

/// Apply `O_NONBLOCK` ('N') through the **safe** `custom_flags` extension on
/// Unix; a no-op on non-Unix targets (and on Unix targets where the flag value
/// is unknown — see [`nonblock_flag`]).
#[cfg(unix)]
fn apply_nonblock(opts: &mut OpenOptions, nonblock: bool) {
    use std::os::unix::fs::OpenOptionsExt;
    if nonblock {
        let flag = nonblock_flag();
        if flag != 0 {
            opts.custom_flags(flag);
        }
    }
}

/// Non-Unix stub: the `O_NONBLOCK` flag has no portable analogue, so 'N' is a
/// no-op (it has no effect on regular-file I/O anyway).
#[cfg(not(unix))]
fn apply_nonblock(_opts: &mut OpenOptions, _nonblock: bool) {}

/// The error-message identifier for an adopted descriptor — C `gzdopen` formats
/// `"<fd:%d>"` (`gzlib.c` L304-307). Uses the **safe** [`AsRawFd`] accessor.
#[cfg(unix)]
fn fd_identifier(file: &File) -> String {
    use std::os::unix::io::AsRawFd;
    format!("<fd:{}>", file.as_raw_fd())
}

/// Non-Unix fallback identifier (no raw descriptor concept to format).
#[cfg(not(unix))]
fn fd_identifier(_file: &File) -> String {
    String::from("<fd>")
}

// ===========================================================================
// Public open functions (port of `gzopen` / `gzopen64` / `gzdopen`,
// gzlib.c L287-312).
// ===========================================================================

/// Open the gzip file at `path` with the given `mode` string — port of C
/// `gzopen` (`gzlib.c` L287-290).
///
/// Returns the owned handle, or `None` if the mode string is invalid or the
/// file cannot be opened. See `gz_open` for the full mode-string contract.
///
/// # Examples
///
/// ```no_run
/// use std::path::Path;
/// use zlib_rs::gz::gzopen;
///
/// // Open for writing at best compression with the filtered strategy.
/// let handle = gzopen(Path::new("out.gz"), "wb9f");
/// assert!(handle.is_some());
/// ```
#[must_use]
pub fn gzopen(path: &Path, mode: &str) -> Option<Box<GzState>> {
    gz_open(GzSource::Path(path), mode)
}

/// Open the gzip file at `path` — port of C `gzopen64` (`gzlib.c` L292-295).
///
/// On platforms with 64-bit file offsets this is identical to [`gzopen`]; this
/// port keeps all positions as `i64` ([`GzState::pos`](crate::gz::state) /
/// `start` / `skip` are `i64`) throughout, so `gzopen64` and [`gzopen`] are the
/// same function. The distinct name is preserved for C-API parity (the FFI
/// shim exports both symbols).
#[must_use]
pub fn gzopen64(path: &Path, mode: &str) -> Option<Box<GzState>> {
    gz_open(GzSource::Path(path), mode)
}

/// Adopt an already-open [`File`] as a gzip stream — port of C `gzdopen`
/// (`gzlib.c` L297-312).
///
/// C `gzdopen` takes a raw file descriptor and formats a `"<fd:N>"` identifier
/// for error messages. Because adopting a raw descriptor requires the `unsafe`
/// `File::from_raw_fd`, that conversion lives in `src/ffi.rs`; the FFI shim
/// performs `unsafe { File::from_raw_fd(fd) }` and then calls this **safe**
/// entry point with the resulting owned [`File`].
///
/// Returns the owned handle, or `None` if the mode string is invalid. See
/// `gz_open` for the mode-string contract.
#[must_use]
pub fn gzdopen(file: File, mode: &str) -> Option<Box<GzState>> {
    gz_open(GzSource::File(file), mode)
}

// ===========================================================================
// gzbuffer — set the internal buffer size (port of `gzbuffer`, gzlib.c L321-343)
// ===========================================================================

/// Set the internal buffer size used by subsequent reads/writes — port of C
/// `gzbuffer` (`gzlib.c` L321-343). Returns `0` on success, `-1` on error.
///
/// Constraints, exactly as C:
///
/// * the stream must be open for reading or writing;
/// * it may only be called **before** the first read/write — once the buffers
///   have been allocated ([`size`](crate::gz::state) `!= 0`) it returns `-1`;
/// * the request must not overflow when doubled (the C `(size << 1) < size`
///   unsigned guard — reproduced with `u32` wrapping semantics; widening to a
///   larger integer first would defeat the guard);
/// * a request below `8` is rounded up to `8` (needed for correct flushing).
///
/// On success the requested size is stored in [`want`](crate::gz::state); the
/// actual allocation happens at the first I/O (in `gz_look` / `gz_init`).
pub fn gzbuffer(state: &mut GzState, size: u32) -> i32 {
    // get internal structure and check integrity (mode must be read or write)
    if state.mode != Mode::Read && state.mode != Mode::Write {
        return -1;
    }

    // make sure we haven't already allocated memory
    if state.size != 0 {
        return -1;
    }

    // check and set requested size. C: `if ((size << 1) < size) return -1;` --
    // an unsigned overflow guard that rejects sizes >= 2^31 (where doubling
    // wraps). `wrapping_shl(1)` reproduces the exact 32-bit wraparound.
    if size.wrapping_shl(1) < size {
        return -1; // need to be able to double it
    }
    let size = if size < 8 { 8 } else { size }; // needed to behave well with flushing

    state.want = size as usize;
    0
}

// ===========================================================================
// gzrewind — rewind a read stream (port of `gzrewind`, gzlib.c L345-364)
// ===========================================================================

/// Rewind a gzip read stream to the start of its compressed data — port of C
/// `gzrewind` (`gzlib.c` L345-364). Returns `0` on success, `-1` on error.
///
/// Valid only for a read stream with no latched fatal error (`err` is `Z_OK`
/// or `Z_BUF_ERROR`). Seeks the underlying file back to the recorded
/// [`start`](crate::gz::state) offset and calls `gz_reset`, which sets
/// `how = LOOK` so the next read re-sniffs the gzip header (the inflate engine
/// is re-initialised lazily by `gz_look` — matching C, which performs no
/// explicit `inflateReset` here).
pub fn gzrewind(state: &mut GzState) -> i32 {
    // check that we're reading and that there's no error
    if state.mode != Mode::Read || (state.err != Z_OK && state.err != Z_BUF_ERROR) {
        return -1;
    }

    // back up and start over
    let start = state.start;
    let Some(file) = state.file.as_mut() else {
        return -1;
    };
    if file.seek(SeekFrom::Start(start as u64)).is_err() {
        return -1;
    }
    gz_reset(state);
    0
}

// ===========================================================================
// gzseek / gzseek64 — set the stream position (port of `gzseek64` / `gzseek`,
// gzlib.c L366-443)
// ===========================================================================

/// Set the position of the next read/write — port of C `gzseek64` / `gzseek`
/// (`gzlib.c` L366-443). Returns the resulting uncompressed offset, or `-1` on
/// error.
///
/// This is the unified 32-/64-bit implementation: positions throughout this
/// port are `i64`, so `gzseek` and [`gzseek64`] are the same function (C's
/// `gzseek` merely range-checks the 64-bit result against `z_off_t`, which is a
/// no-op when both widths are 64-bit).
///
/// Behaviour, exactly as C:
///
/// * the stream must be open for reading or writing, with no latched fatal
///   error, and `whence` must be [`SEEK_SET`] or [`SEEK_CUR`] (no `SEEK_END`);
/// * the offset is normalised to a `SEEK_CUR`-relative value (folding in any
///   pending [`skip`](crate::gz::state) for `SEEK_CUR`);
/// * **fast in-file path** — if reading transparently ([`How::Copy`]) and the
///   target is in range, the underlying file is `lseek`'d directly and the
///   buffered output discarded;
/// * a **backward** seek is only allowed when reading: the file is
///   [`gzrewind`]ed and the seek becomes a forward skip from the start;
/// * any already-buffered output that the forward skip covers is consumed
///   immediately (so a later [`crate::gz::read::gzgetc`] needs one fewer check);
/// * the remaining distance is recorded as a pending forward skip in
///   [`skip`](crate::gz::state) — the actual data movement is deferred to the
///   next read/write (`gz_skip` when reading, `gz_zero` when writing), both of
///   which test `skip != 0`.
pub fn gzseek(state: &mut GzState, mut offset: i64, whence: i32) -> i64 {
    // check integrity: must be open for reading or writing
    if state.mode != Mode::Read && state.mode != Mode::Write {
        return -1;
    }

    // check that there's no error
    if state.err != Z_OK && state.err != Z_BUF_ERROR {
        return -1;
    }

    // can only seek from start or relative to current position
    if whence != SEEK_SET && whence != SEEK_CUR {
        return -1;
    }

    // normalize offset to a SEEK_CUR specification
    if whence == SEEK_SET {
        offset -= state.pos;
    } else {
        // SEEK_CUR: fold in any pending (not-yet-performed) forward skip
        offset += if state.past { 0 } else { state.skip };
        state.skip = 0;
    }

    // if within the raw (transparent) area while reading, just go there
    if state.mode == Mode::Read && state.how == How::Copy && state.pos + offset >= 0 {
        // The file pointer is `have` uncompressed bytes ahead of the logical
        // position (transparent copy => compressed == uncompressed), so move it
        // by `offset - have` from the current file position.
        let delta = offset - state.have as i64;
        let Some(file) = state.file.as_mut() else {
            return -1;
        };
        if file.seek(SeekFrom::Current(delta)).is_err() {
            return -1;
        }
        state.have = 0;
        state.eof = false;
        state.past = false;
        state.skip = 0;
        state.clear_error();
        state.in_avail = 0; // C: strm.avail_in = 0 (drop buffered input)
        state.pos += offset;
        return state.pos;
    }

    // calculate skip amount, rewinding if needed for a back seek when reading
    if offset < 0 {
        if state.mode != Mode::Read {
            // writing -- can't go backwards
            return -1;
        }
        offset += state.pos;
        if offset < 0 {
            // before start of file!
            return -1;
        }
        if gzrewind(state) == -1 {
            // rewind, then skip to offset
            return -1;
        }
    }

    // if reading, skip what's already in the output buffer (one less gzgetc()
    // check). GT_OFF collapses in this port (i64 pos, usize have), so this is
    // simply n = min(have, offset).
    if state.mode == Mode::Read {
        let n: usize = if (state.have as i64) > offset {
            offset as usize
        } else {
            state.have
        };
        state.have -= n;
        state.next += n;
        state.pos += n as i64;
        offset -= n as i64;
    }

    // request skip (if not zero); performed lazily by the next read/write
    state.skip = offset;
    state.pos + offset
}

/// Set the stream position — port of C `gzseek64` (`gzlib.c` L366-435).
///
/// Identical to [`gzseek`] in this port (all positions are `i64`); the distinct
/// name is preserved for C-API parity.
#[inline]
pub fn gzseek64(state: &mut GzState, offset: i64, whence: i32) -> i64 {
    gzseek(state, offset, whence)
}

// ===========================================================================
// gztell / gztell64 — current uncompressed position (port of `gztell64` /
// `gztell`, gzlib.c L445-466)
// ===========================================================================

/// Return the current uncompressed position in the stream — port of C
/// `gztell64` / `gztell` (`gzlib.c` L445-466). Returns `-1` if the handle is
/// not a valid read/write stream.
///
/// The reported position includes any *pending* forward seek that has not yet
/// been performed: `pos + (past ? 0 : skip)`. Unified for 32/64-bit (positions
/// are `i64`); see [`gztell64`].
#[must_use]
pub fn gztell(state: &GzState) -> i64 {
    // check integrity: must be open for reading or writing
    if state.mode != Mode::Read && state.mode != Mode::Write {
        return -1;
    }

    // return position (folding in a not-yet-performed forward skip)
    state.pos + if state.past { 0 } else { state.skip }
}

/// Return the current uncompressed position — port of C `gztell64`
/// (`gzlib.c` L445-458). Identical to [`gztell`] in this port.
#[inline]
#[must_use]
pub fn gztell64(state: &GzState) -> i64 {
    gztell(state)
}

// ===========================================================================
// gzoffset / gzoffset64 — current compressed file offset (port of
// `gzoffset64` / `gzoffset`, gzlib.c L468-495)
// ===========================================================================

/// Return the current **compressed** offset in the underlying file — port of C
/// `gzoffset64` / `gzoffset` (`gzlib.c` L468-495). Returns `-1` on error.
///
/// This is the file's current byte offset (`lseek(fd, 0, SEEK_CUR)`); when
/// reading, the bytes already buffered but not yet consumed by the inflate
/// engine are subtracted so the result reflects the position of the data the
/// caller has actually seen. In this port that buffered-input count is the
/// [`in_avail`](crate::gz::state) cursor (the engine's own `avail_in` is only a
/// transient per-call value), so it is what gets subtracted — the faithful
/// analogue of the C `offset -= state->strm.avail_in`.
pub fn gzoffset(state: &mut GzState) -> i64 {
    // check integrity: must be open for reading or writing
    if state.mode != Mode::Read && state.mode != Mode::Write {
        return -1;
    }

    // compute the effective offset in the file
    let pos = match state.file.as_mut() {
        Some(file) => match file.stream_position() {
            Ok(p) => p as i64,
            Err(_) => return -1,
        },
        None => return -1,
    };

    // reading: don't count input buffered but not yet consumed by the engine
    if state.mode == Mode::Read {
        pos - state.in_avail as i64
    } else {
        pos
    }
}

/// Return the current compressed file offset — port of C `gzoffset64`
/// (`gzlib.c` L468-487). Identical to [`gzoffset`] in this port.
#[inline]
pub fn gzoffset64(state: &mut GzState) -> i64 {
    gzoffset(state)
}

// ===========================================================================
// gzeof — end-of-file status (port of `gzeof`, gzlib.c L497-510)
// ===========================================================================

/// Report whether a read PAST the end of the uncompressed data has been
/// attempted — port of C `gzeof` (`gzlib.c` L497-510).
///
/// This is **not** "are we at the last byte" — it returns
/// [`past`](crate::gz::state), which becomes `true` only once a read is
/// requested beyond the end of the data (the classic zlib subtlety: `gzeof` is
/// `false` while sitting exactly at EOF, and turns `true` only after a further
/// read fails to deliver any bytes). For a write stream — or a non-read/write
/// handle — it is always `false`.
#[must_use]
pub fn gzeof(state: &GzState) -> bool {
    // a non-read/write handle is never "past end"
    if state.mode != Mode::Read && state.mode != Mode::Write {
        return false;
    }

    // end-of-file state: only meaningful (and only ever true) when reading
    state.mode == Mode::Read && state.past
}

// ===========================================================================
// gzerror — last error code + message (port of `gzerror`, gzlib.c L512-528)
// ===========================================================================

/// Return the last error code and message recorded on the stream — port of C
/// `gzerror` (`gzlib.c` L512-528).
///
/// The idiomatic form returns a `(code, message)` tuple instead of writing
/// through an `int*` and returning a `char*`; the FFI shim turns the message
/// into a NUL-terminated C string and writes the code through the caller's
/// pointer. For [`Z_MEM_ERROR`] the message is the literal `"out of memory"`
/// (never an allocated string); otherwise it is the stored `"<path>: <msg>"`,
/// or `None` when no message is set (the FFI maps `None` to the empty string).
#[must_use]
pub fn gzerror(state: &GzState) -> (i32, Option<&str>) {
    let msg = if state.err == Z_MEM_ERROR {
        Some("out of memory")
    } else {
        state.msg.as_deref()
    };
    (state.err, msg)
}

// ===========================================================================
// gzclearerr — clear error + end-of-file state (port of `gzclearerr`,
// gzlib.c L530-547)
// ===========================================================================

/// Clear the error and end-of-file state of the stream — port of C
/// `gzclearerr` (`gzlib.c` L530-547).
///
/// Faithful to the C source: for a read stream it resets `eof` / `past`, and
/// then clears the error (`gz_error(state, Z_OK, NULL)`) for any read/write
/// stream. (C performs no `Z_STREAM_ERROR` special-casing here.) A
/// non-read/write handle is left untouched.
pub fn gzclearerr(state: &mut GzState) {
    // check integrity: must be open for reading or writing
    if state.mode != Mode::Read && state.mode != Mode::Write {
        return;
    }

    // clear end-of-file when reading
    if state.mode == Mode::Read {
        state.eof = false;
        state.past = false;
    }

    // clear the error
    state.clear_error();
}

// ===========================================================================
// gzsetparams — change level/strategy mid-stream (body delegated to write.rs;
// the C body lives in `gzwrite.c` `gzsetparams`, L629-690)
// ===========================================================================

/// Dynamically update the compression `level` and `strategy` of a write stream
/// — the C `gzsetparams` entry point (`gzwrite.c` L629-690).
///
/// The actual work — validating the handle, flushing any pending input with the
/// *old* parameters via `gz_comp(Z_BLOCK)`, and reconfiguring the deflate engine
/// via `deflateParams` — drives the DEFLATE engine and therefore lives in
/// `crate::gz::write::set_params`. This function is the thin wrapper that the
/// `gzlib.c`-derived surface exposes; it adds no logic of its own (which is why
/// `open.rs` lists *both* `gzlib.c` and `gzwrite.c` as its sources). Returns
/// `Z_OK` on success or the relevant `Z_*` error code.
pub fn gzsetparams(state: &mut GzState, level: i32, strategy: i32) -> i32 {
    write::set_params(state, level, strategy)
}

// ===========================================================================
// impl Seek for GzReader — idiomatic positioning (AAP §0.3.2)
// ===========================================================================

/// [`Seek`] support for the idiomatic [`GzReader`] adapter, mapping
/// [`std::io::Seek::seek`] onto [`gzseek`].
///
/// This completes the standard-library reader integration begun by
/// `read.rs`'s `impl Read`/`impl BufRead` for [`GzReader`] (AAP §0.3.2): a gzip
/// read stream can now be repositioned with the familiar [`Seek`] API.
///
/// * [`SeekFrom::Start`] → [`gzseek`] with [`SEEK_SET`];
/// * [`SeekFrom::Current`] → [`gzseek`] with [`SEEK_CUR`];
/// * [`SeekFrom::End`] is **unsupported** — gzip streams have no cheap
///   end-relative seek (mirroring C `gzseek`, which rejects `SEEK_END`), so it
///   returns an [`io::ErrorKind::Unsupported`] error.
///
/// A `gzseek` failure (`-1`) is surfaced as an [`io::Error`]; otherwise the new
/// uncompressed offset is returned.
impl Seek for GzReader<'_> {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let (offset, whence) = match pos {
            SeekFrom::Start(n) => (n as i64, SEEK_SET),
            SeekFrom::Current(n) => (n, SEEK_CUR),
            SeekFrom::End(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "gzip streams do not support SeekFrom::End",
                ));
            }
        };
        let result = gzseek(self.get_mut(), offset, whence);
        if result < 0 {
            Err(io::Error::other("gzseek failed"))
        } else {
            Ok(result as u64)
        }
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gz::read::gzread;
    use crate::gz::write::{gzclose_w, gzwrite};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// A self-cleaning temporary path, unique per (clone, process, call), so the
    /// tests are safe under the parallel test runner and across parallel clones.
    struct TempPath(PathBuf);

    impl TempPath {
        fn new(tag: &str) -> TempPath {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let clone = std::env::var("CLONE_INDEX").unwrap_or_else(|_| "x".to_string());
            let pid = std::process::id();
            let name = format!("blitzy_zlibrs_open_{tag}_{clone}_{pid}_{n}.gz");
            TempPath(std::env::temp_dir().join(name))
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

    /// Open a writable handle, drop the (empty) stream, and return the parsed
    /// `(mode, level, strategy, direct)` for assertion.
    fn parse_write(mode: &str) -> (Mode, i32, i32, i32) {
        let tmp = TempPath::new("parse_w");
        let handle = gzopen(tmp.path(), mode).expect("write open should succeed");
        (handle.mode, handle.level, handle.strategy, handle.direct)
    }

    /// Open a readable handle over a pre-created empty file and return the
    /// parsed `(mode, direct)`.
    fn parse_read(mode: &str) -> (Mode, i32) {
        let tmp = TempPath::new("parse_r");
        std::fs::write(tmp.path(), b"").expect("create empty file");
        let handle = gzopen(tmp.path(), mode).expect("read open should succeed");
        (handle.mode, handle.direct)
    }

    // --- mode-string parsing (frozen table, AAP §0.6.1) ---------------------

    #[test]
    fn mode_rb_is_read() {
        let (mode, direct) = parse_read("rb");
        assert_eq!(mode, Mode::Read);
        // reading with neither 'G' nor 'T' defaults direct to 1 (auto-detect).
        assert_eq!(direct, 1);
    }

    #[test]
    fn mode_wb9_is_write_level_9() {
        let (mode, level, strategy, direct) = parse_write("wb9");
        assert_eq!(mode, Mode::Write);
        assert_eq!(level, 9);
        assert_eq!(strategy, Z_DEFAULT_STRATEGY);
        assert_eq!(direct, 0);
    }

    #[test]
    fn mode_wb1f_is_write_level_1_filtered() {
        let (mode, level, strategy, _direct) = parse_write("wb1f");
        assert_eq!(mode, Mode::Write);
        assert_eq!(level, 1);
        assert_eq!(strategy, Z_FILTERED);
    }

    #[test]
    fn mode_a_is_append_becomes_write() {
        // 'a' parses to Append, then gz_open seeks to end and rewrites to Write.
        let (mode, _level, _strategy, _direct) = parse_write("a");
        assert_eq!(mode, Mode::Write, "append is rewritten to Write after open");
    }

    #[test]
    fn mode_plus_is_rejected() {
        // "rb+" / "wb+" reject during parsing (before any file I/O), so no file
        // is needed.
        assert!(gzopen(Path::new("unused_rb_plus"), "rb+").is_none());
        assert!(gzopen(Path::new("unused_wb_plus"), "wb+").is_none());
    }

    #[test]
    fn mode_wb_t_flag_is_write_direct_transparent() {
        let (mode, _level, _strategy, direct) = parse_write("wbT");
        assert_eq!(mode, Mode::Write);
        assert_eq!(direct, 1, "'T' selects transparent (store) writing");
    }

    #[test]
    fn mode_rb_g_flag_is_read_gzip_only() {
        let (mode, direct) = parse_read("rbG");
        assert_eq!(mode, Mode::Read);
        assert_eq!(direct, -1, "'G' keeps direct == -1 (gzip-only) on read");
    }

    #[test]
    fn mode_wb_r_flag_selects_rle_strategy() {
        let (_mode, _level, strategy, _direct) = parse_write("wbR");
        assert_eq!(strategy, Z_RLE);
    }

    #[test]
    fn mode_strategy_chars_huffman_and_fixed() {
        assert_eq!(parse_write("wbh").2, Z_HUFFMAN_ONLY);
        assert_eq!(parse_write("wbF").2, Z_FIXED);
    }

    #[test]
    fn mode_no_rwa_is_rejected() {
        // "b" alone gives no r/w/a -> Mode::None -> None.
        assert!(gzopen(Path::new("unused_b_only"), "b").is_none());
        assert!(gzopen(Path::new("unused_empty"), "").is_none());
    }

    #[test]
    fn mode_read_with_t_flag_is_rejected() {
        // 'T' (direct==1) is invalid for reading (can't force transparent read).
        let tmp = TempPath::new("read_t");
        std::fs::write(tmp.path(), b"").unwrap();
        assert!(gzopen(tmp.path(), "rbT").is_none());
    }

    #[test]
    fn mode_write_with_g_flag_is_rejected() {
        // 'G' (direct==-1) has no meaning when writing -> rejected.
        assert!(gzopen(Path::new("unused_write_g"), "wbG").is_none());
    }

    #[test]
    fn mode_last_digit_wins() {
        // "wb10" -> level 1 then 0 -> 0 (each digit overwrites, matching C).
        assert_eq!(parse_write("wb10").1, 0);
    }

    #[test]
    fn mode_unknown_chars_ignored() {
        // 'z' / '*' are ignored (C `default: ;`); 'w' still selects Write.
        let (mode, _l, _s, _d) = parse_write("wbz*");
        assert_eq!(mode, Mode::Write);
    }

    // --- gz_reset (C gz_reset, gzlib.c L68-84) ------------------------------

    fn state_for(mode: Mode) -> GzState {
        let tmp = TempPath::new("state");
        std::fs::write(tmp.path(), b"").unwrap();
        let file = File::options()
            .read(true)
            .write(true)
            .open(tmp.path())
            .unwrap();
        // Keep the temp file alive for the state's lifetime by leaking the guard
        // into the path string is unnecessary; the OS keeps the open fd valid
        // even after the dir entry is removed on guard drop.
        let st = GzState::new(file, tmp.path().to_string_lossy().into_owned(), mode);
        drop(tmp); // file stays open via the owned fd; dir entry is removed
        st
    }

    #[test]
    fn gz_reset_read_seeds_look_and_clears() {
        let mut st = state_for(Mode::Read);
        // dirty the fields
        st.have = 5;
        st.eof = true;
        st.past = true;
        st.how = How::Gzip;
        st.junk = 1;
        st.again = true;
        st.skip = 99;
        st.pos = 42;
        st.in_avail = 7;
        st.in_next = 3;
        st.gz_error(Z_BUF_ERROR, Some("x"));

        gz_reset(&mut st);

        assert_eq!(st.have, 0);
        assert!(!st.eof);
        assert!(!st.past);
        assert_eq!(st.how, How::Look);
        assert_eq!(st.junk, -1);
        assert!(!st.again);
        assert_eq!(st.skip, 0);
        assert_eq!(st.pos, 0);
        assert_eq!(st.in_avail, 0);
        assert_eq!(st.in_next, 0);
        assert_eq!(st.err, Z_OK);
        assert!(st.msg.is_none());
    }

    #[test]
    fn gz_reset_write_clears_reset_flag() {
        let mut st = state_for(Mode::Write);
        st.reset = true;
        st.have = 3;
        st.skip = 12;
        gz_reset(&mut st);
        assert!(!st.reset);
        assert_eq!(st.have, 0);
        assert_eq!(st.skip, 0);
        // write streams leave how untouched; junk is not a write concern.
    }

    // --- gzbuffer (C gzbuffer, gzlib.c L321-343) ----------------------------

    #[test]
    fn gzbuffer_sets_want_and_returns_zero() {
        let mut st = state_for(Mode::Write);
        assert_eq!(gzbuffer(&mut st, 16384), 0);
        assert_eq!(st.want, 16384);
    }

    #[test]
    fn gzbuffer_rounds_small_up_to_8() {
        let mut st = state_for(Mode::Read);
        assert_eq!(gzbuffer(&mut st, 4), 0);
        assert_eq!(st.want, 8);
        // 0 is also rounded up to 8.
        let mut st2 = state_for(Mode::Read);
        assert_eq!(gzbuffer(&mut st2, 0), 0);
        assert_eq!(st2.want, 8);
    }

    #[test]
    fn gzbuffer_rejected_after_allocation() {
        let mut st = state_for(Mode::Write);
        st.size = 1; // simulate buffers already allocated
        assert_eq!(gzbuffer(&mut st, 4096), -1);
    }

    #[test]
    fn gzbuffer_rejects_overflow() {
        let mut st = state_for(Mode::Write);
        // 2^31 doubles (with u32 wraparound) to 0, which is < 2^31 -> reject.
        assert_eq!(gzbuffer(&mut st, 0x8000_0000), -1);
        // a large-but-safe value (2^30) is accepted (doubles to 2^31 without
        // wrapping below the input).
        let mut st2 = state_for(Mode::Write);
        assert_eq!(gzbuffer(&mut st2, 0x4000_0000), 0);
        assert_eq!(st2.want, 0x4000_0000);
    }

    #[test]
    fn gzbuffer_rejects_wrong_mode() {
        let mut st = state_for(Mode::Read);
        st.mode = Mode::None;
        assert_eq!(gzbuffer(&mut st, 4096), -1);
    }

    // --- status accessors (gzeof / gzerror / gzclearerr / gztell) -----------

    #[test]
    fn gzeof_is_past_not_eof() {
        let mut st = state_for(Mode::Read);
        st.eof = true; // at EOF...
        st.past = false; // ...but no read-past yet
        assert!(!gzeof(&st), "gzeof is false while merely at EOF");
        st.past = true;
        assert!(gzeof(&st), "gzeof becomes true only after reading past end");
    }

    #[test]
    fn gzeof_false_for_write() {
        let mut st = state_for(Mode::Write);
        st.past = true; // irrelevant for a write stream
        assert!(!gzeof(&st));
    }

    #[test]
    fn gzerror_reports_formatted_message() {
        let mut st = state_for(Mode::Read);
        st.path = "myfile.gz".to_string();
        st.gz_error(crate::constants::Z_DATA_ERROR, Some("invalid block"));
        let (code, msg) = gzerror(&st);
        assert_eq!(code, crate::constants::Z_DATA_ERROR);
        assert_eq!(msg, Some("myfile.gz: invalid block"));
    }

    #[test]
    fn gzerror_mem_error_is_literal() {
        let mut st = state_for(Mode::Read);
        st.gz_error(Z_MEM_ERROR, Some("ignored"));
        let (code, msg) = gzerror(&st);
        assert_eq!(code, Z_MEM_ERROR);
        assert_eq!(msg, Some("out of memory"));
    }

    #[test]
    fn gzerror_no_message_is_none() {
        let st = state_for(Mode::Read);
        let (code, msg) = gzerror(&st);
        assert_eq!(code, Z_OK);
        assert_eq!(msg, None);
    }

    #[test]
    fn gzclearerr_resets_error_and_eof() {
        let mut st = state_for(Mode::Read);
        st.gz_error(crate::constants::Z_DATA_ERROR, Some("boom"));
        st.eof = true;
        st.past = true;
        gzclearerr(&mut st);
        assert_eq!(st.err, Z_OK);
        assert!(st.msg.is_none());
        assert!(!st.eof);
        assert!(!st.past);
    }

    #[test]
    fn gztell_includes_pending_skip() {
        let mut st = state_for(Mode::Read);
        st.pos = 100;
        st.skip = 25;
        st.past = false;
        assert_eq!(gztell(&st), 125, "tell folds in the pending skip");
        st.past = true;
        assert_eq!(gztell(&st), 100, "once past end, the skip is not counted");
    }

    #[test]
    fn gztell_rejects_wrong_mode() {
        let mut st = state_for(Mode::Read);
        st.mode = Mode::None;
        assert_eq!(gztell(&st), -1);
    }

    // --- gzrewind / gzoffset on a fresh handle ------------------------------

    #[test]
    fn gzrewind_rejects_write_mode() {
        let mut st = state_for(Mode::Write);
        assert_eq!(gzrewind(&mut st), -1);
    }

    #[test]
    fn gzoffset_reports_file_position() {
        // A freshly opened read stream over an empty file sits at offset 0.
        let tmp = TempPath::new("offset");
        std::fs::write(tmp.path(), b"hello world").unwrap();
        let mut handle = gzopen(tmp.path(), "rb").expect("open read");
        // No bytes buffered yet, so the compressed offset equals the file pos 0.
        assert_eq!(gzoffset(&mut handle), 0);
    }

    // --- transparent (non-gzip) seek round-trip: exercises the COPY fast path
    //     in gzseek and gz_skip-based forward seek ---------------------------

    #[test]
    fn transparent_read_seek_round_trip() {
        let data: Vec<u8> = (0..=255u8).cycle().take(4096).collect();
        let tmp = TempPath::new("transparent");
        std::fs::write(tmp.path(), &data).unwrap();

        let mut handle = gzopen(tmp.path(), "rb").expect("open read");

        // Prime the reader so it sniffs the (non-gzip) header and enters COPY.
        let mut probe = [0u8; 16];
        let got = gzread(&mut handle, &mut probe);
        assert!(got > 0, "transparent read should deliver bytes");
        assert_eq!(&probe[..got as usize], &data[..got as usize]);

        // Seek to an absolute offset and read; the bytes must match the source.
        let k: i64 = 1000;
        assert_eq!(gzseek(&mut handle, k, SEEK_SET), k, "seek returns target");
        assert_eq!(gztell(&handle), k, "tell reports the sought position");

        let mut buf = [0u8; 32];
        let n = gzread(&mut handle, &mut buf);
        assert!(n > 0);
        assert_eq!(
            &buf[..n as usize],
            &data[k as usize..k as usize + n as usize],
            "bytes after a SEEK_SET match the source at that offset"
        );
        assert_eq!(gztell(&handle), k + i64::from(n));
    }

    // --- gzip write -> read seek round-trip: exercises the deferred-skip seek
    //     across a real compressed stream -------------------------------------

    #[test]
    fn gzip_write_then_seek_read_round_trip() {
        let data: Vec<u8> = (0..8192u32).map(|i| (i * 7 + 3) as u8).collect();
        let tmp = TempPath::new("gzip_rt");

        // Write a gzip file.
        {
            let mut w = gzopen(tmp.path(), "wb6").expect("open write");
            let written = gzwrite(&mut w, &data);
            assert_eq!(written as usize, data.len(), "all bytes written");
            assert_eq!(gzclose_w(&mut w), Z_OK, "clean close");
        }

        // Reopen and seek forward, then read; the bytes must match the source.
        let mut r = gzopen(tmp.path(), "rb").expect("open read");
        let k: i64 = 5000;
        assert_eq!(gzseek(&mut r, k, SEEK_SET), k);
        let mut buf = [0u8; 64];
        let n = gzread(&mut r, &mut buf);
        assert!(n > 0, "read after seek should deliver decompressed bytes");
        assert_eq!(
            &buf[..n as usize],
            &data[k as usize..k as usize + n as usize],
            "decompressed bytes after a seek match the original input"
        );
        assert_eq!(gztell(&r), k + i64::from(n));
    }
}
