//! Gzip write pipeline with buffered compression.
//!
//! This module provides [`GzWriter`], a buffered gzip writer that compresses
//! data on the fly using the DEFLATE algorithm with gzip framing (RFC 1952).
//! It implements the standard `Write` trait for seamless integration with
//! Rust's I/O ecosystem, and additionally supports `fmt::Write` for
//! formatted output via `write!()` macros.
//!
//! Ported from C `gzwrite.c` (700 lines) in the zlib 1.3.2.1-motley library.
//!
//! # Architecture
//!
//! The write pipeline uses a two-buffer design matching the original C
//! implementation:
//!
//! - **Input staging buffer** (`state.input`): Double-sized (`want * 2`)
//!   buffer that accumulates small writes before compression. The double
//!   sizing supports the `printf()` formatting path.
//! - **Output buffer** (managed via `ZStream`): Holds compressed data
//!   produced by the deflate engine before flushing to the output file.
//!
//! # Direct (Transparent) Mode
//!
//! When opened with the `'T'` mode flag, the writer bypasses compression
//! entirely, writing data directly to the file. This is useful for
//! scenarios where gzip framing is not desired.

use std::fmt;
use std::fs::File;
use std::io::{self, Write};

use crate::constants::{
    DEF_MEM_LEVEL, MAX_WBITS, Z_BLOCK, Z_BUF_ERROR, Z_DATA_ERROR, Z_DEFAULT_COMPRESSION,
    Z_DEFAULT_STRATEGY, Z_DEFLATED, Z_ERRNO, Z_FILTERED, Z_FINISH, Z_FIXED, Z_HUFFMAN_ONLY,
    Z_MEM_ERROR, Z_NO_FLUSH, Z_OK, Z_RLE, Z_STREAM_ERROR, Z_SYNC_FLUSH,
};
use crate::deflate;
use crate::error::ReturnCode;

use super::state::GzState;

// ─── Constants ───────────────────────────────────────────────────────────────

/// Maximum chunk size for individual file writes.
///
/// Derived from the C expression `((unsigned)-1 >> 2) + 1`, which limits
/// individual `write()` system calls to approximately 1 GiB to prevent
/// platform-specific overflow issues with large write buffers.
const MAX_WRITE_CHUNK: usize = (u32::MAX as usize >> 2) + 1;

// ─── GzWriter ────────────────────────────────────────────────────────────────

/// A buffered gzip writer that compresses data on the fly.
///
/// `GzWriter` wraps a file handle and an embedded deflate stream to produce
/// gzip-formatted compressed output. It implements the [`Write`] trait for
/// seamless integration with Rust's I/O infrastructure, and [`fmt::Write`]
/// for formatted output via `write!()` macros.
///
/// # Construction
///
/// - [`open`](GzWriter::open) — Create from a file path with default settings
/// - [`open_with_mode`](GzWriter::open_with_mode) — Create with a mode string
/// - [`from_file`](GzWriter::from_file) — Create from an existing [`File`] handle
///
/// # Compression Control
///
/// The buffer size can be configured before the first write via
/// [`set_buffer_size`](GzWriter::set_buffer_size). Compression level and
/// strategy can be changed mid-stream via [`set_params`](GzWriter::set_params).
///
/// # Resource Cleanup
///
/// Call [`close`](GzWriter::close) to finalize the gzip stream and return
/// any I/O errors. The [`Drop`] implementation provides best-effort cleanup
/// if `close` is not called explicitly, but cannot report errors.
///
/// # Examples
///
/// ```no_run
/// use zlib_rs::gz::write::GzWriter;
/// use std::io::Write;
///
/// let mut writer = GzWriter::open("/tmp/test.gz").unwrap();
/// writer.write_all(b"Hello, world!").unwrap();
/// writer.close().unwrap();
/// ```
pub struct GzWriter {
    /// The shared gzip state (file handle, buffers, stream).
    state: GzState,

    /// Number of valid bytes in the `state.input` staging buffer.
    ///
    /// Data is accumulated at `state.input[0..input_len]` for small writes.
    /// When the buffer is full, it is compressed and written to the file.
    input_len: usize,

    /// Whether [`close`](GzWriter::close) has been called.
    ///
    /// Prevents double-finalization in the [`Drop`] implementation.
    closed: bool,
}

// ─── Construction ────────────────────────────────────────────────────────────

impl GzWriter {
    /// Opens a gzip file for writing with default compression settings.
    ///
    /// Creates or truncates the file at `path` and initializes the writer
    /// with [`Z_DEFAULT_COMPRESSION`] level and [`Z_DEFAULT_STRATEGY`].
    ///
    /// Equivalent to C `gzopen(path, "w")`.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the file cannot be created.
    pub fn open(path: &str) -> io::Result<Self> {
        Self::open_with_mode(path, "w")
    }

