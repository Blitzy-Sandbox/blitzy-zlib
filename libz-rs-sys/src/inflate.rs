//! C-compatible FFI wrappers for zlib inflate (decompression) functions.
//!
//! This module provides `#[unsafe(no_mangle)] extern "C"` wrapper functions that
//! bridge between the C zlib inflate API (using raw pointers, `c_int` return
//! codes, and the `#[repr(C)]` [`z_stream`] struct) and the safe Rust
//! implementations in [`zlib_rs::inflate`].
//!
//! # Exported Symbols
//!
//! All 16 inflate symbols from `win32/zlib.def`:
//!
//! ## Basic Functions (def lines 7–8)
//! - [`inflate()`](fn@inflate) — Main decompression function
//! - [`inflateEnd`] — Free decompression state
//!
//! ## Advanced Functions (def lines 22–32)
//! - [`inflateSetDictionary`] — Set preset decompression dictionary
//! - [`inflateGetDictionary`] — Retrieve sliding dictionary
//! - [`inflateSync`] — Skip to next flush point
//! - [`inflateCopy`] — Deep-copy decompression stream
//! - [`inflateReset`] — Reset decompression state
//! - [`inflateReset2`] — Reset with new windowBits
//! - [`inflatePrime`] — Insert bits into inflate input
//! - [`inflateMark`] — Return decompression progress info
//! - [`inflateGetHeader`] — Request gzip header extraction
//! - [`inflateBack`] — Callback-based raw DEFLATE decompression
//! - [`inflateBackEnd`] — Free callback decompression state
//!
//! ## Init Functions (def lines 92–94)
//! - [`inflateInit_`] — Initialize for default (zlib) decompression
//! - [`inflateInit2_`] — Initialize with explicit windowBits
//! - [`inflateBackInit_`] — Initialize for callback decompression
//!
//! # Safety Contract
//!
//! Every function that receives a raw pointer validates it for null before
//! dereferencing. Null stream pointers cause an immediate return of
//! [`Z_STREAM_ERROR`]. Dictionary and header pointers may be null where the
//! C API permits it (e.g., `inflateGetDictionary` with null output buffer).
//!
//! All functions use `extern "C"` (not `extern "C-unwind"`) to abort on
//! panic at the FFI boundary rather than triggering undefined behavior
//! through stack unwinding into C code (per AAP Section 0.7.3).

use libc::{c_char, c_int, c_long, c_uchar, c_uint, c_void};

use crate::types::{Z_STREAM_ERROR, gz_header, in_func, out_func, z_stream};

// ─── Internal helpers ────────────────────────────────────────────────────────

/// The zlib version string for compatibility checking.
///
/// Init functions compare the caller-provided version string against this
/// value. A mismatch indicates an ABI incompatibility between the caller
/// and the library.
const ZLIB_VERSION_BYTES: &[u8] = b"1.3.2.1-motley\0";

