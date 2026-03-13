//! C-compatible FFI wrappers for zlib version, informational, and undocumented
//! utility functions.
//!
//! This module provides `#[unsafe(no_mangle)] extern "C"` wrapper functions that
//! bridge between the C zlib version/info API and the safe Rust implementations
//! in [`zlib_rs`].
//!
//! # Exported Symbols
//!
//! All 10 version/info symbols from `win32/zlib.def`:
//!
//! ## Basic Info (def lines 4, 33)
//! - [`zlibVersion`] — Return the library version string
//! - [`zlibCompileFlags`] — Return compile-time configuration bit flags
//!
//! ## Undocumented Functions (def lines 96–103)
//! - [`zError`] — Convert error code to human-readable message
//! - [`inflateSyncPoint`] — Check if inflate is at a sync point
//! - [`get_crc_table`] — Return pointer to the CRC-32 lookup table
//! - [`inflateUndermine`] — Toggle inflate distance-check undermining
//! - [`inflateValidate`] — Toggle inflate check-value validation
//! - [`inflateCodesUsed`] — Return number of code table entries used
//! - [`inflateResetKeep`] — Reset inflate keeping dictionary and header
//! - [`deflateResetKeep`] — Reset deflate keeping dictionary
//!
//! # Safety Contract
//!
//! Functions accepting raw `*mut z_stream` pointers validate them for null before
//! dereferencing. Null pointers cause an immediate return of [`Z_STREAM_ERROR`].
//!
//! Functions that accept only scalar parameters or return static data (like
//! `zlibVersion`, `zlibCompileFlags`, `get_crc_table`) do **not** require
//! `unsafe` and are safe `extern "C"` functions.
//!
//! All functions use `extern "C"` (not `extern "C-unwind"`) to abort on panic
//! at the FFI boundary rather than triggering undefined behavior through stack
//! unwinding into C code (per AAP Section 0.7.3).

use libc::{c_char, c_int, c_ulong};

use crate::types::{Z_STREAM_ERROR, z_stream};

// =============================================================================
// Null-Terminated Error Messages for FFI
// =============================================================================
//
// The safe Rust core (`zlib_rs::util::err_msg`) returns `&'static str` which
// is NOT null-terminated. For C interoperability the FFI layer maintains its
// own set of null-terminated byte strings, character-for-character identical
// to the C `z_errmsg[10]` array in `zutil.c` lines 13–24.
//
// Indexing scheme: `index = 2 - error_code`, same as the C `ERR_MSG(err)` macro.

/// Null-terminated error message for `Z_NEED_DICT` (code 2, index 0).
const ERR_NEED_DICT: &[u8] = b"need dictionary\0";
/// Null-terminated error message for `Z_STREAM_END` (code 1, index 1).
const ERR_STREAM_END: &[u8] = b"stream end\0";
/// Null-terminated error message for `Z_OK` (code 0, index 2).
const ERR_OK: &[u8] = b"\0";
/// Null-terminated error message for `Z_ERRNO` (code -1, index 3).
const ERR_ERRNO: &[u8] = b"file error\0";
/// Null-terminated error message for `Z_STREAM_ERROR` (code -2, index 4).
const ERR_STREAM_ERROR: &[u8] = b"stream error\0";
/// Null-terminated error message for `Z_DATA_ERROR` (code -3, index 5).
const ERR_DATA_ERROR: &[u8] = b"data error\0";
/// Null-terminated error message for `Z_MEM_ERROR` (code -4, index 6).
const ERR_MEM_ERROR: &[u8] = b"insufficient memory\0";
/// Null-terminated error message for `Z_BUF_ERROR` (code -5, index 7).
const ERR_BUF_ERROR: &[u8] = b"buffer error\0";
/// Null-terminated error message for `Z_VERSION_ERROR` (code -6, index 8).
const ERR_VERSION_ERROR: &[u8] = b"incompatible version\0";
/// Null-terminated sentinel for out-of-range error codes (index 9).
const ERR_SENTINEL: &[u8] = b"\0";