    /// Opens a gzip file for writing with a mode string.
    ///
    /// The mode string controls write/append semantics, compression level,
    /// compression strategy, and transparent mode. This mirrors the mode
    /// string parsing from C `gzlib.c` lines 108–171.
    ///
    /// # Mode Characters
    ///
    /// | Character | Effect |
    /// |-----------|--------|
    /// | `'w'` | Write mode (create/truncate) |
    /// | `'a'` | Append mode (create, seek to end) |
    /// | `'0'`–`'9'` | Compression level (0 = none, 9 = best) |
    /// | `'f'` | Filtered strategy ([`Z_FILTERED`]) |
    /// | `'h'` | Huffman-only strategy ([`Z_HUFFMAN_ONLY`]) |
    /// | `'R'` | RLE strategy ([`Z_RLE`]) |
    /// | `'F'` | Fixed strategy ([`Z_FIXED`]) |
    /// | `'T'` | Transparent (direct, no compression) |
    /// | `'b'` | Binary mode (ignored on POSIX) |
    /// | `'+'` | Error (simultaneous read+write not supported) |
    ///
    /// # Errors
    ///
    /// Returns an error if the mode string is invalid (e.g., contains `'+'`
    /// or lacks `'w'`/`'a'`), or if the file cannot be opened.
    pub fn open_with_mode(path: &str, mode: &str) -> io::Result<Self> {
        let mut write_mode = false;
        let mut append_mode = false;
        let mut level = Z_DEFAULT_COMPRESSION;
        let mut strategy = Z_DEFAULT_STRATEGY;
        let mut direct = false;

        for ch in mode.chars() {
            match ch {
                '0'..='9' => {
                    #[allow(clippy::cast_possible_truncation)]
                    {
                        level = i32::from(ch as u8 - b'0');
                    }
                }
                'w' => write_mode = true,
                'a' => {
                    append_mode = true;
                    write_mode = true;
                }
                'f' => strategy = Z_FILTERED,
                'h' => strategy = Z_HUFFMAN_ONLY,
                'R' => strategy = Z_RLE,
                'F' => strategy = Z_FIXED,
                'T' => direct = true,
                '+' => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "cannot open gzip file for both reading and writing",
                    ));
                }
                // 'b' = binary mode, all others: ignored (matches C behavior)
                _ => {}
            }
        }

        if !write_mode {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "mode must include 'w' or 'a' for GzWriter",
            ));
        }

        if append_mode {
            let file = File::options()
                .write(true)
                .create(true)
                .truncate(false)
                .open(path)?;
            let state = GzState::new_appender(file, path.to_string(), level, strategy)?;
            Ok(Self {
                state,
                input_len: 0,
                closed: false,
            })
        } else {
            let file = File::create(path)?;
            let state = GzState::new_writer(file, path.to_string(), level, strategy, direct);
            Ok(Self {
                state,
                input_len: 0,
                closed: false,
            })
        }
    }

    /// Creates a `GzWriter` from an existing [`File`] handle.
    ///
    /// The file should already be opened for writing. Uses default
    /// compression level and strategy with gzip compression enabled.
    ///
    /// Equivalent to C `gzdopen()` for write mode.
    ///
    /// # Errors
    ///
    /// This constructor is infallible for the initial setup, but returns
    /// `io::Result` for API consistency with the other constructors.
    pub fn from_file(file: File) -> io::Result<Self> {
        let state = GzState::new_writer(
            file,
            "<fd>".to_string(),
            Z_DEFAULT_COMPRESSION,
            Z_DEFAULT_STRATEGY,
            false,
        );
        Ok(Self {
            state,
            input_len: 0,
            closed: false,
        })
    }

    /// Sets the internal buffer size for I/O operations.
    ///
    /// Must be called before the first write operation. Once buffers are
    /// allocated (after the first write), the size cannot be changed.
    ///
    /// The input buffer is allocated at double the requested size (to
    /// support the `printf()` formatting path), so `size` must not exceed
    /// half of `usize::MAX`.
    ///
    /// Ported from C `gzbuffer()` in `gzlib.c` lines 322–343.
    ///
    /// # Errors
    ///
    /// Returns [`ReturnCode::StreamError`] if buffers are already allocated
    /// or `size` would cause an overflow when doubled.
    pub fn set_buffer_size(&mut self, size: usize) -> Result<(), ReturnCode> {
        // Cannot change size after initialization
        if self.state.is_initialized() {
            return Err(ReturnCode::StreamError);
        }

        // The input buffer is double-sized; check for overflow
        if size > usize::MAX / 2 {
            return Err(ReturnCode::StreamError);
        }

        // Enforce minimum size of 2
        self.state.want = if size < 2 { 2 } else { size };
        Ok(())
    }
}

// ─── Internal Initialization ─────────────────────────────────────────────────

