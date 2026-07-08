//! `extern "C"` shims for zlib's **`gz*`** gzip file-I/O API.
//!
//! This module reproduces, with byte-for-byte C ABI fidelity, the family of
//! `gz*` functions declared in `zlib.h` (`gzopen`, `gzread`, `gzwrite`,
//! `gzgets`, `gzclose`, …). Each shim validates raw C inputs, converts C
//! strings / file descriptors into safe Rust types, bridges to the idiomatic
//! [`crate::gz`] implementation, and translates the result back into the exact
//! integer / pointer sentinel a C caller expects.
//!
//! # Opaque handle model
//!
//! The public `gzFile` handle (`*mut gzFile_s`) is an *opaque* pointer to a
//! `#[repr(C)]` `GzHandle` whose FIRST field is a live C-layout `gzFile_s`
//! `{ have, next, pos }` prefix; the idiomatic [`GzState`] lives in its own
//! allocation behind the prefix. The handle is:
//!
//! * **open** — `Box::into_raw(Box::new(GzHandle { prefix, state })) as gzFile`
//!   (the prefix starts cleared).
//! * **operations** — the handle is *borrowed*, never owned, through a
//!   `GzBorrow` guard that reconciles the prefix on entry and re-syncs it on
//!   exit (see below). The box is not reconstructed.
//! * **close** — the `Box<GzHandle>` is reconstructed *exactly once*
//!   (`Box::from_raw(file as *mut GzHandle)`) and its inner `Box<GzState>` is
//!   handed to the idiomatic close routine, which flushes/finishes writers,
//!   emits the gzip trailer, frees buffers, and drops the owned
//!   [`std::fs::File`]; the prefix is dropped with the handle.
//!
//! This guarantees a sound lifecycle: one allocation on open, guarded borrows on
//! every operation, one deallocation on close — no double-free, no leak.
//!
//! # `gzgetc` / `gzgetc_` — live `gzFile_s` prefix (C2)
//!
//! In C, `gzgetc` is a *macro* that peeks the `{have, next, pos}` triple
//! directly through the handle for a branch-free fast path, falling back to the
//! real `gzgetc_` function only when the buffer is empty. To make this
//! macro-compatible, the opaque handle *begins* with a live `gzFile_s` prefix.
//! Each shim borrows the handle through a `GzBorrow` guard that, on entry,
//! **reconciles** any bytes the macro consumed straight from the prefix
//! (advancing the idiomatic cursor to match) and, on exit, **re-syncs** the
//! prefix to re-expose the current read buffer (`have`, a raw `next` pointer
//! into `out_buf`, and `pos`). The real `gzgetc`/`gzgetc_` functions are still
//! exported for callers that take the function pointer, and they observe
//! identical return values; the macro fast path now works too.
//!
//! # `gzprintf` / `gzvprintf` — documented variadic limitation
//!
//! C `gzprintf(gzFile, const char *fmt, ...)` is variadic and
//! `gzvprintf(gzFile, const char *fmt, va_list)` consumes a `va_list`. On
//! **stable Rust (MSRV 1.85)** C variadics (`c_variadic`, `core::ffi::VaList`)
//! are *unstable*, so a true variadic definition cannot be compiled. These two
//! symbols are consequently gated behind an opt-in **nightly** `c-variadic`
//! cargo feature (which also requires `#![feature(c_variadic)]` in the crate
//! root). On stable — the default — they are simply absent; **every other**
//! `gz*` symbol is present. Rather than ship an ABI-incompatible stub that would
//! silently corrupt callers, their absence is the accepted, documented
//! limitation (AAP §0.6 known risk).
//!
//! # `gzerror` message pointer
//!
//! `gzerror` must return a `*const c_char` that stays valid until the next `gz*`
//! call on the handle. The idiomatic error accessor yields a borrowed,
//! non-NUL-terminated `&str`; to hand C a stable, NUL-terminated pointer the
//! handle owns a `CString` mirror of its message (`GzState::msg_c`, kept in
//! lockstep with `GzState::msg` by `GzState::error`). `gzerror` returns a
//! pointer into that mirror — the specific `"{path}: {detail}"` text, matching
//! reference zlib byte-for-byte — falling back to the literal `"out of memory"`
//! for `Z_MEM_ERROR` (which stores no heap message) and to the empty string
//! when there is no detail. The `errnum` out-parameter carries the
//! authoritative, exact code.
//!
//! # Feature gating & safety
//!
//! The entire module is compiled only when the `gz-io` feature is enabled
//! (`gz-io` implies `std`, required for [`std::fs::File`] I/O, [`CStr`], and
//! `from_raw_fd`). This is the crate's `unsafe` boundary: every `unsafe` block
//! carries a `// SAFETY:` justification, and no shim may unwind across the C
//! boundary — fallible bodies run inside the `guard_*` helpers so a panic is
//! caught and converted to the function's C error sentinel.

#![cfg(feature = "gz-io")]
#![allow(clippy::missing_safety_doc)]

use core::ffi::{CStr, c_char, c_int, c_uint};
use core::{ptr, slice};

use alloc::boxed::Box;

#[cfg(unix)]
use std::os::fd::FromRawFd;

use crate::error::ReturnCode;
use crate::ffi::types::*;
use crate::gz::{self, GzMode, GzState};

// ===========================================================================
// Return-code constants
// ===========================================================================
//
// `constants.rs` intentionally does not expose bare `Z_*` return-code integer
// aliases (idiomatic code uses `ReturnCode`), so the two codes the `gz*` C
// contract branches on are materialized locally from the authoritative enum.

/// `Z_OK` (`0`) — success. Part of the return-code vocabulary; consumed by the
/// in-crate tests and available for readable success comparisons.
#[allow(dead_code)]
const Z_OK: c_int = ReturnCode::Ok.as_c_int();
/// `Z_STREAM_ERROR` (`-2`) — inconsistent stream / invalid handle.
const Z_STREAM_ERROR: c_int = ReturnCode::StreamError.as_c_int();

// ===========================================================================
// Local helpers
// ===========================================================================

/// Panic guard for the `z_size_t`-returning shims (`gzfread` / `gzfwrite`).
///
/// [`crate::ffi::types`] provides `guard_int` / `guard_off` / `guard_ptr` but no
/// `usize` variant, so this mirrors the same catch-and-default behavior for
/// [`z_size_t`]. `gz-io` implies `std`, so [`std::panic::catch_unwind`] is
/// always available here.
#[inline]
fn guard_size(
    default: z_size_t,
    f: impl FnOnce() -> z_size_t + core::panic::UnwindSafe,
) -> z_size_t {
    std::panic::catch_unwind(f).unwrap_or(default)
}

/// Panic guard for the sole `*const c_char`-returning shim (`gzerror`).
#[inline]
fn guard_const_ptr<T>(
    default: *const T,
    f: impl FnOnce() -> *const T + core::panic::UnwindSafe,
) -> *const T {
    std::panic::catch_unwind(f).unwrap_or(default)
}

/// Converts a non-null C path string into an owned [`std::path::PathBuf`].
///
/// On Unix the raw bytes are used verbatim (`OsStrExt::from_bytes`) so that
/// paths which are not valid UTF-8 are preserved exactly, matching C behavior.
/// On other targets the bytes must be valid UTF-8 (constructing a `PathBuf`
/// losslessly from arbitrary bytes is not portably available); an invalid
/// encoding yields `None`, causing the open to fail with a null handle.
///
/// # Safety
///
/// `path` must be non-null and point to a NUL-terminated C string that remains
/// valid for the duration of the call.
#[cfg(unix)]
unsafe fn cpath_to_pathbuf(path: *const c_char) -> Option<std::path::PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    // SAFETY: the caller guarantees `path` is non-null and NUL-terminated.
    let cstr = unsafe { CStr::from_ptr(path) };
    Some(std::path::PathBuf::from(std::ffi::OsStr::from_bytes(
        cstr.to_bytes(),
    )))
}

