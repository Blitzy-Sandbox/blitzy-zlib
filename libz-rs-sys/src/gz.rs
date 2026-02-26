//! Gzip file I/O FFI wrappers.
//!
//! This module provides `#[unsafe(no_mangle)] extern "C"` wrappers for all 33
//! gzip file I/O functions (`gz*` family) from the zlib public API. These map
//! to the "utility functions" section of `win32/zlib.def` (lines 45–71), the
//! "large file functions" section (lines 73–76), and additional symbols
//! (`gzgetc_` line 95, `gzopen_w` line 104).
//!
//! The entire module is conditionally compiled with `#[cfg(feature = "gz-io")]`,
//! corresponding to the C `Z_SOLO` exclusion pattern (AAP Section 0.4.3).
//!
//! # Safety
//!
//! All functions that receive pointer arguments validate them for null
//! before dereferencing. The `gzFile` handle is an opaque pointer that
//! wraps a heap-allocated [`GzFile`](zlib_rs::gz::GzFile) enum. Ownership
//! is transferred at open time (via `Box::into_raw`) and reclaimed at
//! close time (via `Box::from_raw`).
//!
//! All functions use `extern "C"` (not `extern "C-unwind"`) per AAP
//! Section 0.7.3 to abort on panic at the FFI boundary rather than
//! triggering undefined behavior through stack unwinding into C code.
//!
//! In accordance with Rust 2024 edition requirements, every unsafe
//! operation within `unsafe fn` bodies is wrapped in explicit `unsafe {}`
//! blocks, and all symbol-exporting attributes use `#[unsafe(no_mangle)]`.
//!
//! # Variadic Functions
//!
//! `gzprintf` and `gzvprintf` are C variadic functions. On stable Rust,
//! c-variadic function definitions are not supported. These functions are
//! provided with a limited non-variadic signature that writes the format
//! string as-is. Full variadic support requires nightly Rust with the
//! `c_variadic` feature.

use std::ffi::CStr;
use std::io::{Read, SeekFrom, Write};
use std::ptr;

use libc::{c_char, c_int, c_uint, c_void};

use crate::types::{gzFile, z_off64_t, z_off_t, z_size_t, Z_ERRNO, Z_OK, Z_STREAM_ERROR};

use zlib_rs::gz::{GzFile as RustGzFile, GzReader, GzWriter};

// ─── Internal Helpers ───────────────────────────────────────────────────────

/// Converts a raw `gzFile` opaque pointer back to a mutable reference to
/// the Rust `GzFile` enum.
///
/// # Safety
///
/// The caller must ensure that `file` was produced by one of the `gzopen*`
/// or `gzdopen` functions in this module (i.e., it points to a valid,
/// heap-allocated `RustGzFile`). The returned reference borrows the
/// pointed-to value for an unbound lifetime; the caller must ensure no
/// aliasing violations occur.
///
/// Returns `None` if `file` is null.
#[inline]
unsafe fn gz_handle_mut(file: gzFile) -> Option<&'static mut RustGzFile> {
    if file.is_null() {
        return None;
    }
    Some(unsafe { &mut *(file as *mut RustGzFile) })
}

/// Converts a raw `gzFile` opaque pointer to a shared reference to the
/// Rust `GzFile` enum.
///
/// # Safety
///
/// Same requirements as [`gz_handle_mut`], except the returned reference
/// is shared (immutable).
#[inline]
unsafe fn gz_handle_ref(file: gzFile) -> Option<&'static RustGzFile> {
    if file.is_null() {
        return None;
    }
    Some(unsafe { &*(file as *const RustGzFile) })
}

/// Boxes a `RustGzFile` and returns the raw pointer cast to `gzFile`.
///
/// Ownership of the heap allocation is transferred to the caller. The
/// pointer must be reclaimed via `Box::from_raw` in a close function
/// to avoid a memory leak.
#[inline]
fn gz_box_into_raw(gz: RustGzFile) -> gzFile {
    let boxed = Box::new(gz);
    Box::into_raw(boxed) as gzFile
}

