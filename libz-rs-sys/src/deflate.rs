//! C-compatible FFI wrappers for zlib deflate (compression) functions.
//!
//! This module provides `#[unsafe(no_mangle)] extern "C"` wrapper functions that
//! bridge between the C zlib deflate API (using raw pointers, `c_int` return
//! codes, and the `#[repr(C)]` [`z_stream`] struct) and the safe Rust
//! implementations in [`zlib_rs::deflate`].
//!
//! # Exported Symbols
//!
//! All 16 deflate symbols from `win32/zlib.def`:
//!
//! ## Basic Functions (def lines 5–6)
//! - [`deflate`] — Main compression function
//! - [`deflateEnd`] — Free compression state
//!
//! ## Advanced Functions (def lines 10–21)
//! - [`deflateSetDictionary`] — Set compression dictionary
//! - [`deflateGetDictionary`] — Retrieve sliding dictionary
//! - [`deflateCopy`] — Deep-copy compression stream
//! - [`deflateReset`] — Reset compression state
//! - [`deflateParams`] — Update compression level and strategy
//! - [`deflateTune`] — Fine-tune match engine parameters
//! - [`deflateBound`] — Upper bound on compressed size (`uLong`)
//! - [`deflateBound_z`] — Upper bound on compressed size (`z_size_t`)
//! - [`deflatePending`] — Query pending output bytes and bits
//! - [`deflateUsed`] — Query used bits at last byte boundary
//! - [`deflatePrime`] — Insert bits into deflate output
//! - [`deflateSetHeader`] — Set gzip header information
//!
//! ## Init Functions (def lines 90–91)
//! - [`deflateInit_`] — Initialize for default compression
//! - [`deflateInit2_`] — Initialize with explicit parameters
//!
//! # Safety Contract
//!
//! Every function that receives a raw `strm` pointer validates it for null
//! before dereferencing. Null stream pointers cause an immediate return of
//! [`Z_STREAM_ERROR`]. Dictionary, pending, and bits pointers may be null
//! where the C API permits it.
//!
//! All functions use `extern "C"` (not `extern "C-unwind"`) to abort on
//! panic at the FFI boundary rather than triggering undefined behavior
//! through stack unwinding into C code (per AAP Section 0.7.3).
//!
//! # State Management
//!
//! The deflate core API embeds state inside [`ZStream`](zlib_rs::stream::ZStream).
//! This module stores the entire Rust `ZStream` as an opaque allocation behind
//! the C `z_stream.state` pointer. Before each call, input/output buffers are
//! synchronized from the C struct into the Rust stream; after each call, results
//! are copied back and C buffer pointers are advanced.

use libc::{c_char, c_int, c_uint, c_ulong, c_void};

use crate::types::{gz_header, z_size_t, z_stream, Z_STREAM_ERROR};

// ─── Internal Constants ──────────────────────────────────────────────────────

/// The zlib version string for ABI compatibility checking.
///
/// Init functions compare the caller-provided version string against this
/// value. Only the first character (major version) is compared, matching
/// C zlib's behavior in `deflateInit2_`.
const ZLIB_VERSION_BYTES: &[u8] = b"1.3.2.1-motley\0";

// ─── Internal Helper Functions ───────────────────────────────────────────────

/// Validate that the version string's first character matches and that
/// `stream_size` matches the expected size of [`z_stream`].
///
/// Returns `true` if version and size are compatible, `false` otherwise.
/// This mirrors the C `deflateInit2_()` compatibility check.
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
    // Compare only the first byte — same check C zlib performs.
    let expected_first = ZLIB_VERSION_BYTES[0];
    let actual_first = unsafe { *version as u8 };
    if actual_first != expected_first {
        return false;
    }
    // Verify struct size to catch 32-bit vs 64-bit ABI issues.
    #[allow(clippy::cast_possible_wrap)]
    let expected_size = std::mem::size_of::<z_stream>() as c_int;
    stream_size == expected_size
}

/// Get a mutable reference to the Rust [`ZStream`] stored in `z_stream.state`.
///
/// Returns `None` if `strm.state` is null.
///
/// # Safety
///
/// `strm` must be a valid, non-null pointer to a `z_stream` whose `state`
/// field was previously set by `deflateInit_` / `deflateInit2_`.
#[inline]
unsafe fn get_stream_mut<'a>(
    strm: *mut z_stream,
) -> Option<&'a mut zlib_rs::stream::ZStream> {
    let state_ptr = unsafe { (*strm).state };
    if state_ptr.is_null() {
        return None;
    }
    Some(unsafe { &mut *(state_ptr as *mut zlib_rs::stream::ZStream) })
}

