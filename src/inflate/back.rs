//! Callback-based raw DEFLATE decompression.
//!
//! This module ports C zlib's `infback.c` (579 lines), implementing the
//! alternative inflate interface where the caller provides callback functions
//! for I/O instead of pre-allocated buffers. The three public functions —
//! [`inflate_back_init`], [`inflate_back`], and [`inflate_back_end`] — provide
//! initialization, decompression, and cleanup for raw DEFLATE streams.
//!
//! # Overview
//!
//! Unlike the standard `inflate()` interface that works with pre-allocated
//! input/output buffers in a [`ZStream`], `inflate_back()` uses a pull-based
//! callback model:
//!
//! - [`InflateBackInput::read()`] is called when more compressed input is
//!   needed.
//! - [`InflateBackOutput::write()`] is called when the decompression window is
//!   full or when decompression completes.
//!
//! This is useful for applications that produce or consume data on demand,
//! such as stream processors or format converters.
//!
//! # Limitations
//!
//! - **Raw DEFLATE only:** No zlib or gzip header/trailer processing.
//! - **No format auto-detection:** The caller must know the stream is raw
//!   DEFLATE.
//! - `inflate_back()` and `inflate()` serve different use cases; the standard
//!   `inflate()` API is preferred for most applications.

// Suppress dead-code warnings: these functions will be called by consumers
// once the full inflate API surface is wired up through mod.rs.
#![allow(dead_code)]

// In no_std mode, pull alloc types that the std prelude normally provides.
#[cfg(not(feature = "std"))]
use alloc::{boxed::Box, vec, vec::Vec};

use crate::error::{ReturnCode, ZlibError, ZlibResult};
use crate::stream::{StreamState, ZStream};

use super::fast::inflate_fast;
use super::state::{InflateMode, InflateState};
use super::tables::{Code, CodeType, inflate_table};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Permutation of code-length code-length indices (RFC 1951 §3.2.7).
///
/// When reading dynamic Huffman table descriptors, the code-length code
/// lengths appear in this permuted order rather than sequentially 0..18.
const ORDER: [usize; 19] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

// ---------------------------------------------------------------------------
// Callback traits
// ---------------------------------------------------------------------------

/// Input callback for [`inflate_back`].
///
/// Implementations provide compressed DEFLATE data on demand. When the
/// decompressor needs more input, it calls [`read()`](Self::read).
///
/// # Contract
///
/// - Return a non-empty byte slice containing the next chunk of compressed
///   data.
/// - Return an empty slice (`&[]`) to signal end-of-input or an I/O error.
///   This causes `inflate_back()` to return `Err(ZlibError::BufError)`.
/// - The returned slice must remain valid until the next call to `read()`.
///   The decompressor copies the data internally before any subsequent call.
///
/// # Examples
///
/// ```ignore
/// use zlib_rs::inflate::back::InflateBackInput;
///
/// struct SliceInput<'a> {
///     data: &'a [u8],
///     consumed: bool,
/// }
///
/// impl<'a> InflateBackInput for SliceInput<'a> {
///     fn read(&mut self) -> &[u8] {
///         if self.consumed {
///             return &[];
///         }
///         self.consumed = true;
///         self.data
///     }
/// }
/// ```
pub trait InflateBackInput {
    /// Read the next chunk of compressed input data.
    ///
    /// Returns a non-empty byte slice on success, or an empty slice on
    /// EOF / failure.
    fn read(&mut self) -> &[u8];
}

/// Output callback for [`inflate_back`].
///
/// Implementations receive decompressed data as it becomes available. When
/// the decompression window fills up or decompression completes, the
/// decompressor calls [`write()`](Self::write) with the decompressed bytes.
///
/// # Contract
///
/// - Process or store the provided `data` slice.
/// - Return `Ok(())` on success.
/// - Return `Err(…)` on failure — this causes `inflate_back()` to return
///   `Err(ZlibError::BufError)`.
/// - The `data` slice is only valid for the duration of the `write()` call.
///
/// # Examples
///
/// ```ignore
/// use zlib_rs::inflate::back::InflateBackOutput;
/// use zlib_rs::error::ZlibError;
///
/// struct VecOutput {
///     buf: Vec<u8>,
/// }
///
/// impl InflateBackOutput for VecOutput {
///     fn write(&mut self, data: &[u8]) -> Result<(), ZlibError> {
///         self.buf.extend_from_slice(data);
///         Ok(())
///     }
/// }
/// ```
pub trait InflateBackOutput {
    /// Write a chunk of decompressed output data.
    ///
    /// Returns `Ok(())` on success, or an appropriate [`ZlibError`] on
    /// failure.
    fn write(&mut self, data: &[u8]) -> Result<(), ZlibError>;
}