/// Converts a C string pointer to a Rust `&str`, returning `None` if the
/// pointer is null or the content is not valid UTF-8.
///
/// # Safety
///
/// The caller must ensure that `ptr` (if non-null) points to a valid,
/// null-terminated C string.
#[inline]
unsafe fn cstr_to_str<'a>(ptr: *const c_char) -> Option<&'a str> {
    if ptr.is_null() {
        return None;
    }
    unsafe { CStr::from_ptr(ptr) }.to_str().ok()
}

/// Converts a C-style `whence` parameter (0=`SEEK_SET`, 1=`SEEK_CUR`) to
/// a Rust [`SeekFrom`] value. The `offset` is carried in a separate
/// parameter, so the [`SeekFrom`] variant's embedded value is always zero
/// (the actual offset is passed separately to the `seek` method).
///
/// Returns `None` for invalid `whence` values (including `SEEK_END`,
/// which is not supported for gzip streams).
#[inline]
fn whence_to_seek_from(whence: c_int) -> Option<SeekFrom> {
    match whence {
        0 => Some(SeekFrom::Start(0)),   // SEEK_SET
        1 => Some(SeekFrom::Current(0)), // SEEK_CUR
        _ => None,
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// File Open / Close Functions
// ═══════════════════════════════════════════════════════════════════════════

/// Opens a gzip file for reading or writing.
///
/// Wraps `zlib_rs::gz::GzFile::open()`. The mode string is parsed to
/// determine the operation (`'r'` for read, `'w'`/`'a'` for write/append)
/// along with optional compression level and strategy flags.
///
/// # Parameters
///
/// * `path` — Null-terminated C string with the file path.
/// * `mode` — Null-terminated mode string (e.g., `"rb"`, `"wb9"`, `"ab"`).
///
/// # Returns
///
/// An opaque `gzFile` handle on success, or null on failure.
///
/// # Symbol
///
/// `gzopen` — `win32/zlib.def` line 45.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzopen(path: *const c_char, mode: *const c_char) -> gzFile {
    let Some(path_str) = (unsafe { cstr_to_str(path) }) else {
        return ptr::null_mut();
    };
    let Some(mode_str) = (unsafe { cstr_to_str(mode) }) else {
        return ptr::null_mut();
    };
    match RustGzFile::open(path_str, mode_str) {
        Ok(gz) => gz_box_into_raw(gz),
        Err(_) => ptr::null_mut(),
    }
}

/// Opens a gzip file for reading or writing (64-bit offset version).
///
/// Identical to [`gzopen`] — on 64-bit platforms (and in this Rust
/// implementation) file offsets are always 64-bit, so this function
/// is a direct alias.
///
/// # Symbol
///
/// `gzopen64` — `win32/zlib.def` line 73.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzopen64(path: *const c_char, mode: *const c_char) -> gzFile {
    unsafe { gzopen(path, mode) }
}

/// Opens a gzip file using a wide-character path (Windows only).
///
/// This function is only available on Windows targets. It accepts a
/// wide-character (`wchar_t *`) path for Unicode file name support.
///
/// # Symbol
///
/// `gzopen_w` — `win32/zlib.def` line 104.
#[cfg(target_os = "windows")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzopen_w(
    path: *const libc::wchar_t,
    mode: *const c_char,
) -> gzFile {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;

    if path.is_null() || mode.is_null() {
        return ptr::null_mut();
    }

    // Measure the wide string length.
    let mut len = 0usize;
    let mut p = path;
    while unsafe { *p } != 0 {
        len += 1;
        p = unsafe { p.add(1) };
    }
    let wide_slice = unsafe { std::slice::from_raw_parts(path as *const u16, len) };
    let os_string = OsString::from_wide(wide_slice);
    let path_str = match os_string.to_str() {
        Some(s) => s,
        None => return ptr::null_mut(),
    };

    let Some(mode_str) = (unsafe { cstr_to_str(mode) }) else {
        return ptr::null_mut();
    };

    match RustGzFile::open(path_str, mode_str) {
        Ok(gz) => gz_box_into_raw(gz),
        Err(_) => ptr::null_mut(),
    }
}

/// Stub for `gzopen_w` on non-Windows targets.
///
/// Always returns null since wide-character paths are a Windows-only
/// concept.
#[cfg(not(target_os = "windows"))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzopen_w(
    _path: *const c_void,
    _mode: *const c_char,
) -> gzFile {
    ptr::null_mut()
}