/// Get an immutable reference to the Rust [`ZStream`] stored in
/// `z_stream.state`.
///
/// Returns `None` if `strm.state` is null.
///
/// # Safety
///
/// `strm` must be a valid pointer to an initialized `z_stream`.
#[inline]
unsafe fn get_stream_ref<'a>(
    strm: *const z_stream,
) -> Option<&'a zlib_rs::stream::ZStream> {
    let state_ptr = unsafe { (*strm).state };
    if state_ptr.is_null() {
        return None;
    }
    Some(unsafe { &*(state_ptr as *const zlib_rs::stream::ZStream) })
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
unsafe fn sync_to_c(
    strm: *mut z_stream,
    rs: &zlib_rs::stream::ZStream,
) {
    let s = unsafe { &mut *strm };
    s.total_in = rs.total_in as c_ulong;
    s.total_out = rs.total_out as c_ulong;
    s.adler = rs.adler as c_ulong;
    s.data_type = rs.data_type;
    match rs.msg {
        Some(msg) => {
            s.msg = msg.as_ptr() as *const c_char;
        }
        None => {
            s.msg = std::ptr::null();
        }
    }
}

/// Compute the length of a null-terminated byte string.
///
/// # Safety
///
/// `ptr` must point to a valid null-terminated byte sequence.
#[inline]
unsafe fn c_strlen(ptr: *const u8) -> usize {
    let mut len: usize = 0;
    while unsafe { *ptr.add(len) } != 0 {
        len += 1;
    }
    len
}

/// Convert a C [`gz_header`] struct to a safe Rust
/// [`GzHeader`](zlib_rs::stream::GzHeader).
///
/// Copies all pointer-based fields (`extra`, `name`, `comment`) into
/// owned [`Vec<u8>`] buffers so the C pointers need not remain valid
/// after this call.
///
/// # Safety
///
/// `head` must be a valid, non-null pointer. Any non-null pointer fields
/// within the `gz_header` must point to valid memory of the indicated
/// lengths. `name` and `comment` must be null-terminated when non-null.
unsafe fn convert_gz_header(
    head: *const gz_header,
) -> zlib_rs::stream::GzHeader {
    let h = unsafe { &*head };

    // extra: raw pointer + length → Option<Vec<u8>>
    let extra = if !h.extra.is_null() && h.extra_len > 0 {
        let slice = unsafe {
            std::slice::from_raw_parts(h.extra, h.extra_len as usize)
        };
        Some(slice.to_vec())
    } else {
        None
    };

    // name: null-terminated raw pointer → Option<Vec<u8>>
    let name = if !h.name.is_null() {
        let len = unsafe { c_strlen(h.name) };
        let slice = unsafe { std::slice::from_raw_parts(h.name, len) };
        Some(slice.to_vec())
    } else {
        None
    };

    // comment: null-terminated raw pointer → Option<Vec<u8>>
    let comment = if !h.comment.is_null() {
        let len = unsafe { c_strlen(h.comment) };
        let slice = unsafe {
            std::slice::from_raw_parts(h.comment, len)
        };
        Some(slice.to_vec())
    } else {
        None
    };

    #[allow(clippy::cast_possible_truncation)]
    zlib_rs::stream::GzHeader {
        text: h.text != 0,
        time: h.time as u32,
        xflags: h.xflags,
        os: h.os,
        extra,
        name,
        comment,
        hcrc: h.hcrc != 0,
        done: h.done != 0,
    }
}

// ==========================================================================
// Init Functions (win32/zlib.def lines 90–91)
// ==========================================================================

/// Initialize a deflate stream for default compression.
///
/// This is the actual function behind the `deflateInit` C macro. It
/// delegates to [`deflateInit2_`] with default parameters:
/// `method = Z_DEFLATED(8)`, `windowBits = 15`, `memLevel = 8`,
/// `strategy = Z_DEFAULT_STRATEGY(0)`.
///
/// C signature (`zlib.h` lines 1903–1904):
/// ```c
/// int deflateInit_(z_streamp strm, int level, const char *version,
///                  int stream_size);
/// ```
///
/// # Returns
///
/// `Z_OK`, `Z_MEM_ERROR`, `Z_STREAM_ERROR`, or `Z_VERSION_ERROR`.
///
/// # Safety
///
/// `strm` must be a valid pointer to a `z_stream` or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn deflateInit_(
    strm: *mut z_stream,
    level: c_int,
    version: *const c_char,
    stream_size: c_int,
) -> c_int {
    // method=Z_DEFLATED(8), windowBits=MAX_WBITS(15), memLevel=8, strategy=0
    unsafe { deflateInit2_(strm, level, 8, 15, 8, 0, version, stream_size) }
}