// ---------------------------------------------------------------------------
// inflate_back_init
// ---------------------------------------------------------------------------

/// Initialise the state for callback-based raw DEFLATE decompression.
///
/// This is the Rust equivalent of C zlib's `inflateBackInit_()` (`infback.c`
/// lines 25–64). It allocates and configures an [`InflateState`] for use
/// with [`inflate_back()`].
///
/// # Arguments
///
/// * `strm` — Stream to initialise. On success, `strm.state` is set to an
///   active [`StreamState::Inflate`] holding the new decompression state.
/// * `window_bits` — Log₂ of the window size, in the range `8..=15`.
///   Only raw DEFLATE is supported (no zlib/gzip wrappers). The
///   decompression window will be `2^window_bits` bytes.
/// * `window` — Caller-provided window buffer. Must be at least
///   `2^window_bits` bytes long. In the original C API this buffer is used
///   directly; in Rust an internal buffer of the same size is allocated and
///   the caller's buffer length is validated for compatibility.
///
/// # Returns
///
/// * `Ok(ReturnCode::Ok)` on success.
/// * `Err(ZlibError::StreamError)` if `window_bits` is out of range or
///   `window` is too small.
///
/// # Examples
///
/// ```ignore
/// use zlib_rs::stream::ZStream;
/// use zlib_rs::inflate::back::inflate_back_init;
///
/// let mut strm = ZStream::new();
/// let mut window = vec![0u8; 1 << 15];
/// inflate_back_init(&mut strm, 15, &mut window).unwrap();
/// ```
pub fn inflate_back_init(strm: &mut ZStream, window_bits: i32, window: &mut [u8]) -> ZlibResult {
    // Validate window_bits range: raw DEFLATE only (8..=15)
    if !(8..=15).contains(&window_bits) {
        return Err(ZlibError::StreamError);
    }

    let wsize = 1usize << window_bits;

    // Validate caller-provided window buffer size
    if window.len() < wsize {
        return Err(ZlibError::StreamError);
    }

    // Create a new inflate state configured for raw DEFLATE.
    // Passing negative window_bits selects raw mode (wrap = 0).
    let mut state = InflateState::new(-window_bits)?;

    // Configure state fields per infback.c lines 56–62
    state.dmax = 32768;
    state.wbits = window_bits as u32;
    state.wsize = wsize;
    state.wnext = 0;
    state.whave = 0;
    state.sane = true;

    // Allocate internal window buffer matching the requested size.
    // In C, state->window points to the caller's buffer; in Rust we
    // own a Vec<u8> of the same size for memory safety.
    state.window = vec![0u8; wsize];

    // Clear any previous error message
    strm.msg = None;

    // Store state in stream
    strm.state = StreamState::Inflate(Box::new(state));

    Ok(ReturnCode::Ok)
}

// ---------------------------------------------------------------------------
// Helper: pull more input from the callback
// ---------------------------------------------------------------------------

/// Attempts to pull more input data from the callback. Returns `true` if
/// input is now available, `false` on EOF/failure.
///
/// On success, `input_buf` is replaced with the new data and `in_pos` is
/// reset to 0, `have` updated to the new length.
#[inline]
fn pull_input<I: InflateBackInput>(
    in_fn: &mut I,
    input_buf: &mut Vec<u8>,
    in_pos: &mut usize,
    have: &mut usize,
) -> bool {
    let data = in_fn.read();
    if data.is_empty() {
        return false;
    }
    input_buf.clear();
    input_buf.extend_from_slice(data);
    *in_pos = 0;
    *have = input_buf.len();
    true
}

// ---------------------------------------------------------------------------
// Macro-equivalent helper: NEEDBITS
// ---------------------------------------------------------------------------

