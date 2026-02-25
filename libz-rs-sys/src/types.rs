//! `#[repr(C)]` FFI type definitions for C ABI compatibility with zlib.
//!
//! This module defines all C-compatible types required for the FFI boundary
//! layer between Rust and C code. These types have the **exact same memory
//! layout** as their C counterparts defined in `zlib.h` and `zconf.h`,
//! ensuring binary compatibility with the original zlib 1.3.2.1-motley
//! library.
//!
//! # Types
//!
//! - [`z_stream`] — The central stream structure for deflate/inflate operations
//! - [`gz_header`] — Gzip header information (RFC 1952)
//! - [`gzFile_s`] / [`gzFile`] — Semi-opaque gzip file descriptor
//! - [`alloc_func`] / [`free_func`] — Custom memory allocator function pointers
//! - [`in_func`] / [`out_func`] — Callback function pointers for `inflateBack`
//!
//! # Safety
//!
//! All structs use `#[repr(C)]` to guarantee C-compatible memory layout.
//! Pointer fields use raw pointers for C interoperability. Function pointer
//! types use `Option<unsafe extern "C" fn(...)>` to represent nullable C
//! function pointers with Rust's niche optimization (where `None` corresponds
//! to a null pointer).

#![allow(non_camel_case_types)]

use libc::{c_char, c_int, c_uchar, c_uint, c_ulong, c_void};

// ---------------------------------------------------------------------------
// Function pointer type aliases
// ---------------------------------------------------------------------------

/// Custom memory allocation function type.
///
/// Equivalent to the C typedef:
/// ```c
/// typedef voidpf (*alloc_func)(voidpf opaque, uInt items, uInt size);
/// ```
///
/// The allocator receives the opaque pointer from [`z_stream`], the number
/// of items to allocate, and the size of each item. It returns a pointer to
/// the allocated memory or null on failure.
pub type alloc_func = Option<
    unsafe extern "C" fn(opaque: *mut c_void, items: c_uint, size: c_uint) -> *mut c_void,
>;

/// Custom memory deallocation function type.
///
/// Equivalent to the C typedef:
/// ```c
/// typedef void (*free_func)(voidpf opaque, voidpf address);
/// ```
///
/// The deallocator receives the opaque pointer from [`z_stream`] and the
/// address of the memory block to free.
pub type free_func =
    Option<unsafe extern "C" fn(opaque: *mut c_void, address: *mut c_void)>;

/// Input callback function type for `inflateBack`.
///
/// Equivalent to the C typedef:
/// ```c
/// typedef unsigned (*in_func)(void FAR *, z_const unsigned char FAR * FAR *);
/// ```
///
/// Called to provide input data. Sets `*buf` to point at the input buffer
/// and returns the number of bytes available. Returns zero when no more
/// input is available.
pub type in_func = Option<
    unsafe extern "C" fn(
        in_desc: *mut c_void,
        buf: *mut *const c_uchar,
    ) -> c_uint,
>;

/// Output callback function type for `inflateBack`.
///
/// Equivalent to the C typedef:
/// ```c
/// typedef int (*out_func)(void FAR *, unsigned char FAR *, unsigned);
/// ```
///
/// Called to consume output data. Receives a pointer to the output buffer
/// and its length. Returns zero on success or non-zero on failure.
pub type out_func = Option<
    unsafe extern "C" fn(
        out_desc: *mut c_void,
        buf: *mut c_uchar,
        len: c_uint,
    ) -> c_int,
>;

// ---------------------------------------------------------------------------
// z_stream — central stream structure
// ---------------------------------------------------------------------------