/// Initialize a deflate stream with explicit parameters.
///
/// This is the actual function behind the `deflateInit2` C macro. The
/// `windowBits` parameter controls both the window size and the output
/// stream container format.
///
/// C signature (`zlib.h` lines 1907–1910):
/// ```c
/// int deflateInit2_(z_streamp strm, int level, int method,
///                   int windowBits, int memLevel, int strategy,
///                   const char *version, int stream_size);
/// ```
///
/// # `windowBits` Encoding
///
/// | Range         | Format                        | Window size      |
/// |---------------|-------------------------------|------------------|
/// | `8..=15`      | zlib (RFC 1950)               | `2^windowBits`   |
/// | `-8..=-15`    | raw DEFLATE (no wrapper)      | `2^abs(wB)`      |
/// | `24..=31`     | gzip only (RFC 1952)          | `2^(wB-16)`      |
///
/// # Safety
///
/// `strm` must be a valid pointer to a `z_stream` or null.
#[unsafe(no_mangle)]
#[allow(non_snake_case, clippy::cast_lossless)]
pub unsafe extern "C" fn deflateInit2_(
    strm: *mut z_stream,
    level: c_int,
    method: c_int,
    windowBits: c_int,
    memLevel: c_int,
    strategy: c_int,
    version: *const c_char,
    stream_size: c_int,
) -> c_int {
    if strm.is_null() {
        return Z_STREAM_ERROR;
    }

    // Validate version string and struct size for ABI compatibility.
    if !(unsafe { version_check(version, stream_size) }) {
        return zlib_rs::error::ReturnCode::VersionError as c_int;
    }

    // Create a new Rust ZStream and initialize the deflate state.
    let mut rust_stream = zlib_rs::stream::ZStream::new();
    let ret = zlib_rs::deflate::deflate_init2(
        &mut rust_stream,
        level,
        method,
        windowBits,
        memLevel,
        strategy,
    );

    if ret != zlib_rs::error::ReturnCode::Ok {
        return ret as c_int;
    }

    // Store the entire Rust ZStream as opaque state in the C z_stream.
    let strm_ref = unsafe { &mut *strm };
    strm_ref.state = Box::into_raw(Box::new(rust_stream)) as *mut c_void;

    // Initialize C scalar fields from the freshly initialized Rust stream.
    let rs_ref =
        unsafe { &*(strm_ref.state as *const zlib_rs::stream::ZStream) };
    strm_ref.total_in = 0;
    strm_ref.total_out = 0;
    strm_ref.adler = rs_ref.adler as c_ulong;
    strm_ref.data_type = rs_ref.data_type;
    strm_ref.msg = std::ptr::null();

    ret as c_int
}

// ==========================================================================
// Core Deflate Functions (win32/zlib.def lines 5–6)
// ==========================================================================

