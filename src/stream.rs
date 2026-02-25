// Copyright (C) 1995-2026 Jean-loup Gailly and Mark Adler
// Rust translation: zlib-rs crate
// SPDX-License-Identifier: Zlib
//
// Central exchange structure for streaming compression/decompression.

//! Streaming compression/decompression exchange structure.
//!
//! This module provides [`ZStream`] — the central data structure through which
//! callers interact with the DEFLATE compression and decompression engines —
//! and [`StreamState`], the enum that owns the internal engine state.
//!
//! # C-to-Rust Translation
//!
//! `ZStream` is the Rust equivalent of C zlib's `z_stream` typedef (defined in
//! `zlib.h` lines 90–112). Key translation decisions:
//!
//! | C `z_stream` Field | Rust `ZStream` Equivalent | Notes |
//! |--------------------|---------------------------|-------|
//! | `next_in` / `avail_in` | `next_in: *const u8` + `avail_in: u32` | Safe wrappers via [`set_input`](ZStream::set_input) |
//! | `next_out` / `avail_out` | `next_out: *mut u8` + `avail_out: u32` | Safe wrappers via [`set_output`](ZStream::set_output) |
//! | `state` (opaque ptr) | `state: StreamState` (owned enum) | `Box`-owned `DeflateState` or `InflateState` |
//! | `zalloc`/`zfree`/`opaque` | *(removed)* | Rust's global allocator replaces C allocator hooks |
//! | `reserved` | *(removed)* | Not needed in Rust |
//!
//! # Ownership Model (AAP §0.7.3)
//!
//! ```text
//!   ┌─────────────────────────────────┐
//!   │ Caller owns ZStream             │
//!   │  ├── input: &[u8]  (borrowed)   │
//!   │  ├── output: &mut [u8] (mut borrow) │
//!   │  └── state: StreamState         │
//!   │       └── Box<DeflateState>     │  ← RAII: auto-freed on Drop
//!   │           ├── window: Vec       │
//!   │           ├── hash: Vec         │
//!   │           └── pending: Vec      │
//!   └─────────────────────────────────┘
//! ```
//!
//! # Examples
//!
//! ```
//! use zlib_rs::stream::ZStream;
//!
//! let mut strm = ZStream::new();
//!
//! // Set up input and output buffers
//! let input = b"Hello, world!";
//! let mut output = [0u8; 1024];
//!
//! strm.set_input(input);
//! strm.set_output(&mut output);
//!
//! assert_eq!(strm.input_remaining(), 13);
//! assert_eq!(strm.output_remaining(), 1024);
//! ```

// Suppress dead-code warnings: many items are consumed by sibling modules
// (deflate, inflate, gz, util) that are being created in parallel.
#![allow(dead_code)]

use core::fmt;
use core::ptr;

use crate::constants::{Z_BINARY, Z_TEXT, Z_UNKNOWN};
use crate::deflate::state::DeflateState;
use crate::error::{ReturnCode, ZlibError};
use crate::inflate::state::InflateState;

// ===========================================================================
// StreamState — internal compression/decompression state discriminant
// ===========================================================================

/// Internal state discriminant for a [`ZStream`].
///
/// When a `ZStream` is first constructed, its state is [`StreamState::None`].
/// After a successful `deflate_init*` call the state transitions to
/// [`StreamState::Deflate`]; after `inflate_init*`, to
/// [`StreamState::Inflate`]. The corresponding `*_end` call resets the state
/// back to `None`.
///
/// The `Box<…>` heap allocation mirrors C zlib's behaviour of allocating
/// `internal_state` via `zcalloc`. When the variant is replaced or the
/// `ZStream` is dropped, Rust's ownership model automatically deallocates
/// all nested `Vec` buffers inside the state structs — sliding window, hash
/// tables, Huffman trees, pending buffer, Huffman decode tables, etc.
pub(crate) enum StreamState {
    /// No compression or decompression state has been initialised.
    None,

    /// An active deflate (compression) session.
    ///
    /// The [`DeflateState`] holds the sliding window, hash tables, Huffman
    /// trees, and pending output buffer. Dropping this variant frees all
    /// owned memory (~256 KB at default settings).
    Deflate(Box<DeflateState>),