/// Associates a `gzFile` with an already-open file descriptor.
///
/// The mode string determines whether the file descriptor is used
/// for reading or writing. Takes ownership of the file descriptor —
/// it will be closed when the gzFile is closed.
///
/// # Parameters
///
/// * `fd` — An open file descriptor.
/// * `mode` — Null-terminated mode string.
///
/// # Returns
///
/// An opaque `gzFile` handle on success, or null on failure.
///
/// # Symbol
///
/// `gzdopen` — `win32/zlib.def` line 46.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzdopen(fd: c_int, mode: *const c_char) -> gzFile {
    let Some(mode_str) = (unsafe { cstr_to_str(mode) }) else {
        return ptr::null_mut();
    };

    // Determine read vs write from mode string.
    let is_read = mode_str.contains('r');

    #[cfg(unix)]
    {
        use std::os::unix::io::FromRawFd;
        let file = unsafe { std::fs::File::from_raw_fd(fd) };

        let result = if is_read {
            GzReader::from_file(file).map(RustGzFile::Reader)
        } else {
            GzWriter::from_file(file).map(RustGzFile::Writer)
        };

        match result {
            Ok(gz) => gz_box_into_raw(gz),
            Err(_) => ptr::null_mut(),
        }
    }

    #[cfg(not(unix))]
    {
        // Non-unix platforms: not supported in this implementation.
        let _ = fd;
        let _ = is_read;
        ptr::null_mut()
    }
}

/// Sets the internal buffer size for subsequent I/O operations.
///
/// Must be called immediately after opening, before any read or write.
/// Returns 0 on success, -1 on failure (buffers already allocated or
/// invalid size).
///
/// # Symbol
///
/// `gzbuffer` — `win32/zlib.def` line 47.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzbuffer(file: gzFile, size: c_uint) -> c_int {
    let Some(gz) = (unsafe { gz_handle_mut(file) }) else {
        return -1;
    };
    match gz.set_buffer_size(size as usize) {
        Ok(()) => 0,
        Err(_) => -1,
    }
}

/// Dynamically updates the compression level and strategy.
///
/// Only valid for files opened for writing. If the level and strategy
/// have not changed, this is a no-op.
///
/// # Symbol
///
/// `gzsetparams` — `win32/zlib.def` line 48.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzsetparams(file: gzFile, level: c_int, strategy: c_int) -> c_int {
    let Some(gz) = (unsafe { gz_handle_mut(file) }) else {
        return Z_STREAM_ERROR;
    };
    match gz {
        RustGzFile::Writer(writer) => match writer.set_params(level, strategy) {
            Ok(()) => Z_OK,
            Err(_) => Z_STREAM_ERROR,
        },
        RustGzFile::Reader(_) => Z_STREAM_ERROR,
    }
}

/// Closes the gzip file, flushing any pending output.
///
/// For writers, this finalizes the gzip stream before closing. The
/// `gzFile` handle is invalid after this call.
///
/// # Symbol
///
/// `gzclose` — `win32/zlib.def` line 67.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzclose(file: gzFile) -> c_int {
    if file.is_null() {
        return Z_STREAM_ERROR;
    }
    // Reclaim ownership of the boxed GzFile and close.
    let gz = unsafe { *Box::from_raw(file as *mut RustGzFile) };
    match gz.close() {
        Ok(()) => Z_OK,
        Err(_) => Z_ERRNO,
    }
}

/// Closes a gzip file opened for reading.
///
/// Equivalent to [`gzclose`] but asserts the file is a reader. Returns
/// `Z_STREAM_ERROR` if the handle is a writer. In the error case, the
/// handle is NOT freed (matching C zlib semantics where calling
/// `gzclose_r` on a writer is a programming error that leaks).
///
/// # Symbol
///
/// `gzclose_r` — `win32/zlib.def` line 68.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzclose_r(file: gzFile) -> c_int {
    if file.is_null() {
        return Z_STREAM_ERROR;
    }
    let gz = unsafe { Box::from_raw(file as *mut RustGzFile) };
    if matches!(gz.as_ref(), RustGzFile::Writer(_)) {
        // Wrong type — return the pointer without dropping the writer.
        let _ = Box::into_raw(gz);
        return Z_STREAM_ERROR;
    }
    // Reader variant: Drop reclaims resources (file handle, buffers).
    drop(gz);
    Z_OK
}