impl GzWriter {
    /// Initialize state for writing a gzip file.
    ///
    /// Allocates the input staging buffer (double-sized for `printf`), the
    /// output buffer, and initializes the deflate engine with gzip headers.
    /// Marks initialization by setting `state.size` to nonzero.
    ///
    /// Ported from C `gz_init()` in `gzwrite.c` lines 11–57.
    fn init(&mut self) -> io::Result<()> {
        let want = self.state.want;

        // Allocate input buffer (double-sized for gzprintf equivalent).
        // gzwrite.c line 16: state->in = malloc(state->want << 1)
        self.state.input = vec![0u8; want * 2];

        // Only need output buffer and deflate state if compressing
        if self.state.direct == 0 {
            // Allocate output buffer
            self.state.output = vec![0u8; want];

            // Initialize deflate for gzip compression.
            // MAX_WBITS + 16 enables gzip header generation.
            let ret = deflate::deflate_init2(
                &mut self.state.strm,
                self.state.level,
                Z_DEFLATED,
                MAX_WBITS + 16,
                DEF_MEM_LEVEL,
                self.state.strategy,
            );
            if ret != ReturnCode::Ok {
                self.state.input.clear();
                self.state.output.clear();
                self.set_error(Z_MEM_ERROR, "out of memory");
                return Err(io::Error::new(io::ErrorKind::OutOfMemory, "out of memory"));
            }

            // Set up the ZStream output buffer for compressed data
            self.state.strm.set_output_buffer(want);
            self.state.x.next = 0;
        }

        // Mark state as initialized
        self.state.size = want;
        self.input_len = 0;

        Ok(())
    }
}

// ─── Internal Compression ────────────────────────────────────────────────────

