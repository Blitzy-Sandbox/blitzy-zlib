//! Buffered gzip file I/O — the `gz*` family.
//!
//! This is the safe-Rust port of the C `gz*` convenience layer
//! (`gzlib.c` / `gzread.c` / `gzwrite.c` / `gzclose.c`, with the shared
//! definitions from `gzguts.h`) re-expressed over [`std::fs::File`] and
//! [`std::io`]. It is the buffered, `FILE`-style layer that sits on top of the
//! streaming deflate/inflate core: it reads and writes `.gz` files (RFC 1952)
//! and transparently passes through non-gzip data. The public entry points are
//! the `gz*` functions (`gzopen` / `gzread` / `gzwrite` / `gzprintf` /
//! `gzseek` / `gzclose` / …) and the owned [`GzFile`] handle that threads
//! through them.
//!
//! # Module map
//!
//! Each C translation unit becomes a submodule; this root wires them together
//! and re-exposes their public surface:
//!
//! | Submodule | C source     | Responsibility                                       |
//! |-----------|--------------|------------------------------------------------------|
//! | [`state`] | `gzguts.h`   | the owned [`GzState`](state::GzState) data model + [`GZBUFSIZE`] / [`GzMode`] / [`GzHow`] |
//! | [`open`]  | `gzlib.c`    | open / buffer / seek / position / error API          |
//! | [`read`]  | `gzread.c`   | the decompression engine + read API                  |
//! | [`write`] | `gzwrite.c`  | the compression engine + write API                   |
//! | [`close`] | `gzclose.c`  | close dispatch (`gzclose` / `gzclose_r` / `gzclose_w`)|
//!
//! # Public API shape
//!
//! The C public type is `typedef struct gzFile_s *gzFile;` — an opaque pointer.
//! The safe analog is the owned newtype [`GzFile`], which wraps the internal
//! `Box<`[`GzState`](state::GzState)`>` and keeps that state fully encapsulated.
//! Two equivalent surfaces are offered (per the crate's "expose both" policy):
//!
//! * **idiomatic methods** on [`GzFile`] for Rust callers — e.g.
//!   [`GzFile::open`], [`GzFile::read`], [`GzFile::write`], [`GzFile::close`];
//! * the **C-named free functions** (`gzopen`, `gzread`, `gzwrite`, `gzclose`,
//!   …), matching `zlib.h` symbol-for-symbol, which the `libz-rs-sys` FFI shim
//!   wraps as `extern "C"` / `#[unsafe(no_mangle)]` exports. Those `unsafe`,
//!   C-ABI wrappers live *only* in that shim, never here.
//!
//! Both surfaces are thin wiring over the submodule implementations; this file
//! contains **no business logic** — all `gz*` behavior lives in the five
//! submodules above.
//!
//! # Availability
//!
//! This entire module is gated behind the `gz-io` Cargo feature (which implies
//! `std` + `gzip`) at its declaration site in the crate root (`lib.rs`:
//! `#[cfg(feature = "gz-io")] pub mod gz;`), mirroring the C
//! `#ifndef NO_GZCOMPRESS` / `Z_SOLO` gating. The gate is owned by the parent,
//! so neither this root nor its submodules carry an inner `#![cfg(...)]`
//! attribute; the module is simply absent from `no_std` / `Z_SOLO` builds.
//!
//! # Safety
//!
//! Like the rest of the core, every file here is compiled under the crate-wide
//! `#![forbid(unsafe_code)]`: the C `int fd` becomes an owned
//! [`File`](std::fs::File), the `malloc`/`free` buffers become owned `Vec<u8>`,
//! and the raw `z_stream` data pointers become plain `usize` offsets. No raw
//! pointers and no `unsafe` appear anywhere in the `gz` subsystem; the
//! raw-pointer `gzFile` ABI is reconstructed exclusively in the `libz-rs-sys`
//! FFI shim.

// ---------------------------------------------------------------------------
// Submodule declarations
// ---------------------------------------------------------------------------
//
// Declared `pub` to mirror the sibling `crate::deflate` / `crate::inflate`
// module roots: this keeps every `crate::gz::<submodule>::*` path resolvable for
// in-crate use while the *items* inside the submodules remain `pub(crate)` (the
// public surface is curated below through `GzFile`, the `gz*` free functions,
// and the explicit re-exports). They are separated by blank lines so this
// dependency-ordered listing (data model first) survives `rustfmt`'s reordering,
// which only sorts contiguous `mod` runs.