/// Validate that the version string's first character matches and that
/// `stream_size` matches the expected size of [`z_stream`].
///
/// Returns `true` if version and size are compatible, `false` otherwise.
/// This mirrors the C `inflateInit2_()` compatibility check in
/// `inflate.c` lines 186–193.
///
/// # Safety
///
/// `version` must be a valid pointer to a null-terminated C string, or
/// null (which is treated as a version mismatch).
#[inline]
unsafe fn version_check(version: *const c_char, stream_size: c_int) -> bool {
    if version.is_null() {
        return false;
    }
    // Check that the first byte of the version string matches.
    // This is the same check C zlib performs — it only compares the
    // major version character to catch gross ABI mismatches.
    let expected_first = ZLIB_VERSION_BYTES[0];
    #[allow(clippy::cast_sign_loss)]
    let actual_first = unsafe { *version } as u8;
    if actual_first != expected_first {
        return false;
    }
    // Verify struct size matches to catch 32-bit vs 64-bit ABI issues.
    #[allow(clippy::cast_possible_wrap)]
    let expected_size = std::mem::size_of::<z_stream>() as c_int;
    stream_size == expected_size
}

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
unsafe fn get_state_mut<'a>(strm: *mut z_stream) -> Option<&'a mut zlib_rs::inflate::InflateState> {
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
unsafe fn get_state_ref<'a>(strm: *mut z_stream) -> Option<&'a zlib_rs::inflate::InflateState> {
    let state_ptr = unsafe { (*strm).state };
    if state_ptr.is_null() {
        return None;
    }
    Some(unsafe { &*(state_ptr as *const zlib_rs::inflate::InflateState) })
}

/// Synchronize scalar stream fields from the Rust [`ZStream`] back to
/// the C [`z_stream`] struct after a core function call.
///
/// Updates `total_in`, `total_out`, `adler`, `data_type`, and `msg`.
///
/// # Safety
///
/// `strm` must be a valid, non-null pointer.
#[inline]
#[allow(clippy::cast_lossless)]
unsafe fn sync_scalars_to_c(strm: *mut z_stream, rust_stream: &zlib_rs::stream::ZStream) {
    let strm_ref = unsafe { &mut *strm };
    strm_ref.total_in = rust_stream.total_in as libc::c_ulong;
    strm_ref.total_out = rust_stream.total_out as libc::c_ulong;
    strm_ref.adler = rust_stream.adler as libc::c_ulong;
    strm_ref.data_type = rust_stream.data_type;
    match rust_stream.msg {
        Some(msg) => {
            strm_ref.msg = msg.as_ptr().cast::<c_char>();
        }
        None => {
            strm_ref.msg = std::ptr::null();
        }
    }
}

/// Build a temporary [`ZStream`] from the scalar fields of a C
/// [`z_stream`], and feed it the input data from the C buffers.
///
/// # Safety
///
/// `strm` must be a valid, non-null pointer with valid `next_in`/`avail_in`.
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

    // Copy input from C buffer into the Rust stream.
    let avail_in = strm_ref.avail_in as usize;
    if !strm_ref.next_in.is_null() && avail_in > 0 {
        let input_slice = unsafe { std::slice::from_raw_parts(strm_ref.next_in, avail_in) };
        rs.set_input(input_slice);
    }

    // Allocate output buffer matching C avail_out.
    let avail_out = strm_ref.avail_out as usize;
    if avail_out > 0 {
        rs.set_output_buffer(avail_out);
    }

    rs
}

/// After a core operation, advance the C `z_stream` buffer pointers by the
/// consumed/produced amounts and copy produced output back to the C buffer.
///
/// # Safety
///
/// `strm` must be a valid pointer. `next_out` at the time of the call
/// must still point to a writable buffer of at least `output_produced` bytes.
#[inline]
#[allow(clippy::cast_possible_truncation)]
unsafe fn finalize_buffers(
    strm: *mut z_stream,
    rust_stream: &zlib_rs::stream::ZStream,
    input_consumed: usize,
    output_produced: usize,
) {
    let strm_ref = unsafe { &mut *strm };

    // Copy output data back to the C buffer.
    if output_produced > 0 && !strm_ref.next_out.is_null() {
        let written = rust_stream.output_written();
        let copy_len = output_produced.min(written.len());
        if copy_len > 0 {
            unsafe {
                std::ptr::copy_nonoverlapping(written.as_ptr(), strm_ref.next_out, copy_len);
            }
        }
    }

    // Advance pointers.
    if !strm_ref.next_in.is_null() {
        strm_ref.next_in = unsafe { strm_ref.next_in.add(input_consumed) };
    }
    strm_ref.avail_in -= input_consumed as c_uint;

    if !strm_ref.next_out.is_null() {
        strm_ref.next_out = unsafe { strm_ref.next_out.add(output_produced) };
    }
    strm_ref.avail_out -= output_produced as c_uint;
}

// =============================================================================
// Phase 1: Init Functions
// =============================================================================

/// Initialize an inflate stream for default (zlib format) decompression.
///
/// This is the actual function behind the `inflateInit` C macro. It
/// initializes the stream with default `windowBits = 15` (zlib format,
/// 32 KiB window).
///
/// C signature (`zlib.h` lines 1905–1906):
/// ```c
/// int inflateInit_(z_streamp strm, const char *version, int stream_size);
/// ```
///
/// # Parameters
///
/// - `strm` — Pointer to the `z_stream` to initialize. Must not be null.
///   The `zalloc`, `zfree`, and `opaque` fields may be set for custom
///   allocation (currently ignored — Rust allocator is always used).
/// - `version` — Pointer to a null-terminated version string (e.g.,
///   `ZLIB_VERSION`). The first character is compared for compatibility.
/// - `stream_size` — `sizeof(z_stream)` as seen by the caller, for ABI
///   compatibility verification.
///
/// # Returns
///
/// - `Z_OK` on success
/// - `Z_STREAM_ERROR` if `strm` is null or parameters are invalid
/// - `Z_VERSION_ERROR` if `version` or `stream_size` is incompatible
/// - `Z_MEM_ERROR` if memory allocation failed
///
/// # Safety
///
/// `strm` must be a valid pointer to a `z_stream` or null. If `version`
/// is non-null, it must point to a valid null-terminated C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn inflateInit_(
    strm: *mut z_stream,
    version: *const c_char,
    stream_size: c_int,
) -> c_int {
    // Delegate to inflateInit2_ with default windowBits = 15 (DEF_WBITS).
    unsafe { inflateInit2_(strm, 15, version, stream_size) }
}