impl GzWriter {
    /// Write bytes to the underlying file, handling partial writes and
    /// non-blocking I/O (EAGAIN/EWOULDBLOCK).
    ///
    /// Sets `state.again` on `WouldBlock` errors so the caller can
    /// distinguish transient stalls from permanent failures.
    fn write_bytes_to_file(&mut self, data: &[u8]) -> io::Result<()> {
        let mut offset = 0;
        while offset < data.len() {
            let chunk = (data.len() - offset).min(MAX_WRITE_CHUNK);
            match self.state.file.write(&data[offset..offset + chunk]) {
                Ok(0) => {
                    // write() returned 0 — treat as an I/O error
                    self.state.again = false;
                    self.set_error(Z_ERRNO, "write returned zero bytes");
                    return Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "write returned zero bytes",
                    ));
                }
                Ok(n) => {
                    self.state.again = false;
                    offset += n;
                }
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                    self.state.again = true;
                    self.set_error(Z_ERRNO, "would block");
                    return Err(io::Error::new(io::ErrorKind::WouldBlock, "would block"));
                }
                Err(e) => {
                    self.state.again = false;
                    let msg = e.to_string();
                    self.set_error(Z_ERRNO, &msg);
                    return Err(e);
                }
            }
        }
        Ok(())
    }

    /// Flush any unflushed compressed output from the `ZStream` to the file.
    ///
    /// Writes `output_written()[x.next..]` to the file and updates `x.next`.
    fn flush_output_to_file(&mut self) -> io::Result<()> {
        let written_len = self.state.strm.output_written().len();
        if written_len > self.state.x.next {
            // Copy the unflushed portion to avoid borrow conflict
            let data = self.state.strm.output_written()[self.state.x.next..].to_vec();
            self.write_bytes_to_file(&data)?;
            self.state.x.next = written_len;
        }
        Ok(())
    }

    /// Compress whatever input is currently set on the `ZStream` and write
    /// the compressed output to the file.
    ///
    /// The caller must set the `ZStream` input (via `strm.set_input()`) before
    /// calling this method. `flush` must be a valid deflate flush value.
    ///
    /// If `flush` is [`Z_FINISH`], the deflate state is marked for reset
    /// before the next gzip member.
    ///
    /// Ported from C `gz_comp()` in `gzwrite.c` lines 65–148.
    fn comp(&mut self, flush: i32) -> io::Result<()> {
        // Allocate memory if this is the first time through
        if self.state.size == 0 {
            self.init()?;
        }

        // ── Direct mode: write input directly to file ──
        // gzwrite.c lines 75–91
        if self.state.direct != 0 {
            while self.state.strm.avail_in() > 0 {
                let remaining = self.state.strm.input_remaining().to_vec();
                let chunk = remaining.len().min(MAX_WRITE_CHUNK);
                match self.state.file.write(&remaining[..chunk]) {
                    Ok(0) => {
                        self.state.again = false;
                        self.set_error(Z_ERRNO, "write returned zero bytes");
                        return Err(io::Error::new(
                            io::ErrorKind::WriteZero,
                            "write returned zero bytes",
                        ));
                    }
                    Ok(n) => {
                        self.state.again = false;
                        let _ = self.state.strm.advance_input(n);
                    }
                    Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                        self.state.again = true;
                        self.set_error(Z_ERRNO, "would block");
                        return Err(io::Error::new(io::ErrorKind::WouldBlock, "would block"));
                    }
                    Err(e) => {
                        self.state.again = false;
                        let msg = e.to_string();
                        self.set_error(Z_ERRNO, &msg);
                        return Err(e);
                    }
                }
            }
            return Ok(());
        }

        // ── Check for a pending reset ──
        // gzwrite.c lines 93–101
        if self.state.reset {
            if self.state.strm.avail_in() == 0 && flush == Z_NO_FLUSH {
                return Ok(());
            }
            let _ = deflate::deflate_reset(&mut self.state.strm);
            self.state.reset = false;
        }

        // ── Compression loop ──
        // gzwrite.c lines 103–140
        let mut ret = ReturnCode::Ok;
        loop {
            // Write out current buffer contents if full, or if flushing.
            // For Z_FINISH, don't write until we get Z_STREAM_END.
            let should_flush_output = self.state.strm.avail_out() == 0
                || (flush != Z_NO_FLUSH && (flush != Z_FINISH || ret == ReturnCode::StreamEnd));

            if should_flush_output {
                // Write unflushed compressed data to file
                self.flush_output_to_file()?;

                // If output buffer was full, reset it
                if self.state.strm.avail_out() == 0 {
                    self.state.strm.set_output_buffer(self.state.size);
                    self.state.x.next = 0;
                }
            }

            // Compress
            let old_avail = self.state.strm.avail_out();
            ret = deflate::deflate(&mut self.state.strm, flush);
            if ret == ReturnCode::StreamError {
                self.set_error(Z_STREAM_ERROR, "internal error: deflate stream corrupt");
                return Err(io::Error::new(
                    io::ErrorKind::Other,
                    "internal error: deflate stream corrupt",
                ));
            }
            let have = old_avail - self.state.strm.avail_out();
            if have == 0 {
                break;
            }
        }

        // If that completed a deflate stream, allow another to start
        // gzwrite.c lines 143–144
        if flush == Z_FINISH {
            self.state.reset = true;
        }

        Ok(())
    }

    /// Compress `state.skip` (> 0) zeros to the output.
    ///
    /// Flushes any buffered input first, then writes `skip` zero bytes in
    /// chunks of `state.size`. Updates `state.x.pos` and `state.skip` as
    /// zeros are successfully written. Handles non-blocking stalls by
    /// preserving the remaining skip count.
    ///
    /// Ported from C `gz_zero()` in `gzwrite.c` lines 154–182.
    fn zero(&mut self) -> io::Result<()> {
        // Consume whatever is left in the input staging buffer
        if self.input_len > 0 {
            self.state
                .strm
                .set_input(&self.state.input[..self.input_len]);
            self.input_len = 0;
            self.comp(Z_NO_FLUSH)?;
            // Sync any unconsumed input back
            let remaining = self.state.strm.avail_in();
            if remaining > 0 {
                let rem_data = self.state.strm.input_remaining().to_vec();
                self.state.input[..remaining].copy_from_slice(&rem_data);
                self.input_len = remaining;
            }
        }

        // Compress state.skip zeros
        let mut first = true;
        while self.state.skip > 0 {
            // Determine chunk size: min(size, skip).
            // `skip` is always > 0 here (while guard) and bounded by i64::MAX.
            // Clamp to usize::MAX then take the smaller of size and clamped skip.
            #[allow(clippy::cast_sign_loss)]
            let skip_clamped = usize::try_from(self.state.skip).unwrap_or(usize::MAX);
            let n: usize = self.state.size.min(skip_clamped);

            if first {
                // Zero-fill the input buffer (only first iteration needed,
                // subsequent iterations reuse the already-zeroed buffer)
                for byte in &mut self.state.input[..n] {
                    *byte = 0;
                }
                first = false;
            }

            self.state.strm.set_input(&self.state.input[..n]);
            let comp_result = self.comp(Z_NO_FLUSH);

            // Track how much was consumed regardless of error
            let consumed = n - self.state.strm.avail_in();
            #[allow(clippy::cast_possible_wrap)]
            {
                self.state.x.pos += consumed as i64;
                self.state.skip -= consumed as i64;
            }

            comp_result?;
        }

        Ok(())
    }

    /// Core write implementation: write `buf` to the gzip file.
    ///
    /// Accumulates small writes in the input staging buffer, compressing
    /// when the buffer is full. Large writes (>= `state.size`) bypass the
    /// staging buffer and feed data directly to the deflate engine.
    ///
    /// Returns the number of bytes consumed from `buf`. A return value less
    /// than `buf.len()` indicates a non-blocking stall (EAGAIN/EWOULDBLOCK).
    ///
    /// Ported from C `gz_write()` in `gzwrite.c` lines 188–252.
    fn gz_write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // If len is zero, avoid unnecessary operations
        if buf.is_empty() {
            return Ok(0);
        }

        // Allocate memory if this is the first time through
        if self.state.size == 0 {
            self.init()?;
        }

        // Check for seek request
        if self.state.skip > 0 {
            self.zero()?;
        }

        let total = buf.len();
        let mut remaining = buf;

        if buf.len() < self.state.size {
            // ── Small write: copy to input buffer, compress when full ──
            // gzwrite.c lines 205–227
            while !remaining.is_empty() {
                let available = self.state.size - self.input_len;
                let copy = available.min(remaining.len());

                self.state.input[self.input_len..self.input_len + copy]
                    .copy_from_slice(&remaining[..copy]);
                self.input_len += copy;
                #[allow(clippy::cast_possible_wrap)]
                {
                    self.state.x.pos += copy as i64;
                }
                remaining = &remaining[copy..];

                if remaining.is_empty() {
                    break;
                }

                // Buffer is full — compress it
                self.state
                    .strm
                    .set_input(&self.state.input[..self.input_len]);
                self.input_len = 0;
                if let Err(e) = self.comp(Z_NO_FLUSH) {
                    // Sync remaining input back
                    let rem = self.state.strm.avail_in();
                    if rem > 0 {
                        let rem_data = self.state.strm.input_remaining().to_vec();
                        self.state.input[..rem].copy_from_slice(&rem_data);
                        self.input_len = rem;
                    }
                    if self.state.again {
                        return Ok(total - remaining.len());
                    }
                    return Err(e);
                }
                // Sync remaining input back from ZStream
                let rem = self.state.strm.avail_in();
                if rem > 0 {
                    let rem_data = self.state.strm.input_remaining().to_vec();
                    self.state.input[..rem].copy_from_slice(&rem_data);
                }
                self.input_len = rem;
            }
        } else {
            // ── Large write: compress user buffer directly ──
            // gzwrite.c lines 228–248

            // First, flush any remaining data in the staging buffer
            if self.input_len > 0 {
                self.state
                    .strm
                    .set_input(&self.state.input[..self.input_len]);
                self.input_len = 0;
                self.comp(Z_NO_FLUSH)?;
            }

            // Directly compress user buffer in chunks
            while !remaining.is_empty() {
                // Use u32::MAX as chunk limit to match C behavior
                let n = remaining.len().min(u32::MAX as usize);

                self.state.strm.set_input(&remaining[..n]);
                if let Err(e) = self.comp(Z_NO_FLUSH) {
                    let consumed = n - self.state.strm.avail_in();
                    #[allow(clippy::cast_possible_wrap)]
                    {
                        self.state.x.pos += consumed as i64;
                    }
                    remaining = &remaining[consumed..];
                    if self.state.again {
                        return Ok(total - remaining.len());
                    }
                    return Err(e);
                }

                let consumed = n - self.state.strm.avail_in();
                #[allow(clippy::cast_possible_wrap)]
                {
                    self.state.x.pos += consumed as i64;
                }
                remaining = &remaining[consumed..];
            }
        }

        // Input was all buffered or compressed
        Ok(total)
    }

    /// If the second half of the input buffer is occupied, flush it.
    ///
    /// Moves any remaining input to the front of the buffer after flushing.
    /// Returns `true` if the second half is *still* occupied after the flush
    /// attempt (due to a non-blocking stall).
    ///
    /// Ported from C `gz_vacate()` in `gzwrite.c` lines 382–397.
    fn vacate(&mut self) -> io::Result<bool> {
        // If input doesn't extend past the first half, nothing to do
        if self.input_len <= self.state.size {
            return Ok(false);
        }

        // Flush the input buffer via compression
        self.state
            .strm
            .set_input(&self.state.input[..self.input_len]);
        self.input_len = 0;
        let comp_result = self.comp(Z_NO_FLUSH);

        // Sync remaining input back to the front of the buffer
        let rem = self.state.strm.avail_in();
        if rem > 0 {
            let rem_data = self.state.strm.input_remaining().to_vec();
            self.state.input[..rem].copy_from_slice(&rem_data);
        }
        self.input_len = rem;

        // If comp failed but we had a WouldBlock, the remaining might
        // still extend past the first half
        if let Err(e) = comp_result {
            self.state.err = Z_ERRNO;
            return Err(e);
        }

        Ok(self.input_len > self.state.size)
    }

    /// Record an error code and message in the state.
    ///
    /// Formats the message as `"{path}: {msg}"` to match C `gz_error()`.
    fn set_error(&mut self, code: i32, msg: &str) {
        self.state.err = code;
        self.state.msg = Some(format!("{}: {}", self.state.path, msg));
    }

    /// Create an [`io::Error`] from the current state error.
    fn make_io_error(&self) -> io::Error {
        let kind = match self.state.err {
            Z_STREAM_ERROR => io::ErrorKind::InvalidInput,
            Z_MEM_ERROR => io::ErrorKind::OutOfMemory,
            Z_DATA_ERROR => io::ErrorKind::InvalidData,
            // Z_ERRNO, Z_BUF_ERROR, and all others map to generic Other
            _ => io::ErrorKind::Other,
        };
        io::Error::new(
            kind,
            self.state
                .msg
                .clone()
                .unwrap_or_else(|| "unknown gz error".to_string()),
        )
    }
}