/// Compress data from the input buffer to the output buffer.
///
/// This is the main compression function — the most performance-critical
/// entry point in the deflate API. The FFI wrapper bridges buffer pointers
/// and delegates directly to the core Rust engine.
///
/// C signature (`zlib.h` line 254):
/// ```c
/// int deflate(z_streamp strm, int flush);
/// ```
///
/// # Flush Modes
///
/// | Value | Constant           | Behavior                              |
/// |-------|--------------------|---------------------------------------|
/// | 0     | `Z_NO_FLUSH`       | Normal — accumulate and compress      |
/// | 1     | `Z_PARTIAL_FLUSH`  | Flush, no byte alignment              |
/// | 2     | `Z_SYNC_FLUSH`     | Flush + byte align + `00 00 FF FF`    |
/// | 3     | `Z_FULL_FLUSH`     | Sync flush + reset state              |
/// | 4     | `Z_FINISH`         | Finish the stream                     |
/// | 5     | `Z_BLOCK`          | Complete current block                |
/// | 6     | `Z_TREES`          | Complete block header                 |
///
/// # Returns
///
/// `Z_OK`, `Z_STREAM_END`, `Z_STREAM_ERROR`, or `Z_BUF_ERROR`.
///
/// # Safety
///
/// `strm` must be a valid pointer to an initialized `z_stream` with valid
/// `next_in`/`avail_in` and `next_out`/`avail_out` fields.
#[unsafe(no_mangle)]
#[allow(clippy::cast_possible_truncation, clippy::cast_lossless)]
pub unsafe extern "C" fn deflate(strm: *mut z_stream, flush: c_int) -> c_int {
    if strm.is_null() {
        return Z_STREAM_ERROR;
    }

    // Snapshot C buffer state before obtaining the mutable Rust stream.
    let (avail_in, next_in, avail_out, next_out) = unsafe {
        let s = &*strm;
        (
            s.avail_in as usize,
            s.next_in,
            s.avail_out as usize,
            s.next_out,
        )
    };
    let (c_total_in, c_total_out, c_adler, c_data_type) = unsafe {
        let s = &*strm;
        (s.total_in, s.total_out, s.adler, s.data_type)
    };

    // Get the stored Rust ZStream.
    let rs = match unsafe { get_stream_mut(strm) } {
        Some(s) => s,
        None => return Z_STREAM_ERROR,
    };

    // Set up input from the C buffer.
    if !next_in.is_null() && avail_in > 0 {
        let input = unsafe { std::slice::from_raw_parts(next_in, avail_in) };
        rs.set_input(input);
    } else {
        rs.set_input(&[]);
    }

    // Set up output buffer matching the C avail_out.
    if avail_out > 0 {
        rs.set_output_buffer(avail_out);
    }

    // Sync scalar fields from C to Rust.
    rs.total_in = c_total_in as u64;
    rs.total_out = c_total_out as u64;
    rs.adler = c_adler as u32;
    rs.data_type = c_data_type;

    let input_before = rs.avail_in();
    let output_before = rs.avail_out();

    // Call the core deflate engine.
    let ret = zlib_rs::deflate::deflate(rs, flush);

    let input_consumed = input_before.saturating_sub(rs.avail_in());
    let output_produced = output_before.saturating_sub(rs.avail_out());

    // Copy produced output back to the C buffer.
    if output_produced > 0 && !next_out.is_null() {
        let written = rs.output_written();
        let copy_len = output_produced.min(written.len());
        if copy_len > 0 {
            unsafe {
                std::ptr::copy_nonoverlapping(
                    written.as_ptr(),
                    next_out,
                    copy_len,
                );
            }
        }
    }

    // Advance C buffer pointers.
    let s = unsafe { &mut *strm };
    if !s.next_in.is_null() {
        s.next_in = unsafe { s.next_in.add(input_consumed) };
    }
    s.avail_in -= input_consumed as c_uint;
    if !s.next_out.is_null() {
        s.next_out = unsafe { s.next_out.add(output_produced) };
    }
    s.avail_out -= output_produced as c_uint;

    // Sync scalar fields back to C.
    unsafe { sync_to_c(strm, rs) };

    ret as c_int
}

/// Free all dynamically allocated data structures for the deflate stream.
///
/// Discards any unprocessed input and does not flush pending output.
/// After this call the stream cannot be used for compression unless
/// re-initialized.
///
/// C signature (`zlib.h` line 367):
/// ```c
/// int deflateEnd(z_streamp strm);
/// ```
///
/// # Returns
///
/// `Z_OK`, `Z_STREAM_ERROR`, or `Z_DATA_ERROR` (if freed prematurely).
///
/// # Safety
///
/// `strm` must be a valid pointer to an initialized `z_stream`.
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub unsafe extern "C" fn deflateEnd(strm: *mut z_stream) -> c_int {
    if strm.is_null() {
        return Z_STREAM_ERROR;
    }

    let strm_ref = unsafe { &mut *strm };
    if strm_ref.state.is_null() {
        return Z_STREAM_ERROR;
    }

    // Reconstruct the Box<ZStream> to take ownership for proper drop.
    let state_ptr = strm_ref.state as *mut zlib_rs::stream::ZStream;
    let mut rust_stream = unsafe { Box::from_raw(state_ptr) };

    let ret = zlib_rs::deflate::deflate_end(&mut rust_stream);

    // Clear the state pointer — RAII cleanup happens when `rust_stream`
    // goes out of scope.
    strm_ref.state = std::ptr::null_mut();
    strm_ref.msg = std::ptr::null();

    drop(rust_stream);

    ret as c_int
}

// ==========================================================================
// Dictionary Functions (win32/zlib.def lines 10–11)
// ==========================================================================