/// Array of null-terminated error message byte strings, indexed by
/// `(2 - error_code)` for codes in `[-6, 2]`. Matches the C `z_errmsg[10]`.
const Z_ERRMSG_FFI: [&[u8]; 10] = [
    ERR_NEED_DICT,     // index 0 — Z_NEED_DICT       (2)
    ERR_STREAM_END,    // index 1 — Z_STREAM_END      (1)
    ERR_OK,            // index 2 — Z_OK              (0)
    ERR_ERRNO,         // index 3 — Z_ERRNO         (-1)
    ERR_STREAM_ERROR,  // index 4 — Z_STREAM_ERROR  (-2)
    ERR_DATA_ERROR,    // index 5 — Z_DATA_ERROR    (-3)
    ERR_MEM_ERROR,     // index 6 — Z_MEM_ERROR     (-4)
    ERR_BUF_ERROR,     // index 7 — Z_BUF_ERROR     (-5)
    ERR_VERSION_ERROR, // index 8 — Z_VERSION_ERROR (-6)
    ERR_SENTINEL,      // index 9 — sentinel (out of range)
];

// =============================================================================
// Version String
// =============================================================================

/// Null-terminated library version string for FFI use.
///
/// Matches `ZLIB_VERSION "1.3.2.1-motley"` from `zlib.h` line 44.
/// The safe core uses [`zlib_rs::util::zlib_version`] which returns a Rust
/// `&str`; the FFI layer needs a null-terminated byte sequence for C callers.
const ZLIB_VERSION_BYTES: &[u8] = b"1.3.2.1-motley\0";

// =============================================================================
// Internal Helper: Error Message Lookup
// =============================================================================

/// Convert an integer error code to a pointer to a null-terminated C string.
///
/// Equivalent to the C `ERR_MSG(err)` macro from `zutil.h` line 65:
/// ```c
/// #define ERR_MSG(err) z_errmsg[(err) < -6 || (err) > 2 ? 9 : 2 - (err)]
/// ```
///
/// Returns a pointer to a static null-terminated byte string from
/// [`Z_ERRMSG_FFI`]. Out-of-range codes yield the empty sentinel string.
#[inline]
fn err_msg_ptr(err: c_int) -> *const c_char {
    let index = if (-6..=2).contains(&err) {
        // `(2 - err)` is in [0, 8] since err is in [-6, 2].
        #[allow(clippy::cast_sign_loss)]
        {
            (2 - err) as usize
        }
    } else {
        9 // sentinel for out-of-range
    };
    Z_ERRMSG_FFI[index].as_ptr().cast::<c_char>()
}

// =============================================================================
// Internal Helpers: State Access
// =============================================================================

/// Extract a mutable reference to the [`InflateState`] stored in
/// `z_stream.state`.
///
/// # Safety
///
/// The caller must guarantee that:
/// - `strm` is a valid, non-null pointer to an initialized `z_stream`
/// - `strm.state` was previously set by `inflateInit_` / `inflateInit2_`
///   to point to a valid `InflateState` allocation
/// - No other mutable references to the state exist
#[inline]
unsafe fn get_inflate_state_mut<'a>(
    strm: *mut z_stream,
) -> Option<&'a mut zlib_rs::inflate::InflateState> {
    let state_ptr = unsafe { (*strm).state };
    if state_ptr.is_null() {
        return None;
    }
    Some(unsafe { &mut *(state_ptr.cast::<zlib_rs::inflate::InflateState>()) })
}

/// Extract an immutable reference to the [`InflateState`] stored in
/// `z_stream.state`.
///
/// # Safety
///
/// The caller must guarantee that:
/// - `strm` is a valid, non-null pointer to an initialized `z_stream`
/// - `strm.state` was previously set by an init function
#[inline]
unsafe fn get_inflate_state_ref<'a>(
    strm: *mut z_stream,
) -> Option<&'a zlib_rs::inflate::InflateState> {
    let state_ptr = unsafe { (*strm).state };
    if state_ptr.is_null() {
        return None;
    }
    Some(unsafe { &*(state_ptr as *const zlib_rs::inflate::InflateState) })
}