/// Closes a gzip file opened for writing.
///
/// Equivalent to [`gzclose`] but asserts the file is a writer. Returns
/// `Z_STREAM_ERROR` if the handle is a reader. In the error case, the
/// handle is NOT freed (matching C zlib semantics where calling
/// `gzclose_w` on a reader is a programming error that leaks).
///
/// # Symbol
///
/// `gzclose_w` — `win32/zlib.def` line 69.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzclose_w(file: gzFile) -> c_int {
    if file.is_null() {
        return Z_STREAM_ERROR;
    }
    let gz = unsafe { Box::from_raw(file as *mut RustGzFile) };
    // Check variant before consuming the box.
    if matches!(gz.as_ref(), RustGzFile::Reader(_)) {
        // Wrong type — return the pointer without dropping the reader.
        let _ = Box::into_raw(gz);
        return Z_STREAM_ERROR;
    }
    // We know it is a Writer. Move out of the Box and close explicitly.
    let gz_val = *gz;
    match gz_val {
        RustGzFile::Writer(writer) => match writer.close() {
            Ok(()) => Z_OK,
            Err(_) => Z_ERRNO,
        },
        // Unreachable due to the check above, but satisfy exhaustiveness.
        RustGzFile::Reader(_) => Z_STREAM_ERROR,
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Read Functions
// ═══════════════════════════════════════════════════════════════════════════

/// Reads up to `len` uncompressed bytes from the gzip file.
///
/// Returns the number of bytes actually read (which may be less than
/// `len`), 0 on end-of-file, or -1 on error.
///
/// # Symbol
///
/// `gzread` — `win32/zlib.def` line 49.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzread(file: gzFile, buf: *mut c_void, len: c_uint) -> c_int {
    let Some(gz) = (unsafe { gz_handle_mut(file) }) else {
        return -1;
    };
    if buf.is_null() && len > 0 {
        return -1;
    }
    let reader = match gz {
        RustGzFile::Reader(r) => r,
        RustGzFile::Writer(_) => return -1,
    };

    if len == 0 {
        return 0;
    }

    let slice = unsafe { std::slice::from_raw_parts_mut(buf as *mut u8, len as usize) };
    match reader.read(slice) {
        Ok(n) => {
            #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
            {
                n as c_int
            }
        }
        Err(_) => -1,
    }
}

/// Reads items from a gzip file (`fread`-style interface).
///
/// Reads up to `nitems` items each of `size` bytes. Returns the number
/// of full items read (which may be less than `nitems` on EOF or error).
///
/// # Symbol
///
/// `gzfread` — `win32/zlib.def` line 50.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzfread(
    buf: *mut c_void,
    size: z_size_t,
    nitems: z_size_t,
    file: gzFile,
) -> z_size_t {
    let Some(gz) = (unsafe { gz_handle_mut(file) }) else {
        return 0;
    };
    if buf.is_null() || size == 0 || nitems == 0 {
        return 0;
    }
    let reader = match gz {
        RustGzFile::Reader(r) => r,
        RustGzFile::Writer(_) => return 0,
    };

    let total_bytes = match size.checked_mul(nitems) {
        Some(n) => n,
        None => return 0,
    };

    let slice = unsafe { std::slice::from_raw_parts_mut(buf as *mut u8, total_bytes) };
    match reader.read(slice) {
        Ok(n) => n / size,
        Err(_) => 0,
    }
}

/// Reads a line from the gzip file, like `fgets`.
///
/// Reads bytes until a newline is found, `len - 1` bytes have been read,
/// or end-of-file is reached. A null terminator is always appended.
///
/// Returns `buf` on success, or null on error/EOF with no data read.
///
/// # Symbol
///
/// `gzgets` — `win32/zlib.def` line 56.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzgets(
    file: gzFile,
    buf: *mut c_char,
    len: c_int,
) -> *mut c_char {
    let Some(gz) = (unsafe { gz_handle_mut(file) }) else {
        return ptr::null_mut();
    };
    if buf.is_null() || len < 1 {
        return ptr::null_mut();
    }
    let reader = match gz {
        RustGzFile::Reader(r) => r,
        RustGzFile::Writer(_) => return ptr::null_mut(),
    };

    // Create a slice for the output buffer (len bytes including null terminator).
    let out_slice =
        unsafe { std::slice::from_raw_parts_mut(buf as *mut u8, len as usize) };

    match reader.gets(out_slice) {
        Ok(n) if n > 0 => {
            // Null-terminate after the data (C convention).
            if n < out_slice.len() {
                out_slice[n] = 0;
            }
            buf
        }
        _ => {
            // On error or zero bytes, null-terminate and return NULL.
            out_slice[0] = 0;
            ptr::null_mut()
        }
    }
}

