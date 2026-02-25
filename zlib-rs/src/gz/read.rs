//! Gzip read pipeline with LOOK/COPY/GZIP auto-detection.
//!
//! Implements the [`GzReader`] type that provides buffered decompression
//! of gzip-format files, with automatic detection of gzip vs transparent
//! (uncompressed) input. This is a direct port of `gzread.c` (668 lines)
//! from zlib 1.3.2.1-motley.
//!
//! The read pipeline operates in three modes, controlled by [`GzHow`]:
//!
//! - **Look** — Examining input to determine if it's gzip-compressed
//! - **Copy** — Passing through uncompressed data transparently
//! - **Gzip** — Decompressing gzip-format data via the inflate engine
//!
//! [`GzReader`] implements the [`Read`](std::io::Read) trait for seamless
//! integration with Rust's I/O ecosystem.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};

use crate::constants::{
    MAX_WBITS, Z_BUF_ERROR, Z_DATA_ERROR, Z_ERRNO, Z_MEM_ERROR, Z_NO_FLUSH, Z_OK,
    Z_STREAM_ERROR,
};
use crate::error::ReturnCode;
use crate::inflate;
use crate::stream::ZStream;

use super::state::{GzHow, GzMode, GzState};

// ─── GzReader ────────────────────────────────────────────────────────────────

/// A buffered gzip reader that decompresses data on the fly.
///
/// Supports auto-detection of gzip vs raw (transparent) input.
/// Implements the [`Read`] trait for seamless I/O composition.
///
/// The reader uses a double-sized output buffer to support the
/// [`ungetc`](GzReader::ungetc) operation, which can push bytes
/// back into the buffer.
///
/// # Examples
///
/// ```no_run
/// use zlib_rs::gz::read::GzReader;
/// use std::io::Read;
///
/// let mut reader = GzReader::open("data.gz").unwrap();
/// let mut buf = Vec::new();
/// reader.read_to_end(&mut buf).unwrap();
/// ```
pub struct GzReader {
    /// The shared gzip state (file handle, buffers, stream).
    state: GzState,
    /// Current position within `state.input` where unprocessed data starts.
    in_pos: usize,
    /// Number of unprocessed bytes available at `in_pos` in the input buffer.
    in_avail: usize,
}

// ─── Constructors ────────────────────────────────────────────────────────────

impl GzReader {
    /// Opens a gzip file at the given path for reading.
    ///
    /// Creates a new [`GzReader`] that will automatically detect whether
    /// the input is gzip-compressed or transparent. The file handle is
    /// owned and will be closed when the reader is dropped.
    ///
    /// This is the equivalent of calling `gzopen(path, "rb")` in C zlib.
    ///
    /// # Arguments
    ///
    /// * `path` — File system path to the gzip file to read.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the file cannot be opened.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use zlib_rs::gz::read::GzReader;
    ///
    /// let reader = GzReader::open("data.gz").unwrap();
    /// ```
    pub fn open(path: &str) -> io::Result<Self> {
        let file = File::open(path)?;
        let mut state = GzState::new_reader(file, path.to_string());
        state.record_start_position()?;
        Ok(Self {
            state,
            in_pos: 0,
            in_avail: 0,
        })
    }

    /// Creates a [`GzReader`] from an existing file handle.
    ///
    /// Takes ownership of the provided [`File`]. The file should be
    /// positioned at the beginning of the gzip data (or at the start
    /// of uncompressed data for transparent reading).
    ///
    /// This is the equivalent of calling `gzdopen(fd, "rb")` in C zlib.
    ///
    /// # Arguments
    ///
    /// * `file` — Owned file handle to read from.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the file position cannot be queried.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use std::fs::File;
    /// use zlib_rs::gz::read::GzReader;
    ///
    /// let file = File::open("data.gz").unwrap();
    /// let reader = GzReader::from_file(file).unwrap();
    /// ```
    pub fn from_file(file: File) -> io::Result<Self> {
        let mut state = GzState::new_reader(file, "<fd>".to_string());
        state.record_start_position()?;
        Ok(Self {
            state,
            in_pos: 0,
            in_avail: 0,
        })
    }