/// Portable (non-Unix) fallback: requires the path to be valid UTF-8.
///
/// # Safety
///
/// See the Unix variant: `path` must be non-null and NUL-terminated.
#[cfg(not(unix))]
unsafe fn cpath_to_pathbuf(path: *const c_char) -> Option<std::path::PathBuf> {
    // SAFETY: the caller guarantees `path` is non-null and NUL-terminated.
    let cstr = unsafe { CStr::from_ptr(path) };
    cstr.to_str().ok().map(std::path::PathBuf::from)
}

// ---------------------------------------------------------------------------
// C2 — live `gzFile_s` prefix for the `gzgetc(g)` macro fast-path
// ---------------------------------------------------------------------------
//
// zlib's `gzgetc(g)` is a *macro* that reads the `{ have, next, pos }` prefix
// directly through the `gzFile` pointer:
//
//   ((g)->have ? ((g)->have--, (g)->pos++, *((g)->next)++) : (gzgetc)(g))
//
// In C, `gz_statep` embeds `struct gzFile_s x` as its first member, so the macro
// and every real `gz*` function share ONE storage for `have`/`next`/`pos`. Our
// idiomatic [`GzState`] is not `#[repr(C)]` (it owns a `Vec`, `File`, `String`,
// …) and tracks `next` as an *index*, so we cannot expose it directly. Instead
// the opaque handle is a `#[repr(C)]` [`GzHandle`] whose FIRST field is a live
// [`gzFile_s`] prefix. The prefix shadows the idiomatic cursor and is bridged
// by [`GzHandle::reconcile`] (absorb macro-side consumption) on entry and
// [`GzHandle::sync`] (re-expose the read buffer) on exit of every shim.

/// The opaque `gzFile` handle: a `#[repr(C)]` wrapper whose leading
/// [`gzFile_s`] prefix (offset 0) is what the C `gzgetc(g)` macro reads and
/// mutates. The idiomatic [`GzState`] lives in its own allocation behind a
/// `Box`; only the prefix is ABI-visible.
#[repr(C)]
struct GzHandle {
    /// C `gzFile_s` prefix consumed by the `gzgetc(g)` macro fast-path. MUST be
    /// the first field so `gzFile` (a `*mut gzFile_s`) aliases it at offset 0.
    prefix: gzFile_s,
    /// The idiomatic gz state (separate allocation; never inspected by C).
    state: Box<GzState>,
}

impl GzHandle {
    /// Absorbs any bytes the C `gzgetc(g)` macro consumed directly from the
    /// prefix since the last [`sync`](Self::sync), advancing the idiomatic cursor
    /// to match, then clears the prefix so `sync` fully re-derives it.
    ///
    /// The macro only runs in READ mode; in WRITE/NONE mode the prefix is always
    /// cleared and this is a no-op beyond clearing.
    #[inline]
    fn reconcile(&mut self) {
        if self.state.mode == GzMode::Read {
            // The macro decrements `prefix.have` (and advances `next`/`pos`) once
            // per byte it delivered; the delta is the count it consumed. Saturate
            // defensively — `prefix.have <= state.have` always holds because
            // `sync` sets them equal and only the macro (a decrement) runs in
            // between.
            let consumed = self.state.have.saturating_sub(self.prefix.have as usize);
            self.state.next += consumed;
            self.state.have -= consumed;
            self.state.pos += consumed as i64;
        }
        self.prefix.have = 0;
        self.prefix.next = ptr::null_mut();
    }

    /// Re-exposes the idiomatic read buffer through the `gzFile_s` prefix so the
    /// C `gzgetc(g)` macro fast-path can consume it directly. In WRITE/NONE mode
    /// (or when nothing is buffered) the prefix is cleared; `pos` always mirrors
    /// the idiomatic position.
    #[inline]
    fn sync(&mut self) {
        self.prefix.pos = self.state.pos;
        if self.state.mode == GzMode::Read && self.state.have > 0 {
            self.prefix.have = self.state.have as c_uint;
            // SAFETY: the read driver maintains `next + have <= out_buf.len()`, so
            // `&out_buf[next]` is in-bounds and `have` bytes are readable from it.
            // The macro only ever READS through this pointer (`*next++`); no Rust
            // code aliases the region while the macro owns it (the shim has
            // returned and the `GzBorrow` has been dropped), so exposing it as a
            // raw `*mut` is sound. It is recomputed on every `sync`, so a buffer
            // reallocation between calls can never leave it dangling.
            self.prefix.next =
                unsafe { self.state.out_buf.as_ptr().add(self.state.next) } as *mut u8;
        } else {
            self.prefix.have = 0;
            self.prefix.next = ptr::null_mut();
        }
    }
}

/// Borrows the [`GzHandle`] behind the opaque `file` pointer.
///
/// # Safety
///
/// `file` must be a non-null handle produced by [`box_state`] (i.e. by
/// `gzopen*`/`gzdopen`) and not yet closed. Because [`gzFile_s`] is the first
/// field of the `#[repr(C)]` [`GzHandle`], the `gzFile` pointer has the same
/// address as the `GzHandle`.
#[inline]
unsafe fn gz_handle<'a>(file: gzFile) -> &'a mut GzHandle {
    // SAFETY: per the contract `file` points at a live `GzHandle` (prefix at
    // offset 0); the returned borrow is used only for the duration of one shim.
    unsafe { &mut *(file as *mut GzHandle) }
}

/// RAII borrow of a [`GzHandle`]'s idiomatic [`GzState`] that keeps the C
/// `gzFile_s` prefix consistent: it [`reconcile`](GzHandle::reconcile)s on
/// creation (absorbing any `gzgetc` macro consumption) and
/// [`sync`](GzHandle::sync)s on drop (re-exposing the read buffer). Dereferences
/// to [`GzState`] so existing shim bodies are unchanged.
struct GzBorrow<'a> {
    handle: &'a mut GzHandle,
}

impl<'a> GzBorrow<'a> {
    #[inline]
    fn new(handle: &'a mut GzHandle) -> Self {
        handle.reconcile();
        Self { handle }
    }
}

impl Drop for GzBorrow<'_> {
    #[inline]
    fn drop(&mut self) {
        self.handle.sync();
    }
}

impl core::ops::Deref for GzBorrow<'_> {
    type Target = GzState;
    #[inline]
    fn deref(&self) -> &GzState {
        &self.handle.state
    }
}

impl core::ops::DerefMut for GzBorrow<'_> {
    #[inline]
    fn deref_mut(&mut self) -> &mut GzState {
        &mut self.handle.state
    }
}

/// Boxes an idiomatic open result into the opaque `gzFile` handle, translating
/// failure into the C `NULL` sentinel. The handle is a [`GzHandle`] whose
/// leading [`gzFile_s`] prefix starts cleared (`have = 0`), so the `gzgetc`
/// macro falls through to the real function until the first read populates it.
#[inline]
fn box_state(result: Result<Box<GzState>, ReturnCode>) -> gzFile {
    match result {
        Ok(state) => {
            let handle = Box::new(GzHandle {
                prefix: gzFile_s {
                    have: 0,
                    next: ptr::null_mut(),
                    pos: 0,
                },
                state,
            });
            Box::into_raw(handle) as gzFile
        }
        Err(_) => ptr::null_mut(),
    }
}