/// Set the compression dictionary from the given byte sequence.
///
/// Must be called immediately after `deflateInit` / `deflateInit2` or
/// `deflateReset`, and before any call to `deflate`. Upon return,
/// `strm->adler` is set to the Adler-32 of the dictionary.
///
/// C signature (`zlib.h` lines 618–620):
/// ```c
/// int deflateSetDictionary(z_streamp strm, const Bytef *dictionary,
///                          uInt dictLength);
/// ```
///
/// # Safety
///
/// `strm` must be valid and initialized. If `dictionary` is non-null,
/// it must point to at least `dictLength` readable bytes.
#[unsafe(no_mangle)]
#[allow(non_snake_case, clippy::cast_lossless)]
pub unsafe extern "C" fn deflateSetDictionary(
    strm: *mut z_stream,
    dictionary: *const u8,
    dictLength: c_uint,
) -> c_int {
    if strm.is_null() {
        return Z_STREAM_ERROR;
    }

    let rs = match unsafe { get_stream_mut(strm) } {
        Some(s) => s,
        None => return Z_STREAM_ERROR,
    };

    // Build the dictionary slice. A null dictionary with zero length is
    // treated as an empty dictionary (no-op).
    let dict_slice = if !dictionary.is_null() && dictLength > 0 {
        unsafe {
            std::slice::from_raw_parts(dictionary, dictLength as usize)
        }
    } else {
        &[]
    };

    let ret = zlib_rs::deflate::deflate_set_dictionary(rs, dict_slice);

    // Sync adler (dictionary Adler-32) and other scalars back to C.
    unsafe { sync_to_c(strm, rs) };

    ret as c_int
}

/// Retrieve the current sliding dictionary from the deflate state.
///
/// If `dictionary` is `Z_NULL`, only the dictionary length is returned
/// via `dictLength`. If `dictLength` is `Z_NULL`, the length is not
/// stored. `dictionary` must have at least 32768 bytes of space when
/// non-null.
///
/// C signature (`zlib.h` lines 662–664):
/// ```c
/// int deflateGetDictionary(z_streamp strm, Bytef *dictionary,
///                          uInt *dictLength);
/// ```
///
/// # Safety
///
/// `strm` must be valid and initialized. If `dictionary` is non-null,
/// it must point to at least 32768 writable bytes. `dictLength` may
/// be null.
#[unsafe(no_mangle)]
#[allow(non_snake_case, clippy::cast_possible_truncation)]
pub unsafe extern "C" fn deflateGetDictionary(
    strm: *mut z_stream,
    dictionary: *mut u8,
    dictLength: *mut c_uint,
) -> c_int {
    if strm.is_null() {
        return Z_STREAM_ERROR;
    }

    let rs = match unsafe { get_stream_ref(strm as *const z_stream) } {
        Some(s) => s,
        None => return Z_STREAM_ERROR,
    };

    if dictionary.is_null() {
        // Caller only wants the length — pass an empty buffer.
        let mut dummy = [0u8; 0];
        match zlib_rs::deflate::deflate_get_dictionary(rs, &mut dummy) {
            Ok(len) => {
                if !dictLength.is_null() {
                    unsafe { *dictLength = len as c_uint };
                }
                zlib_rs::error::ReturnCode::Ok as c_int
            }
            Err(e) => e as c_int,
        }
    } else {
        // Write the dictionary into the caller-provided buffer.
        // Maximum dictionary size is the window size (32768 bytes).
        let buf = unsafe {
            std::slice::from_raw_parts_mut(dictionary, 32768)
        };
        match zlib_rs::deflate::deflate_get_dictionary(rs, buf) {
            Ok(len) => {
                if !dictLength.is_null() {
                    unsafe { *dictLength = len as c_uint };
                }
                zlib_rs::error::ReturnCode::Ok as c_int
            }
            Err(e) => e as c_int,
        }
    }
}

// ==========================================================================
// Stream Control Functions (win32/zlib.def lines 12–15)
// ==========================================================================