/// Reads a single byte from the gzip file.
///
/// Returns the byte value as an `int` (0–255), or -1 on end-of-file
/// or error.
///
/// # Symbol
///
/// `gzgetc` — `win32/zlib.def` line 58.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzgetc(file: gzFile) -> c_int {
    let Some(gz) = (unsafe { gz_handle_mut(file) }) else {
        return -1;
    };
    let reader = match gz {
        RustGzFile::Reader(r) => r,
        RustGzFile::Writer(_) => return -1,
    };
    match reader.getc() {
        Ok(byte) => c_int::from(byte),
        Err(_) => -1,
    }
}

/// Backward-compatible alias for [`gzgetc`].
///
/// This function exists so that the `gzgetc` name can be used as a macro
/// in the C header while still exporting a function symbol.
///
/// # Symbol
///
/// `gzgetc_` — `win32/zlib.def` line 95.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzgetc_(file: gzFile) -> c_int {
    unsafe { gzgetc(file) }
}

/// Pushes a byte back into the gzip read stream.
///
/// The pushed byte will be the next byte returned by subsequent read
/// operations. Returns `c` on success, or -1 on error.
///
/// # Symbol
///
/// `gzungetc` — `win32/zlib.def` line 59.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzungetc(c: c_int, file: gzFile) -> c_int {
    let Some(gz) = (unsafe { gz_handle_mut(file) }) else {
        return -1;
    };
    let reader = match gz {
        RustGzFile::Reader(r) => r,
        RustGzFile::Writer(_) => return -1,
    };
    // C gzungetc accepts int; only the low byte is pushed.
    // Return -1 if c is out of single-byte range.
    if c < 0 || c > 255 {
        return -1;
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let byte = c as u8;
    match reader.ungetc(byte) {
        Ok(()) => c,
        Err(_) => -1,
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Write Functions
// ═══════════════════════════════════════════════════════════════════════════

/// Writes `len` uncompressed bytes to the gzip file.
///
/// Returns the number of bytes written, or 0 on error.
///
/// # Symbol
///
/// `gzwrite` — `win32/zlib.def` line 51.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzwrite(file: gzFile, buf: *const c_void, len: c_uint) -> c_int {
    let Some(gz) = (unsafe { gz_handle_mut(file) }) else {
        return 0;
    };
    if buf.is_null() && len > 0 {
        return 0;
    }
    let writer = match gz {
        RustGzFile::Writer(w) => w,
        RustGzFile::Reader(_) => return 0,
    };

    if len == 0 {
        return 0;
    }

    let slice = unsafe { std::slice::from_raw_parts(buf as *const u8, len as usize) };
    match writer.write(slice) {
        Ok(n) => {
            #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
            {
                n as c_int
            }
        }
        Err(_) => 0,
    }
}

/// Writes items to a gzip file (`fwrite`-style interface).
///
/// Writes up to `nitems` items each of `size` bytes. Returns the number
/// of full items written.
///
/// # Symbol
///
/// `gzfwrite` — `win32/zlib.def` line 52.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzfwrite(
    buf: *const c_void,
    size: z_size_t,
    nitems: z_size_t,
    file: gzFile,
) -> z_size_t {
    let Some(gz) = (unsafe { gz_handle_mut(file) }) else {
        return 0;
    };
    if buf.is_null() || size == 0 || nitems == 0 {
        return 0;
    }
    let writer = match gz {
        RustGzFile::Writer(w) => w,
        RustGzFile::Reader(_) => return 0,
    };

    let total_bytes = match size.checked_mul(nitems) {
        Some(n) => n,
        None => return 0,
    };

    let slice = unsafe { std::slice::from_raw_parts(buf as *const u8, total_bytes) };
    match writer.write(slice) {
        Ok(n) => n / size,
        Err(_) => 0,
    }
}