/// Get a mutable reference to the Rust [`ZStream`] stored in `z_stream.state`
/// for deflate operations. The deflate engine stores a full `ZStream` behind
/// the state pointer.
///
/// # Safety
///
/// `strm` must be a valid, non-null pointer to a `z_stream` whose `state`
/// field was previously set by `deflateInit_` / `deflateInit2_`.
#[inline]
unsafe fn get_deflate_stream_mut<'a>(
    strm: *mut z_stream,
) -> Option<&'a mut zlib_rs::stream::ZStream> {
    let state_ptr = unsafe { (*strm).state };
    if state_ptr.is_null() {
        return None;
    }
    Some(unsafe { &mut *(state_ptr.cast::<zlib_rs::stream::ZStream>()) })
}

/// Build a temporary [`ZStream`] from the scalar fields of a C
/// [`z_stream`], for use in inflate operations that require a stream
/// reference (e.g., `inflate_reset_keep`).
///
/// # Safety
///
/// `strm` must be a valid, non-null pointer.
#[inline]
#[allow(clippy::cast_lossless)]
unsafe fn build_rust_stream(strm: *const z_stream) -> zlib_rs::stream::ZStream {
    let strm_ref = unsafe { &*strm };

    let mut rs = zlib_rs::stream::ZStream::new();
    rs.total_in = strm_ref.total_in;
    rs.total_out = strm_ref.total_out;
    #[allow(clippy::cast_possible_truncation)]
    #[allow(clippy::cast_lossless)]
    {
        rs.adler = strm_ref.adler as u32;
    }
    rs.data_type = strm_ref.data_type;
    rs
}

/// Synchronize scalar stream fields from a Rust [`ZStream`] back to
/// the C [`z_stream`] struct after a core function call. Used for
/// inflate operations via temporary stream.
///
/// # Safety
///
/// `strm` must be a valid, non-null pointer.
#[inline]
#[allow(clippy::cast_lossless)]
unsafe fn sync_inflate_to_c(strm: *mut z_stream, rust_stream: &zlib_rs::stream::ZStream) {
    let s = unsafe { &mut *strm };
    s.total_in = rust_stream.total_in as c_ulong;
    s.total_out = rust_stream.total_out as c_ulong;
    s.adler = rust_stream.adler as c_ulong;
    s.data_type = rust_stream.data_type;
    match rust_stream.msg {
        Some(msg) => {
            s.msg = msg.as_ptr().cast::<c_char>();
        }
        None => {
            s.msg = std::ptr::null();
        }
    }
}

/// Synchronize scalar stream fields from a Rust [`ZStream`] back to
/// the C [`z_stream`] struct after a deflate core function call.
///
/// # Safety
///
/// `strm` must be a valid, non-null pointer.
#[inline]
#[allow(clippy::cast_lossless)]
unsafe fn sync_deflate_to_c(strm: *mut z_stream, rust_stream: &zlib_rs::stream::ZStream) {
    let s = unsafe { &mut *strm };
    s.total_in = rust_stream.total_in as c_ulong;
    s.total_out = rust_stream.total_out as c_ulong;
    s.adler = rust_stream.adler as c_ulong;
    s.data_type = rust_stream.data_type;
    match rust_stream.msg {
        Some(msg) => {
            s.msg = msg.as_ptr().cast::<c_char>();
        }
        None => {
            s.msg = std::ptr::null();
        }
    }
}

// =============================================================================
// Phase 1: Version Functions (win32/zlib.def lines 4, 33)
// =============================================================================

/// Return the zlib library version string.
///
/// Returns a pointer to a static null-terminated C string containing the
/// zlib version identifier `"1.3.2.1-motley"` (matching `ZLIB_VERSION` from
/// `zlib.h` line 44).
///
/// The safe Rust core function [`zlib_rs::zlib_version`] returns a
/// `&'static str`; this FFI wrapper returns the equivalent null-terminated
/// byte string as `*const c_char` for C callers.
///
/// C signature (`zlib.h` line 224):
/// ```c
/// const char * zlibVersion(void);
/// ```
///
/// This function is always safe — it returns a pointer to static data and
/// takes no arguments.
#[unsafe(no_mangle)]
pub extern "C" fn zlibVersion() -> *const c_char {
    // Verify consistency with the safe Rust core at debug time.
    debug_assert!(
        zlib_rs::zlib_version().starts_with("1.3.2"),
        "version mismatch between FFI and core"
    );
    ZLIB_VERSION_BYTES.as_ptr().cast::<c_char>()
}