    /// An active inflate (decompression) session.
    ///
    /// The [`InflateState`] holds the sliding window, bit accumulator,
    /// Huffman decode tables, and bookkeeping fields. Dropping this variant
    /// frees all owned memory (~40 KB at default settings).
    Inflate(Box<InflateState>),
}

// ---------------------------------------------------------------------------
// StreamState — convenience accessors
// ---------------------------------------------------------------------------

impl StreamState {
    /// Returns `true` if no state has been initialised yet.
    #[inline]
    pub fn is_none(&self) -> bool {
        matches!(self, StreamState::None)
    }

    /// Returns `true` if this is an active deflate (compression) session.
    #[inline]
    pub fn is_deflate(&self) -> bool {
        matches!(self, StreamState::Deflate(_))
    }

    /// Returns `true` if this is an active inflate (decompression) session.
    #[inline]
    pub fn is_inflate(&self) -> bool {
        matches!(self, StreamState::Inflate(_))
    }

    /// Returns a shared reference to the inner [`DeflateState`], or `None`
    /// if the stream is not in deflate mode.
    #[inline]
    pub fn as_deflate(&self) -> Option<&DeflateState> {
        match self {
            StreamState::Deflate(s) => Some(s),
            _ => Option::None,
        }
    }

    /// Returns an exclusive reference to the inner [`DeflateState`], or
    /// `None` if the stream is not in deflate mode.
    #[inline]
    pub fn as_deflate_mut(&mut self) -> Option<&mut DeflateState> {
        match self {
            StreamState::Deflate(s) => Some(s),
            _ => Option::None,
        }
    }

    /// Returns a shared reference to the inner [`InflateState`], or `None`
    /// if the stream is not in inflate mode.
    #[inline]
    pub fn as_inflate(&self) -> Option<&InflateState> {
        match self {
            StreamState::Inflate(s) => Some(s),
            _ => Option::None,
        }
    }

    /// Returns an exclusive reference to the inner [`InflateState`], or
    /// `None` if the stream is not in inflate mode.
    #[inline]
    pub fn as_inflate_mut(&mut self) -> Option<&mut InflateState> {
        match self {
            StreamState::Inflate(s) => Some(s),
            _ => Option::None,
        }
    }
}

// ---------------------------------------------------------------------------
// StreamState — trait implementations
// ---------------------------------------------------------------------------

impl fmt::Debug for StreamState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StreamState::None => f.write_str("StreamState::None"),
            StreamState::Deflate(_) => f.write_str("StreamState::Deflate(...)"),
            StreamState::Inflate(_) => f.write_str("StreamState::Inflate(...)"),
        }
    }
}

impl fmt::Display for StreamState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StreamState::None => f.write_str("none"),
            StreamState::Deflate(_) => f.write_str("deflate"),
            StreamState::Inflate(_) => f.write_str("inflate"),
        }
    }
}

impl Default for StreamState {
    /// The default state is [`StreamState::None`] (no engine initialised).
    #[inline]
    fn default() -> Self {
        StreamState::None
    }
}

// ===========================================================================
// ZStream — central exchange structure
// ===========================================================================

/// Central exchange structure for streaming compression and decompression.
///
/// This is the Rust equivalent of C zlib's `z_stream` (defined in `zlib.h`
/// lines 90–112). The caller owns the `ZStream` and provides input/output
/// buffers per-call via [`set_input`](Self::set_input) and
/// [`set_output`](Self::set_output). Internal compression or decompression
/// state is heap-owned via [`StreamState`].
///
/// # Streaming Semantics
///
/// The streaming contract matches C zlib exactly:
///
/// 1. The caller provides input data via [`set_input`](Self::set_input).
/// 2. The caller provides output space via [`set_output`](Self::set_output).
/// 3. The caller invokes `deflate()` or `inflate()` one or more times.
/// 4. After each call, [`avail_in`](Self::avail_in) /
///    [`avail_out`](Self::avail_out) indicate how much input was consumed
///    and how much output space remains.
/// 5. The caller provides more input or consumes output as needed.
///
/// # RAII / Drop
///
/// When the `ZStream` is dropped, the internal [`StreamState`] is
/// automatically dropped, which in turn frees all heap-allocated buffers
/// (sliding window, hash tables, pending buffer, Huffman trees, etc.).
/// Explicitly calling `deflate_end` / `inflate_end` is not required for
/// memory safety, but allows inspecting the final checksum and reclaiming
/// memory earlier.
///
/// # Thread Safety
///
/// `ZStream` is **not** `Send` or `Sync` because it holds raw pointers to
/// caller-provided buffers. This mirrors C zlib's contract where a
/// `z_stream` must not be shared across threads.
///
/// # Examples
///
/// ```
/// use zlib_rs::stream::ZStream;
///
/// let mut strm = ZStream::new();
/// assert_eq!(strm.avail_in, 0);
/// assert_eq!(strm.avail_out, 0);
/// assert_eq!(strm.total_in, 0);
/// assert_eq!(strm.total_out, 0);
/// assert!(strm.msg.is_none());
/// ```
pub struct ZStream {
    // --- Input buffer management ---