// ===========================================================================
// Phase 1 — Open shims  (<- gzlib.c)
// ===========================================================================

/// `gzFile gzopen(const char *path, const char *mode)`
///
/// Opens `path` for reading or writing according to the zlib `mode` string
/// (level digits, `r`/`w`/`a`, `b`, strategy flags, `T`, `G`, …). The mode
/// string is parsed entirely inside the idiomatic layer. Returns `NULL` on any
/// failure (including a null `path`/`mode` or a non-decodable path).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzopen(path: *const c_char, mode: *const c_char) -> gzFile {
    guard_ptr(ptr::null_mut(), || -> gzFile {
        if path.is_null() || mode.is_null() {
            return ptr::null_mut();
        }
        // SAFETY: `path` is non-null (checked) and, per the C contract, a
        // caller-owned NUL-terminated string valid for this call.
        let Some(pathbuf) = (unsafe { cpath_to_pathbuf(path) }) else {
            return ptr::null_mut();
        };
        // SAFETY: `mode` is non-null (checked) and NUL-terminated.
        let Ok(mode_str) = (unsafe { CStr::from_ptr(mode) }).to_str() else {
            return ptr::null_mut();
        };
        box_state(gz::gzopen(pathbuf, mode_str))
    })
}

/// `gzFile gzopen64(const char *path, const char *mode)`
///
/// 64-bit-offset variant. Rust file offsets are 64-bit by default, so this
/// simply delegates to the idiomatic `gzopen64` (behaviorally identical to
/// [`gzopen`] on this platform).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzopen64(path: *const c_char, mode: *const c_char) -> gzFile {
    guard_ptr(ptr::null_mut(), || -> gzFile {
        if path.is_null() || mode.is_null() {
            return ptr::null_mut();
        }
        // SAFETY: `path` is non-null (checked) and NUL-terminated.
        let Some(pathbuf) = (unsafe { cpath_to_pathbuf(path) }) else {
            return ptr::null_mut();
        };
        // SAFETY: `mode` is non-null (checked) and NUL-terminated.
        let Ok(mode_str) = (unsafe { CStr::from_ptr(mode) }).to_str() else {
            return ptr::null_mut();
        };
        box_state(gz::gzopen64(pathbuf, mode_str))
    })
}

/// `gzFile gzdopen(int fd, const char *mode)`  *(Unix)*
///
/// Associates a `gz*` stream with an already-open file descriptor. Ownership of
/// `fd` is **transferred** to the returned handle (Rust RAII): it is closed by
/// [`gzclose`], or — should the open fail — closed when the transient [`File`]
/// is dropped. This is a minor, documented divergence from C (which leaves the
/// descriptor open on failure) sanctioned by the FFI ownership convention.
///
/// Returns `NULL` for a null `mode` or a negative `fd`.
///
/// [`File`]: std::fs::File
#[cfg(unix)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzdopen(fd: c_int, mode: *const c_char) -> gzFile {
    guard_ptr(ptr::null_mut(), || -> gzFile {
        if mode.is_null() || fd < 0 {
            return ptr::null_mut();
        }
        // SAFETY: `mode` is non-null (checked) and NUL-terminated.
        let Ok(mode_str) = (unsafe { CStr::from_ptr(mode) }).to_str() else {
            return ptr::null_mut();
        };
        // SAFETY: the caller transfers ownership of `fd`, a valid open OS file
        // descriptor (negatives rejected above). The resulting `File` owns the
        // descriptor and closes it on drop or via `gzclose`.
        let file = unsafe { std::fs::File::from_raw_fd(fd) };
        box_state(gz::gzdopen(file, mode_str))
    })
}

/// `gzFile gzdopen(int fd, const char *mode)`  *(non-Unix fallback)*
///
/// Adopting a raw C `int` file descriptor is a POSIX concept with no portable
/// `std` equivalent off Unix (Windows uses `HANDLE`s). The symbol is retained
/// for ABI completeness but always fails here; use [`gzopen`]/`gzopen_w`
/// instead on such targets.
#[cfg(not(unix))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzdopen(_fd: c_int, _mode: *const c_char) -> gzFile {
    ptr::null_mut()
}

/// `int gzbuffer(gzFile file, unsigned size)`
///
/// Sets the internal buffer size for a freshly opened handle. Returns `0` on
/// success and `-1` if called after the first read/write has begun (the engine
/// enforces the ordering) or for a null handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzbuffer(file: gzFile, size: c_uint) -> c_int {
    guard_int(-1, || {
        if file.is_null() {
            return -1;
        }
        // SAFETY: non-null handle from `gzopen*`/`gzdopen`; borrowed, not owned.
        let mut guard = GzBorrow::new(unsafe { gz_handle(file) });
        let state = &mut *guard;
        gz::gzbuffer(state, size)
    })
}

/// `int gzsetparams(gzFile file, int level, int strategy)`
///
/// Dynamically updates the compression level and strategy of a write stream.
/// Returns a zlib return code (`Z_OK` on success), or `Z_STREAM_ERROR` for a
/// null handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzsetparams(file: gzFile, level: c_int, strategy: c_int) -> c_int {
    guard_int(Z_STREAM_ERROR, || {
        if file.is_null() {
            return Z_STREAM_ERROR;
        }
        // SAFETY: non-null handle; borrowed, not owned.
        let mut guard = GzBorrow::new(unsafe { gz_handle(file) });
        let state = &mut *guard;
        gz::gzsetparams(state, level, strategy)
    })
}

// ===========================================================================
// Phase 2 — Read shims  (<- gzread.c)
// ===========================================================================

/// `int gzread(gzFile file, voidp buf, unsigned len)`
///
/// Reads up to `len` uncompressed bytes into `buf`. Returns the number of bytes
/// actually read (`0` at end of file), or `-1` on error or a null handle/buffer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzread(file: gzFile, buf: voidp, len: c_uint) -> c_int {
    guard_int(-1, || {
        if file.is_null() || buf.is_null() {
            return -1;
        }
        // SAFETY: non-null handle; borrowed, not owned.
        let mut guard = GzBorrow::new(unsafe { gz_handle(file) });
        let state = &mut *guard;
        // SAFETY: `buf` is non-null (checked) and, per the C contract, valid for
        // writes of `len` bytes.
        let out = unsafe { slice::from_raw_parts_mut(buf as *mut u8, len as usize) };
        gz::gzread(state, out)
    })
}

/// `z_size_t gzfread(voidp buf, z_size_t size, z_size_t nitems, gzFile file)`
///
/// Reads `nitems` items of `size` bytes each. Returns the number of full items
/// read. On multiplication overflow the idiomatic layer records
/// `Z_STREAM_ERROR` and returns `0`, mirroring C. Note the C argument order:
/// the handle is the *last* parameter.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzfread(
    buf: voidp,
    size: z_size_t,
    nitems: z_size_t,
    file: gzFile,
) -> z_size_t {
    guard_size(0, || {
        if file.is_null() {
            return 0;
        }
        // SAFETY: non-null handle; borrowed, not owned.
        let mut guard = GzBorrow::new(unsafe { gz_handle(file) });
        let state = &mut *guard;
        let Some(len) = size.checked_mul(nitems) else {
            // Overflow: let the idiomatic layer record Z_STREAM_ERROR and
            // return 0 (an empty slice cannot itself trigger a read).
            return gz::gzfread(state, &mut [], size, nitems);
        };
        if len == 0 {
            // Zero-length request (size == 0 or nitems == 0): drive the
            // idiomatic checks with an empty slice; `buf` may legitimately be
            // null in this case.
            return gz::gzfread(state, &mut [], size, nitems);
        }
        if buf.is_null() {
            // Non-zero length with no destination: decline safely rather than
            // constructing a slice over a null pointer.
            return 0;
        }
        // SAFETY: `buf` is non-null (checked) and valid for `len` bytes;
        // `len == size * nitems` did not overflow.
        let out = unsafe { slice::from_raw_parts_mut(buf as *mut u8, len) };
        gz::gzfread(state, out, size, nitems)
    })
}