/// Return compile-time configuration flags as a bit field.
///
/// Delegates to [`zlib_rs::zlib_compile_flags`] which encodes type sizes
/// (bits 0–7) and feature flags (bits 8–31) in the returned value. See the
/// core function documentation for the complete bit layout.
///
/// C signature (`zlib.h` lines 1216–1256):
/// ```c
/// uLong zlibCompileFlags(void);
/// ```
///
/// This function is always safe — it computes and returns a scalar value.
#[unsafe(no_mangle)]
#[allow(clippy::cast_possible_truncation)]
pub extern "C" fn zlibCompileFlags() -> c_ulong {
    // The Rust core returns u64; c_ulong is u32 on 32-bit and u64 on LP64.
    // The value always fits in 32 bits (only bits 0–27 are used), so the
    // truncation on 32-bit systems is safe and correct.
    zlib_rs::zlib_compile_flags() as c_ulong
}

// =============================================================================
// Phase 2: Error and Info Functions (win32/zlib.def lines 96, 98)
// =============================================================================

/// Convert a zlib error code to a human-readable error message string.
///
/// Returns a pointer to a static null-terminated C string from the
/// error message table. The error codes range from `Z_VERSION_ERROR` (-6)
/// to `Z_NEED_DICT` (2). Out-of-range codes yield an empty string.
///
/// This is the FFI equivalent of the safe Rust
/// `zlib_rs::err_msg` function, returning a C-compatible
/// null-terminated string instead of a Rust `&str`.
///
/// C signature (`zlib.h` line 2033):
/// ```c
/// const char * zError(int err);
/// ```
///
/// # C Equivalent
///
/// From `zutil.c` line 139:
/// ```c
/// const char * ZEXPORT zError(int err) {
///     return ERR_MSG(err);
/// }
/// ```
#[unsafe(no_mangle)]
pub extern "C" fn zError(err: c_int) -> *const c_char {
    err_msg_ptr(err)
}

/// Return a pointer to the CRC-32 lookup table.
///
/// Delegates to [`zlib_rs::checksum::crc32::get_crc_table`] and returns a raw
/// pointer to the first element of the 256-entry `u32` CRC table. The table
/// is a compile-time constant, so the returned pointer is always valid.
///
/// C signature (`zlib.h` line 2035):
/// ```c
/// const z_crc_t FAR * get_crc_table(void);
/// ```
///
/// This function is always safe — it returns a pointer to static const data.
#[unsafe(no_mangle)]
pub extern "C" fn get_crc_table() -> *const u32 {
    let table: &'static [u32; 256] = zlib_rs::checksum::crc32::get_crc_table();
    table.as_ptr()
}

// =============================================================================
// Phase 3: Undocumented Inflate Functions (win32/zlib.def lines 97, 99–102)
// =============================================================================

/// Check whether the inflate decompressor is at a sync point.
///
/// Returns non-zero if inflate is currently at a sync point (a stored block
/// boundary with no pending bits), zero otherwise. Returns
/// [`Z_STREAM_ERROR`] if the stream pointer is null or the state is invalid.
///
/// Delegates to [`zlib_rs::inflate::inflate_sync_point`].
///
/// C signature (`zlib.h` line 2034):
/// ```c
/// int inflateSyncPoint(z_streamp strm);
/// ```
///
/// # Safety
///
/// `strm` must be a valid, non-null pointer to an initialized inflate
/// `z_stream`, or null (in which case `Z_STREAM_ERROR` is returned).
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub unsafe extern "C" fn inflateSyncPoint(strm: *mut z_stream) -> c_int {
    if strm.is_null() {
        return Z_STREAM_ERROR;
    }

    let Some(state) = (unsafe { get_inflate_state_ref(strm) }) else {
        return Z_STREAM_ERROR;
    };

    // inflate_sync_point returns bool; C API returns int (non-zero = true).
    c_int::from(zlib_rs::inflate::inflate_sync_point(state))
}