    /// Sets the internal buffer size for subsequent operations.
    ///
    /// Must be called before any read operation. Once buffers are allocated
    /// (after the first read), the size cannot be changed. The minimum
    /// buffer size is 2 bytes (though 8 or more is recommended).
    ///
    /// Port of `gzbuffer()` from `gzlib.c` lines 322–343.
    ///
    /// # Arguments
    ///
    /// * `size` — Desired buffer size in bytes.
    ///
    /// # Errors
    ///
    /// Returns [`ReturnCode::StreamError`] if buffers are already allocated,
    /// the size is less than 2, or doubling the size would overflow.
    pub fn set_buffer_size(&mut self, size: usize) -> Result<(), ReturnCode> {
        // Cannot change size after buffers are allocated
        if self.state.size != 0 {
            return Err(ReturnCode::StreamError);
        }
        // Minimum 2 bytes
        if size < 2 {
            return Err(ReturnCode::StreamError);
        }
        // Ensure size * 2 doesn't overflow (double-size for output buffer)
        if size.checked_mul(2).is_none() {
            return Err(ReturnCode::StreamError);
        }
        self.state.want = size;
        Ok(())
    }
}

// ─── Error Helpers ───────────────────────────────────────────────────────────

impl GzReader {
    /// Sets the error state with an error code and message.
    ///
    /// Formats the message as `"{path}: {msg}"` for non-memory errors,
    /// matching the C `gz_error()` behavior from `gzlib.c`.
    fn set_error(&mut self, err: i32, msg: &str) {
        if err == Z_OK {
            self.state.err = Z_OK;
            self.state.msg = None;
            return;
        }
        self.state.err = err;
        if err != Z_MEM_ERROR {
            self.state.msg = Some(format!("{}: {}", self.state.path, msg));
        } else {
            self.state.msg = Some(msg.to_string());
        }
    }

    /// Creates an [`io::Error`] from the current error state.
    fn make_io_error(&self) -> io::Error {
        let kind = match self.state.err {
            Z_ERRNO => io::ErrorKind::Other,
            Z_DATA_ERROR | Z_STREAM_ERROR => io::ErrorKind::InvalidData,
            Z_MEM_ERROR => io::ErrorKind::OutOfMemory,
            Z_BUF_ERROR => io::ErrorKind::UnexpectedEof,
            _ => io::ErrorKind::Other,
        };
        match &self.state.msg {
            Some(msg) => io::Error::new(kind, msg.clone()),
            None => io::Error::new(kind, "unknown gz error"),
        }
    }

    /// Clears the current error state, resetting to [`Z_OK`].
    ///
    /// Port of the `gz_error(state, Z_OK, NULL)` pattern used at the
    /// start of each public API entry point in C zlib.
    fn clear_error(&mut self) {
        self.state.err = Z_OK;
        self.state.msg = None;
    }
}

// ─── Inflate Helpers ─────────────────────────────────────────────────────────

impl GzReader {
    /// Calls an inflate API function, handling the state extraction pattern.
    ///
    /// The [`InflateState`](inflate::InflateState) is stored inside the
    /// [`ZStream`]'s opaque state as a `Box<dyn Any>`. This helper
    /// temporarily extracts it, calls the provided function, then puts
    /// it back.
    fn with_inflate<F>(&mut self, f: F) -> io::Result<ReturnCode>
    where
        F: FnOnce(&mut inflate::InflateState, &mut ZStream) -> ReturnCode,
    {
        let mut boxed = self.state.strm.state.take().ok_or_else(|| {
            io::Error::new(io::ErrorKind::Other, "inflate state not initialized")
        })?;
        let istate =
            boxed
                .downcast_mut::<inflate::InflateState>()
                .ok_or_else(|| {
                    // Put state back before returning error
                    io::Error::new(io::ErrorKind::Other, "invalid inflate state type")
                })?;
        let ret = f(istate, &mut self.state.strm);
        self.state.strm.state = Some(boxed);
        Ok(ret)
    }
}

// ─── I/O Helpers ─────────────────────────────────────────────────────────────