pub mod state;

pub mod open;

pub mod read;

pub mod write;

pub mod close;

use std::fs::File;
use std::path::Path;

// ---------------------------------------------------------------------------
// Shared re-exports
// ---------------------------------------------------------------------------

/// Re-export the shared gz constants/enums from [`state`] so consumers and the
/// FFI shim reach them as `crate::gz::{GZBUFSIZE, GzMode, GzHow}`. [`GZBUFSIZE`]
/// is defined *once* in [`state`] (C `#define GZBUFSIZE 8192`) and only
/// re-exported here — there is no second definition.
pub use state::{GZBUFSIZE, GzHow, GzMode};

/// `SEEK_SET` (`0`) — the [`gzseek`] origin meaning "relative to the start of
/// the stream", matching the C `<stdio.h>` constant. Exposed here so callers
/// (and the `libz-rs-sys` FFI shim) can pass a documented origin to [`gzseek`].
pub const SEEK_SET: i32 = 0;

/// `SEEK_CUR` (`1`) — the [`gzseek`] origin meaning "relative to the current
/// position", matching the C `<stdio.h>` constant. (`SEEK_END` is intentionally
/// unsupported by [`gzseek`], mirroring C zlib.)
pub const SEEK_CUR: i32 = 1;

// ---------------------------------------------------------------------------
// The public `GzFile` handle
// ---------------------------------------------------------------------------

/// An open gzip file — the safe-Rust analog of the C `gzFile`
/// (`typedef struct gzFile_s *gzFile;`).
///
/// In C, `gzFile` is an opaque pointer to a heap-allocated `gz_state`. Here it
/// is an **owned** newtype wrapping the internal `Box<`[`GzState`](state::GzState)`>`:
/// ownership replaces the manual `malloc`/`free` lifecycle, and dropping the
/// handle releases every buffer and the embedded stream through `Drop`
/// (the safe-Rust replacement for the C `gz*` cleanup paths). The wrapped state
/// is a private field, so the internal `GzState` type stays fully encapsulated;
/// callers interact only through the methods and `gz*` functions below.
///
/// Construct one with [`GzFile::open`] / [`GzFile::open64`] / [`GzFile::dopen`]
/// (or the free `gzopen` / `gzopen64` / `gzdopen` functions), and release it
/// with [`GzFile::close`] (or `gzclose`).
pub struct GzFile(Box<state::GzState>);

impl GzFile {
    // -- constructors (idiomatic counterparts of gzopen / gzopen64 / gzdopen) --

    /// Open `path` for gzip reading or writing per the C `mode` string
    /// (`"r"`, `"w"`, `"a"`, with optional `b`, level digit, strategy letter).
    /// Returns [`None`] on failure, mirroring the C `NULL`-on-error contract.
    /// Idiomatic counterpart of the free [`gzopen`] function.
    pub fn open(path: &Path, mode: &str) -> Option<GzFile> {
        open::gzopen(path, mode).map(GzFile)
    }

    /// 64-bit-offset variant of [`open`](GzFile::open) (C `gzopen64`). On this
    /// target offsets are always 64-bit, so it is behaviorally identical;
    /// provided for `zlib.h` symbol parity.
    pub fn open64(path: &Path, mode: &str) -> Option<GzFile> {
        open::gzopen64(path, mode).map(GzFile)
    }

    /// Associate an already-open [`File`] with a gzip handle (C `gzdopen`).
    /// `fd_label` is the descriptor number recorded for diagnostic messages
    /// (the safe core never performs raw-fd arithmetic). Returns [`None`] on
    /// failure. Idiomatic counterpart of the free [`gzdopen`] function.
    pub fn dopen(file: File, fd_label: i32, mode: &str) -> Option<GzFile> {
        open::gzdopen(file, fd_label, mode).map(GzFile)
    }

    // -- idiomatic operations (delegate to the C-named free functions) --

    /// Read up to `buf.len()` uncompressed bytes; returns the count read, or a
    /// negative error code (C `gzread`). See [`gzread`].
    pub fn read(&mut self, buf: &mut [u8]) -> i32 {
        gzread(self, buf)
    }

    /// Compress and write `buf`; returns the number of *uncompressed* bytes
    /// written, or `0` on error (C `gzwrite`). See [`gzwrite`].
    pub fn write(&mut self, buf: &[u8]) -> i32 {
        gzwrite(self, buf)
    }