/// The `z_stream` structure for deflate/inflate operations.
///
/// This structure is passed to all deflate and inflate functions and tracks
/// the state of a compression or decompression operation. The application
/// sets the input/output buffer pointers and sizes; the library updates the
/// totals, checksum, data type, and internal state.
///
/// This struct **must** have an identical memory layout to the C
/// `z_stream_s` struct defined in `zlib.h` lines 90–110. Layout
/// compatibility is verified via compile-time size assertions.
///
/// # C Definition
///
/// ```c
/// typedef struct z_stream_s {
///     z_const Bytef *next_in;
///     uInt     avail_in;
///     uLong    total_in;
///     Bytef    *next_out;
///     uInt     avail_out;
///     uLong    total_out;
///     z_const char *msg;
///     struct internal_state FAR *state;
///     alloc_func zalloc;
///     free_func  zfree;
///     voidpf     opaque;
///     int     data_type;
///     uLong   adler;
///     uLong   reserved;
/// } z_stream;
/// ```
#[repr(C)]
pub struct z_stream {
    /// Pointer to the next input byte to be consumed.
    pub next_in: *const u8,

    /// Number of bytes available at `next_in`.
    pub avail_in: c_uint,

    /// Total number of input bytes read so far.
    pub total_in: c_ulong,

    /// Pointer to the next output byte to be written.
    pub next_out: *mut u8,

    /// Remaining free space (in bytes) at `next_out`.
    pub avail_out: c_uint,

    /// Total number of bytes output so far.
    pub total_out: c_ulong,

    /// Last error message, or null if no error.
    pub msg: *const c_char,

    /// Internal state — opaque to applications. Managed by the library.
    pub state: *mut c_void,

    /// Custom allocator function, or `None` for the default allocator.
    pub zalloc: alloc_func,

    /// Custom deallocator function, or `None` for the default deallocator.
    pub zfree: free_func,

    /// Private data object passed as the first argument to `zalloc`/`zfree`.
    pub opaque: *mut c_void,

    /// Best guess about the data type: [`Z_BINARY`], [`Z_TEXT`], or
    /// [`Z_UNKNOWN`].
    pub data_type: c_int,

    /// Adler-32 or CRC-32 checksum of the uncompressed data.
    pub adler: c_ulong,

    /// Reserved for future use.
    pub reserved: c_ulong,
}

// Compile-time layout verification for z_stream ABI compatibility.
// The #[repr(C)] attribute guarantees C-compatible field ordering and
// alignment. These platform-specific assertions verify the exact struct size.
//
// LP64  (64-bit Unix/Linux/macOS): c_ulong=8, ptr=8 → 112 bytes
// LLP64 (64-bit Windows):         c_ulong=4, ptr=8 → 88 bytes
// ILP32 (32-bit):                  c_ulong=4, ptr=4 → 56 bytes
#[cfg(all(target_pointer_width = "64", not(target_os = "windows")))]
const _: () = assert!(std::mem::size_of::<z_stream>() == 112);

#[cfg(all(target_pointer_width = "64", target_os = "windows"))]
const _: () = assert!(std::mem::size_of::<z_stream>() == 88);

#[cfg(target_pointer_width = "32")]
const _: () = assert!(std::mem::size_of::<z_stream>() == 56);

