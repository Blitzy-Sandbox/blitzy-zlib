//! Rust-native streaming interface for zlib compression/decompression.
//!
//! This module provides the core streaming types that replace the C `z_stream`
//! and `gz_header` structures with safe Rust equivalents. All raw pointer
//! buffer management from the C API is replaced with owned [`Vec<u8>`] buffers
//! and safe index-based position tracking.
//!
//! # Types
//!
//! - [`ZStream`] — The main compression/decompression stream, replacing
//!   the C `z_stream_s` struct from `zlib.h` lines 90–110.
//! - [`GzHeader`] — Gzip header metadata, replacing the C `gz_header_s`
//!   struct from `zlib.h` lines 118–133.
//! - `Byte`, `UInt`, `ULong` — Type aliases mapping C zlib types to
//!   standard Rust integer types.
//!
//! # Safety
//!
//! This module contains zero `unsafe` code. All buffer access is bounds-checked
//! through safe Rust slice and vector operations.

use std::fmt;

use crate::constants;
use crate::error::ReturnCode;

// ─── Type Aliases ─────────────────────────────────────────────────────────────

/// Unsigned byte type, equivalent to C zlib's `Byte` (`unsigned char`).
///
/// Defined in `zconf.h` line 403 as `typedef unsigned char Byte`.
/// In Rust, this maps directly to [`u8`].
pub type Byte = u8;

/// Unsigned integer type, equivalent to C zlib's `uInt` (`unsigned int`).
///
/// Defined in `zconf.h` line 405 as `typedef unsigned int uInt`.
/// On most platforms this is a 32-bit unsigned integer. In Rust, this maps
/// to [`u32`].
pub type UInt = u32;

/// Unsigned long type, equivalent to C zlib's `uLong` (`unsigned long`).
///
/// Defined in `zconf.h` line 406 as `typedef unsigned long uLong`.
/// In Rust, we use [`u64`] for safety and large file support, ensuring
/// counters like `total_in` and `total_out` can handle streams larger than
/// 4 GiB without overflow.
pub type ULong = u64;

// ─── GzHeader ─────────────────────────────────────────────────────────────────

/// Gzip header metadata for reading and writing gzip-format streams.
///
/// Ports the C `gz_header_s` structure from `zlib.h` lines 118–133. See
/// [RFC 1952](https://datatracker.ietf.org/doc/html/rfc1952) for the
/// gzip format specification that defines the meaning of each field.
///
/// # C-to-Rust Transformations
///
/// The C structure uses separate pointer, length, and max-capacity fields for
/// variable-length header entries (`extra`, `name`, `comment`). In Rust, these
/// are combined into single [`Option<Vec<u8>>`] fields that own their data.
/// Boolean flags that were C `int` values are now Rust [`bool`].
///
/// # Examples
///
/// ```
/// use zlib_rs::stream::GzHeader;
///
/// let header = GzHeader::new();
/// assert!(!header.text);
/// assert_eq!(header.os, 255); // unknown OS
/// assert!(header.extra.is_none());
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GzHeader {
    /// `true` if the compressed data is believed to be text.
    ///
    /// When writing, this sets the FTEXT flag in the gzip header. When
    /// reading, this reflects the state of that flag in the parsed header.
    pub text: bool,

    /// Modification time of the original file as a Unix timestamp.
    ///
    /// Stored as seconds since the Unix epoch (1970-01-01 00:00:00 UTC).
    /// A value of zero means the modification time is not set.
    pub time: u32,

    /// Extra flags providing additional information about the compression.
    ///
    /// For gzip compression, this is set by the compressor (2 = maximum
    /// compression, 4 = fastest algorithm). Not used when writing.
    pub xflags: i32,

    /// Operating system identifier for the system that created the file.
    ///
    /// Values are defined in RFC 1952 §2.3.1: 0 = FAT/DOS, 3 = Unix,
    /// 7 = Macintosh, 11 = NTFS, 255 = unknown.
    pub os: i32,

    /// Extra field data, or `None` if no extra field is present.
    ///
    /// Replaces the C triple (`extra`, `extra_len`, `extra_max`) with a
    /// single owned vector. The length is implicit in the vector.
    pub extra: Option<Vec<u8>>,

    /// Original file name as raw bytes, or `None` if not present.
    ///
    /// In gzip format, the file name is zero-terminated ISO 8859-1 (LATIN-1).
    /// Replaces the C pair (`name`, `name_max`) with a single owned vector.
    pub name: Option<Vec<u8>>,

    /// Comment string as raw bytes, or `None` if not present.
    ///
    /// In gzip format, the comment is zero-terminated ISO 8859-1 (LATIN-1).
    /// Replaces the C pair (`comment`, `comm_max`) with a single owned vector.
    pub comment: Option<Vec<u8>>,

    /// `true` if a header CRC-16 was or will be present.
    ///
    /// When reading, indicates that the FHCRC flag is set and a CRC-16 of
    /// the header was found. When writing, requests that a CRC-16 be emitted.
    pub hcrc: bool,

    /// `true` when the gzip header has been completely read.
    ///
    /// Only meaningful during decompression. Set to `true` by the inflate
    /// engine once all header fields have been parsed.
    pub done: bool,
}