/// Initialize an inflate stream with explicit `windowBits`.
///
/// This is the actual function behind the `inflateInit2` C macro. The
/// `windowBits` parameter controls both the window size and the expected
/// stream container format.
///
/// C signature (`zlib.h` lines 1911–1912):
/// ```c
/// int inflateInit2_(z_streamp strm, int windowBits, const char *version,
///                   int stream_size);
/// ```
///
/// # `windowBits` Encoding
///
/// | Range         | Format                        | Window size      |
/// |---------------|-------------------------------|------------------|
/// | `8..=15`      | zlib (RFC 1950)               | `2^windowBits`   |
/// | `-8..=-15`    | raw DEFLATE (no wrapper)      | `2^(-windowBits)`|
/// | `24..=31`     | gzip only (RFC 1952)          | `2^(wB-16)`      |
/// | `40..=47`     | auto-detect (zlib or gzip)    | `2^(wB-32)`      |
/// | `0`           | use size from zlib header      |                  |
///
/// # Safety
///
/// `strm` must be a valid pointer to a `z_stream` or null.
#[unsafe(no_mangle)]
#[allow(non_snake_case, clippy::cast_lossless)]
pub unsafe extern "C" fn inflateInit2_(
    strm: *mut z_stream,
    windowBits: c_int,
    version: *const c_char,
    stream_size: c_int,
) -> c_int {
    // Validate stream pointer.
    if strm.is_null() {
        return Z_STREAM_ERROR;
    }

    // Validate version and struct size.
    if !(unsafe { version_check(version, stream_size) }) {
        return zlib_rs::error::ReturnCode::VersionError as c_int;
    }

    // Create the InflateState directly. InflateState::new() defaults to
    // mode = Head, which is exactly what inflate_reset2 requires. This
    // replicates the initialization sequence from inflate.c inflateInit2_()
    // without needing to access the pub(crate) ZStream.state field.
    let mut state = zlib_rs::inflate::InflateState::new();

    // Create a temporary Rust ZStream for the reset call.
    let mut rust_stream = zlib_rs::stream::ZStream::new();

    // inflate_reset2 initializes the state fields (wrap, wbits, etc.)
    // and resets the stream counters.
    let ret = zlib_rs::inflate::inflate_reset2(&mut state, &mut rust_stream, windowBits);
    if ret != zlib_rs::error::ReturnCode::Ok {
        return ret as c_int;
    }

    // Store the state as a raw pointer in the C z_stream.
    let strm_ref = unsafe { &mut *strm };
    strm_ref.state = Box::into_raw(Box::new(state)).cast::<c_void>();

    // Initialize the C stream scalar fields from the Rust stream.
    strm_ref.total_in = 0;
    strm_ref.total_out = 0;
    strm_ref.adler = rust_stream.adler as libc::c_ulong;
    strm_ref.data_type = rust_stream.data_type;
    strm_ref.msg = std::ptr::null();

    ret as c_int
}

// =============================================================================
// Phase 2: Core Inflate Function
// =============================================================================