/// Writes a formatted string to the gzip file (C variadic).
///
/// In the original C API, this function accepts `printf`-style format
/// arguments. On stable Rust, C variadic functions cannot be defined.
/// This implementation accepts only the format string pointer and writes
/// it as-is (treating it as a pre-formatted string).
///
/// For full variadic support, nightly Rust with the `c_variadic` feature
/// is required.
///
/// # Symbol
///
/// `gzprintf` — `win32/zlib.def` line 53.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzprintf(file: gzFile, format: *const c_char) -> c_int {
    let Some(gz) = (unsafe { gz_handle_mut(file) }) else {
        return -1;
    };
    if format.is_null() {
        return -1;
    }
    let writer = match gz {
        RustGzFile::Writer(w) => w,
        RustGzFile::Reader(_) => return -1,
    };

    let fmt_str = match unsafe { CStr::from_ptr(format) }.to_str() {
        Ok(s) => s,
        Err(_) => return -1,
    };

    match writer.printf(fmt_str) {
        Ok(n) => {
            #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
            {
                n as c_int
            }
        }
        Err(_) => -1,
    }
}

/// Writes a formatted string using a `va_list` (C variadic).
///
/// On stable Rust, `va_list` types are not available. This implementation
/// is provided as a symbol stub that returns -1. Full support requires
/// nightly Rust with the `c_variadic` feature.
///
/// # Symbol
///
/// `gzvprintf` — `win32/zlib.def` line 54.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzvprintf(
    _file: gzFile,
    _format: *const c_char,
    _va: *mut c_void,
) -> c_int {
    // va_list is not available on stable Rust. Return error.
    // A full implementation would require nightly Rust with c_variadic.
    -1
}

/// Writes a null-terminated string to the gzip file.
///
/// The null terminator is not written. Returns the number of characters
/// written, or -1 on error.
///
/// # Symbol
///
/// `gzputs` — `win32/zlib.def` line 55.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzputs(file: gzFile, s: *const c_char) -> c_int {
    let Some(gz) = (unsafe { gz_handle_mut(file) }) else {
        return -1;
    };
    if s.is_null() {
        return -1;
    }
    let writer = match gz {
        RustGzFile::Writer(w) => w,
        RustGzFile::Reader(_) => return -1,
    };

    let c_str = unsafe { CStr::from_ptr(s) };
    let bytes = c_str.to_bytes();
    match writer.write(bytes) {
        Ok(n) => {
            #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
            {
                n as c_int
            }
        }
        Err(_) => -1,
    }
}