impl GzHeader {
    /// Creates a new [`GzHeader`] with default field values.
    ///
    /// All boolean flags default to `false`, the timestamp defaults to zero,
    /// the OS is set to 255 (unknown per RFC 1952), and all optional byte
    /// fields default to `None`.
    ///
    /// # Examples
    ///
    /// ```
    /// use zlib_rs::stream::GzHeader;
    ///
    /// let header = GzHeader::new();
    /// assert_eq!(header.time, 0);
    /// assert!(!header.hcrc);
    /// assert!(!header.done);
    /// ```
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl Default for GzHeader {
    fn default() -> Self {
        Self {
            text: false,
            time: 0,
            xflags: 0,
            os: 255, // RFC 1952: 0xFF = unknown operating system
            extra: None,
            name: None,
            comment: None,
            hcrc: false,
            done: false,
        }
    }
}

// ─── ZStream ──────────────────────────────────────────────────────────────────

/// Rust-native streaming interface for zlib compression and decompression.
///
/// Replaces the C `z_stream_s` structure from `zlib.h` lines 90–110 with
/// a safe Rust type that uses owned [`Vec<u8>`] buffers instead of raw
/// pointers, and index-based position tracking instead of pointer arithmetic.
///
/// # Buffer Management
///
/// The C `z_stream` uses raw pointer fields (`next_in`, `avail_in`, `next_out`,
/// `avail_out`) for I/O buffer management. In this Rust port:
///
/// - **Input** is stored in an internal [`Vec<u8>`] with an index tracking the
///   current read position. Call [`set_input`](ZStream::set_input) to provide
///   data and [`avail_in`](ZStream::avail_in) to query remaining bytes.
///
/// - **Output** is stored in an internal [`Vec<u8>`] pre-allocated to the desired
///   capacity, with an index tracking the current write position. Call
///   [`set_output_buffer`](ZStream::set_output_buffer) to configure the buffer
///   and [`avail_out`](ZStream::avail_out) to query remaining space.
///
/// # Running Counters
///
/// The [`total_in`](ZStream::total_in) and [`total_out`](ZStream::total_out)
/// fields track the cumulative bytes processed across multiple calls. These
/// use [`u64`] instead of the C `uLong` to support streams larger than 4 GiB.
///
/// # Internal State
///
/// The deflate and inflate engines attach opaque internal state to the stream
/// via a crate-internal field. This state is not accessible to library users.
///
/// # Examples
///
/// ```
/// use zlib_rs::stream::ZStream;
///
/// let mut stream = ZStream::new();
/// stream.set_input(b"Hello, world!");
/// assert_eq!(stream.avail_in(), 13);
///
/// stream.set_output_buffer(128);
/// assert_eq!(stream.avail_out(), 128);
/// ```
pub struct ZStream {
    // ── Input buffer management ──
    // Replaces C's `next_in` (raw pointer) + `avail_in` (remaining count).
    // Position tracking via index instead of pointer arithmetic.
    input: Vec<u8>,
    input_pos: usize,

    // ── Output buffer management ──
    // Replaces C's `next_out` (raw pointer) + `avail_out` (remaining space).
    // Position tracking via index instead of pointer arithmetic.
    output: Vec<u8>,
    output_pos: usize,

    /// Total number of input bytes read so far.
    ///
    /// Equivalent to C `z_stream.total_in`. Uses [`u64`] instead of `uLong`
    /// for large file support beyond 4 GiB.
    pub total_in: u64,

    /// Total number of output bytes produced so far.
    ///
    /// Equivalent to C `z_stream.total_out`. Uses [`u64`] instead of `uLong`
    /// for large file support beyond 4 GiB.
    pub total_out: u64,