/// Decompress data from the input buffer to the output buffer.
///
/// This is the main decompression function. It processes as much input as
/// possible and produces as much output as possible, advancing the stream's
/// buffer pointers accordingly.
///
/// C signature (`zlib.h` line 405):
/// ```c
/// int inflate(z_streamp strm, int flush);
/// ```
///
/// # Performance
///
/// This is the most performance-critical function in the inflate API. The
/// FFI wrapper is kept as thin as possible — it bridges buffer pointers
/// and delegates directly to the core Rust engine.
///
/// # Safety
///
/// `strm` must be a valid pointer to an initialized `z_stream` (i.e.,
/// `inflateInit_` or `inflateInit2_` must have been called). The
/// `next_in`/`avail_in` and `next_out`/`avail_out` fields must describe
/// valid memory regions.
#[unsafe(no_mangle)]
#[allow(clippy::cast_possible_truncation, clippy::cast_lossless)]
pub unsafe extern "C" fn inflate(strm: *mut z_stream, flush: c_int) -> c_int {
    // Validate stream pointer.
    if strm.is_null() {
        return Z_STREAM_ERROR;
    }

    // Extract the inflate state.
    let Some(state) = (unsafe { get_state_mut(strm) }) else {
        return Z_STREAM_ERROR;
    };

    // Build a temporary Rust stream from the C fields.
    let mut rust_stream = unsafe { build_rust_stream(strm) };

    // Record starting positions to compute consumed/produced.
    let input_before = rust_stream.avail_in();
    let output_before = rust_stream.avail_out();

    // Call the core inflate engine.
    let ret = zlib_rs::inflate::inflate(state, &mut rust_stream, flush);

    // Compute how much was consumed and produced.
    let input_consumed = input_before - rust_stream.avail_in();
    let output_produced = output_before - rust_stream.avail_out();

    // Copy output back and advance C buffer pointers.
    unsafe {
        finalize_buffers(strm, &rust_stream, input_consumed, output_produced);
    }

    // Synchronize scalar fields (total_in, total_out, adler, etc.).
    unsafe {
        sync_scalars_to_c(strm, &rust_stream);
    }

    ret as c_int
}

/// Free all dynamically allocated data structures for the inflate stream.
///
/// Discards any unprocessed input and does not flush pending output. After
/// this call, the stream cannot be used for decompression unless
/// re-initialized.
///
/// C signature (`zlib.h` line 525):
/// ```c
/// int inflateEnd(z_streamp strm);
/// ```
///
/// # Safety
///
/// `strm` must be a valid pointer to an initialized `z_stream`.
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub unsafe extern "C" fn inflateEnd(strm: *mut z_stream) -> c_int {
    if strm.is_null() {
        return Z_STREAM_ERROR;
    }

    let strm_ref = unsafe { &mut *strm };
    if strm_ref.state.is_null() {
        return Z_STREAM_ERROR;
    }

    // Reconstruct the Box<InflateState> to properly drop it.
    // The Box reclaims ownership of the heap allocation, and when dropped,
    // all owned buffers (window, codes, etc.) are freed automatically.
    let state_ptr = strm_ref.state.cast::<zlib_rs::inflate::InflateState>();
    let mut state = unsafe { Box::from_raw(state_ptr) };

    // Build a temporary Rust stream for the end call.
    let mut rust_stream = zlib_rs::stream::ZStream::new();
    let ret = zlib_rs::inflate::inflate_end(&mut state, &mut rust_stream);

    // Clear the state pointer — the Box is dropped when `state` goes
    // out of scope, deallocating the InflateState.
    strm_ref.state = std::ptr::null_mut();
    strm_ref.msg = std::ptr::null();

    // Explicitly drop the state to finalize deallocation.
    drop(state);

    ret as c_int
}

// =============================================================================
// Phase 3: Dictionary Functions
// =============================================================================

/// Set the decompression dictionary from the given byte sequence.
///
/// Must be called immediately after [`inflate()`](fn@inflate) returns `Z_NEED_DICT`.
/// The provided dictionary must match the one used during compression
/// (verified by Adler-32 checksum comparison).
///
/// C signature (`zlib.h` lines 913–915):
/// ```c
/// int inflateSetDictionary(z_streamp strm, const Bytef *dictionary,
///                          uInt dictLength);
/// ```
///
/// # Safety
///
/// `strm` must be valid and initialized. If `dictionary` is non-null, it
/// must point to at least `dictLength` readable bytes.
#[unsafe(no_mangle)]
#[allow(non_snake_case, clippy::cast_lossless)]
pub unsafe extern "C" fn inflateSetDictionary(
    strm: *mut z_stream,
    dictionary: *const u8,
    dictLength: c_uint,
) -> c_int {
    if strm.is_null() {
        return Z_STREAM_ERROR;
    }

    let Some(state) = (unsafe { get_state_mut(strm) }) else {
        return Z_STREAM_ERROR;
    };

    // Construct a slice from the dictionary pointer.
    // Null dictionary with zero length is a valid no-op per C zlib.
    // Null dictionary with non-zero length is an error.
    if dictionary.is_null() && dictLength > 0 {
        return Z_STREAM_ERROR;
    }

    let dict_slice = if dictionary.is_null() || dictLength == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(dictionary, dictLength as usize) }
    };

    // Build a temporary Rust stream for scalar field synchronization.
    let mut rust_stream = unsafe { build_rust_stream(strm) };

    let ret = zlib_rs::inflate::inflate_set_dictionary(state, &mut rust_stream, dict_slice);

    // Sync back adler and msg which may have been updated.
    unsafe { sync_scalars_to_c(strm, &rust_stream) };

    ret as c_int
}