/// `int gzgetc(gzFile file)`
///
/// Reads one byte, returning it as `0..=255`, or `-1` at end of file / on error
/// / for a null handle. Exported as a real function; see the module docs for
/// the macro-parity note.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzgetc(file: gzFile) -> c_int {
    guard_int(-1, || {
        if file.is_null() {
            return -1;
        }
        // SAFETY: non-null handle; borrowed, not owned.
        let mut guard = GzBorrow::new(unsafe { gz_handle(file) });
        let state = &mut *guard;
        gz::gzgetc(state)
    })
}

/// `int gzgetc_(gzFile file)`
///
/// The explicit-function form of [`gzgetc`] (`ZLIB_1.2.5.2`). Behaviorally
/// identical; both symbols are exported so either linkage resolves.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzgetc_(file: gzFile) -> c_int {
    guard_int(-1, || {
        if file.is_null() {
            return -1;
        }
        // SAFETY: non-null handle; borrowed, not owned.
        let mut guard = GzBorrow::new(unsafe { gz_handle(file) });
        let state = &mut *guard;
        gz::gzgetc_(state)
    })
}

/// `char *gzgets(gzFile file, char *buf, int len)`
///
/// Reads a NUL-terminated line (at most `len - 1` bytes plus the terminator)
/// into `buf`. Returns `buf` on success, or `NULL` at end of file with no data
/// read, on error, or for a null handle / null buffer / non-positive `len`. The
/// idiomatic layer guarantees NUL-termination within `len`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzgets(file: gzFile, buf: *mut c_char, len: c_int) -> *mut c_char {
    guard_ptr(ptr::null_mut(), || -> *mut c_char {
        if file.is_null() || buf.is_null() || len <= 0 {
            return ptr::null_mut();
        }
        // SAFETY: non-null handle; borrowed, not owned.
        let mut guard = GzBorrow::new(unsafe { gz_handle(file) });
        let state = &mut *guard;
        // SAFETY: `buf` is non-null (checked) and valid for `len` bytes
        // (`len > 0` checked). The idiomatic writer NUL-terminates within it.
        let out = unsafe { slice::from_raw_parts_mut(buf as *mut u8, len as usize) };
        match gz::gzgets(state, out) {
            Some(_) => buf,
            None => ptr::null_mut(),
        }
    })
}

/// `int gzungetc(int c, gzFile file)`
///
/// Pushes one byte back into the read stream. Returns `c` on success, or `-1`
/// on error / for a null handle. Note the C argument order: `c` precedes the
/// handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzungetc(c: c_int, file: gzFile) -> c_int {
    guard_int(-1, || {
        if file.is_null() {
            return -1;
        }
        // SAFETY: non-null handle; borrowed, not owned.
        let mut guard = GzBorrow::new(unsafe { gz_handle(file) });
        let state = &mut *guard;
        gz::gzungetc(c, state)
    })
}

// ===========================================================================
// Phase 3 — Write shims  (<- gzwrite.c)
// ===========================================================================

/// `int gzwrite(gzFile file, voidpc buf, unsigned len)`
///
/// Compresses and writes `len` bytes from `buf`. Returns the number of
/// (uncompressed) bytes written, or `0` on error / for a null handle (or a null
/// `buf` with `len > 0`) — note C `gzwrite` uses `0`, not `-1`, as its error
/// sentinel.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzwrite(file: gzFile, buf: voidpc, len: c_uint) -> c_int {
    guard_int(0, || {
        if file.is_null() {
            return 0;
        }
        if buf.is_null() && len != 0 {
            return 0;
        }
        // SAFETY: non-null handle; borrowed, not owned.
        let mut guard = GzBorrow::new(unsafe { gz_handle(file) });
        let state = &mut *guard;
        let input = if len == 0 {
            // Empty write: avoid forming a slice over a possibly-null pointer.
            &[][..]
        } else {
            // SAFETY: `buf` is non-null (checked above for `len > 0`) and valid
            // for reads of `len` bytes per the C contract.
            unsafe { slice::from_raw_parts(buf as *const u8, len as usize) }
        };
        gz::gzwrite(state, input)
    })
}

/// `z_size_t gzfwrite(voidpc buf, z_size_t size, z_size_t nitems, gzFile file)`
///
/// Writes `nitems` items of `size` bytes each. Returns the number of full items
/// written. On multiplication overflow the idiomatic layer records
/// `Z_STREAM_ERROR` and returns `0`. Note the C argument order: the handle is
/// the *last* parameter.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzfwrite(
    buf: voidpc,
    size: z_size_t,
    nitems: z_size_t,
    file: gzFile,
) -> z_size_t {
    guard_size(0, || {
        if file.is_null() {
            return 0;
        }
        // SAFETY: non-null handle; borrowed, not owned.
        let mut guard = GzBorrow::new(unsafe { gz_handle(file) });
        let state = &mut *guard;
        let Some(len) = size.checked_mul(nitems) else {
            // Overflow: idiomatic layer records Z_STREAM_ERROR and returns 0.
            return gz::gzfwrite(state, &[], size, nitems);
        };
        if len == 0 {
            // Zero-length request: drive the idiomatic checks with an empty
            // slice; `buf` may legitimately be null.
            return gz::gzfwrite(state, &[], size, nitems);
        }
        if buf.is_null() {
            return 0;
        }
        // SAFETY: `buf` is non-null (checked) and valid for `len` bytes;
        // `len == size * nitems` did not overflow.
        let input = unsafe { slice::from_raw_parts(buf as *const u8, len) };
        gz::gzfwrite(state, input, size, nitems)
    })
}

/// `int gzputc(gzFile file, int c)`
///
/// Writes the low byte of `c`. Returns the byte written on success, or `-1` on
/// error / for a null handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzputc(file: gzFile, c: c_int) -> c_int {
    guard_int(-1, || {
        if file.is_null() {
            return -1;
        }
        // SAFETY: non-null handle; borrowed, not owned.
        let mut guard = GzBorrow::new(unsafe { gz_handle(file) });
        let state = &mut *guard;
        gz::gzputc(state, c)
    })
}

/// `int gzputs(gzFile file, const char *s)`
///
/// Writes the NUL-terminated string `s` (excluding the terminator). Returns the
/// number of bytes written, or `-1` on error / for a null handle or null `s`.
///
/// The idiomatic writer takes `&str`; a string that is not valid UTF-8 is
/// written faithfully via the byte-oriented writer, still honoring the `-1`
/// error sentinel.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzputs(file: gzFile, s: *const c_char) -> c_int {
    guard_int(-1, || {
        if file.is_null() || s.is_null() {
            return -1;
        }
        // SAFETY: non-null handle; borrowed, not owned.
        let mut guard = GzBorrow::new(unsafe { gz_handle(file) });
        let state = &mut *guard;
        // SAFETY: `s` is non-null (checked) and a NUL-terminated C string.
        let bytes = unsafe { CStr::from_ptr(s) }.to_bytes();
        match core::str::from_utf8(bytes) {
            Ok(text) => gz::gzputs(state, text),
            Err(_) => {
                // Non-UTF-8: write raw bytes, preserving gzputs' -1-on-error
                // contract (`gzwrite` returns 0 on error).
                let n = gz::gzwrite(state, bytes);
                if n == 0 && !bytes.is_empty() { -1 } else { n }
            }
        }
    })
}