    /// Raw pointer to the next unprocessed input byte.
    ///
    /// Updated by [`set_input`](Self::set_input) and advanced internally by
    /// the engine as input is consumed. Null when no input buffer has been
    /// provided.
    pub(crate) next_in: *const u8,

    /// Number of bytes available at [`next_in`](Self::next_in).
    ///
    /// Decremented as the engine consumes input. When this reaches zero the
    /// caller must provide more input via [`set_input`](Self::set_input)
    /// before calling `deflate()` / `inflate()` again.
    pub avail_in: u32,

    /// Total number of input bytes read so far across all calls.
    ///
    /// Maintained internally by the engine; useful for progress reporting
    /// and statistics. After compression, this holds the total size of the
    /// uncompressed data.
    pub total_in: u64,

    // --- Output buffer management ---

    /// Raw pointer to the next output position.
    ///
    /// Updated by [`set_output`](Self::set_output) and advanced internally
    /// as compressed/decompressed bytes are written. Null when no output
    /// buffer has been provided.
    pub(crate) next_out: *mut u8,

    /// Remaining free space at [`next_out`](Self::next_out) in bytes.
    ///
    /// Decremented as the engine produces output. When this reaches zero the
    /// caller must consume the output and provide more space via
    /// [`set_output`](Self::set_output).
    pub avail_out: u32,

    /// Total number of bytes written to output buffers so far.
    ///
    /// Cumulative across all calls. Useful for statistics and progress.
    pub total_out: u64,

    // --- Error / status ---

    /// Last error message, or `None` if no error has occurred.
    ///
    /// Set by the engine when a [`ZlibError`] is returned. Provides
    /// human-readable diagnostic detail beyond the error enum variant.
    /// Cleared on successful operations.
    pub msg: Option<String>,

    // --- Internal state ---

    /// Internal compression or decompression engine state.
    ///
    /// - [`StreamState::None`] before any `*_init` call.
    /// - [`StreamState::Deflate`] after `deflate_init*`.
    /// - [`StreamState::Inflate`] after `inflate_init*`.
    ///
    /// The `Box`-owned state structs contain all algorithm buffers and are
    /// automatically freed when the state is replaced or the `ZStream` is
    /// dropped.
    pub(crate) state: StreamState,

    // --- Data type and checksum ---

    /// Best guess about the data type being compressed.
    ///
    /// One of [`Z_BINARY`], [`Z_TEXT`], or [`Z_UNKNOWN`]. Updated by the
    /// deflate engine after analysing input data. For inflate, this field
    /// carries the decoding state information.
    pub data_type: i32,

    /// Running Adler-32 or CRC-32 checksum of the uncompressed data.
    ///
    /// - For zlib-wrapped streams: Adler-32.
    /// - For gzip-wrapped streams: CRC-32.
    /// - For raw DEFLATE: Adler-32 (updated but not embedded in the stream).
    ///
    /// Initialised to `1` (Adler-32 identity value) by [`new`](Self::new)
    /// and updated after each `deflate()` / `inflate()` call. At the end of
    /// a stream this contains the final checksum.
    pub adler: u64,
}

// ===========================================================================
// ZStream — construction and buffer management
// ===========================================================================