/// Ensures at least `n` bits are in the accumulator. Pulls more input as
/// needed. Returns `false` if input runs out (caller should `break 'inf`).
#[inline]
fn need_bits<I: InflateBackInput>(
    n: u32,
    hold: &mut u64,
    bits: &mut u32,
    have: &mut usize,
    in_fn: &mut I,
    input_buf: &mut Vec<u8>,
    in_pos: &mut usize,
) -> bool {
    while *bits < n {
        if *have == 0 && !pull_input(in_fn, input_buf, in_pos, have) {
            return false;
        }
        *have -= 1;
        *hold |= (input_buf[*in_pos] as u64) << *bits;
        *in_pos += 1;
        *bits += 8;
    }
    true
}

// ---------------------------------------------------------------------------
// inflate_back -- main decompression function
// ---------------------------------------------------------------------------

/// Decompress a raw DEFLATE stream using callback-based I/O.
///
/// This is the Rust equivalent of C zlib's `inflateBack()` (`infback.c`
/// lines 191-569). It reads compressed input via the [`InflateBackInput`]
/// callback and writes decompressed output via the [`InflateBackOutput`]
/// callback, using a sliding window as an intermediate buffer.
///
/// # Arguments
///
/// * `strm` -- Stream previously initialised with [`inflate_back_init()`].
/// * `in_fn` -- Input callback providing compressed data chunks.
/// * `out_fn` -- Output callback receiving decompressed data chunks.
///
/// # Returns
///
/// * `Ok(ReturnCode::StreamEnd)` -- All blocks decompressed successfully.
/// * `Err(ZlibError::DataError)` -- Invalid DEFLATE data.
/// * `Err(ZlibError::BufError)` -- Input callback returned empty or output
///   callback returned an error.
/// * `Err(ZlibError::StreamError)` -- Stream not properly initialised.
pub fn inflate_back<I, O>(strm: &mut ZStream, in_fn: &mut I, out_fn: &mut O) -> ZlibResult
where
    I: InflateBackInput,
    O: InflateBackOutput,
{
    // Validate stream state and extract window size
    let wsize = match strm.state.as_inflate() {
        Some(s) => s.wsize,
        None => return Err(ZlibError::StreamError),
    };
    if wsize == 0 {
        return Err(ZlibError::StreamError);
    }

    // Reset state for fresh decompression (infback.c lines 213-223)
    {
        let state = strm.state.as_inflate_mut().unwrap();
        state.mode = InflateMode::Type;
        state.last = false;
        state.whave = 0;
    }
    strm.msg = None;

    // Local variables matching C infback.c locals
    let mut hold: u64 = 0;
    let mut bits: u32 = 0;
    let mut put: usize = 0;
    let mut left: usize = wsize;

    // Input management: callback data is copied here for safe processing
    let mut input_buf: Vec<u8> = Vec::new();
    let mut in_pos: usize = 0;
    let mut have: usize = 0;

    // Return value — set by every `break 'inf` path before exiting.
    #[allow(unused_assignments)]
    let mut ret: ZlibResult = Err(ZlibError::StreamError);

    // Main decompression loop (infback.c lines 226-558)
    'inf: loop {
        let mode = {
            match strm.state.as_inflate() {
                Some(s) => s.mode,
                None => {
                    ret = Err(ZlibError::StreamError);
                    break 'inf;
                }
            }
        };

        match mode {
            // TYPE: determine and dispatch block type (lines 228-260)
            InflateMode::Type => {
                let is_last = strm.state.as_inflate().unwrap().last;
                if is_last {
                    let drop_count = bits & 7;
                    hold >>= drop_count;
                    bits -= drop_count;
                    strm.state.as_inflate_mut().unwrap().mode = InflateMode::Done;
                    continue;
                }

                if !need_bits(
                    3,
                    &mut hold,
                    &mut bits,
                    &mut have,
                    in_fn,
                    &mut input_buf,
                    &mut in_pos,
                ) {
                    ret = Err(ZlibError::BufError);
                    break 'inf;
                }

                let last_flag = (hold & 1) != 0;
                hold >>= 1;
                bits -= 1;

                let block_type = hold & 3;
                hold >>= 2;
                bits -= 2;

                let state = strm.state.as_inflate_mut().unwrap();
                state.last = last_flag;
                match block_type {
                    0 => {
                        state.mode = InflateMode::Stored;
                    }
                    1 => {
                        state.use_fixed_codes();
                        state.mode = InflateMode::Len;
                    }
                    2 => {
                        state.mode = InflateMode::Table;
                    }
                    _ => {
                        strm.msg = Some("invalid block type".into());
                        state.mode = InflateMode::Bad;
                    }
                }
            }

            // STORED: copy stored block (lines 262-292)
            InflateMode::Stored => {
                let drop_count = bits & 7;
                hold >>= drop_count;
                bits -= drop_count;

                if !need_bits(
                    32,
                    &mut hold,
                    &mut bits,
                    &mut have,
                    in_fn,
                    &mut input_buf,
                    &mut in_pos,
                ) {
                    ret = Err(ZlibError::BufError);
                    break 'inf;
                }

                if (hold & 0xffff) != ((hold >> 16) ^ 0xffff) {
                    strm.msg = Some("invalid stored block lengths".into());
                    strm.state.as_inflate_mut().unwrap().mode = InflateMode::Bad;
                    continue;
                }

                let block_len = (hold & 0xffff) as u32;
                strm.state.as_inflate_mut().unwrap().length = block_len;
                hold = 0;
                bits = 0;

                while strm.state.as_inflate().unwrap().length != 0 {
                    if have == 0 && !pull_input(in_fn, &mut input_buf, &mut in_pos, &mut have) {
                        ret = Err(ZlibError::BufError);
                        break 'inf;
                    }

                    if left == 0 {
                        let state = strm.state.as_inflate_mut().unwrap();
                        let flush_data = state.window[..wsize].to_vec();
                        if let Err(e) = out_fn.write(&flush_data) {
                            ret = Err(e);
                            break 'inf;
                        }
                        put = 0;
                        left = wsize;
                        state.whave = wsize;
                    }

                    let remaining = strm.state.as_inflate().unwrap().length as usize;
                    let mut copy = remaining;
                    if copy > have {
                        copy = have;
                    }
                    if copy > left {
                        copy = left;
                    }

                    let state = strm.state.as_inflate_mut().unwrap();
                    state.window[put..put + copy]
                        .copy_from_slice(&input_buf[in_pos..in_pos + copy]);
                    have -= copy;
                    in_pos += copy;
                    left -= copy;
                    put += copy;
                    state.length -= copy as u32;
                }

                strm.state.as_inflate_mut().unwrap().mode = InflateMode::Type;
            }

            // TABLE: read dynamic table descriptor (lines 294-418)
            InflateMode::Table => {
                if !need_bits(
                    14,
                    &mut hold,
                    &mut bits,
                    &mut have,
                    in_fn,
                    &mut input_buf,
                    &mut in_pos,
                ) {
                    ret = Err(ZlibError::BufError);
                    break 'inf;
                }

                let nlen = (hold & 0x1f) as u32 + 257;
                hold >>= 5;
                bits -= 5;
                let ndist = (hold & 0x1f) as u32 + 1;
                hold >>= 5;
                bits -= 5;
                let ncode = (hold & 0x0f) as u32 + 4;
                hold >>= 4;
                bits -= 4;

                if nlen > 286 || ndist > 30 {
                    strm.msg = Some("too many length or distance symbols".into());
                    strm.state.as_inflate_mut().unwrap().mode = InflateMode::Bad;
                    continue;
                }

                {
                    let state = strm.state.as_inflate_mut().unwrap();
                    state.nlen = nlen;
                    state.ndist = ndist;
                    state.ncode = ncode;
                    state.have = 0;
                }

                // Read code-length code lengths
                {
                    let state = strm.state.as_inflate_mut().unwrap();
                    let mut local_have = state.have;
                    while local_have < ncode {
                        if !need_bits(
                            3,
                            &mut hold,
                            &mut bits,
                            &mut have,
                            in_fn,
                            &mut input_buf,
                            &mut in_pos,
                        ) {
                            ret = Err(ZlibError::BufError);
                            break 'inf;
                        }
                        state.lens[ORDER[local_have as usize]] = (hold & 7) as u16;
                        hold >>= 3;
                        bits -= 3;
                        local_have += 1;
                    }
                    while local_have < 19 {
                        state.lens[ORDER[local_have as usize]] = 0;
                        local_have += 1;
                    }
                    state.have = local_have;
                }

                // Build code-length decode table
                {
                    let state = strm.state.as_inflate_mut().unwrap();
                    state.lencode_idx = 0;
                    state.next = 0;
                    let mut root_bits = 7u32;
                    let lens_copy: Vec<u16> = state.lens.to_vec();
                    if inflate_table(
                        CodeType::Codes,
                        &lens_copy,
                        19,
                        &mut state.codes,
                        &mut state.next,
                        &mut root_bits,
                        &mut state.work,
                    )
                    .is_err()
                    {
                        strm.msg = Some("invalid code lengths set".into());
                        state.mode = InflateMode::Bad;
                        continue;
                    }
                    state.lenbits = root_bits;
                }

                // Read lit/len and distance code lengths
                let total_codes = {
                    let st = strm.state.as_inflate_mut().unwrap();
                    st.have = 0;
                    st.nlen + st.ndist
                };

                'codelens: loop {
                    let current_have = strm.state.as_inflate().unwrap().have;
                    if current_have >= total_codes {
                        break 'codelens;
                    }

                    let here: Code;
                    loop {
                        let state = strm.state.as_inflate().unwrap();
                        let idx = (hold & ((1u64 << state.lenbits) - 1)) as usize;
                        let entry = state.codes[state.lencode_idx + idx];
                        if (entry.bits as u32) <= bits {
                            here = entry;
                            break;
                        }
                        if !need_bits(
                            entry.bits as u32,
                            &mut hold,
                            &mut bits,
                            &mut have,
                            in_fn,
                            &mut input_buf,
                            &mut in_pos,
                        ) {
                            ret = Err(ZlibError::BufError);
                            break 'inf;
                        }
                    }

                    if here.val < 16 {
                        hold >>= here.bits as u32;
                        bits -= here.bits as u32;
                        let state = strm.state.as_inflate_mut().unwrap();
                        state.lens[state.have as usize] = here.val;
                        state.have += 1;
                    } else {
                        let len: u16;
                        let copy_count: u32;

                        if here.val == 16 {
                            let need = here.bits as u32 + 2;
                            if !need_bits(
                                need,
                                &mut hold,
                                &mut bits,
                                &mut have,
                                in_fn,
                                &mut input_buf,
                                &mut in_pos,
                            ) {
                                ret = Err(ZlibError::BufError);
                                break 'inf;
                            }
                            hold >>= here.bits as u32;
                            bits -= here.bits as u32;
                            let sh = strm.state.as_inflate().unwrap().have;
                            if sh == 0 {
                                strm.msg = Some("invalid bit length repeat".into());
                                strm.state.as_inflate_mut().unwrap().mode = InflateMode::Bad;
                                break 'codelens;
                            }
                            len = strm.state.as_inflate().unwrap().lens[(sh - 1) as usize];
                            copy_count = 3 + (hold & 3) as u32;
                            hold >>= 2;
                            bits -= 2;
                        } else if here.val == 17 {
                            let need = here.bits as u32 + 3;
                            if !need_bits(
                                need,
                                &mut hold,
                                &mut bits,
                                &mut have,
                                in_fn,
                                &mut input_buf,
                                &mut in_pos,
                            ) {
                                ret = Err(ZlibError::BufError);
                                break 'inf;
                            }
                            hold >>= here.bits as u32;
                            bits -= here.bits as u32;
                            len = 0;
                            copy_count = 3 + (hold & 7) as u32;
                            hold >>= 3;
                            bits -= 3;
                        } else {
                            // here.val == 18
                            let need = here.bits as u32 + 7;
                            if !need_bits(
                                need,
                                &mut hold,
                                &mut bits,
                                &mut have,
                                in_fn,
                                &mut input_buf,
                                &mut in_pos,
                            ) {
                                ret = Err(ZlibError::BufError);
                                break 'inf;
                            }
                            hold >>= here.bits as u32;
                            bits -= here.bits as u32;
                            len = 0;
                            copy_count = 11 + (hold & 0x7f) as u32;
                            hold >>= 7;
                            bits -= 7;
                        }

                        let sh = strm.state.as_inflate().unwrap().have;
                        if sh + copy_count > total_codes {
                            strm.msg = Some("invalid bit length repeat".into());
                            strm.state.as_inflate_mut().unwrap().mode = InflateMode::Bad;
                            break 'codelens;
                        }
                        let state = strm.state.as_inflate_mut().unwrap();
                        for _ in 0..copy_count {
                            state.lens[state.have as usize] = len;
                            state.have += 1;
                        }
                    }
                } // end codelens

                if strm.state.as_inflate().unwrap().mode == InflateMode::Bad {
                    continue;
                }

                // Validate end-of-block code
                if strm.state.as_inflate().unwrap().lens[256] == 0 {
                    strm.msg = Some("invalid code -- missing end-of-block".into());
                    strm.state.as_inflate_mut().unwrap().mode = InflateMode::Bad;
                    continue;
                }

                // Build literal/length decode table
                {
                    let state = strm.state.as_inflate_mut().unwrap();
                    state.lencode_idx = 0;
                    state.next = 0;
                    let nlen_usize = state.nlen as usize;
                    let mut len_root = 9u32;
                    let lens_copy: Vec<u16> = state.lens.to_vec();
                    if inflate_table(
                        CodeType::Lens,
                        &lens_copy,
                        nlen_usize,
                        &mut state.codes,
                        &mut state.next,
                        &mut len_root,
                        &mut state.work,
                    )
                    .is_err()
                    {
                        strm.msg = Some("invalid literal/lengths set".into());
                        state.mode = InflateMode::Bad;
                        continue;
                    }
                    state.lenbits = len_root;
                }

                // Build distance decode table
                {
                    let state = strm.state.as_inflate_mut().unwrap();
                    state.distcode_idx = state.next;
                    let nlen_usize = state.nlen as usize;
                    let ndist_usize = state.ndist as usize;
                    let mut dist_root = 6u32;
                    let lens_copy: Vec<u16> = state.lens.to_vec();
                    if inflate_table(
                        CodeType::Dists,
                        &lens_copy[nlen_usize..],
                        ndist_usize,
                        &mut state.codes,
                        &mut state.next,
                        &mut dist_root,
                        &mut state.work,
                    )
                    .is_err()
                    {
                        strm.msg = Some("invalid distances set".into());
                        state.mode = InflateMode::Bad;
                        continue;
                    }
                    state.distbits = dist_root;
                }

                strm.state.as_inflate_mut().unwrap().mode = InflateMode::Len;
            }

            // LEN: decode literals, lengths, distances (lines 422-543)
            InflateMode::Len => {
                // Fast path via inflate_fast
                if have >= 6 && left >= 258 {
                    let state = strm.state.as_inflate_mut().unwrap();
                    state.hold = hold;
                    state.bits = bits;

                    let mut work_buf = state.window.clone();
                    let mut fi_in = in_pos;
                    let mut fi_out = put;
                    // `start` = 0 because `put` (the output position)
                    // is 0-based into the window buffer. In the C
                    // version, `start = wsize` because the C code
                    // computes `written = start - avail_out`; the
                    // Rust adaptation computes `written = out_pos -
                    // start`, so start must be 0.
                    inflate_fast(state, &input_buf, &mut work_buf, &mut fi_in, &mut fi_out, 0);
                    state.window[..wsize].copy_from_slice(&work_buf[..wsize]);

                    in_pos = fi_in;
                    put = fi_out;
                    hold = state.hold;
                    bits = state.bits;
                    have = input_buf.len().saturating_sub(in_pos);
                    left = wsize.saturating_sub(put);

                    if state.mode == InflateMode::Bad {
                        strm.msg = state.msg.as_ref().cloned();
                        continue;
                    }
                    continue;
                }

                // Slow path: one symbol at a time
                let here: Code;
                loop {
                    let state = strm.state.as_inflate().unwrap();
                    let idx = (hold & ((1u64 << state.lenbits) - 1)) as usize;
                    let entry = state.len_code(idx);
                    if (entry.bits as u32) <= bits {
                        here = entry;
                        break;
                    }
                    if !need_bits(
                        entry.bits as u32,
                        &mut hold,
                        &mut bits,
                        &mut have,
                        in_fn,
                        &mut input_buf,
                        &mut in_pos,
                    ) {
                        ret = Err(ZlibError::BufError);
                        break 'inf;
                    }
                }

                // Handle 2nd-level sub-table for length codes
                let here = if here.op != 0 && (here.op & 0xf0) == 0 {
                    let last = here;
                    let resolved: Code;
                    loop {
                        let state = strm.state.as_inflate().unwrap();
                        let idx = last.val as usize
                            + ((hold & ((1u64 << (last.bits + last.op)) - 1)) >> last.bits)
                                as usize;
                        let entry = state.len_code(idx);
                        if (last.bits as u32 + entry.bits as u32) <= bits {
                            resolved = entry;
                            break;
                        }
                        if !need_bits(
                            last.bits as u32 + entry.bits as u32,
                            &mut hold,
                            &mut bits,
                            &mut have,
                            in_fn,
                            &mut input_buf,
                            &mut in_pos,
                        ) {
                            ret = Err(ZlibError::BufError);
                            break 'inf;
                        }
                    }
                    hold >>= last.bits as u32;
                    bits -= last.bits as u32;
                    resolved
                } else {
                    here
                };

                hold >>= here.bits as u32;
                bits -= here.bits as u32;

                // Literal byte (op == 0)
                if here.op == 0 {
                    if left == 0 {
                        let state = strm.state.as_inflate_mut().unwrap();
                        let flush_data = state.window[..wsize].to_vec();
                        if let Err(e) = out_fn.write(&flush_data) {
                            ret = Err(e);
                            break 'inf;
                        }
                        put = 0;
                        left = wsize;
                        state.whave = wsize;
                    }
                    let state = strm.state.as_inflate_mut().unwrap();
                    state.window[put] = here.val as u8;
                    put += 1;
                    left -= 1;
                    state.mode = InflateMode::Len;
                    continue;
                }

                // End of block (op & 32)
                if here.op & 32 != 0 {
                    strm.state.as_inflate_mut().unwrap().mode = InflateMode::Type;
                    continue;
                }

                // Invalid code (op & 64)
                if here.op & 64 != 0 {
                    strm.msg = Some("invalid literal/length code".into());
                    strm.state.as_inflate_mut().unwrap().mode = InflateMode::Bad;
                    continue;
                }

                // Length code: extract extra bits
                let length_base = here.val as u32;
                let extra = (here.op & 15) as u32;
                let length_val = if extra != 0 {
                    if !need_bits(
                        extra,
                        &mut hold,
                        &mut bits,
                        &mut have,
                        in_fn,
                        &mut input_buf,
                        &mut in_pos,
                    ) {
                        ret = Err(ZlibError::BufError);
                        break 'inf;
                    }
                    let extra_val = (hold & ((1u64 << extra) - 1)) as u32;
                    hold >>= extra;
                    bits -= extra;
                    length_base + extra_val
                } else {
                    length_base
                };

                strm.state.as_inflate_mut().unwrap().length = length_val;

                // Distance code
                let dist_here: Code;
                loop {
                    let state = strm.state.as_inflate().unwrap();
                    let idx = (hold & ((1u64 << state.distbits) - 1)) as usize;
                    let entry = state.dist_code(idx);
                    if (entry.bits as u32) <= bits {
                        dist_here = entry;
                        break;
                    }
                    if !need_bits(
                        entry.bits as u32,
                        &mut hold,
                        &mut bits,
                        &mut have,
                        in_fn,
                        &mut input_buf,
                        &mut in_pos,
                    ) {
                        ret = Err(ZlibError::BufError);
                        break 'inf;
                    }
                }

                // Handle 2nd-level distance sub-table
                let dist_here = if (dist_here.op & 0xf0) == 0 {
                    let last = dist_here;
                    let resolved: Code;
                    loop {
                        let state = strm.state.as_inflate().unwrap();
                        let idx = last.val as usize
                            + ((hold & ((1u64 << (last.bits + last.op)) - 1)) >> last.bits)
                                as usize;
                        let entry = state.dist_code(idx);
                        if (last.bits as u32 + entry.bits as u32) <= bits {
                            resolved = entry;
                            break;
                        }
                        if !need_bits(
                            last.bits as u32 + entry.bits as u32,
                            &mut hold,
                            &mut bits,
                            &mut have,
                            in_fn,
                            &mut input_buf,
                            &mut in_pos,
                        ) {
                            ret = Err(ZlibError::BufError);
                            break 'inf;
                        }
                    }
                    hold >>= last.bits as u32;
                    bits -= last.bits as u32;
                    resolved
                } else {
                    dist_here
                };

                hold >>= dist_here.bits as u32;
                bits -= dist_here.bits as u32;

                // Invalid distance code
                if dist_here.op & 64 != 0 {
                    strm.msg = Some("invalid distance code".into());
                    strm.state.as_inflate_mut().unwrap().mode = InflateMode::Bad;
                    continue;
                }

                let mut offset_val = dist_here.val as u32;
                let dist_extra = (dist_here.op & 15) as u32;
                if dist_extra != 0 {
                    if !need_bits(
                        dist_extra,
                        &mut hold,
                        &mut bits,
                        &mut have,
                        in_fn,
                        &mut input_buf,
                        &mut in_pos,
                    ) {
                        ret = Err(ZlibError::BufError);
                        break 'inf;
                    }
                    offset_val += (hold & ((1u64 << dist_extra) - 1)) as u32;
                    hold >>= dist_extra;
                    bits -= dist_extra;
                }

                let offset = offset_val as usize;
                {
                    let state = strm.state.as_inflate_mut().unwrap();
                    state.offset = offset_val;
                }
                let whave = strm.state.as_inflate().unwrap().whave;
                let max_dist = wsize - if whave < wsize { left } else { 0 };
                if offset > max_dist {
                    strm.msg = Some("invalid distance too far back".into());
                    strm.state.as_inflate_mut().unwrap().mode = InflateMode::Bad;
                    continue;
                }

                // Copy match from window (lines 500-542)
                let mut remaining = strm.state.as_inflate().unwrap().length as usize;
                while remaining > 0 {
                    if left == 0 {
                        let state = strm.state.as_inflate_mut().unwrap();
                        let flush_data = state.window[..wsize].to_vec();
                        if let Err(e) = out_fn.write(&flush_data) {
                            ret = Err(e);
                            break 'inf;
                        }
                        put = 0;
                        left = wsize;
                        state.whave = wsize;
                    }

                    let from = if offset > put {
                        wsize - (offset - put)
                    } else {
                        put - offset
                    };

                    let state = strm.state.as_inflate_mut().unwrap();
                    state.window[put] = state.window[from];
                    put += 1;
                    left -= 1;
                    remaining -= 1;
                }

                strm.state.as_inflate_mut().unwrap().mode = InflateMode::Len;
            }

            // DONE: decompression completed (lines 545-548)
            InflateMode::Done => {
                ret = Ok(ReturnCode::StreamEnd);
                break 'inf;
            }

            // BAD: error encountered (lines 550-552)
            InflateMode::Bad => {
                ret = Err(ZlibError::DataError);
                break 'inf;
            }

            // Any other mode is invalid for inflate_back
            _ => {
                ret = Err(ZlibError::StreamError);
                break 'inf;
            }
        }
    } // end inf loop

    // Flush any unwritten data remaining in the window
    if left < wsize {
        let flush_len = wsize - left;
        if let Some(state) = strm.state.as_inflate() {
            let flush_data = state.window[..flush_len].to_vec();
            if out_fn.write(&flush_data).is_err() && ret.is_ok() {
                ret = Err(ZlibError::BufError);
            }
        }
    }

    strm.avail_in = have as u32;

    ret
}

// ---------------------------------------------------------------------------
// inflate_back_end
// ---------------------------------------------------------------------------

/// Clean up state after callback-based decompression.
///
/// This is the Rust equivalent of C zlib's `inflateBackEnd()` (`infback.c`
/// lines 572-579). It drops the [`InflateState`] and all its owned buffers,
/// then resets the stream state to [`StreamState::None`].
///
/// **Note:** The caller's window buffer (passed to [`inflate_back_init()`])
/// is not freed here -- it is owned by the caller.
///
/// # Arguments
///
/// * `strm` -- Stream previously initialised with [`inflate_back_init()`].
///
/// # Returns
///
/// * `Ok(ReturnCode::Ok)` on success.
/// * `Err(ZlibError::StreamError)` if the stream was not in inflate mode.
pub fn inflate_back_end(strm: &mut ZStream) -> ZlibResult {
    if !strm.state.is_inflate() {
        return Err(ZlibError::StreamError);
    }
    // Drop the InflateState and all its owned buffers.
    // The caller's window buffer is NOT freed -- they own it.
    strm.state = StreamState::None;
    Ok(ReturnCode::Ok)
}