/// `int gzflush(gzFile file, int flush)`
///
/// Flushes pending output with the given flush mode. Returns a zlib return code
/// (`Z_OK` on success), or `Z_STREAM_ERROR` for a null handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzflush(file: gzFile, flush: c_int) -> c_int {
    guard_int(Z_STREAM_ERROR, || {
        if file.is_null() {
            return Z_STREAM_ERROR;
        }
        // SAFETY: non-null handle; borrowed, not owned.
        let mut guard = GzBorrow::new(unsafe { gz_handle(file) });
        let state = &mut *guard;
        gz::gzflush(state, flush)
    })
}

// ---------------------------------------------------------------------------
// gzprintf / gzvprintf — nightly `c-variadic` only (see module docs)
// ---------------------------------------------------------------------------
//
// On stable Rust (MSRV 1.85) C variadics are unstable and CANNOT compile, so
// these two symbols are gated behind the opt-in nightly `c-variadic` feature
// (which additionally requires `#![feature(c_variadic)]` in the crate root).
// On stable they are simply absent; every other `gz*` symbol is present. The
// idiomatic writer accepts pre-formatted `core::fmt::Arguments`, so a faithful
// nightly implementation would parse the C `printf` format string and forward
// the rendered result to `gz::gzvprintf`. We deliberately do NOT ship an
// ABI-incompatible stub on stable — silently mismatching a variadic ABI would
// corrupt callers.

// The platform `vsnprintf`, used only on the nightly variadic path to render C
// `printf`-style format strings — exactly as upstream C zlib's `gzvprintf`
// does. This has no effect on the stable, shipped artifact, which omits
// `gzprintf`/`gzvprintf` entirely and links no C beyond the libc that any Rust
// binary already links.
#[cfg(feature = "c-variadic")]
unsafe extern "C" {
    #[link_name = "vsnprintf"]
    fn libc_vsnprintf(
        buf: *mut c_char,
        size: usize,
        format: *const c_char,
        va: core::ffi::VaList,
    ) -> c_int;
}

/// `int gzvprintf(gzFile file, const char *format, va_list va)` *(nightly only)*
///
/// Available only under the nightly `c-variadic` feature. Renders the C
/// `printf`-style format with its `va_list` into a bounded buffer (mirroring
/// zlib's single bounded `vsnprintf`) and forwards the rendered text to the
/// idiomatic writer. Returns the number of uncompressed bytes written, or a
/// negative zlib error code / `Z_STREAM_ERROR`.
#[cfg(feature = "c-variadic")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzvprintf(
    file: gzFile,
    format: *const c_char,
    va: core::ffi::VaList,
) -> c_int {
    guard_int(Z_STREAM_ERROR, || {
        if file.is_null() || format.is_null() {
            return Z_STREAM_ERROR;
        }
        // SAFETY: non-null handle; borrowed, not owned.
        let mut guard = GzBorrow::new(unsafe { gz_handle(file) });
        let state = &mut *guard;
        // A generously sized, bounded render buffer (zlib bounds its own render
        // to twice the stream buffer); the last byte is reserved for the NUL.
        let mut buf = std::vec![0u8; (crate::gz::GZBUFSIZE << 1) + 1];
        let cap = buf.len();
        // SAFETY: `format` is a valid NUL-terminated C format string; `va`
        // supplies its arguments; `buf` is writable for `cap` bytes.
        let n = unsafe { libc_vsnprintf(buf.as_mut_ptr() as *mut c_char, cap, format, va) };
        if n < 0 {
            return Z_STREAM_ERROR;
        }
        // `vsnprintf` returns the count it *would* have written; clamp to the
        // bytes actually stored (excluding the reserved NUL slot).
        let written = core::cmp::min(n as usize, cap - 1);
        let text = String::from_utf8_lossy(&buf[..written]);
        gz::gzvprintf(state, format_args!("{text}"))
    })
}

/// `int gzprintf(gzFile file, const char *format, ...)` *(nightly only)*
///
/// Available only under the nightly `c-variadic` feature. Forwards its variadic
/// arguments to [`gzvprintf`].
#[cfg(feature = "c-variadic")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzprintf(file: gzFile, format: *const c_char, mut args: ...) -> c_int {
    // SAFETY: forward the live `va_list` to the vprintf implementation, which
    // consumes exactly the arguments described by `format`.
    unsafe { gzvprintf(file, format, args.as_va_list()) }
}

// ---------------------------------------------------------------------------
// Default (stable) `gzprintf`/`gzvprintf` — ABI-compatible error stubs
// ---------------------------------------------------------------------------
//
// Rust's C-variadic support (`...` / `VaList`) is unstable, so the fully
// functional implementations above compile only under the nightly `c-variadic`
// feature. To keep the shipped, stable `cdylib`/`staticlib` a faithful drop-in,
// these symbols must still EXIST on the default build — a C caller linking or
// `LD_PRELOAD`-injecting the object expects to resolve `gzprintf`/`gzvprintf`.
//
// zlib itself defines this exact scenario: a build without secure
// `vsnprintf`/`snprintf` still exports `gzprintf`/`gzvprintf`, but they return
// `Z_STREAM_ERROR` (`zlib.h`: "gzprintf() returns Z_STREAM_ERROR"; the
// `zlibCompileFlags` bit 27 is set — see `crate::util::version`). These stubs
// reproduce that behavior precisely and are mutually exclusive with the
// variadic versions above (`c-variadic` on vs. off), so exactly one definition
// of each symbol is emitted in any build configuration.
//
// ABI note: the stubs use the *fixed* leading parameters of the C prototypes.
// On the SysV (x86-64) and Win64 C ABIs a caller invoking `gzprintf(f, fmt,
// ...)` passes the fixed leading arguments (`file`, `format`) in the same
// registers a non-variadic callee reads, and the caller owns stack cleanup, so
// a non-variadic callee that ignores the trailing arguments is ABI-safe. The C
// `va_list` of `gzvprintf` is represented as an opaque pointer.

/// `int gzvprintf(gzFile file, const char *format, va_list va)`
/// *(default build: error-returning stub)*
///
/// The stable artifact cannot render a C `va_list` (Rust C-variadics are
/// unstable), so — exactly as a zlib built without secure `*printf` does — this
/// returns [`Z_STREAM_ERROR`] unconditionally. The functional implementation is
/// compiled instead under the nightly `c-variadic` feature. See the module note
/// above for the ABI rationale of the fixed parameter list.
#[cfg(not(feature = "c-variadic"))]
#[unsafe(no_mangle)]
pub extern "C" fn gzvprintf(
    _file: gzFile,
    _format: *const c_char,
    _va: *mut core::ffi::c_void,
) -> c_int {
    Z_STREAM_ERROR
}

/// `int gzprintf(gzFile file, const char *format, ...)`
/// *(default build: error-returning stub)*
///
/// See [`gzvprintf`]: the stable artifact provides an ABI-compatible
/// error-returning stub because Rust C-variadics are unstable. Returns
/// [`Z_STREAM_ERROR`], matching a zlib built without secure `*printf`.
#[cfg(not(feature = "c-variadic"))]
#[unsafe(no_mangle)]
pub extern "C" fn gzprintf(_file: gzFile, _format: *const c_char) -> c_int {
    Z_STREAM_ERROR
}