/// Retrieve the sliding dictionary being maintained by inflate.
///
/// If `dictionary` is not null, copies up to 32768 bytes of the dictionary
/// into it. If `dictLength` is not null, writes the actual dictionary
/// length there.
///
/// C signature (`zlib.h` lines 936–938):
/// ```c
/// int inflateGetDictionary(z_streamp strm, Bytef *dictionary,
///                          uInt *dictLength);
/// ```
///
/// # Safety
///
/// `strm` must be valid. `dictionary` (if non-null) must point to at
/// least 32768 writable bytes. `dictLength` (if non-null) must point to
/// a writable `c_uint`.
#[unsafe(no_mangle)]
#[allow(non_snake_case, clippy::cast_possible_truncation)]
pub unsafe extern "C" fn inflateGetDictionary(
    strm: *mut z_stream,
    dictionary: *mut u8,
    dictLength: *mut c_uint,
) -> c_int {
    if strm.is_null() {
        return Z_STREAM_ERROR;
    }

    let Some(state) = (unsafe { get_state_ref(strm) }) else {
        return Z_STREAM_ERROR;
    };

    if dictionary.is_null() {
        // Caller only wants the length — pass an empty buffer.
        let (ret, len) = zlib_rs::inflate::inflate_get_dictionary(state, &mut []);
        if !dictLength.is_null() {
            unsafe { *dictLength = len as c_uint };
        }
        return ret as c_int;
    }

    // Provide a sufficiently large scratch buffer (32 KiB max window).
    let mut buf = vec![0u8; 32_768];
    let (ret, len) = zlib_rs::inflate::inflate_get_dictionary(state, &mut buf);

    // Copy the dictionary bytes to the caller's buffer.
    if len > 0 {
        unsafe {
            std::ptr::copy_nonoverlapping(buf.as_ptr(), dictionary, len);
        }
    }
    if !dictLength.is_null() {
        unsafe { *dictLength = len as c_uint };
    }

    ret as c_int
}

// =============================================================================
// Phase 4: Stream Control Functions
// =============================================================================

/// Skip invalid compressed data until a flush point is found.
///
/// Searches for the sync marker pattern `00 00 FF FF` in the compressed
/// data. After finding it, the inflate state is reset for decompression
/// from that point.
///
/// C signature (`zlib.h` line 951):
/// ```c
/// int inflateSync(z_streamp strm);
/// ```
///
/// # Safety
///
/// `strm` must be valid and initialized.
#[unsafe(no_mangle)]
#[allow(non_snake_case, clippy::cast_lossless)]
pub unsafe extern "C" fn inflateSync(strm: *mut z_stream) -> c_int {
    if strm.is_null() {
        return Z_STREAM_ERROR;
    }

    let Some(state) = (unsafe { get_state_mut(strm) }) else {
        return Z_STREAM_ERROR;
    };

    let mut rust_stream = unsafe { build_rust_stream(strm) };
    let input_before = rust_stream.avail_in();

    let ret = zlib_rs::inflate::inflate_sync(state, &mut rust_stream);

    let input_consumed = input_before - rust_stream.avail_in();

    // inflateSync only consumes input (no output produced).
    unsafe {
        finalize_buffers(strm, &rust_stream, input_consumed, 0);
        sync_scalars_to_c(strm, &rust_stream);
    }

    ret as c_int
}

/// Deep-copy the inflate stream from source to dest.
///
/// Creates a complete independent copy of the decompression state,
/// allowing decompression to continue independently from the same point
/// in both streams.
///
/// C signature (`zlib.h` lines 970–971):
/// ```c
/// int inflateCopy(z_streamp dest, z_streamp source);
/// ```
///
/// # Safety
///
/// Both `dest` and `source` must be valid pointers. `source` must be an
/// initialized inflate stream.
#[unsafe(no_mangle)]
#[allow(non_snake_case, clippy::cast_lossless)]
pub unsafe extern "C" fn inflateCopy(dest: *mut z_stream, source: *mut z_stream) -> c_int {
    if dest.is_null() || source.is_null() {
        return Z_STREAM_ERROR;
    }

    let Some(src_state) = (unsafe { get_state_ref(source) }) else {
        return Z_STREAM_ERROR;
    };

    // Deep-copy the inflate state.
    let Some(new_state) = zlib_rs::inflate::inflate_copy(src_state) else {
        return zlib_rs::error::ReturnCode::MemError as c_int;
    };

    // Copy the C z_stream scalar fields from source to dest.
    unsafe {
        std::ptr::copy_nonoverlapping(source, dest, 1);
    }

    // Store the new (independent) state in the destination stream.
    let dest_ref = unsafe { &mut *dest };
    dest_ref.state = Box::into_raw(Box::new(new_state)).cast::<c_void>();

    zlib_rs::error::ReturnCode::Ok as c_int
}