    /// Flush buffered compressed output using the given zlib flush mode
    /// (C `gzflush`). See [`gzflush`].
    pub fn flush(&mut self, flush: i32) -> i32 {
        gzflush(self, flush)
    }

    /// Return non-zero once a read has tried to go past end-of-file
    /// (C `gzeof`). See [`gzeof`].
    pub fn eof(&self) -> i32 {
        gzeof(self)
    }

    /// Close the handle, dispatching on its open mode (C `gzclose`). Consumes
    /// the handle. See [`gzclose`].
    pub fn close(self) -> i32 {
        gzclose(self)
    }

    /// Close a read-mode handle (C `gzclose_r`). Consumes the handle.
    /// See [`gzclose_r`].
    pub fn close_read(self) -> i32 {
        gzclose_r(self)
    }

    /// Close a write-mode handle, flushing pending output (C `gzclose_w`).
    /// Consumes the handle. See [`gzclose_w`].
    pub fn close_write(self) -> i32 {
        gzclose_w(self)
    }

    // -- accessors backing the FFI `gzgetc` fast path (C `struct gzFile_s`) --
    //
    // The C `gzgetc()` macro reads the exposed `gzFile_s` members `have`/`next`/
    // `pos` directly. Those fields live on the encapsulated `GzState`, so the
    // `libz-rs-sys` shim cannot touch them; these accessors expose exactly what
    // the macro fast path needs, without leaking the internal type or any raw
    // pointer.

    /// Number of decompressed bytes currently buffered and ready to deliver
    /// without touching the engine (C `gzFile_s.have`).
    pub fn have(&self) -> usize {
        self.0.have
    }

    /// Current position in the uncompressed data stream (C `gzFile_s.pos`).
    pub fn pos(&self) -> i64 {
        self.0.pos
    }

    /// The slice of buffered, not-yet-delivered output (the bytes the C
    /// `gzgetc()` macro would read through `gzFile_s.next`). Empty when
    /// [`have`](GzFile::have) is `0`.
    pub fn output(&self) -> &[u8] {
        self.0.output()
    }

    /// Pop one buffered output byte, advancing the cursor — the safe equivalent
    /// of the C `gzgetc()` macro fast path. Returns [`None`] when no byte is
    /// buffered, signaling the caller to fall back to [`gzgetc`].
    pub fn next_byte(&mut self) -> Option<u8> {
        self.0.pop_byte()
    }
}

// ===========================================================================
// C-named public gz API (matches `zlib.h` symbol-for-symbol)
// ===========================================================================
//
// Each function is a thin wrapper that operates on the public `GzFile` handle
// and delegates to its submodule implementation (which takes the encapsulated
// `&mut GzState` / `&GzState` / `Box<GzState>`). The `&mut file.0` /
// `&file.0` arguments deref-coerce `Box<GzState>` to the borrow the submodule
// expects. These names are what the `libz-rs-sys` shim wraps as `extern "C"`
// exports; the shim performs the raw-pointer ↔ `GzFile` conversion (the only
// place `unsafe` is permitted).

// ---------------------------------------------------------------------------
// open / buffer / seek / position / error  (`gzlib.c`)
// ---------------------------------------------------------------------------

/// Open `path` as a gzip file (C `gzopen`); returns [`None`] on failure.
pub fn gzopen(path: &Path, mode: &str) -> Option<GzFile> {
    open::gzopen(path, mode).map(GzFile)
}

/// 64-bit-offset [`gzopen`] (C `gzopen64`).
pub fn gzopen64(path: &Path, mode: &str) -> Option<GzFile> {
    open::gzopen64(path, mode).map(GzFile)
}

/// Wrap an already-open [`File`] in a gzip handle (C `gzdopen`); `fd_label` is
/// the descriptor number recorded for diagnostics. Returns [`None`] on failure.
pub fn gzdopen(file: File, fd_label: i32, mode: &str) -> Option<GzFile> {
    open::gzdopen(file, fd_label, mode).map(GzFile)
}

/// Set the internal buffer size (C `gzbuffer`); must be called before any I/O.
pub fn gzbuffer(file: &mut GzFile, size: u32) -> i32 {
    open::gzbuffer(&mut file.0, size)
}

/// Update the compression level and strategy mid-stream (C `gzsetparams`).
pub fn gzsetparams(file: &mut GzFile, level: i32, strategy: i32) -> i32 {
    open::gzsetparams(&mut file.0, level, strategy)
}