// ===========================================================================
// Phase 4 — Seek / position / status shims  (<- gzlib.c)
// ===========================================================================
//
// The 32-bit-offset entry points (`gzseek`, `gztell`, `gzoffset`) share their
// implementation with the 64-bit variants; the idiomatic layer computes in
// `i64` and the result is narrowed to `z_off_t` (`c_long`) at the boundary,
// exactly matching the C `off_t`/`off64_t` split.

/// `z_off_t gzseek(gzFile file, z_off_t offset, int whence)`
///
/// Repositions the stream. Returns the resulting uncompressed offset, or `-1`
/// on error / for a null handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzseek(file: gzFile, offset: z_off_t, whence: c_int) -> z_off_t {
    guard_off(-1, || {
        if file.is_null() {
            return -1;
        }
        // SAFETY: non-null handle; borrowed, not owned.
        let mut guard = GzBorrow::new(unsafe { gz_handle(file) });
        let state = &mut *guard;
        // Widen the C `off_t` to the engine's 64-bit offset. On 32-bit targets
        // (`z_off_t == i32`) this is a real widening; on 64-bit it is a no-op.
        gz::gzseek(state, offset as z_off64_t, whence)
    }) as z_off_t
}

/// `z_off64_t gzseek64(gzFile file, z_off64_t offset, int whence)` (`ZLIB_1.2.3.3`)
///
/// 64-bit-offset variant of [`gzseek`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzseek64(file: gzFile, offset: z_off64_t, whence: c_int) -> z_off64_t {
    guard_off(-1, || {
        if file.is_null() {
            return -1;
        }
        // SAFETY: non-null handle; borrowed, not owned.
        let mut guard = GzBorrow::new(unsafe { gz_handle(file) });
        let state = &mut *guard;
        gz::gzseek64(state, offset, whence)
    })
}

/// `int gzrewind(gzFile file)`
///
/// Rewinds a read stream to the beginning. Returns `0` on success, or `-1` on
/// error / for a null handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzrewind(file: gzFile) -> c_int {
    guard_int(-1, || {
        if file.is_null() {
            return -1;
        }
        // SAFETY: non-null handle; borrowed, not owned.
        let mut guard = GzBorrow::new(unsafe { gz_handle(file) });
        let state = &mut *guard;
        gz::gzrewind(state)
    })
}

/// `z_off_t gztell(gzFile file)`
///
/// Returns the current uncompressed offset, or `-1` for a null handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gztell(file: gzFile) -> z_off_t {
    guard_off(-1, || {
        if file.is_null() {
            return -1;
        }
        // SAFETY: non-null handle; borrowed immutably.
        let guard = GzBorrow::new(unsafe { gz_handle(file) });
        let state = &*guard;
        gz::gztell(state)
    }) as z_off_t
}

/// `z_off64_t gztell64(gzFile file)` (`ZLIB_1.2.3.3`)
///
/// 64-bit-offset variant of [`gztell`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gztell64(file: gzFile) -> z_off64_t {
    guard_off(-1, || {
        if file.is_null() {
            return -1;
        }
        // SAFETY: non-null handle; borrowed immutably.
        let guard = GzBorrow::new(unsafe { gz_handle(file) });
        let state = &*guard;
        gz::gztell64(state)
    })
}

/// `z_off_t gzoffset(gzFile file)` (`ZLIB_1.2.3.5`)
///
/// Returns the current *compressed* file offset, or `-1` on error / for a null
/// handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzoffset(file: gzFile) -> z_off_t {
    guard_off(-1, || {
        if file.is_null() {
            return -1;
        }
        // SAFETY: non-null handle; borrowed, not owned.
        let mut guard = GzBorrow::new(unsafe { gz_handle(file) });
        let state = &mut *guard;
        gz::gzoffset(state)
    }) as z_off_t
}

/// `z_off64_t gzoffset64(gzFile file)` (`ZLIB_1.2.3.5`)
///
/// 64-bit-offset variant of [`gzoffset`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzoffset64(file: gzFile) -> z_off64_t {
    guard_off(-1, || {
        if file.is_null() {
            return -1;
        }
        // SAFETY: non-null handle; borrowed, not owned.
        let mut guard = GzBorrow::new(unsafe { gz_handle(file) });
        let state = &mut *guard;
        gz::gzoffset64(state)
    })
}

/// `int gzeof(gzFile file)`
///
/// Returns `1` once a read has attempted to go past end of file (mirroring C's
/// `past` flag semantics), otherwise `0`; `0` for a null handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzeof(file: gzFile) -> c_int {
    guard_int(0, || {
        if file.is_null() {
            return 0;
        }
        // SAFETY: non-null handle; borrowed immutably.
        let guard = GzBorrow::new(unsafe { gz_handle(file) });
        let state = &*guard;
        gz::gzeof(state)
    })
}

/// `int gzdirect(gzFile file)` (`ZLIB_1.2.2.3`)
///
/// Returns `1` if the stream is being copied through transparently
/// (uncompressed input), `0` if it is being decompressed as gzip; `0` for a
/// null handle. May trigger a header look on first read, matching C.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzdirect(file: gzFile) -> c_int {
    guard_int(0, || {
        if file.is_null() {
            return 0;
        }
        // SAFETY: non-null handle; borrowed, not owned.
        let mut guard = GzBorrow::new(unsafe { gz_handle(file) });
        let state = &mut *guard;
        gz::gzdirect(state)
    })
}

// ===========================================================================
// Phase 5 — Error / close shims  (<- gzlib.c, gzclose.c)
// ===========================================================================

/// `const char *gzerror(gzFile file, int *errnum)`
///
/// Returns a pointer to the current error message for `file` and, if `errnum`
/// is non-null, writes the machine-readable error code there. The returned
/// pointer is the handle's own NUL-terminated message mirror — the specific
/// `"{path}: {detail}"` text (byte-for-byte matching reference zlib), the
/// literal `"out of memory"` for `Z_MEM_ERROR`, or the empty string when there
/// is no detail (see the module docs). It stays valid until the next `gz*` call
/// records a new error on the handle; the `errnum` value is the authoritative
/// code.
///
/// Matches C by returning `NULL` for a null handle (and does not write
/// `errnum`). A handle that is neither an open reader nor writer likewise
/// yields `NULL`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzerror(file: gzFile, errnum: *mut c_int) -> *const c_char {
    guard_const_ptr(ptr::null(), || -> *const c_char {
        if file.is_null() {
            return ptr::null();
        }
        // SAFETY: non-null handle; borrowed immutably.
        let guard = GzBorrow::new(unsafe { gz_handle(file) });
        let state = &*guard;
        // Only a live reader/writer reports an error (matches C's mode check).
        if state.mode != GzMode::Read && state.mode != GzMode::Write {
            return ptr::null();
        }
        let code = state.err;
        if !errnum.is_null() {
            // SAFETY: `errnum` is a non-null, caller-owned `int` slot.
            unsafe { *errnum = code.as_c_int() };
        }
        // Return the same text the idiomatic `gz::gzerror` reports, but as a
        // stable, NUL-terminated pointer (see the module docs). `Z_MEM_ERROR`
        // deliberately stores no heap message, so synthesise the literal
        // `"out of memory"` without allocating; otherwise hand back a pointer
        // into the handle's owned `msg_c` mirror of `msg` — the specific
        // `"{path}: {detail}"` string, or the empty string when no detail is
        // present. The pointer stays valid until the next `gz*` call records a
        // new error and replaces `msg_c`, exactly matching the C lifetime.
        if code == ReturnCode::MemError {
            c"out of memory".as_ptr()
        } else {
            match &state.msg_c {
                Some(cs) => cs.as_ptr(),
                None => c"".as_ptr(),
            }
        }
    })
}