/// Reset the inflate state, equivalent to `inflateEnd` + `inflateInit`.
///
/// The internal decompression state is reset without freeing and
/// reallocating it. The stream keeps attributes set by `inflateInit2`.
///
/// C signature (`zlib.h` line 986):
/// ```c
/// int inflateReset(z_streamp strm);
/// ```
///
/// # Safety
///
/// `strm` must be valid and initialized.
#[unsafe(no_mangle)]
#[allow(non_snake_case, clippy::cast_lossless)]
pub unsafe extern "C" fn inflateReset(strm: *mut z_stream) -> c_int {
    if strm.is_null() {
        return Z_STREAM_ERROR;
    }

    let Some(state) = (unsafe { get_state_mut(strm) }) else {
        return Z_STREAM_ERROR;
    };

    let mut rust_stream = unsafe { build_rust_stream(strm) };

    let ret = zlib_rs::inflate::inflate_reset(state, &mut rust_stream);

    // Sync back reset counters.
    let strm_ref = unsafe { &mut *strm };
    strm_ref.total_in = rust_stream.total_in as libc::c_ulong;
    strm_ref.total_out = rust_stream.total_out as libc::c_ulong;
    strm_ref.adler = rust_stream.adler as libc::c_ulong;
    strm_ref.msg = std::ptr::null();

    ret as c_int
}

/// Reset the inflate state with new `windowBits`.
///
/// Same as [`inflateReset`] but also permits changing the wrap format
/// and window size. If the window size changes, the window memory is
/// freed and will be reallocated on the next `inflate()` call.
///
/// C signature (`zlib.h` lines 997–998):
/// ```c
/// int inflateReset2(z_streamp strm, int windowBits);
/// ```
///
/// # Safety
///
/// `strm` must be valid and initialized.
#[unsafe(no_mangle)]
#[allow(non_snake_case, clippy::cast_lossless)]
pub unsafe extern "C" fn inflateReset2(strm: *mut z_stream, windowBits: c_int) -> c_int {
    if strm.is_null() {
        return Z_STREAM_ERROR;
    }

    let Some(state) = (unsafe { get_state_mut(strm) }) else {
        return Z_STREAM_ERROR;
    };

    let mut rust_stream = unsafe { build_rust_stream(strm) };

    let ret = zlib_rs::inflate::inflate_reset2(state, &mut rust_stream, windowBits);

    let strm_ref = unsafe { &mut *strm };
    strm_ref.total_in = rust_stream.total_in as libc::c_ulong;
    strm_ref.total_out = rust_stream.total_out as libc::c_ulong;
    strm_ref.adler = rust_stream.adler as libc::c_ulong;
    strm_ref.msg = std::ptr::null();

    ret as c_int
}

/// Insert bits into the inflate input stream.
///
/// Allows injection of bits before any bytes are consumed from `next_in`.
/// Useful for starting decompression at a bit position in the middle of
/// a byte for random access applications.
///
/// C signature (`zlib.h` lines 1011–1013):
/// ```c
/// int inflatePrime(z_streamp strm, int bits, int value);
/// ```
///
/// # Safety
///
/// `strm` must be valid and initialized.
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub unsafe extern "C" fn inflatePrime(strm: *mut z_stream, bits: c_int, value: c_int) -> c_int {
    if strm.is_null() {
        return Z_STREAM_ERROR;
    }

    let Some(state) = (unsafe { get_state_mut(strm) }) else {
        return Z_STREAM_ERROR;
    };

    let ret = zlib_rs::inflate::inflate_prime(state, bits, value);
    ret as c_int
}