    /// Adler-32 or CRC-32 checksum of the uncompressed data.
    ///
    /// For deflate operations, this holds the Adler-32 of the input data
    /// processed so far. For inflate, this holds the Adler-32 (zlib format)
    /// or CRC-32 (gzip format) of the output data. The value is updated
    /// automatically by the compression/decompression engine.
    pub adler: u32,

    /// Best guess about the data type: binary, text, or unknown.
    ///
    /// Set by the deflate engine to indicate whether the data appears to be
    /// binary ([`Z_BINARY`](crate::constants::Z_BINARY)) or text
    /// ([`Z_TEXT`](crate::constants::Z_TEXT)). Initialized to
    /// [`Z_UNKNOWN`](crate::constants::Z_UNKNOWN).
    pub data_type: i32,

    /// Last error message, or `None` if no error has occurred.
    ///
    /// Set by compression/decompression functions when an error is detected.
    /// The message strings are static and correspond to the C `z_errmsg[]`
    /// array from `zutil.c`.
    pub msg: Option<&'static str>,

    // ── Internal state (crate-visible only) ──
    // Replaces C's `struct internal_state FAR *state` opaque pointer.
    // The actual deflate or inflate engine state is stored here.
    pub(crate) state: Option<Box<dyn std::any::Any + Send>>,
}

impl ZStream {
    /// Creates a new [`ZStream`] with all fields initialized to their default
    /// values.
    ///
    /// All counters are zeroed, buffers are empty, the checksum is initialized
    /// to 1 (the Adler-32 identity value), the data type is set to
    /// [`Z_UNKNOWN`](crate::constants::Z_UNKNOWN), and no error message is
    /// present.
    ///
    /// # Examples
    ///
    /// ```
    /// use zlib_rs::stream::ZStream;
    ///
    /// let stream = ZStream::new();
    /// assert_eq!(stream.total_in, 0);
    /// assert_eq!(stream.total_out, 0);
    /// assert_eq!(stream.avail_in(), 0);
    /// assert_eq!(stream.avail_out(), 0);
    /// assert!(stream.msg.is_none());
    /// ```
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Resets the stream counters, buffers, and error state.
    ///
    /// After a reset:
    /// - `total_in` and `total_out` are zeroed
    /// - Input and output buffers are cleared
    /// - The checksum (`adler`) is reset to 1 (Adler-32 identity value)
    /// - The data type is reset to [`Z_UNKNOWN`](crate::constants::Z_UNKNOWN)
    /// - The error message is cleared
    /// - The internal compression/decompression state is dropped
    ///
    /// To reuse the stream after reset, call the appropriate initialization
    /// function (e.g., `deflate_init2` or `inflate_init2`).
    ///
    /// # Examples
    ///
    /// ```
    /// use zlib_rs::stream::ZStream;
    ///
    /// let mut stream = ZStream::new();
    /// stream.total_in = 1000;
    /// stream.total_out = 500;
    /// stream.msg = Some("previous error");
    /// stream.reset();
    /// assert_eq!(stream.total_in, 0);
    /// assert_eq!(stream.total_out, 0);
    /// assert!(stream.msg.is_none());
    /// ```
    pub fn reset(&mut self) {
        self.input.clear();
        self.input_pos = 0;
        self.output.clear();
        self.output_pos = 0;
        self.total_in = 0;
        self.total_out = 0;
        self.adler = 1; // Adler-32 identity value
        self.data_type = constants::Z_UNKNOWN;
        self.msg = None;
        self.state = None;
    }

    // ── Buffer Query Methods ──

    /// Returns the number of unprocessed bytes remaining in the input buffer.
    ///
    /// This is the Rust equivalent of the C `z_stream.avail_in` field.
    /// The value decreases as the compression or decompression engine
    /// consumes input data via [`advance_input`](ZStream::advance_input).
    ///
    /// # Examples
    ///
    /// ```
    /// use zlib_rs::stream::ZStream;
    ///
    /// let mut stream = ZStream::new();
    /// stream.set_input(b"Hello");
    /// assert_eq!(stream.avail_in(), 5);
    /// ```
    #[must_use]
    pub fn avail_in(&self) -> usize {
        self.input.len().saturating_sub(self.input_pos)
    }

    /// Returns the remaining writable space in the output buffer.
    ///
    /// This is the Rust equivalent of the C `z_stream.avail_out` field.
    /// The value decreases as the compression or decompression engine
    /// produces output data via [`advance_output`](ZStream::advance_output).
    ///
    /// # Examples
    ///
    /// ```
    /// use zlib_rs::stream::ZStream;
    ///
    /// let mut stream = ZStream::new();
    /// stream.set_output_buffer(64);
    /// assert_eq!(stream.avail_out(), 64);
    /// ```
    #[must_use]
    pub fn avail_out(&self) -> usize {
        self.output.len().saturating_sub(self.output_pos)
    }