impl ZStream {
    /// Creates a new uninitialised `ZStream`.
    ///
    /// All counters are zeroed, buffer pointers are null, the error message
    /// is `None`, `data_type` is [`Z_UNKNOWN`], `adler` is `1` (the Adler-32
    /// identity value), and the internal state is [`StreamState::None`].
    ///
    /// Before using the stream the caller must invoke `deflate_init*` or
    /// `inflate_init*` to initialise the internal state, and provide buffers
    /// via [`set_input`](Self::set_input) / [`set_output`](Self::set_output).
    ///
    /// # Examples
    ///
    /// ```
    /// use zlib_rs::stream::ZStream;
    ///
    /// let strm = ZStream::new();
    /// assert_eq!(strm.total_in, 0);
    /// assert_eq!(strm.total_out, 0);
    /// assert_eq!(strm.adler, 1);
    /// ```
    #[inline]
    pub fn new() -> Self {
        Self {
            next_in: ptr::null(),
            avail_in: 0,
            total_in: 0,
            next_out: ptr::null_mut(),
            avail_out: 0,
            total_out: 0,
            msg: None,
            state: StreamState::None,
            data_type: Z_UNKNOWN,
            adler: 1, // Adler-32 identity value
        }
    }

    // -----------------------------------------------------------------------
    // Public buffer management methods
    // -----------------------------------------------------------------------

    /// Sets the input buffer for the next `deflate()` / `inflate()` call.
    ///
    /// Records the slice's pointer and length so the engine can read from it.
    /// The caller must ensure the slice remains valid and unmodified for the
    /// duration of the subsequent engine call.
    ///
    /// # Panics
    ///
    /// Panics if `input.len()` exceeds [`u32::MAX`].
    ///
    /// # Examples
    ///
    /// ```
    /// use zlib_rs::stream::ZStream;
    ///
    /// let mut strm = ZStream::new();
    /// strm.set_input(b"Hello, world!");
    /// assert_eq!(strm.input_remaining(), 13);
    /// assert_eq!(strm.avail_in, 13);
    /// ```
    #[inline]
    pub fn set_input(&mut self, input: &[u8]) {
        assert!(
            input.len() <= u32::MAX as usize,
            "input buffer length {} exceeds u32::MAX",
            input.len()
        );
        self.next_in = input.as_ptr();
        self.avail_in = input.len() as u32;
    }

    /// Sets the output buffer for the next `deflate()` / `inflate()` call.
    ///
    /// The engine writes compressed or decompressed bytes into this buffer.
    /// The caller must ensure the slice remains valid for the duration of the
    /// subsequent engine call.
    ///
    /// # Panics
    ///
    /// Panics if `output.len()` exceeds [`u32::MAX`].
    ///
    /// # Examples
    ///
    /// ```
    /// use zlib_rs::stream::ZStream;
    ///
    /// let mut strm = ZStream::new();
    /// let mut buf = [0u8; 1024];
    /// strm.set_output(&mut buf);
    /// assert_eq!(strm.output_remaining(), 1024);
    /// assert_eq!(strm.avail_out, 1024);
    /// ```
    #[inline]
    pub fn set_output(&mut self, output: &mut [u8]) {
        assert!(
            output.len() <= u32::MAX as usize,
            "output buffer length {} exceeds u32::MAX",
            output.len()
        );
        self.next_out = output.as_mut_ptr();
        self.avail_out = output.len() as u32;
    }

    /// Returns the number of unprocessed input bytes remaining.
    ///
    /// Equivalent to reading [`avail_in`](Self::avail_in) but returns
    /// `usize` for ergonomic use in Rust slice and iterator contexts.
    ///
    /// # Examples
    ///
    /// ```
    /// use zlib_rs::stream::ZStream;
    ///
    /// let mut strm = ZStream::new();
    /// strm.set_input(b"data");
    /// assert_eq!(strm.input_remaining(), 4);
    /// ```
    #[inline]
    pub fn input_remaining(&self) -> usize {
        self.avail_in as usize
    }

    /// Returns the remaining output buffer capacity in bytes.
    ///
    /// Equivalent to reading [`avail_out`](Self::avail_out) but returns
    /// `usize` for ergonomic Rust usage.
    ///
    /// # Examples
    ///
    /// ```
    /// use zlib_rs::stream::ZStream;
    ///
    /// let mut strm = ZStream::new();
    /// let mut buf = [0u8; 512];
    /// strm.set_output(&mut buf);
    /// assert_eq!(strm.output_remaining(), 512);
    /// ```
    #[inline]
    pub fn output_remaining(&self) -> usize {
        self.avail_out as usize
    }