impl GzReader {
    /// Reads data from a file into a buffer, looping to handle partial reads.
    ///
    /// Port of `gz_load()` from `gzread.c` lines 18–47. This is a static
    /// method that takes individual field references to avoid borrow
    /// conflicts with the caller's other fields.
    ///
    /// Returns the number of bytes read. On EOF, sets `*eof_flag = true`.
    /// On `EAGAIN`/`EWOULDBLOCK`, sets `*again_flag = true` and returns
    /// the partial count (or an error if nothing was read).
    fn load_buf(
        file: &mut File,
        buf: &mut [u8],
        eof_flag: &mut bool,
        again_flag: &mut bool,
    ) -> io::Result<usize> {
        *again_flag = false;
        let len = buf.len();
        let mut have = 0usize;

        while have < len {
            match file.read(&mut buf[have..]) {
                Ok(0) => {
                    *eof_flag = true;
                    break;
                }
                Ok(n) => {
                    have += n;
                }
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                    *again_flag = true;
                    if have != 0 {
                        return Ok(have);
                    }
                    return Err(io::Error::from(io::ErrorKind::WouldBlock));
                }
                Err(e) => {
                    return Err(e);
                }
            }
        }
        Ok(have)
    }

    /// Loads input data from the file into the input buffer.
    ///
    /// Port of `gz_avail()` from `gzread.c` lines 56–81.
    /// Moves any remaining unprocessed data to the start of the input
    /// buffer, then fills the rest with data from the file.
    fn avail(&mut self) -> io::Result<()> {
        if self.state.err != Z_OK && self.state.err != Z_BUF_ERROR {
            return Err(self.make_io_error());
        }
        if !self.state.eof {
            // Move remaining input to beginning of buffer
            if self.in_avail > 0 && self.in_pos > 0 {
                self.state
                    .input
                    .copy_within(self.in_pos..self.in_pos + self.in_avail, 0);
            }
            self.in_pos = 0;

            // Load more data from file
            let load_start = self.in_avail;
            let size = self.state.size;
            if load_start < size {
                self.state.input.resize(size, 0);
                let result = Self::load_buf(
                    &mut self.state.file,
                    &mut self.state.input[load_start..size],
                    &mut self.state.eof,
                    &mut self.state.again,
                );
                match result {
                    Ok(got) => {
                        self.in_avail += got;
                    }
                    Err(e) => {
                        self.state.err = Z_ERRNO;
                        self.state.msg =
                            Some(format!("{}: {}", self.state.path, e));
                        return Err(e);
                    }
                }
            }
        }
        Ok(())
    }

    /// Looks for a gzip header, sets up for inflate or copy.
    ///
    /// Port of `gz_look()` from `gzread.c` lines 93–169.
    /// On first call, allocates buffers and initializes inflate.
    /// Sets `state.how` to [`GzHow::Copy`] or [`GzHow::Gzip`]
    /// depending on auto-detection of the input format.
    fn look(&mut self) -> io::Result<()> {
        // Allocate read buffers and inflate memory on first call
        if self.state.size == 0 {
            let want = self.state.want;
            self.state.input = vec![0u8; want];
            // Output buffer is double-sized for gzungetc() space
            self.state.output = vec![0u8; want.saturating_mul(2)];

            // Initialize inflate for gunzip auto-detect (windowBits = 15 + 16)
            let ret = inflate::inflate_init2(&mut self.state.strm, MAX_WBITS + 16);
            if ret != ReturnCode::Ok {
                self.state.input = Vec::new();
                self.state.output = Vec::new();
                self.set_error(Z_MEM_ERROR, "out of memory");
                return Err(self.make_io_error());
            }
            self.state.size = want;
        }

        // If transparent reading is disabled (direct == -1, gzip-only mode),
        // or if we're past the first member (junk == 0), go directly to GZIP.
        // Port of gzread.c lines 127–133.
        if self.state.direct == -1 || self.state.junk == 0 {
            let reset_ret =
                self.with_inflate(|s, strm| inflate::inflate_reset(s, strm))?;
            if reset_ret != ReturnCode::Ok {
                self.set_error(Z_MEM_ERROR, "out of memory");
                return Err(self.make_io_error());
            }
            self.state.how = GzHow::Gzip;
            self.state.junk = if self.state.junk != -1 { 1 } else { 0 };
            self.state.direct = 0;
            return Ok(());
        }

        // Auto-detect: load header bytes and check for gzip signature.
        // Port of gzread.c lines 135–169.
        self.avail()?;

        // If no input is available, or non-blocking stalled before 4 bytes,
        // return and wait for more data.
        if self.in_avail == 0 || (self.state.again && self.in_avail < 4) {
            return Ok(());
        }

        // Check first four bytes for gzip signature: [31, 139, 8, <32]
        if self.in_avail > 3
            && self.state.input[self.in_pos] == 31
            && self.state.input[self.in_pos + 1] == 139
            && self.state.input[self.in_pos + 2] == 8
            && self.state.input[self.in_pos + 3] < 32
        {
            let reset_ret =
                self.with_inflate(|s, strm| inflate::inflate_reset(s, strm))?;
            if reset_ret != ReturnCode::Ok {
                self.set_error(Z_MEM_ERROR, "out of memory");
                return Err(self.make_io_error());
            }
            self.state.how = GzHow::Gzip;
            self.state.junk = 1;
            self.state.direct = 0;
            return Ok(());
        }

        // Not gzip: copy input to output buffer for transparent reading.
        // The output buffer is larger than the input buffer, ensuring
        // there is also room for gzungetc().
        let n = self.in_avail;
        self.state.output[..n]
            .copy_from_slice(&self.state.input[self.in_pos..self.in_pos + n]);
        self.state.x.next = 0;
        self.state.x.have = n;
        self.in_avail = 0;
        self.in_pos = 0;
        self.state.how = GzHow::Copy;
        Ok(())
    }

    /// Decompresses data from input into the provided output slice.
    ///
    /// Port of `gz_decomp()` from `gzread.c` lines 180–240.
    /// Returns the number of decompressed bytes written to `out_buf`.
    /// If the end of a gzip member is reached, sets `state.how = Look`.
    ///
    /// This function feeds all available input to inflate in a loop and
    /// uses `total_in`/`total_out` deltas to accurately track
    /// consumption and production.
    fn decomp(&mut self, out_buf: &mut [u8]) -> io::Result<usize> {
        let capacity = out_buf.len();
        if capacity == 0 {
            return Ok(0);
        }

        let mut written = 0usize;
        let mut last_ret = ReturnCode::Ok;

        while written < capacity {
            // Ensure we have input for inflate.
            if self.in_avail == 0 {
                if self.avail().is_err() {
                    break;
                }
            }
            if self.in_avail == 0 {
                if !self.state.again {
                    self.set_error(Z_BUF_ERROR, "unexpected end of file");
                }
                break;
            }

            // Request the full remaining output capacity.
            let out_chunk = capacity - written;

            // Snapshot totals so we can compute deltas.
            let ti_before = self.state.strm.total_in;
            let to_before = self.state.strm.total_out;

            // Set up ZStream input and output.
            self.state.strm.set_input(
                &self.state.input[self.in_pos..self.in_pos + self.in_avail],
            );
            self.state.strm.set_output_buffer(out_chunk);

            // Call inflate once.
            last_ret =
                self.with_inflate(|s, strm| inflate::inflate(s, strm, Z_NO_FLUSH))?;

            // Derive consumed/produced from total deltas.
            let consumed = (self.state.strm.total_in - ti_before) as usize;
            let produced = (self.state.strm.total_out - to_before) as usize;

            // Advance our input tracking.
            let actual_consumed = consumed.min(self.in_avail);
            self.in_pos += actual_consumed;
            self.in_avail -= actual_consumed;

            // Copy decompressed output.
            let copy_len = produced.min(out_chunk);
            if copy_len > 0 {
                let src = self.state.strm.output_written();
                let real_len = copy_len.min(src.len());
                if real_len > 0 {
                    out_buf[written..written + real_len]
                        .copy_from_slice(&src[..real_len]);
                    written += real_len;
                }
                self.state.junk = 0;
            }

            // Handle inflate return codes.
            match last_ret {
                ReturnCode::StreamError | ReturnCode::NeedDict => {
                    self.set_error(
                        Z_STREAM_ERROR,
                        "internal error: inflate stream corrupt",
                    );
                    return Err(self.make_io_error());
                }
                ReturnCode::MemError => {
                    self.set_error(Z_MEM_ERROR, "out of memory");
                    return Err(self.make_io_error());
                }
                ReturnCode::DataError => {
                    // Data was already produced — keep it and treat as
                    // end of member.
                    if written > 0 {
                        self.state.how = GzHow::Look;
                        return Ok(written);
                    }
                    // Trailing garbage after a gzip member is acceptable.
                    if self.state.junk == 1 {
                        self.in_avail = 0;
                        self.state.eof = true;
                        self.state.how = GzHow::Look;
                        return Ok(0);
                    }
                    let msg = self
                        .state
                        .strm
                        .msg
                        .unwrap_or("compressed data error")
                        .to_string();
                    self.set_error(Z_DATA_ERROR, &msg);
                    return Err(self.make_io_error());
                }
                ReturnCode::StreamEnd => {
                    break;
                }
                _ => { /* Ok, BufError — continue */ }
            }
        }

        // If the gzip member completed, look for the next one.
        if last_ret == ReturnCode::StreamEnd {
            self.state.junk = 0;
            self.state.how = GzHow::Look;
        }

        Ok(written)
    }

    /// Fetches data into the output buffer from the input file.
    ///
    /// Port of `gz_fetch()` from `gzread.c` lines 248–277.
    /// Assumes `state.x.have == 0`. Loops until the output buffer has
    /// data or EOF is reached.
    fn fetch(&mut self) -> io::Result<()> {
        loop {
            match self.state.how {
                GzHow::Look => {
                    self.look()?;
                    if self.state.how == GzHow::Look {
                        // Still in LOOK — not enough data yet
                        return Ok(());
                    }
                }
                GzHow::Copy => {
                    // Direct file read into output buffer
                    let out_len = self.state.size.saturating_mul(2);
                    self.state.output.resize(out_len, 0);
                    let result = Self::load_buf(
                        &mut self.state.file,
                        &mut self.state.output[..out_len],
                        &mut self.state.eof,
                        &mut self.state.again,
                    );
                    match result {
                        Ok(got) => {
                            self.state.x.have = got;
                            self.state.x.next = 0;
                        }
                        Err(e) => {
                            self.state.err = Z_ERRNO;
                            self.state.msg =
                                Some(format!("{}: {}", self.state.path, e));
                            return Err(e);
                        }
                    }
                    return Ok(());
                }
                GzHow::Gzip => {
                    // Decompress into output buffer
                    let out_len = self.state.size.saturating_mul(2);
                    self.state.output.resize(out_len, 0);
                    // We need a temporary buffer to decomp into, then copy
                    // because decomp needs &mut self and &mut output simultaneously.
                    let mut temp = vec![0u8; out_len];
                    let produced = self.decomp(&mut temp)?;
                    self.state.output[..produced]
                        .copy_from_slice(&temp[..produced]);
                    self.state.x.have = produced;
                    self.state.x.next = 0;
                }
            }

            // Loop until we have output or reach EOF with no remaining input
            if self.state.x.have != 0
                || (self.state.eof && self.in_avail == 0)
            {
                break;
            }
        }
        Ok(())
    }

    /// Skips `state.skip` uncompressed bytes of output.
    ///
    /// Port of `gz_skip()` from `gzread.c` lines 281–309.
    /// Consumes buffered output without copying to a caller buffer.
    fn skip_pending(&mut self) -> io::Result<()> {
        while self.state.skip > 0 {
            // Skip from output buffer
            if self.state.x.have > 0 {
                #[allow(clippy::cast_sign_loss)]
                let n = if (self.state.x.have as i64) > self.state.skip {
                    self.state.skip as usize
                } else {
                    self.state.x.have
                };
                self.state.x.have -= n;
                self.state.x.next += n;
                self.state.x.pos += n as i64;
                self.state.skip -= n as i64;
            } else if self.state.eof && self.in_avail == 0 {
                // Reached end of input
                break;
            } else {
                // Need more output — fetch it
                self.fetch()?;
            }
        }
        Ok(())
    }

    /// Core read implementation — reads `len` bytes into `buf`.
    ///
    /// Port of `gz_read()` from `gzread.c` lines 317–393.
    /// Copies from the output buffer first, then fetches or does
    /// direct I/O for the remainder.
    #[allow(clippy::cast_possible_wrap)]
    fn gz_read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let len = buf.len();
        if len == 0 {
            return Ok(0);
        }

        // Process a pending skip request
        if self.state.skip > 0 {
            if self.skip_pending().is_err() {
                return Ok(0);
            }
        }

        // Read bytes into buf
        let mut got = 0usize;
        let mut remaining = len;
        let mut had_error = false;

        while remaining > 0 && !had_error {
            // First: try copying from the output buffer
            if self.state.x.have > 0 {
                let n = self.state.x.have.min(remaining);
                let src_start = self.state.x.next;
                buf[got..got + n]
                    .copy_from_slice(&self.state.output[src_start..src_start + n]);
                self.state.x.next += n;
                self.state.x.have -= n;
                if self.state.err != Z_OK {
                    // Caught deferred error from gz_fetch()
                    had_error = true;
                }
                remaining -= n;
                got += n;
                self.state.x.pos += n as i64;
            }
            // Second: check for EOF
            else if self.state.eof && self.in_avail == 0 {
                break;
            }
            // Third: small read or LOOK mode — buffer through output
            else if self.state.how == GzHow::Look
                || remaining < self.state.size.saturating_mul(2)
            {
                match self.fetch() {
                    Ok(()) => {
                        if self.state.x.have == 0
                            && self.state.err != Z_OK
                            && self.state.err != Z_BUF_ERROR
                        {
                            had_error = true;
                        }
                    }
                    Err(_) => {
                        if self.state.x.have == 0 {
                            had_error = true;
                        }
                    }
                }
                // Go back to the top to copy from output buffer
                continue;
            }
            // Fourth: large COPY — read directly into user buffer
            else if self.state.how == GzHow::Copy {
                let result = Self::load_buf(
                    &mut self.state.file,
                    &mut buf[got..got + remaining],
                    &mut self.state.eof,
                    &mut self.state.again,
                );
                match result {
                    Ok(n) => {
                        remaining -= n;
                        got += n;
                        self.state.x.pos += n as i64;
                    }
                    Err(e) => {
                        self.state.err = Z_ERRNO;
                        self.state.msg =
                            Some(format!("{}: {}", self.state.path, e));
                        had_error = true;
                    }
                }
            }
            // Fifth: large GZIP — decompress directly into user buffer
            else {
                // state.how == GzHow::Gzip
                match self.decomp(&mut buf[got..got + remaining]) {
                    Ok(n) => {
                        remaining -= n;
                        got += n;
                        self.state.x.pos += n as i64;
                    }
                    Err(_) => {
                        had_error = true;
                    }
                }
            }
        }

        // Note read past EOF
        if remaining > 0 && self.state.eof {
            self.state.past = true;
        }

        Ok(got)
    }
}