    // ── Buffer Management Methods ──

    /// Sets the input data for the stream to process.
    ///
    /// Copies the provided byte slice into the stream's internal input buffer
    /// and resets the input position to the beginning. Any previously
    /// unprocessed input data is replaced.
    ///
    /// This replaces the C pattern of setting `next_in` and `avail_in` on
    /// a `z_stream`.
    ///
    /// # Examples
    ///
    /// ```
    /// use zlib_rs::stream::ZStream;
    ///
    /// let mut stream = ZStream::new();
    /// stream.set_input(b"data to compress");
    /// assert_eq!(stream.avail_in(), 16);
    /// assert_eq!(stream.input_remaining(), b"data to compress");
    /// ```
    pub fn set_input(&mut self, data: &[u8]) {
        self.input = data.to_vec();
        self.input_pos = 0;
    }

    /// Allocates an output buffer with the specified capacity.
    ///
    /// Creates a zero-initialized output buffer of exactly `capacity` bytes
    /// and resets the output write position. Any previously written output
    /// data is discarded.
    ///
    /// This replaces the C pattern of setting `next_out` and `avail_out` on
    /// a `z_stream`.
    ///
    /// # Examples
    ///
    /// ```
    /// use zlib_rs::stream::ZStream;
    ///
    /// let mut stream = ZStream::new();
    /// stream.set_output_buffer(4096);
    /// assert_eq!(stream.avail_out(), 4096);
    /// assert!(stream.output_written().is_empty());
    /// ```
    pub fn set_output_buffer(&mut self, capacity: usize) {
        self.output = vec![0u8; capacity];
        self.output_pos = 0;
    }

    /// Returns the unprocessed portion of the input as a byte slice.
    ///
    /// The returned slice starts at the current read position and extends
    /// to the end of the input buffer. This is the Rust-safe equivalent of
    /// reading from C's `next_in` pointer.
    ///
    /// # Examples
    ///
    /// ```
    /// use zlib_rs::stream::ZStream;
    ///
    /// let mut stream = ZStream::new();
    /// stream.set_input(b"Hello, world!");
    /// assert_eq!(stream.input_remaining(), b"Hello, world!");
    /// ```
    #[must_use]
    pub fn input_remaining(&self) -> &[u8] {
        &self.input[self.input_pos..]
    }

    /// Returns the output data that has been written so far.
    ///
    /// The returned slice contains all bytes written by the compression or
    /// decompression engine since the last call to
    /// [`set_output_buffer`](ZStream::set_output_buffer) or
    /// [`take_output`](ZStream::take_output).
    ///
    /// # Examples
    ///
    /// ```
    /// use zlib_rs::stream::ZStream;
    ///
    /// let stream = ZStream::new();
    /// assert!(stream.output_written().is_empty());
    /// ```
    #[must_use]
    pub fn output_written(&self) -> &[u8] {
        &self.output[..self.output_pos]
    }

    /// Returns the writable portion of the output buffer as a mutable slice.
    ///
    /// The returned slice starts at the current write position and extends
    /// to the end of the output buffer. The compression or decompression
    /// engine writes data into this region, then calls
    /// [`advance_output`](ZStream::advance_output) to update the position.
    #[must_use]
    pub fn output_remaining_mut(&mut self) -> &mut [u8] {
        &mut self.output[self.output_pos..]
    }

    /// Advances the input read position by `count` bytes.
    ///
    /// Updates the internal position tracker and increments `total_in`
    /// by the same amount. Returns [`ReturnCode::Ok`] on success.
    ///
    /// Returns [`ReturnCode::StreamError`] if `count` exceeds the available
    /// input bytes, in which case neither the position nor the total counter
    /// is modified.
    ///
    /// This replaces the C pattern of incrementing `next_in` and decrementing
    /// `avail_in` after processing input data.
    ///
    /// # Errors
    ///
    /// Returns [`ReturnCode::StreamError`] if `count` is greater than
    /// [`avail_in()`](ZStream::avail_in).
    ///
    /// # Examples
    ///
    /// ```
    /// use zlib_rs::stream::ZStream;
    /// use zlib_rs::error::ReturnCode;
    ///
    /// let mut stream = ZStream::new();
    /// stream.set_input(b"Hello");
    /// assert_eq!(stream.advance_input(3), ReturnCode::Ok);
    /// assert_eq!(stream.avail_in(), 2);
    /// assert_eq!(stream.input_remaining(), b"lo");
    /// assert_eq!(stream.total_in, 3);
    /// ```
    pub fn advance_input(&mut self, count: usize) -> ReturnCode {
        if count > self.avail_in() {
            self.msg = Some("input advance beyond available data");
            return ReturnCode::StreamError;
        }
        self.input_pos += count;
        self.total_in += count as u64;
        ReturnCode::Ok
    }