// ─── Public Write API ────────────────────────────────────────────────────────

impl GzWriter {
    /// Write a single byte to the gzip file.
    ///
    /// Optimized fast path: if the staging buffer is initialized and has
    /// room, the byte is inserted directly without calling the full write
    /// pipeline.
    ///
    /// Ported from C `gzputc()` in `gzwrite.c` lines 307–347.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the byte cannot be written.
    pub fn putc(&mut self, c: u8) -> io::Result<()> {
        // Clear any stale error
        self.state.err = Z_OK;
        self.state.msg = None;

        // Check for seek request
        if self.state.skip > 0 {
            self.zero()?;
        }

        // Fast path: if buffer initialized and has room, insert directly
        if self.state.size != 0 && self.input_len < self.state.size {
            self.state.input[self.input_len] = c;
            self.input_len += 1;
            self.state.x.pos += 1;
            return Ok(());
        }

        // Slow path: use the full write pipeline
        let buf = [c];
        let written = self.gz_write(&buf)?;
        if written == 0 {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "gzputc: failed to write byte",
            ));
        }
        Ok(())
    }

    /// Write a string to the gzip file.
    ///
    /// Writes the UTF-8 byte representation of `s` (without a trailing
    /// null terminator, unlike C `gzputs`).
    ///
    /// Ported from C `gzputs()` in `gzwrite.c` lines 350–372.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the string cannot be written.
    pub fn puts(&mut self, s: &str) -> io::Result<usize> {
        // Clear any stale error
        self.state.err = Z_OK;
        self.state.msg = None;

        let bytes = s.as_bytes();
        if bytes.is_empty() {
            return Ok(0);
        }

        let written = self.gz_write(bytes)?;
        if !bytes.is_empty() && written == 0 {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "gzputs: failed to write string",
            ));
        }
        Ok(written)
    }

    /// Write a pre-formatted string to the gzip file.
    ///
    /// In C, `gzprintf()` accepts a format string and variadic arguments.
    /// In Rust, formatting is done with [`format!()`] or `write!()` macros
    /// before calling this method, so the C variadic pattern is unnecessary.
    ///
    /// The double-sized input buffer (allocated in `init()`)
    /// supports the vacate logic that ensures buffer space is available for
    /// formatted output, matching the C `gzvprintf` / `gz_vacate` behavior.
    ///
    /// Ported from C `gzprintf()`/`gzvprintf()` in `gzwrite.c` lines
    /// 399–495 and `gz_vacate()` lines 382–397.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the formatted string cannot be written.
    pub fn printf(&mut self, formatted: &str) -> io::Result<usize> {
        // Clear any stale error
        self.state.err = Z_OK;
        self.state.msg = None;

        let bytes = formatted.as_bytes();
        if bytes.is_empty() {
            return Ok(0);
        }

        // Ensure initialized
        if self.state.size == 0 {
            self.init()?;
        }

        // Handle pending skip
        if self.state.skip > 0 {
            self.zero()?;
        }

        // Vacate the second half of the buffer if occupied
        // (ported from gz_vacate, gzwrite.c lines 382–397)
        let stalled = self.vacate()?;
        if self.state.err != Z_OK {
            if stalled && self.state.again {
                self.set_error(Z_BUF_ERROR, "stalled write on printf");
            }
            if !self.state.again {
                return Err(self.make_io_error());
            }
        }

        // Truncate to available buffer space (second half of input buffer)
        let available = self.state.size.saturating_sub(1);
        let len = bytes.len().min(available);
        if len == 0 {
            return Ok(0);
        }

        // Write formatted data into the input staging buffer
        self.state.input[self.input_len..self.input_len + len].copy_from_slice(&bytes[..len]);
        self.input_len += len;
        #[allow(clippy::cast_possible_wrap)]
        {
            self.state.x.pos += len as i64;
        }

        // Flush if more than half the buffer is occupied
        let _ = self.vacate()?;

        Ok(len)
    }

    /// Flush the gzip write pipeline with the specified flush mode.
    ///
    /// Valid flush values are [`Z_NO_FLUSH`] through [`Z_FINISH`]:
    /// - [`Z_SYNC_FLUSH`]: Flush pending output and insert a sync point
    /// - `Z_FULL_FLUSH`: Like sync flush, but also reset compression state
    /// - [`Z_FINISH`]: Complete the gzip stream (required before close)
    /// - [`Z_BLOCK`]: Flush to the next block boundary
    ///
    /// Ported from C `gzflush()` in `gzwrite.c` lines 603–627.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if flushing fails, or if the flush parameter
    /// is invalid.
    pub fn gz_flush(&mut self, flush: i32) -> io::Result<i32> {
        // Clear any stale error
        self.state.err = Z_OK;
        self.state.msg = None;

        // Validate flush parameter
        if !(0..=Z_FINISH).contains(&flush) {
            self.set_error(Z_STREAM_ERROR, "invalid flush parameter");
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid flush parameter",
            ));
        }

        // Handle pending skip
        if self.state.skip > 0 {
            if let Err(_e) = self.zero() {
                return Ok(self.state.err);
            }
        }

        // Flush remaining staging buffer input
        if self.input_len > 0 {
            self.state
                .strm
                .set_input(&self.state.input[..self.input_len]);
            self.input_len = 0;
        } else {
            self.state.strm.set_input(&[]);
        }

        // Compress remaining data with requested flush
        let _ = self.comp(flush);

        // Sync any remaining unconsumed input back to staging buffer
        let rem = self.state.strm.avail_in();
        if rem > 0 {
            let rem_data = self.state.strm.input_remaining().to_vec();
            self.state.input[..rem].copy_from_slice(&rem_data);
            self.input_len = rem;
        }

        Ok(self.state.err)
    }

    /// Change the compression level and/or strategy for subsequent writes.
    ///
    /// If neither the level nor strategy has changed, this is a no-op.
    /// Otherwise, any buffered input is flushed with [`Z_BLOCK`] before
    /// the parameters are updated via `deflate_params`.
    ///
    /// Ported from C `gzsetparams()` in `gzwrite.c` lines 630–664.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if flushing fails or if the deflate state
    /// cannot accept the new parameters.
    pub fn set_params(&mut self, level: i32, strategy: i32) -> io::Result<()> {
        // Cannot change params in direct mode
        if self.state.direct != 0 {
            self.set_error(Z_STREAM_ERROR, "cannot set params in direct mode");
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "cannot set params in direct mode",
            ));
        }

        // Clear error
        self.state.err = Z_OK;
        self.state.msg = None;

        // If no change is requested, do nothing
        if level == self.state.level && strategy == self.state.strategy {
            return Ok(());
        }

        // Handle pending skip
        if self.state.skip > 0 {
            self.zero()?;
        }

        // Flush previous input with previous parameters before changing
        if self.state.size != 0 && self.input_len > 0 {
            self.state
                .strm
                .set_input(&self.state.input[..self.input_len]);
            self.input_len = 0;
            self.comp(Z_BLOCK)?;
            // Sync remaining
            let rem = self.state.strm.avail_in();
            if rem > 0 {
                let rem_data = self.state.strm.input_remaining().to_vec();
                self.state.input[..rem].copy_from_slice(&rem_data);
                self.input_len = rem;
            }
        }

        // Update deflate parameters
        if self.state.size != 0 {
            let _ = deflate::deflate_params(&mut self.state.strm, level, strategy);
        }

        self.state.level = level;
        self.state.strategy = strategy;
        Ok(())
    }

    /// Close the gzip writer, finalizing the gzip stream.
    ///
    /// This method consumes the writer, flushing all pending data,
    /// completing the gzip stream with a [`Z_FINISH`] flush, and
    /// cleaning up the deflate state. The underlying file handle is
    /// closed automatically when the `GzState` is dropped.
    ///
    /// Prefer calling `close()` explicitly over relying on [`Drop`],
    /// since `Drop` cannot report I/O errors.
    ///
    /// Ported from C `gzclose_w()` in `gzwrite.c` lines 667–700.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if flushing or writing fails. Even on error,
    /// all resources are cleaned up.
    pub fn close(mut self) -> io::Result<()> {
        self.close_inner()
    }

    /// Internal close implementation shared by [`close`] and [`Drop`].
    fn close_inner(&mut self) -> io::Result<()> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;

        let mut result: io::Result<()> = Ok(());

        // Handle pending skip
        if self.state.skip > 0 {
            if let Err(e) = self.zero() {
                result = Err(e);
            }
        }

        // Flush remaining input and finalize the gzip stream
        if self.input_len > 0 {
            self.state
                .strm
                .set_input(&self.state.input[..self.input_len]);
            self.input_len = 0;
        } else {
            self.state.strm.set_input(&[]);
        }

        if let Err(e) = self.comp(Z_FINISH) {
            if result.is_ok() {
                result = Err(e);
            }
        }

        // Flush any remaining output after Z_FINISH
        if let Err(e) = self.flush_output_to_file() {
            if result.is_ok() {
                result = Err(e);
            }
        }

        // Clean up deflate state
        if self.state.size != 0 && self.state.direct == 0 {
            let _ = deflate::deflate_end(&mut self.state.strm);
        }

        // Clear error state (matches C: gz_error(state, Z_OK, NULL))
        self.state.err = Z_OK;
        self.state.msg = None;

        result
    }
}