/// Writes a single byte to the gzip file.
///
/// Returns the byte value written, or -1 on error.
///
/// # Symbol
///
/// `gzputc` — `win32/zlib.def` line 57.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzputc(file: gzFile, c: c_int) -> c_int {
    let Some(gz) = (unsafe { gz_handle_mut(file) }) else {
        return -1;
    };
    let writer = match gz {
        RustGzFile::Writer(w) => w,
        RustGzFile::Reader(_) => return -1,
    };

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let byte = c as u8;
    match writer.putc(byte) {
        Ok(()) => c,
        Err(_) => -1,
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Flush Function
// ═══════════════════════════════════════════════════════════════════════════

/// Flushes all pending output to the gzip file.
///
/// The `flush` parameter can be `Z_SYNC_FLUSH`, `Z_FULL_FLUSH`, or
/// `Z_FINISH`. Flushing with `Z_FINISH` writes out the gzip trailer.
///
/// Returns `Z_OK` on success, or an error code.
///
/// # Symbol
///
/// `gzflush` — `win32/zlib.def` line 60.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzflush(file: gzFile, flush: c_int) -> c_int {
    let Some(gz) = (unsafe { gz_handle_mut(file) }) else {
        return Z_STREAM_ERROR;
    };
    let writer = match gz {
        RustGzFile::Writer(w) => w,
        RustGzFile::Reader(_) => return Z_STREAM_ERROR,
    };
    match writer.gz_flush(flush) {
        Ok(code) => code,
        Err(_) => Z_ERRNO,
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Seek / Position Functions
// ═══════════════════════════════════════════════════════════════════════════

/// Seeks to a position in the uncompressed data stream (64-bit offset).
///
/// Supports `SEEK_SET` (0) and `SEEK_CUR` (1). `SEEK_END` is not
/// supported for gzip streams. For write-mode files, only forward seeks
/// are supported.
///
/// Returns the resulting offset from the beginning of the uncompressed
/// stream, or -1 on error.
///
/// # Symbol
///
/// `gzseek64` — `win32/zlib.def` line 74.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzseek64(
    file: gzFile,
    offset: z_off64_t,
    whence: c_int,
) -> z_off64_t {
    let Some(gz) = (unsafe { gz_handle_mut(file) }) else {
        return -1;
    };
    let Some(seek_from) = whence_to_seek_from(whence) else {
        return -1;
    };
    match gz {
        RustGzFile::Reader(reader) => match reader.seek(offset, seek_from) {
            Ok(pos) => pos,
            Err(_) => -1,
        },
        RustGzFile::Writer(writer) => match writer.seek(offset, seek_from) {
            Ok(pos) => pos,
            Err(_) => -1,
        },
    }
}

/// Seeks to a position in the uncompressed data stream.
///
/// This is the non-64-bit version. On most platforms, `z_off_t` is
/// already 64-bit, making this equivalent to [`gzseek64`].
///
/// # Symbol
///
/// `gzseek` — `win32/zlib.def` line 61.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzseek(file: gzFile, offset: z_off_t, whence: c_int) -> z_off_t {
    #[allow(clippy::cast_possible_truncation)]
    unsafe {
        gzseek64(file, offset as z_off64_t, whence) as z_off_t
    }
}

/// Rewinds a gzip file opened for reading to the beginning.
///
/// Equivalent to `gzseek(file, 0, SEEK_SET)` but also resets the
/// internal decompression state.
///
/// Returns 0 on success, -1 on error.
///
/// # Symbol
///
/// `gzrewind` — `win32/zlib.def` line 62.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzrewind(file: gzFile) -> c_int {
    let Some(gz) = (unsafe { gz_handle_mut(file) }) else {
        return -1;
    };
    let reader = match gz {
        RustGzFile::Reader(r) => r,
        RustGzFile::Writer(_) => return -1,
    };
    match reader.rewind() {
        Ok(()) => 0,
        Err(_) => -1,
    }
}

/// Returns the current position in the uncompressed stream (64-bit).
///
/// This position accounts for data that has been read or written but
/// not yet flushed.
///
/// Returns the position, or -1 on error.
///
/// # Symbol
///
/// `gztell64` — `win32/zlib.def` line 75.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gztell64(file: gzFile) -> z_off64_t {
    let Some(gz) = (unsafe { gz_handle_ref(file) }) else {
        return -1;
    };
    match gz {
        RustGzFile::Reader(reader) => reader.tell(),
        RustGzFile::Writer(writer) => writer.tell(),
    }
}

/// Returns the current position in the uncompressed stream.
///
/// Non-64-bit version of [`gztell64`].
///
/// # Symbol
///
/// `gztell` — `win32/zlib.def` line 63.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gztell(file: gzFile) -> z_off_t {
    #[allow(clippy::cast_possible_truncation)]
    unsafe {
        gztell64(file) as z_off_t
    }
}

/// Returns the current byte offset in the compressed file (64-bit).
///
/// This is the position in the underlying file, adjusted for buffered
/// data, not the position in the uncompressed stream.
///
/// Returns the offset, or -1 on error.
///
/// # Symbol
///
/// `gzoffset64` — `win32/zlib.def` line 76.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzoffset64(file: gzFile) -> z_off64_t {
    let Some(gz) = (unsafe { gz_handle_mut(file) }) else {
        return -1;
    };
    match gz {
        RustGzFile::Reader(reader) => reader.offset(),
        RustGzFile::Writer(writer) => writer.offset(),
    }
}