    /// Advances the output write position by `count` bytes.
    ///
    /// Updates the internal position tracker and increments `total_out`
    /// by the same amount. Returns [`ReturnCode::Ok`] on success.
    ///
    /// Returns [`ReturnCode::StreamError`] if `count` exceeds the available
    /// output space, in which case neither the position nor the total counter
    /// is modified.
    ///
    /// This replaces the C pattern of incrementing `next_out` and decrementing
    /// `avail_out` after producing output data.
    ///
    /// # Errors
    ///
    /// Returns [`ReturnCode::StreamError`] if `count` is greater than
    /// [`avail_out()`](ZStream::avail_out).
    ///
    /// # Examples
    ///
    /// ```
    /// use zlib_rs::stream::ZStream;
    /// use zlib_rs::error::ReturnCode;
    ///
    /// let mut stream = ZStream::new();
    /// stream.set_output_buffer(10);
    /// assert_eq!(stream.advance_output(5), ReturnCode::Ok);
    /// assert_eq!(stream.avail_out(), 5);
    /// assert_eq!(stream.total_out, 5);
    /// ```
    pub fn advance_output(&mut self, count: usize) -> ReturnCode {
        if count > self.avail_out() {
            self.msg = Some("output advance beyond available space");
            return ReturnCode::StreamError;
        }
        self.output_pos += count;
        self.total_out += count as u64;
        ReturnCode::Ok
    }

    /// Takes the output buffer, returning all written data as a [`Vec<u8>`].
    ///
    /// The internal output buffer is replaced with an empty vector and the
    /// write position is reset to zero. The returned vector is truncated to
    /// contain only the bytes that were actually written (from position 0 to
    /// the current write position).
    ///
    /// # Examples
    ///
    /// ```
    /// use zlib_rs::stream::ZStream;
    ///
    /// let mut stream = ZStream::new();
    /// stream.set_output_buffer(64);
    /// // After compression writes some data ...
    /// let output = stream.take_output();
    /// assert_eq!(stream.avail_out(), 0);
    /// ```
    pub fn take_output(&mut self) -> Vec<u8> {
        let mut output = std::mem::take(&mut self.output);
        output.truncate(self.output_pos);
        self.output_pos = 0;
        output
    }

    /// Returns `true` if the current data type is
    /// [`Z_BINARY`](crate::constants::Z_BINARY).
    ///
    /// This is a convenience method for checking the `data_type` field.
    ///
    /// # Examples
    ///
    /// ```
    /// use zlib_rs::stream::ZStream;
    /// use zlib_rs::constants;
    ///
    /// let mut stream = ZStream::new();
    /// assert!(!stream.is_binary_data()); // default is Z_UNKNOWN
    ///
    /// stream.data_type = constants::Z_BINARY;
    /// assert!(stream.is_binary_data());
    /// ```
    #[must_use]
    pub fn is_binary_data(&self) -> bool {
        self.data_type == constants::Z_BINARY
    }
}

impl Default for ZStream {
    fn default() -> Self {
        Self {
            input: Vec::new(),
            input_pos: 0,
            output: Vec::new(),
            output_pos: 0,
            total_in: 0,
            total_out: 0,
            adler: 1, // Adler-32 identity value
            data_type: constants::Z_UNKNOWN,
            msg: None,
            state: None,
        }
    }
}

impl fmt::Debug for ZStream {
    /// Formats the stream for debugging, showing public fields and buffer
    /// summary information while hiding opaque internal state details.
    ///
    /// Private buffer data and the internal compression/decompression state
    /// are represented in summary form: `avail_in`/`avail_out` show the
    /// buffer utilization, and `has_state` indicates whether an engine is
    /// attached. The `..` suffix signals intentionally omitted fields.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ZStream")
            .field("avail_in", &self.avail_in())
            .field("total_in", &self.total_in)
            .field("avail_out", &self.avail_out())
            .field("total_out", &self.total_out)
            .field("adler", &self.adler)
            .field("data_type", &self.data_type)
            .field("msg", &self.msg)
            .field("has_state", &self.state.is_some())
            .finish_non_exhaustive()
    }
}