// ─── FFI-Support Methods (seek, tell, offset, error, clearerr) ──────────────

impl GzWriter {
    /// Seeks to a position in the uncompressed output stream.
    ///
    /// For write-mode gzip files, only forward seeks from the current
    /// position (`SeekFrom::Current`) or absolute forward seeks
    /// (`SeekFrom::Start`) are supported. Forward seeking is implemented
    /// by writing zero bytes to advance the position.
    ///
    /// Port of `gz_seek()` write-mode path from `gzlib.c`.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the seek target is negative or backward.
    pub fn seek(&mut self, offset: i64, whence: io::SeekFrom) -> io::Result<i64> {
        use crate::constants::Z_BUF_ERROR;

        // Check for serious error
        if self.state.err != Z_OK && self.state.err != Z_BUF_ERROR {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "stream in error state",
            ));
        }

        // Compute the absolute target position.
        let target = match whence {
            io::SeekFrom::Start(_) => offset,
            io::SeekFrom::Current(_) => {
                let current = self.state.x.pos;
                current.checked_add(offset).ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "seek position overflow")
                })?
            }
            io::SeekFrom::End(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "SEEK_END not supported for gzip streams",
                ));
            }
        };

        if target < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "negative seek position",
            ));
        }

        // For write mode, we can only seek forward. Compute how many
        // zero bytes need to be written to reach the target.
        let current = self.state.x.pos;
        if target < current {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "backward seek not supported for write mode",
            ));
        }

        let n = target - current;
        self.state.skip = n;
        Ok(target)
    }

    /// Returns the current position in the uncompressed output stream.
    ///
    /// Port of `gztell64()` from `gzlib.c`.
    #[must_use]
    pub fn tell(&self) -> i64 {
        self.state.x.pos
    }

    /// Returns the current offset in the compressed file, adjusted for
    /// buffered data.
    ///
    /// Port of `gzoffset64()` from `gzlib.c`.
    ///
    /// Returns the byte offset, or -1 on I/O error.
    pub fn offset(&mut self) -> i64 {
        super::gz_offset(&mut self.state).unwrap_or(-1)
    }

    /// Returns the current error code for the last operation.
    ///
    /// The return value is one of the `Z_*` constants.
    #[must_use]
    pub fn error_code(&self) -> i32 {
        self.state.err
    }

    /// Clears the error state for this writer.
    ///
    /// Resets the error code to `Z_OK` and clears the error message.
    ///
    /// Port of `gzclearerr()` from `gzlib.c`.
    pub fn clearerr(&mut self) {
        super::gz_clearerr(&mut self.state);
    }
}