/// Rewind a read-mode file to the beginning (C `gzrewind`).
pub fn gzrewind(file: &mut GzFile) -> i32 {
    open::gzrewind(&mut file.0)
}

/// Seek to `offset` relative to `whence` (C `gzseek`); returns the resulting
/// uncompressed offset, or `-1` on error.
pub fn gzseek(file: &mut GzFile, offset: i64, whence: i32) -> i64 {
    open::gzseek(&mut file.0, offset, whence)
}

/// 64-bit-offset [`gzseek`] (C `gzseek64`).
pub fn gzseek64(file: &mut GzFile, offset: i64, whence: i32) -> i64 {
    open::gzseek64(&mut file.0, offset, whence)
}

/// Current uncompressed offset (C `gztell`).
pub fn gztell(file: &GzFile) -> i64 {
    open::gztell(&file.0)
}

/// 64-bit-offset [`gztell`] (C `gztell64`).
pub fn gztell64(file: &GzFile) -> i64 {
    open::gztell64(&file.0)
}

/// Current offset into the underlying compressed file (C `gzoffset`).
pub fn gzoffset(file: &mut GzFile) -> i64 {
    open::gzoffset(&mut file.0)
}

/// 64-bit-offset [`gzoffset`] (C `gzoffset64`).
pub fn gzoffset64(file: &mut GzFile) -> i64 {
    open::gzoffset64(&mut file.0)
}

/// Return the most recent error message and code for `file` (C `gzerror`).
/// The message borrows from `file`; `errnum` receives the numeric code.
pub fn gzerror<'a>(file: &'a GzFile, errnum: &mut i32) -> Option<&'a str> {
    open::gzerror(&file.0, errnum)
}

/// Clear the error and end-of-file state for `file` (C `gzclearerr`).
pub fn gzclearerr(file: &mut GzFile) {
    open::gzclearerr(&mut file.0)
}

// ---------------------------------------------------------------------------
// read API  (`gzread.c`)
// ---------------------------------------------------------------------------

/// Read and decompress up to `buf.len()` bytes (C `gzread`); returns the number
/// of bytes read, or a negative error code.
pub fn gzread(file: &mut GzFile, buf: &mut [u8]) -> i32 {
    read::gzread(&mut file.0, buf)
}

/// Read `nitems` items of `size` bytes each (C `gzfread`); returns the number
/// of *complete* items read.
pub fn gzfread(buf: &mut [u8], size: usize, nitems: usize, file: &mut GzFile) -> usize {
    read::gzfread(buf, size, nitems, &mut file.0)
}

/// Read one byte (C `gzgetc`); returns the byte (`0..=255`) or `-1` at EOF/error.
/// FFI callers may first try the [`GzFile::next_byte`] fast path before falling
/// back to this function, replicating the C `gzgetc()` macro.
pub fn gzgetc(file: &mut GzFile) -> i32 {
    read::gzgetc(&mut file.0)
}

/// Always-a-function form of [`gzgetc`] for backward compatibility
/// (C `gzgetc_`).
pub fn gzgetc_(file: &mut GzFile) -> i32 {
    read::gzgetc_(&mut file.0)
}

/// Push one byte back into the stream (C `gzungetc`); returns the byte, or `-1`.
pub fn gzungetc(c: i32, file: &mut GzFile) -> i32 {
    read::gzungetc(c, &mut file.0)
}

/// Read a `\n`-terminated line into `buf` (C `gzgets`); returns the filled
/// slice, or [`None`] at immediate EOF/error.
pub fn gzgets<'a>(file: &mut GzFile, buf: &'a mut [u8]) -> Option<&'a [u8]> {
    read::gzgets(&mut file.0, buf)
}

/// Return non-zero if `file` is being read through transparently (not as a gzip
/// stream) (C `gzdirect`).
pub fn gzdirect(file: &mut GzFile) -> i32 {
    read::gzdirect(&mut file.0)
}

/// Return non-zero once a read has tried to go past end-of-file (C `gzeof`).
pub fn gzeof(file: &GzFile) -> i32 {
    read::gzeof(&file.0)
}

// ---------------------------------------------------------------------------
// write API  (`gzwrite.c`)
// ---------------------------------------------------------------------------

/// Compress and write `buf` (C `gzwrite`); returns the number of uncompressed
/// bytes written, or `0` on error.
pub fn gzwrite(file: &mut GzFile, buf: &[u8]) -> i32 {
    write::gzwrite(&mut file.0, buf)
}