// ─── Read Trait ──────────────────────────────────────────────────────────────

impl Read for GzReader {
    /// Reads decompressed bytes from the gzip stream.
    ///
    /// Transparently handles gzip-compressed and uncompressed input,
    /// decompressing on the fly when gzip data is detected.
    ///
    /// Port of `gzread()` / `gzfread()` from `gzread.c` lines 396–465.
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        // Check mode
        if self.state.mode != GzMode::Read {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not in read mode",
            ));
        }

        // Check that there was no serious error
        if self.state.err != Z_OK
            && self.state.err != Z_BUF_ERROR
            && !self.state.again
        {
            return Err(self.make_io_error());
        }

        // Clear previous non-fatal error
        self.clear_error();

        self.gz_read(buf)
    }
}

// ─── Public API Methods ──────────────────────────────────────────────────────

impl GzReader {
    /// Reads a single byte from the gzip stream.
    ///
    /// This is the optimized equivalent of `gzgetc()` from `gzread.c`
    /// lines 473–498. If data is buffered, the byte is returned
    /// immediately without entering the full read pipeline.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if reading fails or the stream is exhausted.
    pub fn getc(&mut self) -> io::Result<u8> {
        // Check mode
        if self.state.mode != GzMode::Read {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not in read mode",
            ));
        }

        // Check for serious error
        if self.state.err != Z_OK
            && self.state.err != Z_BUF_ERROR
            && !self.state.again
        {
            return Err(self.make_io_error());
        }
        self.clear_error();

        // Fast path: read from output buffer without skip check
        if self.state.x.have > 0 {
            self.state.x.have -= 1;
            self.state.x.pos += 1;
            let idx = self.state.x.next;
            self.state.x.next += 1;
            return Ok(self.state.output[idx]);
        }

        // Slow path: go through gz_read
        let mut byte = [0u8; 1];
        let n = self.gz_read(&mut byte)?;
        if n < 1 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "end of gzip stream",
            ));
        }
        Ok(byte[0])
    }

    /// Pushes a byte back into the read buffer.
    ///
    /// The pushed byte will be the next byte returned by [`read`](Read::read)
    /// or [`getc`](GzReader::getc). Uses the double-sized output buffer
    /// to provide push-back space.
    ///
    /// Port of `gzungetc()` from `gzread.c` lines 505–563.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the buffer is full or if a serious error
    /// state is active.
    pub fn ungetc(&mut self, c: u8) -> io::Result<()> {
        // Check mode
        if self.state.mode != GzMode::Read {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not in read mode",
            ));
        }

        // Ensure buffers are allocated (in case just opened)
        if self.state.how == GzHow::Look && self.state.x.have == 0 {
            let _ = self.look();
        }

        // Check for serious error
        if self.state.err != Z_OK
            && self.state.err != Z_BUF_ERROR
            && !self.state.again
        {
            return Err(self.make_io_error());
        }
        self.clear_error();

        // Process pending skip
        if self.state.skip > 0 {
            self.skip_pending()?;
        }

        // If output buffer empty, put byte at end (allows more pushing)
        if self.state.x.have == 0 {
            let end_idx = self.state.size.saturating_mul(2).saturating_sub(1);
            if end_idx < self.state.output.len() {
                self.state.output[end_idx] = c;
                self.state.x.next = end_idx;
                self.state.x.have = 1;
                self.state.x.pos -= 1;
                self.state.past = false;
                return Ok(());
            }
            return Err(io::Error::new(
                io::ErrorKind::Other,
                "output buffer not allocated",
            ));
        }

        // If buffer is full, give up
        if self.state.x.have == self.state.size.saturating_mul(2) {
            self.set_error(Z_DATA_ERROR, "out of room to push characters");
            return Err(self.make_io_error());
        }

        // Slide output data if needed and insert byte before existing data
        if self.state.x.next == 0 {
            let have = self.state.x.have;
            let buf_end = self.state.size.saturating_mul(2);
            // Slide data from [0..have] to [buf_end - have..buf_end]
            let dest_start = buf_end - have;
            self.state.output.copy_within(0..have, dest_start);
            self.state.x.next = dest_start;
        }
        self.state.x.have += 1;
        self.state.x.next -= 1;
        self.state.output[self.state.x.next] = c;
        self.state.x.pos -= 1;
        self.state.past = false;
        Ok(())
    }

    /// Reads a line (up to a newline or buffer capacity) from the stream.
    ///
    /// Copies bytes into `buf` until a newline is found, the buffer is
    /// full (leaving room for a null terminator), or EOF is reached.
    /// The newline byte is included in the output. No null terminator
    /// is appended (this is idiomatic Rust; use the returned length).
    ///
    /// Port of `gzgets()` from `gzread.c` lines 566–624.
    ///
    /// # Returns
    ///
    /// The number of bytes written to `buf`, or 0 on EOF/error.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if reading fails or the stream is in error.
    pub fn gets(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        // Check mode
        if self.state.mode != GzMode::Read {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not in read mode",
            ));
        }

        // Check for serious error
        if self.state.err != Z_OK
            && self.state.err != Z_BUF_ERROR
            && !self.state.again
        {
            return Err(self.make_io_error());
        }
        self.clear_error();

        // Process pending skip
        if self.state.skip > 0 {
            if self.skip_pending().is_err() {
                return Ok(0);
            }
        }

        // Reserve one byte for potential null terminator (C compat)
        let max_bytes = buf.len().saturating_sub(1);
        if max_bytes == 0 {
            return Ok(0);
        }

        let mut written = 0usize;
        let mut found_eol = false;

        while written < max_bytes && !found_eol {
            // Ensure something is in the output buffer
            if self.state.x.have == 0 {
                if self.fetch().is_err() {
                    break;
                }
            }
            if self.state.x.have == 0 {
                self.state.past = true;
                break;
            }

            // Determine how many bytes to scan
            let avail = self.state.x.have.min(max_bytes - written);
            let src_start = self.state.x.next;

            // Look for newline in available output
            let scan_slice =
                &self.state.output[src_start..src_start + avail];
            let n = match scan_slice.iter().position(|&b| b == b'\n') {
                Some(pos) => {
                    found_eol = true;
                    pos + 1 // include the newline
                }
                None => avail,
            };

            // Copy through end-of-line or remainder
            buf[written..written + n]
                .copy_from_slice(&self.state.output[src_start..src_start + n]);
            self.state.x.have -= n;
            self.state.x.next += n;
            self.state.x.pos += n as i64;
            written += n;
        }

        Ok(written)
    }

    /// Returns `true` if the stream is being read transparently.
    ///
    /// A transparent read means the input is not gzip-compressed —
    /// bytes are passed through directly without decompression.
    ///
    /// Port of `gzdirect()` from `gzread.c` lines 627–642.
    pub fn is_direct(&mut self) -> bool {
        // If state is not yet known, determine it
        if self.state.mode == GzMode::Read
            && self.state.how == GzHow::Look
            && self.state.x.have == 0
        {
            let _ = self.look();
        }
        self.state.direct == 1
    }
}