/// Return progress information about the inflate decompression.
///
/// The return value encodes two fields:
/// - Lower 16 bits: bytes remaining in a stored block, or zero
/// - Upper bits: -1 if processing outside of a block, or the number of
///   bits back from the current position of the code being processed
///
/// Returns `-65536` (`-(1 << 16)`) if the state is invalid.
///
/// C signature (`zlib.h` line 1042):
/// ```c
/// long inflateMark(z_streamp strm);
/// ```
///
/// # Important
///
/// This function returns `c_long` (not `c_int`), matching the C
/// prototype `long ZEXPORT inflateMark(z_streamp)`.
///
/// # Safety
///
/// `strm` must be valid and initialized.
#[unsafe(no_mangle)]
#[allow(non_snake_case, clippy::cast_possible_truncation)]
pub unsafe extern "C" fn inflateMark(strm: *mut z_stream) -> c_long {
    // Error sentinel: -(1 << 16) = -65536
    const ERROR_MARK: c_long = -(1i64 as c_long).wrapping_shl(16);

    if strm.is_null() {
        return ERROR_MARK;
    }

    let Some(state) = (unsafe { get_state_ref(strm) }) else {
        return ERROR_MARK;
    };

    zlib_rs::inflate::inflate_mark(state) as c_long
}

/// Request that gzip header information be stored during decompression.
///
/// Must be called after `inflateInit2_()` or `inflateReset()`, and before
/// the first call to `inflate()`. The provided `gz_header` structure's
/// fields are filled in as the gzip stream header is processed.
///
/// C signature (`zlib.h` lines 1070–1071):
/// ```c
/// int inflateGetHeader(z_streamp strm, gz_headerp head);
/// ```
///
/// # Safety
///
/// `strm` must be valid and initialized. `head` must be a valid pointer
/// to a `gz_header` struct.
#[unsafe(no_mangle)]
#[allow(non_snake_case, clippy::cast_lossless)]
pub unsafe extern "C" fn inflateGetHeader(strm: *mut z_stream, head: *mut gz_header) -> c_int {
    if strm.is_null() || head.is_null() {
        return Z_STREAM_ERROR;
    }

    let Some(state) = (unsafe { get_state_mut(strm) }) else {
        return Z_STREAM_ERROR;
    };

    // Convert the C gz_header to a Rust GzHeader.
    // The core function stores the GzHeader internally for population
    // during subsequent inflate() calls.
    let head_ref = unsafe { &*head };
    let rust_header = zlib_rs::stream::GzHeader {
        text: head_ref.text != 0,
        #[allow(clippy::cast_possible_truncation)]
        time: head_ref.time as u32,
        xflags: head_ref.xflags,
        os: head_ref.os,
        extra: None,
        name: None,
        comment: None,
        hcrc: head_ref.hcrc != 0,
        done: head_ref.done != 0,
    };

    let ret = zlib_rs::inflate::inflate_get_header(state, Box::new(rust_header));
    ret as c_int
}

// =============================================================================
// Phase 5: inflateBack Functions (Callback-Based Decompression)
// =============================================================================

/// Initialize for callback-based raw DEFLATE decompression.
///
/// Sets up the internal state for subsequent [`inflateBack`] calls. The
/// caller must supply a window buffer of `2^windowBits` bytes. Only raw
/// DEFLATE streams (no zlib or gzip wrappers) are supported.
///
/// C signature (`zlib.h` lines 1913–1916):
/// ```c
/// int inflateBackInit_(z_streamp strm, int windowBits,
///                      unsigned char FAR *window,
///                      const char *version, int stream_size);
/// ```
///
/// # Safety
///
/// `strm` must be valid. `window` must point to at least `2^windowBits`
/// writable bytes for the duration of subsequent `inflateBack` calls.
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub unsafe extern "C" fn inflateBackInit_(
    strm: *mut z_stream,
    windowBits: c_int,
    _window: *mut c_uchar,
    version: *const c_char,
    stream_size: c_int,
) -> c_int {
    if strm.is_null() {
        return Z_STREAM_ERROR;
    }

    // Validate version and struct size.
    if !(unsafe { version_check(version, stream_size) }) {
        return zlib_rs::error::ReturnCode::VersionError as c_int;
    }

    // Create a new InflateState and initialize it for back mode.
    let mut state = zlib_rs::inflate::InflateState::new();
    let ret = zlib_rs::inflate::back::inflate_back_init(&mut state, windowBits);

    if ret != zlib_rs::error::ReturnCode::Ok {
        return ret as c_int;
    }

    // Store the state in the C stream.
    let strm_ref = unsafe { &mut *strm };
    strm_ref.state = Box::into_raw(Box::new(state)).cast::<c_void>();
    strm_ref.msg = std::ptr::null();

    ret as c_int
}