/// Write `nitems` items of `size` bytes each (C `gzfwrite`); returns the number
/// of *complete* items written.
pub fn gzfwrite(buf: &[u8], size: usize, nitems: usize, file: &mut GzFile) -> usize {
    write::gzfwrite(buf, size, nitems, &mut file.0)
}

/// Write one byte (C `gzputc`); returns the byte written, or `-1` on error.
pub fn gzputc(file: &mut GzFile, c: i32) -> i32 {
    write::gzputc(&mut file.0, c)
}

/// Write a byte string (C `gzputs`); returns the number of bytes written, or
/// `-1` on error.
pub fn gzputs(file: &mut GzFile, s: &[u8]) -> i32 {
    write::gzputs(&mut file.0, s)
}

/// Formatted write (C `gzprintf`). The safe core takes pre-formatted
/// [`core::fmt::Arguments`] (produced by `format_args!`) instead of C varargs;
/// returns the number of bytes written, or a negative error code.
pub fn gzprintf(file: &mut GzFile, args: core::fmt::Arguments<'_>) -> i32 {
    write::gzprintf(&mut file.0, args)
}

/// `core::fmt::Arguments`-based formatted write (C `gzvprintf`); the underlying
/// implementation that [`gzprintf`] forwards to.
pub fn gzvprintf(file: &mut GzFile, args: core::fmt::Arguments<'_>) -> i32 {
    write::gzvprintf(&mut file.0, args)
}

/// Formatter-agnostic core of `gzprintf` / `gzvprintf`, exposed for the C-ABI
/// shim (`libz-rs-sys`).
///
/// Both [`gzprintf`] and [`gzvprintf`] format through Rust's [`core::fmt`] and
/// so cannot serve the C-variadic `int gzprintf(gzFile, const char *, ...)`
/// symbol, which must format with `vsnprintf`. Rather than duplicate the C
/// `gzvprintf` overflow/accounting discipline in the (unsafe) shim, this
/// function exposes that single faithful copy: the shim supplies a `render`
/// closure that runs `vsnprintf` into the provided **state-sized** scratch
/// region and reports whether the result fit, and all the state gating,
/// `gz_vacate` book-ending, scratch sizing, the `len == 0 || len >= size`
/// rejection, and the byte accounting happen here, identically to the safe path.
///
/// `render` is handed the scratch slice (length exactly the current
/// `gzbuffer()` size) and returns `Some(len)` with the formatted byte count, or
/// `None` if the output did not fit / the formatter errored. See
/// [`write::gz_printf_into`] for the precise contract. Returns the number of
/// bytes written, `0` if the output did not fit, or a negative `Z_*` code.
pub fn gz_printf_into<F>(file: &mut GzFile, render: F) -> i32
where
    F: FnOnce(&mut [u8]) -> Option<usize>,
{
    write::gz_printf_into(&mut file.0, render)
}

/// Flush buffered compressed output using the given zlib flush mode
/// (C `gzflush`).
pub fn gzflush(file: &mut GzFile, flush: i32) -> i32 {
    write::gzflush(&mut file.0, flush)
}

// ---------------------------------------------------------------------------
// close dispatch  (`gzclose.c`)
// ---------------------------------------------------------------------------

/// Close `file`, dispatching to the read- or write-mode finalizer based on how
/// it was opened (C `gzclose`). Consumes the handle.
pub fn gzclose(file: GzFile) -> i32 {
    close::gzclose(file.0)
}

/// Close a read-mode `file` (C `gzclose_r`). Consumes the handle.
pub fn gzclose_r(file: GzFile) -> i32 {
    close::gzclose_r(file.0)
}

/// Close a write-mode `file`, flushing any pending output (C `gzclose_w`).
/// Consumes the handle.
pub fn gzclose_w(file: GzFile) -> i32 {
    close::gzclose_w(file.0)
}

// ===========================================================================
// Smoke tests — public-path resolution
// ===========================================================================
//
// These do not exercise gz behavior (the full read/write/round-trip suites live
// in the submodules and the crate `tests/` directory); they only assert that
// every public symbol this root is responsible for re-exposing resolves with the
// expected signature, catching any drift between this wiring layer and the
// submodules at compile time.

#[cfg(test)]
mod tests {
    use super::*;