    // -----------------------------------------------------------------------
    // Crate-internal helpers consumed by the deflate / inflate engines
    // -----------------------------------------------------------------------

    /// Reads bytes from the input buffer into `buf`.
    ///
    /// This is the Rust equivalent of C zlib's `read_buf()` function
    /// (defined in `deflate.c`). It copies up to `size` bytes (but no more
    /// than [`avail_in`](Self::avail_in) and no more than `buf.len()`) from
    /// the current input position into `buf`, then advances the input
    /// pointer and updates counters.
    ///
    /// **Note:** Unlike the C version, this method does **not** update the
    /// running checksum ([`adler`](Self::adler)). The calling engine is
    /// responsible for computing checksums after data is read, keeping the
    /// stream module decoupled from the checksum module.
    ///
    /// # Arguments
    ///
    /// * `buf` — Destination buffer to copy input bytes into.
    /// * `size` — Maximum number of bytes to read.
    ///
    /// # Returns
    ///
    /// The number of bytes actually read (may be less than `size` if input
    /// is exhausted, or zero if no input is available).
    pub(crate) fn read_buf(&mut self, buf: &mut [u8], size: usize) -> usize {
        // Determine how many bytes we can actually copy.
        let len = (self.avail_in as usize).min(size).min(buf.len());
        if len == 0 {
            return 0;
        }

        // SAFETY: `next_in` is guaranteed to be a valid pointer into a buffer
        // of at least `avail_in` bytes — this invariant is established by
        // `set_input` and maintained by the engine's own buffer management.
        // `buf` is a valid mutable slice. Source and destination do not
        // overlap because `next_in` points into a caller-owned input buffer
        // and `buf` is a separate engine-internal buffer.
        unsafe {
            ptr::copy_nonoverlapping(self.next_in, buf.as_mut_ptr(), len);
            self.next_in = self.next_in.add(len);
        }

        self.avail_in -= len as u32;
        self.total_in += len as u64;

        len
    }

    /// Returns pending output information from the deflate state.
    ///
    /// This is the Rust equivalent of C zlib's `deflatePending()` function.
    /// It queries the internal deflate state for:
    ///
    /// - The number of bytes waiting in the pending output buffer.
    /// - The number of valid bits in the bit accumulator (`bi_valid`).
    ///
    /// # Returns
    ///
    /// A tuple `(pending_bytes, pending_bits)`:
    /// - `pending_bytes` — Number of bytes in the pending output buffer.
    /// - `pending_bits` — Number of valid bits in the bit accumulator (0–16).
    ///
    /// If the stream is not in deflate mode (i.e. the state is
    /// [`StreamState::None`] or [`StreamState::Inflate`]), returns `(0, 0)`.
    pub(crate) fn pending(&self) -> (usize, i32) {
        match &self.state {
            StreamState::Deflate(ds) => (ds.pending, ds.bi_valid),
            _ => (0, 0),
        }
    }

    // -----------------------------------------------------------------------
    // Crate-internal state validation helpers
    // -----------------------------------------------------------------------

    /// Returns a shared reference to the deflate state, or
    /// [`ZlibError::StreamError`] if the stream is not in deflate mode.
    ///
    /// This is the Rust equivalent of C zlib's `deflateStateCheck()` — it
    /// validates that the stream has been properly initialised for
    /// compression before allowing operations to proceed.
    pub(crate) fn deflate_state(&self) -> Result<&DeflateState, ZlibError> {
        match &self.state {
            StreamState::Deflate(ds) => Ok(ds),
            _ => Err(ZlibError::StreamError),
        }
    }

    /// Returns an exclusive reference to the deflate state, or
    /// [`ZlibError::StreamError`] if the stream is not in deflate mode.
    pub(crate) fn deflate_state_mut(&mut self) -> Result<&mut DeflateState, ZlibError> {
        match &mut self.state {
            StreamState::Deflate(ds) => Ok(ds),
            _ => Err(ZlibError::StreamError),
        }
    }