/// Deep-copy the compression stream from source to dest.
///
/// Sets the destination stream as a complete copy of the source stream,
/// duplicating all internal compression state.
///
/// C signature (`zlib.h` lines 684–685):
/// ```c
/// int deflateCopy(z_streamp dest, z_streamp source);
/// ```
///
/// # Returns
///
/// `Z_OK`, `Z_MEM_ERROR`, or `Z_STREAM_ERROR`.
///
/// # Safety
///
/// Both `dest` and `source` must be valid, non-null pointers. `source`
/// must be a properly initialized deflate stream.
#[unsafe(no_mangle)]
#[allow(non_snake_case, clippy::cast_lossless)]
pub unsafe extern "C" fn deflateCopy(
    dest: *mut z_stream,
    source: *mut z_stream,
) -> c_int {
    if dest.is_null() || source.is_null() {
        return Z_STREAM_ERROR;
    }

    let source_rs =
        match unsafe { get_stream_ref(source as *const z_stream) } {
            Some(s) => s,
            None => return Z_STREAM_ERROR,
        };

    // Create a fresh Rust ZStream for the destination and deep-copy.
    let mut dest_rs = zlib_rs::stream::ZStream::new();
    let ret = zlib_rs::deflate::deflate_copy(&mut dest_rs, source_rs);

    if ret != zlib_rs::error::ReturnCode::Ok {
        return ret as c_int;
    }

    // Store the new ZStream in the destination C z_stream.
    let dest_ref = unsafe { &mut *dest };
    dest_ref.state = Box::into_raw(Box::new(dest_rs)) as *mut c_void;

    // Sync scalar fields from the copied Rust stream to C.
    let dest_rs_ref = unsafe {
        &*(dest_ref.state as *const zlib_rs::stream::ZStream)
    };
    unsafe { sync_to_c(dest, dest_rs_ref) };

    ret as c_int
}

/// Reset the deflate stream state, keeping allocated buffers.
///
/// Equivalent to `deflateEnd` followed by `deflateInit`, but does not
/// free and reallocate the internal compression state. Resets `total_in`,
/// `total_out`, `adler`, and `msg`.
///
/// C signature (`zlib.h` line 702):
/// ```c
/// int deflateReset(z_streamp strm);
/// ```
///
/// # Safety
///
/// `strm` must be valid and initialized.
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub unsafe extern "C" fn deflateReset(strm: *mut z_stream) -> c_int {
    if strm.is_null() {
        return Z_STREAM_ERROR;
    }

    let rs = match unsafe { get_stream_mut(strm) } {
        Some(s) => s,
        None => return Z_STREAM_ERROR,
    };

    let ret = zlib_rs::deflate::deflate_reset(rs);

    // Sync the reset scalar fields back to C.
    unsafe { sync_to_c(strm, rs) };

    ret as c_int
}

/// Dynamically update the compression level and strategy.
///
/// May internally call `deflate(Z_BLOCK)` to flush existing data before
/// changing parameters, which requires valid I/O buffers.
///
/// C signature (`zlib.h` lines 713–715):
/// ```c
/// int deflateParams(z_streamp strm, int level, int strategy);
/// ```
///
/// # Returns
///
/// `Z_OK`, `Z_STREAM_ERROR`, or `Z_BUF_ERROR`.
///
/// # Safety
///
/// `strm` must be valid and initialized with valid I/O buffer pointers.
#[unsafe(no_mangle)]
#[allow(non_snake_case, clippy::cast_possible_truncation, clippy::cast_lossless)]
pub unsafe extern "C" fn deflateParams(
    strm: *mut z_stream,
    level: c_int,
    strategy: c_int,
) -> c_int {
    if strm.is_null() {
        return Z_STREAM_ERROR;
    }

    // Snapshot C buffer state before obtaining the mutable Rust stream.
    let (avail_in, next_in, avail_out, next_out) = unsafe {
        let s = &*strm;
        (
            s.avail_in as usize,
            s.next_in,
            s.avail_out as usize,
            s.next_out,
        )
    };
    let (c_total_in, c_total_out, c_adler, c_data_type) = unsafe {
        let s = &*strm;
        (s.total_in, s.total_out, s.adler, s.data_type)
    };

    let rs = match unsafe { get_stream_mut(strm) } {
        Some(s) => s,
        None => return Z_STREAM_ERROR,
    };

    // Set up I/O — deflate_params may call deflate(Z_BLOCK) internally.
    if !next_in.is_null() && avail_in > 0 {
        let input = unsafe { std::slice::from_raw_parts(next_in, avail_in) };
        rs.set_input(input);
    } else {
        rs.set_input(&[]);
    }
    if avail_out > 0 {
        rs.set_output_buffer(avail_out);
    }
    rs.total_in = c_total_in as u64;
    rs.total_out = c_total_out as u64;
    rs.adler = c_adler as u32;
    rs.data_type = c_data_type;

    let input_before = rs.avail_in();
    let output_before = rs.avail_out();

    let ret = zlib_rs::deflate::deflate_params(rs, level, strategy);

    let input_consumed = input_before.saturating_sub(rs.avail_in());
    let output_produced = output_before.saturating_sub(rs.avail_out());

    // Finalize I/O: copy output back, advance C buffer pointers.
    if output_produced > 0 && !next_out.is_null() {
        let written = rs.output_written();
        let copy_len = output_produced.min(written.len());
        if copy_len > 0 {
            unsafe {
                std::ptr::copy_nonoverlapping(
                    written.as_ptr(),
                    next_out,
                    copy_len,
                );
            }
        }
    }
    let s = unsafe { &mut *strm };
    if !s.next_in.is_null() {
        s.next_in = unsafe { s.next_in.add(input_consumed) };
    }
    s.avail_in -= input_consumed as c_uint;
    if !s.next_out.is_null() {
        s.next_out = unsafe { s.next_out.add(output_produced) };
    }
    s.avail_out -= output_produced as c_uint;

    // Sync scalar fields back to C.
    unsafe { sync_to_c(strm, rs) };

    ret as c_int
}