// ─── Seek / Position Methods ─────────────────────────────────────────────────

impl GzReader {
    /// Rewinds the reader to the beginning of the gzip data.
    ///
    /// Seeks the underlying file back to the position recorded at open
    /// time, resets the inflate state and all buffers.
    ///
    /// Port of `gzrewind()` from `gzlib.c` lines 346–364.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the file seek fails.
    pub fn rewind(&mut self) -> io::Result<()> {
        if self.state.mode != GzMode::Read {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not in read mode",
            ));
        }

        // Seek to the start of the gzip data
        let start = self.state.start;
        #[allow(clippy::cast_sign_loss)]
        self.state.file.seek(SeekFrom::Start(start as u64))?;

        // Reset state (gz_reset equivalent)
        self.state.x.have = 0;
        self.state.x.next = 0;
        self.state.x.pos = 0;
        self.state.eof = false;
        self.state.past = false;
        self.state.how = GzHow::Look;
        self.state.skip = 0;
        self.in_pos = 0;
        self.in_avail = 0;
        self.clear_error();

        Ok(())
    }

    /// Seeks to a position in the uncompressed output stream.
    ///
    /// For forward seeks, bytes are skipped lazily during subsequent
    /// reads. For backward seeks, the file is rewound and then the
    /// reader skips forward to the target position.
    ///
    /// Port of `gzseek64()` read-mode logic from `gzlib.c` lines 367–435.
    ///
    /// # Arguments
    ///
    /// * `offset` — Byte offset relative to `whence`.
    /// * `whence` — Seek origin: [`Start`](SeekFrom::Start),
    ///   [`Current`](SeekFrom::Current), or [`End`](SeekFrom::End).
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the computed position is negative or
    /// if [`SeekFrom::End`] is used (not supported for gzip streams).
    pub fn seek(&mut self, offset: i64, whence: SeekFrom) -> io::Result<i64> {
        if self.state.mode != GzMode::Read {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not in read mode",
            ));
        }

        // Check for serious error
        if self.state.err != Z_OK && self.state.err != Z_BUF_ERROR {
            return Err(self.make_io_error());
        }

        // Compute absolute target position.
        // The `offset` parameter carries the seek delta (matching the C
        // `gzseek64(file, offset, whence)` API).  `whence` is used only
        // as a discriminant; the embedded `SeekFrom` value is ignored in
        // favour of the explicit `offset` parameter.
        let target = match whence {
            SeekFrom::Start(_) => offset,
            SeekFrom::Current(_) => {
                let current = self.state.x.pos - self.state.x.have as i64;
                current.checked_add(offset).ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "seek position overflow",
                    )
                })?
            }
            SeekFrom::End(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "SEEK_END not supported for gzip streams",
                ));
            }
        };

        // Cannot seek to negative position
        if target < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "negative seek position",
            ));
        }

        // Compute how far we need to go from current position
        let current = self.state.x.pos - self.state.x.have as i64;
        if target < current {
            // Need to rewind first
            self.rewind()?;
            // After rewind, current position is 0
            self.state.skip = target;
        } else {
            // Forward seek: skip from current position
            // First consume what we can from the output buffer
            let in_buffer = self.state.x.have as i64;
            let advance = (target - current).min(in_buffer);
            #[allow(clippy::cast_sign_loss)]
            let advance_usize = advance as usize;
            self.state.x.have -= advance_usize;
            self.state.x.next += advance_usize;
            self.state.x.pos += advance;
            self.state.skip = target - current - advance;
        }

        Ok(target)
    }

    /// Returns the current position in the uncompressed output stream.
    ///
    /// Port of `gztell64()` from `gzlib.c` lines 446–458.
    #[must_use]
    pub fn tell(&self) -> i64 {
        self.state.x.pos
    }

    /// Returns `true` if the end of the input has been reached.
    ///
    /// This returns `true` only after a read operation has attempted
    /// to read past the end of available data. Simply reaching the
    /// end of the compressed stream does not set this flag until the
    /// next read attempt.
    ///
    /// Port of `gzeof()` from `gzlib.c` lines 498–510.
    #[must_use]
    pub fn eof(&self) -> bool {
        self.state.past
    }
}

// ─── Drop ────────────────────────────────────────────────────────────────────

/// Resource cleanup for [`GzReader`].
///
/// Port of `gzclose_r()` from `gzread.c` lines 645–668.
/// Calls `inflate_end()` to clean up the inflate engine state.
/// The file handle, buffers, and other resources are freed
/// automatically by Rust's ownership model.
impl Drop for GzReader {
    fn drop(&mut self) {
        // Clean up inflate state if it was initialized
        if self.state.size > 0 {
            if let Some(mut boxed) = self.state.strm.state.take() {
                if let Some(istate) =
                    boxed.downcast_mut::<inflate::InflateState>()
                {
                    let _ =
                        inflate::inflate_end(istate, &mut self.state.strm);
                }
                // Don't put state back — we're dropping
            }
        }
        // File handle, buffers, path string, and GzState itself
        // are freed automatically by their own Drop implementations.
    }
}