    /// Returns a shared reference to the inflate state, or
    /// [`ZlibError::StreamError`] if the stream is not in inflate mode.
    ///
    /// This is the Rust equivalent of C zlib's `inflateStateCheck()`.
    pub(crate) fn inflate_state(&self) -> Result<&InflateState, ZlibError> {
        match &self.state {
            StreamState::Inflate(is) => Ok(is),
            _ => Err(ZlibError::StreamError),
        }
    }

    /// Returns an exclusive reference to the inflate state, or
    /// [`ZlibError::StreamError`] if the stream is not in inflate mode.
    pub(crate) fn inflate_state_mut(&mut self) -> Result<&mut InflateState, ZlibError> {
        match &mut self.state {
            StreamState::Inflate(is) => Ok(is),
            _ => Err(ZlibError::StreamError),
        }
    }

    // -----------------------------------------------------------------------
    // Data type query helpers (use Z_BINARY, Z_TEXT from constants)
    // -----------------------------------------------------------------------

    /// Returns `true` if the engine's best guess is that the data is binary.
    ///
    /// Checks whether [`data_type`](Self::data_type) equals [`Z_BINARY`].
    #[inline]
    pub fn is_binary(&self) -> bool {
        self.data_type == Z_BINARY
    }

    /// Returns `true` if the engine's best guess is that the data is text.
    ///
    /// Checks whether [`data_type`](Self::data_type) equals [`Z_TEXT`].
    #[inline]
    pub fn is_text(&self) -> bool {
        self.data_type == Z_TEXT
    }

    /// Returns a human-readable name for the current [`data_type`](Self::data_type).
    ///
    /// | Value | Name |
    /// |-------|------|
    /// | [`Z_BINARY`] | `"binary"` |
    /// | [`Z_TEXT`] | `"text"` |
    /// | [`Z_UNKNOWN`] | `"unknown"` |
    /// | other | `"unknown"` |
    #[inline]
    pub fn data_type_name(&self) -> &'static str {
        match self.data_type {
            Z_BINARY => "binary",
            Z_TEXT => "text",
            _ => "unknown",
        }
    }

    // -----------------------------------------------------------------------
    // Crate-internal error management helpers
    // -----------------------------------------------------------------------

    /// Sets the stream's error message.
    ///
    /// Called by the deflate/inflate engines when returning an error
    /// condition. If `detail` is `None`, the default message for the
    /// [`ZlibError`] variant is used.
    pub(crate) fn set_error_msg(&mut self, err: ZlibError, detail: Option<&str>) {
        self.msg = Some(
            detail
                .map(|s| s.to_string())
                .unwrap_or_else(|| err.to_string()),
        );
    }

    /// Clears the stream's error message.
    ///
    /// Called when an operation returns [`ReturnCode::Ok`] to reset any
    /// previous error indication.
    #[inline]
    pub(crate) fn clear_error(&mut self) {
        self.msg = None;
    }

    /// Validates that the stream has an active compression or decompression
    /// state.
    ///
    /// Returns [`ReturnCode::Ok`] if the state is initialised (either
    /// [`StreamState::Deflate`] or [`StreamState::Inflate`]), or
    /// [`ZlibError::StreamError`] if the state is [`StreamState::None`].
    ///
    /// This is a convenience check used by the public API functions
    /// (`deflate`, `inflate`, etc.) before accessing internal state.
    pub(crate) fn validate_state(&self) -> Result<ReturnCode, ZlibError> {
        if self.state.is_none() {
            Err(ZlibError::StreamError)
        } else {
            Ok(ReturnCode::Ok)
        }
    }

    /// Resets the stream's counters and buffer pointers to their initial
    /// state, preserving the current internal engine state.
    ///
    /// This is used internally by `deflateReset` / `inflateReset` to
    /// prepare the stream for a new data sequence while keeping the existing
    /// engine state (which handles its own reset logic separately).
    pub(crate) fn reset_counters(&mut self) {
        self.total_in = 0;
        self.total_out = 0;
        self.msg = None;
        self.adler = 1; // Adler-32 identity
        self.next_in = ptr::null();
        self.avail_in = 0;
        self.next_out = ptr::null_mut();
        self.avail_out = 0;
    }
}

// ===========================================================================
// ZStream — trait implementations
// ===========================================================================