    /// Reference every public `gz*` free function as a typed function pointer.
    /// Compiling this is the assertion: a missing or signature-changed submodule
    /// function fails the build here.
    #[test]
    fn free_function_surface_resolves() {
        // open / buffer / seek / position / error
        let _: fn(&Path, &str) -> Option<GzFile> = gzopen;
        let _: fn(&Path, &str) -> Option<GzFile> = gzopen64;
        let _: fn(File, i32, &str) -> Option<GzFile> = gzdopen;
        let _: fn(&mut GzFile, u32) -> i32 = gzbuffer;
        let _: fn(&mut GzFile, i32, i32) -> i32 = gzsetparams;
        let _: fn(&mut GzFile) -> i32 = gzrewind;
        let _: fn(&mut GzFile, i64, i32) -> i64 = gzseek;
        let _: fn(&mut GzFile, i64, i32) -> i64 = gzseek64;
        let _: fn(&GzFile) -> i64 = gztell;
        let _: fn(&GzFile) -> i64 = gztell64;
        let _: fn(&mut GzFile) -> i64 = gzoffset;
        let _: fn(&mut GzFile) -> i64 = gzoffset64;
        let _: for<'a> fn(&'a GzFile, &mut i32) -> Option<&'a str> = gzerror;
        let _: fn(&mut GzFile) = gzclearerr;

        // read
        let _: fn(&mut GzFile, &mut [u8]) -> i32 = gzread;
        let _: fn(&mut [u8], usize, usize, &mut GzFile) -> usize = gzfread;
        let _: fn(&mut GzFile) -> i32 = gzgetc;
        let _: fn(&mut GzFile) -> i32 = gzgetc_;
        let _: fn(i32, &mut GzFile) -> i32 = gzungetc;
        let _: for<'a> fn(&mut GzFile, &'a mut [u8]) -> Option<&'a [u8]> = gzgets;
        let _: fn(&mut GzFile) -> i32 = gzdirect;
        let _: fn(&GzFile) -> i32 = gzeof;

        // write
        let _: fn(&mut GzFile, &[u8]) -> i32 = gzwrite;
        let _: fn(&[u8], usize, usize, &mut GzFile) -> usize = gzfwrite;
        let _: fn(&mut GzFile, i32) -> i32 = gzputc;
        let _: fn(&mut GzFile, &[u8]) -> i32 = gzputs;
        let _: for<'a> fn(&mut GzFile, core::fmt::Arguments<'a>) -> i32 = gzprintf;
        let _: for<'a> fn(&mut GzFile, core::fmt::Arguments<'a>) -> i32 = gzvprintf;
        let _: fn(&mut GzFile, i32) -> i32 = gzflush;

        // close
        let _: fn(GzFile) -> i32 = gzclose;
        let _: fn(GzFile) -> i32 = gzclose_r;
        let _: fn(GzFile) -> i32 = gzclose_w;
    }

    /// Reference the idiomatic [`GzFile`] methods and the FFI fast-path
    /// accessors as typed function pointers.
    #[test]
    fn handle_method_surface_resolves() {
        let _: fn(&Path, &str) -> Option<GzFile> = GzFile::open;
        let _: fn(&Path, &str) -> Option<GzFile> = GzFile::open64;
        let _: fn(File, i32, &str) -> Option<GzFile> = GzFile::dopen;
        let _: fn(&mut GzFile, &mut [u8]) -> i32 = GzFile::read;
        let _: fn(&mut GzFile, &[u8]) -> i32 = GzFile::write;
        let _: fn(&mut GzFile, i32) -> i32 = GzFile::flush;
        let _: fn(&GzFile) -> i32 = GzFile::eof;
        let _: fn(GzFile) -> i32 = GzFile::close;
        let _: fn(GzFile) -> i32 = GzFile::close_read;
        let _: fn(GzFile) -> i32 = GzFile::close_write;
        let _: fn(&GzFile) -> usize = GzFile::have;
        let _: fn(&GzFile) -> i64 = GzFile::pos;
        let _: for<'a> fn(&'a GzFile) -> &'a [u8] = GzFile::output;
        let _: fn(&mut GzFile) -> Option<u8> = GzFile::next_byte;
    }

    /// The shared constant and enums re-exported from [`state`] resolve here and
    /// carry their documented values.
    #[test]
    fn shared_reexports_resolve() {
        // C `#define GZBUFSIZE 8192`.
        assert_eq!(GZBUFSIZE, 8192);

        // Every documented `GzMode` / `GzHow` variant is reachable.
        let _ = [GzMode::None, GzMode::Read, GzMode::Write, GzMode::Append];
        let _ = [GzHow::Look, GzHow::Copy, GzHow::Gzip];
    }
}