/// `void gzclearerr(gzFile file)` (`ZLIB_1.2.0.2`)
///
/// Clears the error and end-of-file indicators for `file`. A no-op for a null
/// handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzclearerr(file: gzFile) {
    // Void return: catch any panic and swallow it (never unwind into C).
    let _ = std::panic::catch_unwind(|| {
        if file.is_null() {
            return;
        }
        // SAFETY: non-null handle; borrowed, not owned.
        let mut guard = GzBorrow::new(unsafe { gz_handle(file) });
        let state = &mut *guard;
        gz::gzclearerr(state);
    });
}

/// `int gzclose(gzFile file)`
///
/// Flushes and closes `file` (finishing the gzip stream and freeing buffers for
/// writers), reclaiming the boxed state. Returns a zlib return code, or
/// `Z_STREAM_ERROR` for a null handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzclose(file: gzFile) -> c_int {
    guard_int(Z_STREAM_ERROR, || {
        if file.is_null() {
            return Z_STREAM_ERROR;
        }
        // SAFETY: `file` was produced by `gzopen*`/`gzdopen` as
        // `Box::into_raw(Box<GzHandle>)`; reconstruct the box exactly once to
        // take ownership, then hand its inner `Box<GzState>` to the idiomatic
        // close (which dispatches on read/write mode). The `gzFile_s` prefix is
        // dropped with the handle.
        let handle = unsafe { Box::from_raw(file as *mut GzHandle) };
        gz::gzclose(handle.state)
    })
}

/// `int gzclose_r(gzFile file)` (`ZLIB_1.2.3.5`)
///
/// Closes a read stream specifically. Returns a zlib return code, or
/// `Z_STREAM_ERROR` for a null handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzclose_r(file: gzFile) -> c_int {
    guard_int(Z_STREAM_ERROR, || {
        if file.is_null() {
            return Z_STREAM_ERROR;
        }
        // SAFETY: `file` was produced as `Box::into_raw(Box<GzHandle>)`;
        // reconstruct the owning box exactly once and hand off its inner state.
        let handle = unsafe { Box::from_raw(file as *mut GzHandle) };
        gz::gzclose_r(handle.state)
    })
}

/// `int gzclose_w(gzFile file)` (`ZLIB_1.2.3.5`)
///
/// Closes a write stream specifically, emitting the final `Z_FINISH` block and
/// the gzip trailer. Returns a zlib return code, or `Z_STREAM_ERROR` for a null
/// handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzclose_w(file: gzFile) -> c_int {
    guard_int(Z_STREAM_ERROR, || {
        if file.is_null() {
            return Z_STREAM_ERROR;
        }
        // SAFETY: `file` was produced as `Box::into_raw(Box<GzHandle>)`;
        // reconstruct the owning box exactly once and hand off its inner state.
        let handle = unsafe { Box::from_raw(file as *mut GzHandle) };
        gz::gzclose_w(handle.state)
    })
}

// ===========================================================================
// Windows wide-character open  (<- gzlib.c: gzopen_w)
// ===========================================================================