/// Fine-tune deflate's internal compression parameters.
///
/// This should only be used by someone who understands the algorithm
/// used by zlib's deflate for searching for the best matching string.
///
/// C signature (`zlib.h` lines 751–755):
/// ```c
/// int deflateTune(z_streamp strm, int good_length, int max_lazy,
///                 int nice_length, int max_chain);
/// ```
///
/// # Safety
///
/// `strm` must be valid and initialized.
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub unsafe extern "C" fn deflateTune(
    strm: *mut z_stream,
    good_length: c_int,
    max_lazy: c_int,
    nice_length: c_int,
    max_chain: c_int,
) -> c_int {
    if strm.is_null() {
        return Z_STREAM_ERROR;
    }

    let rs = match unsafe { get_stream_mut(strm) } {
        Some(s) => s,
        None => return Z_STREAM_ERROR,
    };

    zlib_rs::deflate::deflate_tune(
        rs,
        good_length,
        max_lazy,
        nice_length,
        max_chain,
    ) as c_int
}

// ==========================================================================
// Bound Functions (win32/zlib.def lines 16–17)
// ==========================================================================

/// Return an upper bound on the compressed size after deflation.
///
/// Must be called after `deflateInit()` or `deflateInit2()`, and after
/// `deflateSetHeader()` if used. If `strm` is null or not initialized,
/// returns a generic conservative bound.
///
/// C signature (`zlib.h` line 768):
/// ```c
/// uLong deflateBound(z_streamp strm, uLong sourceLen);
/// ```
///
/// # Safety
///
/// `strm` may be null (returns a generic upper bound).
#[unsafe(no_mangle)]
#[allow(non_snake_case, clippy::cast_lossless, clippy::cast_possible_truncation)]
pub unsafe extern "C" fn deflateBound(
    strm: *mut z_stream,
    sourceLen: c_ulong,
) -> c_ulong {
    let source_len = sourceLen as usize;

    if strm.is_null() {
        // Use a dummy stream for a generic bound calculation.
        let dummy = zlib_rs::stream::ZStream::new();
        return zlib_rs::deflate::deflate_bound(&dummy, source_len)
            as c_ulong;
    }

    match unsafe { get_stream_ref(strm as *const z_stream) } {
        Some(rs) => {
            zlib_rs::deflate::deflate_bound(rs, source_len) as c_ulong
        }
        None => {
            // State not initialized — return generic bound.
            let dummy = zlib_rs::stream::ZStream::new();
            zlib_rs::deflate::deflate_bound(&dummy, source_len) as c_ulong
        }
    }
}

/// Return an upper bound on the compressed size — `z_size_t` variant.
///
/// Identical to [`deflateBound`] but takes and returns `z_size_t`
/// (`size_t`) instead of `uLong`. This avoids truncation on platforms
/// where `uLong` is 32-bit (e.g., 64-bit Windows).
///
/// C signature (`zlib.h` line 769):
/// ```c
/// z_size_t deflateBound_z(z_streamp strm, z_size_t sourceLen);
/// ```
///
/// # Safety
///
/// `strm` may be null (returns a generic upper bound).
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub unsafe extern "C" fn deflateBound_z(
    strm: *mut z_stream,
    sourceLen: z_size_t,
) -> z_size_t {
    if strm.is_null() {
        let dummy = zlib_rs::stream::ZStream::new();
        return zlib_rs::deflate::deflate_bound(&dummy, sourceLen);
    }

    match unsafe { get_stream_ref(strm as *const z_stream) } {
        Some(rs) => zlib_rs::deflate::deflate_bound(rs, sourceLen),
        None => {
            let dummy = zlib_rs::stream::ZStream::new();
            zlib_rs::deflate::deflate_bound(&dummy, sourceLen)
        }
    }
}

// ==========================================================================
// Query Functions (win32/zlib.def lines 18–21)
// ==========================================================================