impl z_stream {
    /// Creates a zeroed `z_stream`, equivalent to C's
    /// `memset(&strm, 0, sizeof(z_stream))`.
    ///
    /// All pointer fields are set to null, all numeric fields to zero, and
    /// both allocator function pointers to `None` (indicating that the
    /// library should use its default allocator).
    #[must_use]
    pub const fn zeroed() -> Self {
        Self {
            next_in: std::ptr::null(),
            avail_in: 0,
            total_in: 0,
            next_out: std::ptr::null_mut(),
            avail_out: 0,
            total_out: 0,
            msg: std::ptr::null(),
            state: std::ptr::null_mut(),
            zalloc: None,
            zfree: None,
            opaque: std::ptr::null_mut(),
            data_type: 0,
            adler: 0,
            reserved: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// gz_header — gzip header information
// ---------------------------------------------------------------------------

/// Gzip header information passed to and from zlib routines.
///
/// See RFC 1952 for details on the meanings of these fields. This structure
/// is used by `deflateSetHeader` to provide header data when writing a gzip
/// stream, and by `inflateGetHeader` to receive header data when reading one.
///
/// This struct **must** have an identical memory layout to the C
/// `gz_header_s` struct defined in `zlib.h` lines 118–133.
///
/// # C Definition
///
/// ```c
/// typedef struct gz_header_s {
///     int     text;
///     uLong   time;
///     int     xflags;
///     int     os;
///     Bytef   *extra;
///     uInt    extra_len;
///     uInt    extra_max;
///     Bytef   *name;
///     uInt    name_max;
///     Bytef   *comment;
///     uInt    comm_max;
///     int     hcrc;
///     int     done;
/// } gz_header;
/// ```
#[repr(C)]
pub struct gz_header {
    /// True if the compressed data is believed to be text.
    pub text: c_int,

    /// Modification time (Unix timestamp).
    pub time: c_ulong,

    /// Extra flags (not used when writing a gzip file).
    pub xflags: c_int,

    /// Operating system identifier.
    pub os: c_int,

    /// Pointer to the extra field, or null if none.
    pub extra: *mut u8,

    /// Length of the extra field (valid if `extra` is not null).
    pub extra_len: c_uint,

    /// Space available at `extra` (only used when reading a header).
    pub extra_max: c_uint,

    /// Pointer to a zero-terminated file name, or null if none.
    pub name: *mut u8,

    /// Space available at `name` (only used when reading a header).
    pub name_max: c_uint,

    /// Pointer to a zero-terminated comment, or null if none.
    pub comment: *mut u8,

    /// Space available at `comment` (only used when reading a header).
    pub comm_max: c_uint,

    /// True if there was or will be a header CRC.
    pub hcrc: c_int,

    /// True when done reading the gzip header (not used when writing).
    pub done: c_int,
}

// ---------------------------------------------------------------------------
// gzFile — semi-opaque gzip file descriptor
// ---------------------------------------------------------------------------

/// Semi-opaque gzip file descriptor structure.
///
/// The full internal state is larger than this exposed structure. Only these
/// three fields are exposed for use by the `gzgetc()` macro. Users should
/// not access these fields directly; their names and behavior may change.
///
/// Corresponds to `struct gzFile_s` in `zlib.h` lines 1956–1960.
#[repr(C)]
pub struct gzFile_s {
    /// Number of bytes available at `next`.
    pub have: c_uint,

    /// Pointer to the next byte in the buffer.
    pub next: *mut c_uchar,

    /// Current position in the uncompressed data stream (`z_off64_t`).
    pub pos: i64,
}

/// Opaque gzip file handle type — a pointer to [`gzFile_s`].
///
/// Equivalent to the C typedef: `typedef struct gzFile_s *gzFile;`
pub type gzFile = *mut gzFile_s;

// ---------------------------------------------------------------------------
// Type aliases mapping C types to Rust
// ---------------------------------------------------------------------------

/// File offset type (signed). Equivalent to C `z_off_t` / `off_t`.
pub type z_off_t = libc::off_t;

/// 64-bit file offset type. Equivalent to C `z_off64_t`.
pub type z_off64_t = i64;

/// Size type for length parameters. Equivalent to C `z_size_t` / `size_t`.
pub type z_size_t = usize;

/// Mutable pointer to [`z_stream`]. Equivalent to C `z_streamp`.
pub type z_streamp = *mut z_stream;

/// Mutable pointer to [`gz_header`]. Equivalent to C `gz_headerp`.
pub type gz_headerp = *mut gz_header;

// ---------------------------------------------------------------------------
// Return codes (zlib.h lines 181–189)
// ---------------------------------------------------------------------------

/// Operation completed successfully.
pub const Z_OK: c_int = 0;

/// End of the compressed stream reached.
pub const Z_STREAM_END: c_int = 1;

/// A preset dictionary is needed for decompression.
pub const Z_NEED_DICT: c_int = 2;

/// An error occurred in the underlying I/O (check `errno`).
pub const Z_ERRNO: c_int = -1;

/// The stream state is inconsistent (e.g., null stream pointer).
pub const Z_STREAM_ERROR: c_int = -2;

/// The compressed data is corrupted or incomplete.
pub const Z_DATA_ERROR: c_int = -3;

/// Insufficient memory to complete the operation.
pub const Z_MEM_ERROR: c_int = -4;

/// Insufficient buffer space for the operation.
pub const Z_BUF_ERROR: c_int = -5;

/// The zlib library version is incompatible with the caller's expected
/// version.
pub const Z_VERSION_ERROR: c_int = -6;

// ---------------------------------------------------------------------------
// Flush mode values (zlib.h lines 172–178)
// ---------------------------------------------------------------------------

/// No flush — accumulate input and produce output when the buffer is full.
pub const Z_NO_FLUSH: c_int = 0;

/// Partial flush (deprecated; use [`Z_SYNC_FLUSH`] instead).
pub const Z_PARTIAL_FLUSH: c_int = 1;

/// Flush all pending output and align the output to a byte boundary.
pub const Z_SYNC_FLUSH: c_int = 2;

/// Flush all pending output and reset the compression state.
pub const Z_FULL_FLUSH: c_int = 3;

/// Signal that this is the last chunk of input data.
pub const Z_FINISH: c_int = 4;

/// Request enough output to complete a deflate block.
pub const Z_BLOCK: c_int = 5;

/// Request the Huffman trees (for debugging / advanced usage).
pub const Z_TREES: c_int = 6;

// ---------------------------------------------------------------------------
// Compression levels (zlib.h lines 194–197)
// ---------------------------------------------------------------------------

/// No compression — the input data is simply copied.
pub const Z_NO_COMPRESSION: c_int = 0;

/// Fastest compression (least compression ratio).
pub const Z_BEST_SPEED: c_int = 1;

/// Best compression ratio (slowest speed).
pub const Z_BEST_COMPRESSION: c_int = 9;

/// Default compromise between speed and compression (equivalent to level 6).
pub const Z_DEFAULT_COMPRESSION: c_int = -1;

// ---------------------------------------------------------------------------
// Compression strategies (zlib.h lines 200–204)
// ---------------------------------------------------------------------------

/// Optimized for filtered data (data produced by a filter or predictor).
pub const Z_FILTERED: c_int = 1;

/// Use Huffman coding only (no string matching).
pub const Z_HUFFMAN_ONLY: c_int = 2;

/// Use run-length encoding for the input.
pub const Z_RLE: c_int = 3;

/// Use fixed Huffman codes (prevents dynamic tree generation).
pub const Z_FIXED: c_int = 4;

/// Default strategy — let the library choose based on the data.
pub const Z_DEFAULT_STRATEGY: c_int = 0;

// ---------------------------------------------------------------------------
// Data type hints (zlib.h lines 207–210)
// ---------------------------------------------------------------------------

/// The data appears to be binary (non-text).
pub const Z_BINARY: c_int = 0;

/// The data appears to be text.
pub const Z_TEXT: c_int = 1;

/// Alias for [`Z_TEXT`] for compatibility with zlib 1.2.2 and earlier.
pub const Z_ASCII: c_int = Z_TEXT;

/// The data type is unknown.
pub const Z_UNKNOWN: c_int = 2;

// ---------------------------------------------------------------------------
// Miscellaneous constants (zlib.h lines 213–216)
// ---------------------------------------------------------------------------

/// The deflate compression method (the only method supported by zlib).
pub const Z_DEFLATED: c_int = 8;

/// Null value for initializing `zalloc`, `zfree`, `opaque`, and pointer
/// fields.
pub const Z_NULL: c_int = 0;

// ---------------------------------------------------------------------------
// Window and memory configuration (zconf.h lines 272–288)
// ---------------------------------------------------------------------------

/// Maximum value for `windowBits` in `deflateInit2` and `inflateInit2`.
///
/// A value of 15 corresponds to a 32 KiB LZ77 sliding window.
pub const MAX_WBITS: c_int = 15;

/// Maximum value for `memLevel` in `deflateInit2`.
pub const MAX_MEM_LEVEL: c_int = 9;

/// Default `memLevel` value for `deflateInit2`.
pub const DEF_MEM_LEVEL: c_int = 8;