/// `gzFile gzopen_w(const wchar_t *path, const char *mode)`  *(Windows)*
///
/// Wide-character (`UTF-16`) path variant of [`gzopen`], present only on
/// Windows to match zlib's `_WIN32` build. The UTF-16 path is decoded via
/// `OsString::from_wide` and forwarded to the idiomatic open. Returns `NULL` on
/// any failure.
#[cfg(windows)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzopen_w(path: *const u16, mode: *const c_char) -> gzFile {
    guard_ptr(ptr::null_mut(), || -> gzFile {
        if path.is_null() || mode.is_null() {
            return ptr::null_mut();
        }
        // SAFETY: `path` is a non-null, NUL-terminated wide string; count its
        // code units up to (excluding) the terminator.
        let len = unsafe {
            let mut n = 0usize;
            while *path.add(n) != 0 {
                n += 1;
            }
            n
        };
        // SAFETY: `path[..len]` are `len` initialized `u16` code units.
        let units = unsafe { slice::from_raw_parts(path, len) };
        use std::os::windows::ffi::OsStringExt;
        let os = std::ffi::OsString::from_wide(units);
        // SAFETY: `mode` is non-null (checked) and NUL-terminated.
        let Ok(mode_str) = (unsafe { CStr::from_ptr(mode) }).to_str() else {
            return ptr::null_mut();
        };
        box_state(gz::gzopen(std::path::PathBuf::from(os), mode_str))
    })
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;
    use std::path::PathBuf;

    /// Builds a process-unique temp path (and its `CString` form) for a test.
    fn unique_path(tag: &str) -> (PathBuf, CString) {
        let mut p = std::env::temp_dir();
        p.push(std::format!(
            "zlibrs_ffi_gz_{tag}_{}.gz",
            std::process::id()
        ));
        let c = CString::new(p.to_str().expect("temp path is valid UTF-8")).unwrap();
        (p, c)
    }

    #[test]
    fn write_then_read_round_trip() {
        let (path, cpath) = unique_path("rt");
        let data = b"hello, gzip world!\nsecond line with more bytes\n";
        unsafe {
            // Write path: open "wb", write, close_w.
            let wf = gzopen(cpath.as_ptr(), c"wb".as_ptr());
            assert!(!wf.is_null(), "gzopen for write returned NULL");
            let n = gzwrite(wf, data.as_ptr() as voidpc, data.len() as c_uint);
            assert_eq!(n, data.len() as c_int);
            assert_eq!(gzclose_w(wf), Z_OK);

            // Read path: open "rb", read back, verify bytes, verify EOF.
            let rf = gzopen(cpath.as_ptr(), c"rb".as_ptr());
            assert!(!rf.is_null(), "gzopen for read returned NULL");
            let mut buf = std::vec![0u8; data.len()];
            let got = gzread(rf, buf.as_mut_ptr() as voidp, buf.len() as c_uint);
            assert_eq!(got, data.len() as c_int);
            assert_eq!(&buf[..], &data[..]);
            // Reading past the end yields 0 and sets the EOF indicator.
            let mut extra = [0u8; 1];
            assert_eq!(gzread(rf, extra.as_mut_ptr() as voidp, 1), 0);
            assert_eq!(gzeof(rf), 1);
            assert_eq!(gzclose_r(rf), Z_OK);
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn puts_gets_getc_round_trip() {
        let (path, cpath) = unique_path("lines");
        unsafe {
            let wf = gzopen(cpath.as_ptr(), c"wb".as_ptr());
            assert!(!wf.is_null());
            // "first line\n" is 11 bytes; gzputs returns the count written.
            assert_eq!(gzputs(wf, c"first line\n".as_ptr()), 11);
            assert_eq!(gzputc(wf, b'A' as c_int), b'A' as c_int);
            assert_eq!(gzclose_w(wf), Z_OK);

            let rf = gzopen(cpath.as_ptr(), c"rb".as_ptr());
            assert!(!rf.is_null());
            let mut line = [0u8; 64];
            let p = gzgets(rf, line.as_mut_ptr() as *mut c_char, line.len() as c_int);
            assert!(!p.is_null());
            // The returned line includes the newline and is NUL-terminated.
            let s = CStr::from_ptr(p).to_str().unwrap();
            assert_eq!(s, "first line\n");
            // The trailing 'A' written via gzputc follows.
            assert_eq!(gzgetc(rf), b'A' as c_int);
            // At end of file, gzgetc_ returns -1.
            assert_eq!(gzgetc_(rf), -1);
            assert_eq!(gzclose_r(rf), Z_OK);
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn null_handle_sentinels() {
        unsafe {
            let nul: gzFile = ptr::null_mut();
            let mut b = [0u8; 4];
            // Reads / writes.
            assert_eq!(gzread(nul, b.as_mut_ptr() as voidp, 4), -1);
            assert_eq!(gzwrite(nul, b.as_ptr() as voidpc, 4), 0);
            assert_eq!(gzfread(b.as_mut_ptr() as voidp, 1, 4, nul), 0);
            assert_eq!(gzfwrite(b.as_ptr() as voidpc, 1, 4, nul), 0);
            // Close family.
            assert_eq!(gzclose(nul), Z_STREAM_ERROR);
            assert_eq!(gzclose_r(nul), Z_STREAM_ERROR);
            assert_eq!(gzclose_w(nul), Z_STREAM_ERROR);
            // Status.
            assert_eq!(gzeof(nul), 0);
            assert_eq!(gzdirect(nul), 0);
            // Position.
            assert_eq!(gztell(nul), -1);
            assert_eq!(gztell64(nul), -1);
            assert_eq!(gzoffset(nul), -1);
            assert_eq!(gzoffset64(nul), -1);
            assert_eq!(gzseek(nul, 0, 0), -1);
            assert_eq!(gzseek64(nul, 0, 0), -1);
            assert_eq!(gzrewind(nul), -1);
            // Byte I/O.
            assert_eq!(gzgetc(nul), -1);
            assert_eq!(gzgetc_(nul), -1);
            assert_eq!(gzputc(nul, b'x' as c_int), -1);
            assert_eq!(gzungetc(b'x' as c_int, nul), -1);
            assert_eq!(gzputs(nul, c"x".as_ptr()), -1);
            assert_eq!(gzflush(nul, 0), Z_STREAM_ERROR);
            // Config.
            assert_eq!(gzbuffer(nul, 8192), -1);
            assert_eq!(gzsetparams(nul, 6, 0), Z_STREAM_ERROR);
            // Pointer-returning.
            assert!(gzgets(nul, b.as_mut_ptr() as *mut c_char, 4).is_null());
            assert!(gzerror(nul, ptr::null_mut()).is_null());
            // Void: must not panic or crash.
            gzclearerr(nul);
        }
    }

    #[test]
    fn gzbuffer_before_and_after_io() {
        let (path, cpath) = unique_path("buf");
        unsafe {
            let wf = gzopen(cpath.as_ptr(), c"wb".as_ptr());
            assert!(!wf.is_null());
            // Before any I/O: accepted.
            assert_eq!(gzbuffer(wf, 16384), 0);
            // Trigger I/O.
            let d = b"x";
            assert_eq!(gzwrite(wf, d.as_ptr() as voidpc, 1), 1);
            // After I/O has begun: rejected with -1.
            assert_eq!(gzbuffer(wf, 8192), -1);
            assert_eq!(gzclose_w(wf), Z_OK);
        }
        let _ = std::fs::remove_file(&path);
    }

    #[cfg(unix)]
    #[test]
    fn gzdopen_fd_round_trip() {
        use std::os::fd::IntoRawFd;
        let (path, _cpath) = unique_path("dopen");
        let data = b"descriptor round trip\n";
        unsafe {
            // Write via a descriptor handed to gzdopen.
            let file = std::fs::File::create(&path).unwrap();
            let fd = file.into_raw_fd();
            let wf = gzdopen(fd, c"wb".as_ptr());
            assert!(!wf.is_null());
            assert_eq!(
                gzwrite(wf, data.as_ptr() as voidpc, data.len() as c_uint),
                data.len() as c_int
            );
            assert_eq!(gzclose_w(wf), Z_OK);

            // Read back via a fresh descriptor.
            let file = std::fs::File::open(&path).unwrap();
            let fd = file.into_raw_fd();
            let rf = gzdopen(fd, c"rb".as_ptr());
            assert!(!rf.is_null());
            let mut buf = std::vec![0u8; data.len()];
            assert_eq!(
                gzread(rf, buf.as_mut_ptr() as voidp, buf.len() as c_uint),
                data.len() as c_int
            );
            assert_eq!(&buf[..], &data[..]);
            assert_eq!(gzclose_r(rf), Z_OK);
        }
        let _ = std::fs::remove_file(&path);
    }

    /// C2: the live `gzFile_s` prefix lets the C `gzgetc(g)` *macro* consume
    /// buffered bytes directly through the handle pointer, and the next real
    /// `gz*` call reconciles that consumption into the idiomatic cursor.
    ///
    /// This exercises the [`GzHandle`] prefix / [`GzBorrow`] reconcile-sync
    /// bridge end to end: a real read populates the prefix, we mutate it exactly
    /// as the C macro does (`have--, pos++, *next++`), then assert that a later
    /// library call absorbs the delta and continues byte-exactly.
    #[test]
    fn gzgetc_macro_prefix_reconcile() {
        let (path, cpath) = unique_path("c2prefix");
        let data = b"ABCDEFGHIJ";
        unsafe {
            // Write known bytes and close.
            let wf = gzopen(cpath.as_ptr(), c"wb".as_ptr());
            assert!(!wf.is_null());
            assert_eq!(
                gzwrite(wf, data.as_ptr() as voidpc, data.len() as c_uint),
                data.len() as c_int
            );
            assert_eq!(gzclose_w(wf), Z_OK);

            // Open for read; the handle's prefix starts cleared (`have == 0`).
            let rf = gzopen(cpath.as_ptr(), c"rb".as_ptr());
            assert!(!rf.is_null());

            // First real read returns 'A' and, on shim exit, syncs the prefix so
            // the macro fast-path can take over the buffered remainder.
            assert_eq!(gzgetc(rf), b'A' as c_int);

            // Inspect the live `gzFile_s` prefix exactly as the C macro would.
            let handle = &mut *(rf as *mut GzHandle);
            assert!(
                handle.prefix.have >= 1,
                "prefix must expose buffered bytes after the first read"
            );
            assert!(!handle.prefix.next.is_null(), "prefix.next must be live");
            assert_eq!(handle.prefix.pos, 1, "pos reflects the one byte read");

            // Simulate the macro consuming ONE byte straight from the prefix:
            //   ((g)->have ? ((g)->have--, (g)->pos++, *((g)->next)++) : ...)
            let macro_byte = *handle.prefix.next;
            assert_eq!(macro_byte, b'B', "macro reads the 2nd byte directly");
            handle.prefix.have -= 1;
            handle.prefix.next = handle.prefix.next.add(1);
            handle.prefix.pos += 1;

            // A subsequent library call reconciles the macro-side consumption:
            // gztell reports the reconciled position (2 bytes consumed total).
            assert_eq!(gztell(rf), 2, "gztell reconciles the macro-consumed byte");

            // Reading continues byte-exactly from the 3rd byte onward.
            assert_eq!(gzgetc(rf), b'C' as c_int);
            assert_eq!(gzgetc(rf), b'D' as c_int);

            let mut rest = [0u8; 6];
            assert_eq!(
                gzread(rf, rest.as_mut_ptr() as voidp, rest.len() as c_uint),
                6
            );
            assert_eq!(&rest, b"EFGHIJ");
            assert_eq!(gztell(rf), data.len() as z_off_t);
            assert_eq!(gzclose_r(rf), Z_OK);
        }
        let _ = std::fs::remove_file(&path);
    }
}