/// Toggle inflate distance-check undermining for testing.
///
/// When `subvert` is non-zero, the inflate engine disables distance-too-far
/// error checking, allowing inspection of corrupted deflate streams. When
/// `subvert` is zero, normal checking is restored.
///
/// Delegates to [`zlib_rs::inflate::inflate_undermine`].
///
/// C signature (`zlib.h` line 2036):
/// ```c
/// int inflateUndermine(z_streamp strm, int subvert);
/// ```
///
/// # Safety
///
/// `strm` must be a valid, non-null pointer to an initialized inflate
/// `z_stream`, or null (returns `Z_STREAM_ERROR`).
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub unsafe extern "C" fn inflateUndermine(strm: *mut z_stream, subvert: c_int) -> c_int {
    if strm.is_null() {
        return Z_STREAM_ERROR;
    }

    let Some(state) = (unsafe { get_inflate_state_mut(strm) }) else {
        return Z_STREAM_ERROR;
    };

    let ret = zlib_rs::inflate::inflate_undermine(state, subvert != 0);
    ret as c_int
}

/// Toggle inflate check-value validation.
///
/// When `check` is non-zero, the inflate engine validates the Adler-32 or
/// CRC-32 check value at the end of the stream (the default behavior). When
/// `check` is zero, check-value validation is disabled.
///
/// Delegates to [`zlib_rs::inflate::inflate_validate`].
///
/// C signature (`zlib.h` line 2037):
/// ```c
/// int inflateValidate(z_streamp strm, int check);
/// ```
///
/// # Safety
///
/// `strm` must be a valid, non-null pointer to an initialized inflate
/// `z_stream`, or null (returns `Z_STREAM_ERROR`).
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub unsafe extern "C" fn inflateValidate(strm: *mut z_stream, check: c_int) -> c_int {
    if strm.is_null() {
        return Z_STREAM_ERROR;
    }

    let Some(state) = (unsafe { get_inflate_state_mut(strm) }) else {
        return Z_STREAM_ERROR;
    };

    let ret = zlib_rs::inflate::inflate_validate(state, check != 0);
    ret as c_int
}

/// Return the number of code/length table entries currently in use by the
/// inflate state.
///
/// Delegates to [`zlib_rs::inflate::inflate_codes_used`]. Returns `0` on error
/// (null pointer). The C return type is `unsigned long` to match the C API.
///
/// C signature (`zlib.h` line 2038):
/// ```c
/// unsigned long inflateCodesUsed(z_streamp strm);
/// ```
///
/// # Safety
///
/// `strm` must be a valid, non-null pointer to an initialized inflate
/// `z_stream`, or null (returns `Z_STREAM_ERROR` cast to `unsigned long`).
#[unsafe(no_mangle)]
#[allow(non_snake_case, clippy::cast_possible_wrap)]
pub unsafe extern "C" fn inflateCodesUsed(strm: *mut z_stream) -> c_ulong {
    if strm.is_null() {
        return Z_STREAM_ERROR as c_ulong;
    }

    let Some(state) = (unsafe { get_inflate_state_ref(strm) }) else {
        return Z_STREAM_ERROR as c_ulong;
    };

    zlib_rs::inflate::inflate_codes_used(state) as c_ulong
}

// =============================================================================
// Phase 4: Undocumented Reset-Keep Functions (win32/zlib.def lines 102–103)
// =============================================================================