/// Perform callback-based raw DEFLATE decompression.
///
/// Decompresses an entire raw DEFLATE stream using caller-supplied I/O
/// callbacks. Input is provided via `in_fn(in_desc, &buf)` which returns
/// the number of available bytes and sets `*buf` to point at them. Output
/// is consumed via `out_fn(out_desc, buf, len)` which returns zero on
/// success or non-zero on failure.
///
/// C signature (`zlib.h` lines 1138–1140):
/// ```c
/// int inflateBack(z_streamp strm, in_func in, void FAR *in_desc,
///                 out_func out, void FAR *out_desc);
/// ```
///
/// # Safety
///
/// `strm` must be valid and initialized via [`inflateBackInit_`]. The
/// callback function pointers `in_fn` and `out_fn` must be non-null and
/// point to valid C functions. `in_desc` and `out_desc` are opaque
/// context pointers passed through to the callbacks.
#[unsafe(no_mangle)]
#[allow(non_snake_case, clippy::cast_lossless)]
pub unsafe extern "C" fn inflateBack(
    strm: *mut z_stream,
    in_fn: in_func,
    in_desc: *mut c_void,
    out_fn: out_func,
    out_desc: *mut c_void,
) -> c_int {
    if strm.is_null() {
        return Z_STREAM_ERROR;
    }

    // Validate callback function pointers. `in_func` and `out_func` are
    // `Option<unsafe extern "C" fn(...)>`, so `None` means null pointer.
    let Some(in_callback) = in_fn else {
        return Z_STREAM_ERROR;
    };
    let Some(out_callback) = out_fn else {
        return Z_STREAM_ERROR;
    };

    let Some(state) = (unsafe { get_state_mut(strm) }) else {
        return Z_STREAM_ERROR;
    };

    let mut rust_stream = unsafe { build_rust_stream(strm) };

    // Create closures that bridge the C function pointer callbacks to
    // the Rust trait objects expected by the core inflate_back function.
    let mut input_cb = || -> Vec<u8> {
        let mut buf_ptr: *const c_uchar = std::ptr::null();
        let avail = unsafe { in_callback(in_desc, &mut buf_ptr) };
        if avail == 0 || buf_ptr.is_null() {
            return Vec::new();
        }
        let slice = unsafe { std::slice::from_raw_parts(buf_ptr, avail as usize) };
        slice.to_vec()
    };

    let mut output_cb = |data: &[u8]| -> bool {
        if data.is_empty() {
            return true;
        }
        let result =
            unsafe { out_callback(out_desc, data.as_ptr().cast_mut(), data.len() as c_uint) };
        result == 0
    };

    let ret = zlib_rs::inflate::back::inflate_back(
        state,
        &mut rust_stream,
        &mut input_cb,
        &mut output_cb,
    );

    // Sync back error message if one was set.
    let strm_ref = unsafe { &mut *strm };
    match rust_stream.msg {
        Some(msg) => strm_ref.msg = msg.as_ptr().cast::<c_char>(),
        None => strm_ref.msg = std::ptr::null(),
    }

    ret as c_int
}

/// Free all memory allocated by [`inflateBackInit_`].
///
/// C signature (`zlib.h` line 1208):
/// ```c
/// int inflateBackEnd(z_streamp strm);
/// ```
///
/// # Safety
///
/// `strm` must be valid and have been initialized via [`inflateBackInit_`].
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub unsafe extern "C" fn inflateBackEnd(strm: *mut z_stream) -> c_int {
    if strm.is_null() {
        return Z_STREAM_ERROR;
    }

    let strm_ref = unsafe { &mut *strm };
    if strm_ref.state.is_null() {
        return Z_STREAM_ERROR;
    }

    // Reconstruct the Box to get mutable access, then clean up via
    // the core inflate_back_end function which releases internal buffers.
    let state_ptr = strm_ref.state.cast::<zlib_rs::inflate::InflateState>();
    let mut state = unsafe { Box::from_raw(state_ptr) };

    let ret = zlib_rs::inflate::back::inflate_back_end(&mut state);

    // Clear the state pointer — the Box is dropped when `state` goes
    // out of scope, deallocating the InflateState struct itself.
    strm_ref.state = std::ptr::null_mut();

    // Explicit drop for clarity (Box dropped, deallocating InflateState).
    drop(state);

    ret as c_int
}