impl Default for ZStream {
    /// Returns [`ZStream::new()`].
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for ZStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ZStream")
            .field("avail_in", &self.avail_in)
            .field("total_in", &self.total_in)
            .field("avail_out", &self.avail_out)
            .field("total_out", &self.total_out)
            .field("msg", &self.msg)
            .field("state", &self.state)
            .field("data_type", &self.data_type_name())
            .field("adler", &format_args!("0x{:08x}", self.adler))
            .finish()
    }
}

impl fmt::Display for ZStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "ZStream(state={}, in={}/{}, out={}/{}, adler=0x{:08x})",
            self.state,
            self.avail_in,
            self.total_in,
            self.avail_out,
            self.total_out,
            self.adler,
        )
    }
}

// ===========================================================================
// Unit tests (crate-internal: can access pub(crate) items)
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::{Z_BINARY, Z_TEXT, Z_UNKNOWN};
    use crate::error::{ReturnCode, ZlibError};

    // --- StreamState ---

    #[test]
    fn stream_state_default_is_none() {
        let state = StreamState::default();
        assert!(state.is_none());
        assert!(!state.is_deflate());
        assert!(!state.is_inflate());
    }

    #[test]
    fn stream_state_accessors_none() {
        let state = StreamState::None;
        assert!(state.as_deflate().is_none());
        assert!(state.as_inflate().is_none());
    }

    #[test]
    fn stream_state_debug_display() {
        let state = StreamState::None;
        assert_eq!(format!("{state:?}"), "StreamState::None");
        assert_eq!(format!("{state}"), "none");
    }

    // --- ZStream construction ---

    #[test]
    fn new_has_correct_defaults() {
        let strm = ZStream::new();
        assert_eq!(strm.avail_in, 0);
        assert_eq!(strm.avail_out, 0);
        assert_eq!(strm.total_in, 0);
        assert_eq!(strm.total_out, 0);
        assert!(strm.msg.is_none());
        assert_eq!(strm.data_type, Z_UNKNOWN);
        assert_eq!(strm.adler, 1);
        assert!(strm.state.is_none());
        assert!(strm.next_in.is_null());
        assert!(strm.next_out.is_null());
    }

    // --- Buffer management ---

    #[test]
    fn set_input_and_query() {
        let mut strm = ZStream::new();
        strm.set_input(b"Hello");
        assert_eq!(strm.avail_in, 5);
        assert_eq!(strm.input_remaining(), 5);
    }

    #[test]
    fn set_output_and_query() {
        let mut strm = ZStream::new();
        let mut buf = [0u8; 256];
        strm.set_output(&mut buf);
        assert_eq!(strm.avail_out, 256);
        assert_eq!(strm.output_remaining(), 256);
    }

    // --- read_buf ---

    #[test]
    fn read_buf_basic() {
        let mut strm = ZStream::new();
        strm.set_input(b"Hello, world!");

        let mut dest = [0u8; 5];
        let n = strm.read_buf(&mut dest, 5);
        assert_eq!(n, 5);
        assert_eq!(&dest, b"Hello");
        assert_eq!(strm.avail_in, 8);
        assert_eq!(strm.total_in, 5);
    }

    #[test]
    fn read_buf_partial_avail() {
        let mut strm = ZStream::new();
        strm.set_input(b"Hi");

        let mut dest = [0u8; 10];
        let n = strm.read_buf(&mut dest, 10);
        assert_eq!(n, 2);
        assert_eq!(&dest[..2], b"Hi");
        assert_eq!(strm.avail_in, 0);
        assert_eq!(strm.total_in, 2);
    }

    #[test]
    fn read_buf_empty_input() {
        let mut strm = ZStream::new();
        let mut dest = [0u8; 10];
        let n = strm.read_buf(&mut dest, 10);
        assert_eq!(n, 0);
        assert_eq!(strm.total_in, 0);
    }

    #[test]
    fn read_buf_successive_reads() {
        let mut strm = ZStream::new();
        strm.set_input(b"ABCDEF");

        let mut d1 = [0u8; 3];
        assert_eq!(strm.read_buf(&mut d1, 3), 3);
        assert_eq!(&d1, b"ABC");
        assert_eq!(strm.total_in, 3);
        assert_eq!(strm.avail_in, 3);

        let mut d2 = [0u8; 3];
        assert_eq!(strm.read_buf(&mut d2, 3), 3);
        assert_eq!(&d2, b"DEF");
        assert_eq!(strm.total_in, 6);
        assert_eq!(strm.avail_in, 0);
    }

    #[test]
    fn read_buf_size_limits_copy() {
        let mut strm = ZStream::new();
        strm.set_input(b"ABCDEF");

        let mut dest = [0u8; 10];
        let n = strm.read_buf(&mut dest, 2);
        assert_eq!(n, 2);
        assert_eq!(&dest[..2], b"AB");
        assert_eq!(strm.avail_in, 4);
    }

    // --- pending ---

    #[test]
    fn pending_returns_zero_for_none_state() {
        let strm = ZStream::new();
        let (bytes, bits) = strm.pending();
        assert_eq!(bytes, 0);
        assert_eq!(bits, 0);
    }

    // --- validate_state ---

    #[test]
    fn validate_state_none_returns_error() {
        let strm = ZStream::new();
        let result = strm.validate_state();
        assert_eq!(result, Err(ZlibError::StreamError));
    }

    // --- deflate_state / inflate_state ---

    #[test]
    fn deflate_state_returns_error_when_none() {
        let strm = ZStream::new();
        assert!(strm.deflate_state().is_err());
    }

    #[test]
    fn inflate_state_returns_error_when_none() {
        let strm = ZStream::new();
        assert!(strm.inflate_state().is_err());
    }

    #[test]
    fn deflate_state_mut_returns_error_when_none() {
        let mut strm = ZStream::new();
        assert!(strm.deflate_state_mut().is_err());
    }

    #[test]
    fn inflate_state_mut_returns_error_when_none() {
        let mut strm = ZStream::new();
        assert!(strm.inflate_state_mut().is_err());
    }

    // --- Error management ---

    #[test]
    fn set_error_msg_with_detail() {
        let mut strm = ZStream::new();
        strm.set_error_msg(ZlibError::DataError, Some("custom detail"));
        assert_eq!(strm.msg, Some("custom detail".to_string()));
    }

    #[test]
    fn set_error_msg_without_detail_uses_default() {
        let mut strm = ZStream::new();
        strm.set_error_msg(ZlibError::DataError, None);
        assert_eq!(strm.msg, Some("data error".to_string()));
    }

    #[test]
    fn clear_error_clears_msg() {
        let mut strm = ZStream::new();
        strm.msg = Some("error".to_string());
        strm.clear_error();
        assert!(strm.msg.is_none());
    }

    // --- reset_counters ---

    #[test]
    fn reset_counters_resets_all() {
        let mut strm = ZStream::new();
        strm.set_input(b"data");
        strm.total_in = 100;
        strm.total_out = 200;
        strm.msg = Some("error".to_string());
        strm.adler = 12345;

        strm.reset_counters();
        assert_eq!(strm.total_in, 0);
        assert_eq!(strm.total_out, 0);
        assert!(strm.msg.is_none());
        assert_eq!(strm.adler, 1);
        assert_eq!(strm.avail_in, 0);
        assert_eq!(strm.avail_out, 0);
    }

    // --- Data type helpers ---

    #[test]
    fn data_type_helpers() {
        let mut strm = ZStream::new();

        assert_eq!(strm.data_type_name(), "unknown");
        assert!(!strm.is_binary());
        assert!(!strm.is_text());

        strm.data_type = Z_BINARY;
        assert!(strm.is_binary());
        assert_eq!(strm.data_type_name(), "binary");

        strm.data_type = Z_TEXT;
        assert!(strm.is_text());
        assert_eq!(strm.data_type_name(), "text");
    }

    // --- Debug / Display ---

    #[test]
    fn debug_format() {
        let strm = ZStream::new();
        let debug = format!("{strm:?}");
        assert!(debug.contains("ZStream"));
        assert!(debug.contains("avail_in: 0"));
    }

    #[test]
    fn display_format() {
        let strm = ZStream::new();
        let display = format!("{strm}");
        assert!(display.contains("ZStream"));
        assert!(display.contains("state=none"));
        assert!(display.contains("0x00000001"));
    }

    // --- Ensure ReturnCode is used in validate_state ---

    #[test]
    fn validate_state_return_type_check() {
        let strm = ZStream::new();
        let result: Result<ReturnCode, ZlibError> = strm.validate_state();
        assert!(result.is_err());
    }
}