/// Reset the inflate state while preserving dictionary and header info.
///
/// Like [`inflateReset`](super::inflate::inflateReset) but retains the
/// preset dictionary data and any gzip header information. This is used
/// for efficient reuse of an inflate stream for multiple decompression
/// operations with the same dictionary.
///
/// Delegates to [`zlib_rs::inflate::inflate_reset_keep`].
///
/// C signature (`zlib.h` line 2039):
/// ```c
/// int inflateResetKeep(z_streamp strm);
/// ```
///
/// # Safety
///
/// `strm` must be a valid, non-null pointer to an initialized inflate
/// `z_stream`, or null (returns `Z_STREAM_ERROR`).
#[unsafe(no_mangle)]
#[allow(non_snake_case, clippy::cast_lossless)]
pub unsafe extern "C" fn inflateResetKeep(strm: *mut z_stream) -> c_int {
    if strm.is_null() {
        return Z_STREAM_ERROR;
    }

    let Some(state) = (unsafe { get_inflate_state_mut(strm) }) else {
        return Z_STREAM_ERROR;
    };

    // Build a temporary Rust ZStream from the C z_stream scalar fields.
    // The inflate core function needs both state and stream references.
    let mut rust_stream = unsafe { build_rust_stream(strm) };

    let ret = zlib_rs::inflate::inflate_reset_keep(state, &mut rust_stream);

    // Sync the reset counters (total_in, total_out, adler, msg) back to C.
    unsafe { sync_inflate_to_c(strm, &rust_stream) };

    ret as c_int
}

/// Reset the deflate state while preserving dictionary data.
///
/// Like [`deflateReset`](super::deflate::deflateReset) but retains the
/// preset dictionary. This is used for efficient reuse of a deflate stream
/// for multiple compression operations with the same dictionary.
///
/// Delegates to [`zlib_rs::deflate::deflate_reset_keep`].
///
/// C signature (`zlib.h` line 2040):
/// ```c
/// int deflateResetKeep(z_streamp strm);
/// ```
///
/// # Safety
///
/// `strm` must be a valid, non-null pointer to an initialized deflate
/// `z_stream`, or null (returns `Z_STREAM_ERROR`).
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub unsafe extern "C" fn deflateResetKeep(strm: *mut z_stream) -> c_int {
    if strm.is_null() {
        return Z_STREAM_ERROR;
    }

    let Some(rs) = (unsafe { get_deflate_stream_mut(strm) }) else {
        return Z_STREAM_ERROR;
    };

    let ret = zlib_rs::deflate::deflate_reset_keep(rs);

    // Sync the reset scalar fields back to the C z_stream struct.
    unsafe { sync_deflate_to_c(strm, rs) };

    ret as c_int
}

// ===========================================================================
// Additional utility symbols — extending the 96-symbol canonical surface
// ===========================================================================
//
// These symbols supplement the canonical 96 from `win32/zlib.def` with
// additional informational and diagnostic functions commonly expected by
// zlib-compatible libraries and language bindings.

/// Returns the version number as a packed integer.
///
/// This is the function equivalent of the `ZLIB_VERNUM` macro from
/// `zlib.h` line 45. Returns `0x1321` for version 1.3.2.1.
///
/// Useful for version comparisons in language bindings that cannot
/// parse version strings but can compare integers.
#[unsafe(no_mangle)]
pub extern "C" fn zlibVerNum() -> c_ulong {
    crate::ZLIB_VERNUM as c_ulong
}

/// Returns the major version number.
///
/// Equivalent to the `ZLIB_VER_MAJOR` macro from `zlib.h` line 46.
#[unsafe(no_mangle)]
pub extern "C" fn zlibVerMajor() -> c_int {
    crate::ZLIB_VER_MAJOR as c_int
}

/// Returns the minor version number.
///
/// Equivalent to the `ZLIB_VER_MINOR` macro from `zlib.h` line 47.
#[unsafe(no_mangle)]
pub extern "C" fn zlibVerMinor() -> c_int {
    crate::ZLIB_VER_MINOR as c_int
}

/// Returns the revision number.
///
/// Equivalent to the `ZLIB_VER_REVISION` macro from `zlib.h` line 48.
#[unsafe(no_mangle)]
pub extern "C" fn zlibVerRevision() -> c_int {
    crate::ZLIB_VER_REVISION as c_int
}