/// Returns the current byte offset in the compressed file.
///
/// Non-64-bit version of [`gzoffset64`].
///
/// # Symbol
///
/// `gzoffset` — `win32/zlib.def` line 64.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzoffset(file: gzFile) -> z_off_t {
    #[allow(clippy::cast_possible_truncation)]
    unsafe {
        gzoffset64(file) as z_off_t
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Status / Error Functions
// ═══════════════════════════════════════════════════════════════════════════

/// Returns 1 if the end of the uncompressed data has been reached.
///
/// Returns 0 if not at EOF or on error.
///
/// # Symbol
///
/// `gzeof` — `win32/zlib.def` line 65.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzeof(file: gzFile) -> c_int {
    let Some(gz) = (unsafe { gz_handle_ref(file) }) else {
        return 0;
    };
    match gz {
        RustGzFile::Reader(reader) => {
            if reader.eof() {
                1
            } else {
                0
            }
        }
        RustGzFile::Writer(_) => 0,
    }
}

/// Returns 1 if the file is being read transparently (not gzip).
///
/// A direct/transparent read means the input is not gzip-compressed and
/// bytes are passed through without decompression.
///
/// # Symbol
///
/// `gzdirect` — `win32/zlib.def` line 66.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzdirect(file: gzFile) -> c_int {
    let Some(gz) = (unsafe { gz_handle_mut(file) }) else {
        return 0;
    };
    match gz {
        RustGzFile::Reader(reader) => {
            if reader.is_direct() {
                1
            } else {
                0
            }
        }
        RustGzFile::Writer(_) => 0,
    }
}

/// Returns the error code and message for the last operation.
///
/// Sets `*errnum` to the error code and returns a pointer to the error
/// message string. If no error, returns an empty string and sets
/// `*errnum` to `Z_OK`.
///
/// The returned string pointer is valid for the lifetime of the process
/// (all strings are static). This matches C zlib behaviour for the
/// standard error messages. Custom error messages containing file paths
/// are mapped to their corresponding static category string.
///
/// # Symbol
///
/// `gzerror` — `win32/zlib.def` line 70.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzerror(file: gzFile, errnum: *mut c_int) -> *const c_char {
    // Static null-terminated error messages matching C zlib `z_errmsg[]`.
    static EMPTY: &[u8] = b"\0";
    static OOM_MSG: &[u8] = b"out of memory\0";
    static STREAM_MSG: &[u8] = b"stream error\0";
    static DATA_MSG: &[u8] = b"data error\0";
    static BUF_MSG: &[u8] = b"buffer error\0";
    static ERRNO_MSG: &[u8] = b"file error\0";
    static VERSION_MSG: &[u8] = b"incompatible version\0";

    let Some(gz) = (unsafe { gz_handle_ref(file) }) else {
        if !errnum.is_null() {
            unsafe { *errnum = Z_STREAM_ERROR };
        }
        return EMPTY.as_ptr().cast::<c_char>();
    };

    let code = match gz {
        RustGzFile::Reader(reader) => reader.error_code(),
        RustGzFile::Writer(writer) => writer.error_code(),
    };

    if !errnum.is_null() {
        unsafe { *errnum = code };
    }

    // Map error code to a static C-compatible message string.
    let msg = match code {
        0 | 1 => EMPTY.as_ptr(),     // Z_OK (0), Z_STREAM_END (1)
        2 => EMPTY.as_ptr(),          // Z_NEED_DICT (no message)
        -1 => ERRNO_MSG.as_ptr(),     // Z_ERRNO
        -2 => STREAM_MSG.as_ptr(),    // Z_STREAM_ERROR
        -3 => DATA_MSG.as_ptr(),      // Z_DATA_ERROR
        -4 => OOM_MSG.as_ptr(),       // Z_MEM_ERROR
        -5 => BUF_MSG.as_ptr(),       // Z_BUF_ERROR
        -6 => VERSION_MSG.as_ptr(),   // Z_VERSION_ERROR
        _ => EMPTY.as_ptr(),
    };
    msg.cast::<c_char>()
}

/// Clears the error and end-of-file state.
///
/// Resets the internal error code to `Z_OK` and clears any EOF flags,
/// allowing further read attempts (e.g., if the file has been extended).
///
/// # Symbol
///
/// `gzclearerr` — `win32/zlib.def` line 71.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzclearerr(file: gzFile) {
    let Some(gz) = (unsafe { gz_handle_mut(file) }) else {
        return;
    };
    match gz {
        RustGzFile::Reader(reader) => reader.clearerr(),
        RustGzFile::Writer(writer) => writer.clearerr(),
    }
}