// ─── Write Trait Implementation ──────────────────────────────────────────────

impl Write for GzWriter {
    /// Write a buffer into the gzip file, returning how many bytes were
    /// consumed.
    ///
    /// This is the primary I/O integration point. Bytes are accumulated
    /// in the internal staging buffer and compressed when the buffer is
    /// full or on explicit flush.
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.gz_write(buf)
    }

    /// Flush the gzip write pipeline with [`Z_SYNC_FLUSH`].
    ///
    /// Inserts a sync point in the compressed output, ensuring all
    /// pending data is flushed to the underlying file.
    fn flush(&mut self) -> io::Result<()> {
        self.gz_flush(Z_SYNC_FLUSH).map(|_| ())
    }
}

// ─── fmt::Write Implementation ──────────────────────────────────────────────

impl fmt::Write for GzWriter {
    /// Write a string slice into the gzip file.
    ///
    /// Enables use of the `write!()` and `writeln!()` macros for
    /// formatted output to the gzip stream. This is the Rust equivalent
    /// of C's `gzprintf()` variadic formatting.
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.gz_write(s.as_bytes())
            .map(|_| ())
            .map_err(|_| fmt::Error)
    }
}

// ─── Drop Implementation ────────────────────────────────────────────────────

impl Drop for GzWriter {
    /// Best-effort cleanup: finalize the gzip stream and release resources.
    ///
    /// Since [`Drop`] cannot return errors, any I/O failures during
    /// finalization are silently ignored. For reliable error handling,
    /// call [`close`](GzWriter::close) explicitly before the writer is
    /// dropped.
    fn drop(&mut self) {
        if !self.closed {
            let _ = self.close_inner();
        }
    }
}