/// Query the number of pending bytes and bits in the deflate output buffer.
///
/// The `pending` and `bits` pointers may each be `Z_NULL`; if so, the
/// corresponding value is not stored.
///
/// C signature (`zlib.h` lines 786–788):
/// ```c
/// int deflatePending(z_streamp strm, unsigned *pending, int *bits);
/// ```
///
/// # Returns
///
/// `Z_OK`, `Z_STREAM_ERROR`, or `Z_BUF_ERROR` (if pending count overflows
/// `unsigned`).
///
/// # Safety
///
/// `strm` must be valid and initialized. `pending` and `bits` may be null.
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub unsafe extern "C" fn deflatePending(
    strm: *mut z_stream,
    pending: *mut c_uint,
    bits: *mut c_int,
) -> c_int {
    if strm.is_null() {
        return Z_STREAM_ERROR;
    }

    let rs = match unsafe { get_stream_ref(strm as *const z_stream) } {
        Some(s) => s,
        None => return Z_STREAM_ERROR,
    };

    match zlib_rs::deflate::deflate_pending(rs) {
        Ok((p, b)) => {
            if !pending.is_null() {
                unsafe { *pending = p };
            }
            if !bits.is_null() {
                unsafe { *bits = b };
            }
            zlib_rs::error::ReturnCode::Ok as c_int
        }
        Err(e) => e as c_int,
    }
}

/// Query the number of used bits at the last byte boundary.
///
/// The `bits` pointer may be `Z_NULL`.
///
/// C signature (`zlib.h` lines 804–805):
/// ```c
/// int deflateUsed(z_streamp strm, int *bits);
/// ```
///
/// # Safety
///
/// `strm` must be valid and initialized. `bits` may be null.
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub unsafe extern "C" fn deflateUsed(
    strm: *mut z_stream,
    bits: *mut c_int,
) -> c_int {
    if strm.is_null() {
        return Z_STREAM_ERROR;
    }

    let rs = match unsafe { get_stream_ref(strm as *const z_stream) } {
        Some(s) => s,
        None => return Z_STREAM_ERROR,
    };

    match zlib_rs::deflate::deflate_used(rs) {
        Ok(b) => {
            if !bits.is_null() {
                unsafe { *bits = b };
            }
            zlib_rs::error::ReturnCode::Ok as c_int
        }
        Err(e) => e as c_int,
    }
}

/// Insert bits in the deflate output stream.
///
/// Used to start off the deflate output with the bits leftover from a
/// previous deflate stream when appending to it. Can only be used for
/// raw deflate, and must be used before the first `deflate()` call.
///
/// C signature (`zlib.h` lines 816–818):
/// ```c
/// int deflatePrime(z_streamp strm, int bits, int value);
/// ```
///
/// # Returns
///
/// `Z_OK`, `Z_BUF_ERROR`, or `Z_STREAM_ERROR`.
///
/// # Safety
///
/// `strm` must be valid and initialized.
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub unsafe extern "C" fn deflatePrime(
    strm: *mut z_stream,
    bits: c_int,
    value: c_int,
) -> c_int {
    if strm.is_null() {
        return Z_STREAM_ERROR;
    }

    let rs = match unsafe { get_stream_mut(strm) } {
        Some(s) => s,
        None => return Z_STREAM_ERROR,
    };

    zlib_rs::deflate::deflate_prime(rs, bits, value) as c_int
}

/// Provide gzip header information for a gzip-format stream.
///
/// Must be called after `deflateInit2()` or `deflateReset()` and before
/// the first call to `deflate()`, only when the stream was initialized
/// with `windowBits` in the gzip range (24–31).
///
/// C signature (`zlib.h` lines 833–834):
/// ```c
/// int deflateSetHeader(z_streamp strm, gz_headerp head);
/// ```
///
/// # Safety
///
/// `strm` must be valid and initialized for gzip output. `head` must be
/// a valid, non-null pointer. Any non-null pointer fields within `head`
/// (`extra`, `name`, `comment`) must point to valid memory.
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub unsafe extern "C" fn deflateSetHeader(
    strm: *mut z_stream,
    head: *mut gz_header,
) -> c_int {
    if strm.is_null() {
        return Z_STREAM_ERROR;
    }
    if head.is_null() {
        return Z_STREAM_ERROR;
    }

    let rs = match unsafe { get_stream_mut(strm) } {
        Some(s) => s,
        None => return Z_STREAM_ERROR,
    };

    let rust_header = unsafe { convert_gz_header(head as *const gz_header) };
    zlib_rs::deflate::deflate_set_header(rs, rust_header) as c_int
}
