//! One-shot whole-buffer decompression — a 100% safe-Rust port of zlib's
//! `uncompr.c`.
//!
//! This module provides the convenience helpers that decompress an entire
//! in-memory buffer in a single call, mirroring the C entry points
//! `uncompress`, `uncompress2`, `uncompress_z`, and `uncompress2_z`
//! (`uncompr.c`, zlib `1.3.2.1-motley`). They are the decompression
//! counterparts of the `compress` helpers in `crate::util::compress`.
//!
//! # Public surface
//!
//! * [`uncompress`] / [`uncompress2`] — the idiomatic, crate-level API
//!   (re-exported by the crate root as `zlib_rs::uncompress` and
//!   `zlib_rs::uncompress2`). The caller supplies a destination buffer large
//!   enough to hold the *entire* decompressed output; its size is assumed to
//!   have been recorded by the compressor and transmitted out of band, exactly
//!   as the C contract requires (`uncompr.c` L11-L20).
//! * [`uncompress2_to_buf`] / [`uncompress_to_buf`] — the C-semantics core that
//!   the FFI shim (`crate::ffi`) wraps. They honour the C `destLen` /
//!   `sourceLen` in/out contract by returning **both** the number of bytes
//!   produced *and* the number of input bytes consumed.
//!
//! # Relationship to the C source
//!
//! The C implementation drives `inflate()` inside a `do { … } while (err ==
//! Z_OK)` loop whose only purpose is to refill the 32-bit `avail_in` /
//! `avail_out` registers from larger `z_size_t` lengths. Rust slices already
//! carry their length, so this port issues a **single** [`Inflate::inflate`]
//! call over the whole input and output and reads the consumed / produced
//! counts directly from its return value. The observable result — bytes
//! produced, input consumed, and the final status — is identical.
//!
//! The status-to-result mapping reproduces `uncompress2_z` (`uncompr.c`
//! L78-L81) exactly:
//!
//! | C `inflate` outcome                        | This module returns          |
//! |--------------------------------------------|------------------------------|
//! | `Z_STREAM_END`                             | `Ok((produced, consumed))`   |
//! | `Z_NEED_DICT`                              | `Err(ZlibError::DataError)`  |
//! | incomplete stream (all input consumed)     | `Err(ZlibError::DataError)`  |
//! | output buffer too small (input remaining)  | `Err(ZlibError::BufError)`   |
//! | any other error code                       | passed through unchanged     |
//!
//! See [`map_result`] for the precise rules and how they correspond to the C
//! `len == 0` test (where `len` counts *unused* input).
//!
//! # Safety and `no_std`
//!
//! The entire module is **100% safe Rust** (no `unsafe`) and `no_std`-clean: it
//! refers only to other crate modules and language primitives, never to `std`.
//! The decoder is created on the stack, owns its heap state through
//! [`Inflate`], and is released by `Drop` (the RAII analogue of C
//! `inflateEnd`) when the helper returns — there is no manual teardown and no
//! leak path.
//!
//! # Examples
//!
//! ```
//! use zlib_rs::uncompress;
//!
//! // A canonical RFC 1950 zlib stream produced by stock zlib for `b"zlib-rs"`.
//! let packed = [
//!     0x78, 0x9c, 0xab, 0xca, 0xc9, 0x4c, 0xd2, 0x2d, 0x2a, 0x06, 0x00, 0x0b, 0x58, 0x02, 0xc4,
//! ];
//! let mut out = [0u8; 7];
//! let produced = uncompress(&mut out, &packed).unwrap();
//! assert_eq!(&out[..produced], b"zlib-rs");
//! ```

use crate::constants::{Z_BUF_ERROR, Z_NEED_DICT, Z_NO_FLUSH, Z_OK, Z_STREAM_END};
use crate::error::ZlibError;
use crate::inflate::Inflate;

// ===========================================================================
// Status-to-result mapping — the exact reproduction of `uncompr.c` L78-L81.
// ===========================================================================

/// Translate a raw `inflate` outcome into the idiomatic `uncompress` result,
/// reproducing the C `uncompress2_z` return expression (`uncompr.c` L78-L81).
///
/// The inputs are the raw C status `code`, the input bytes `consumed` and
/// output bytes `produced` reported by [`Inflate::inflate`], and the length
/// `dest_len` of the destination buffer.
///
/// * **`Z_STREAM_END`** — the whole stream decoded; map to `Ok`, surfacing the
///   produced and consumed byte counts (C maps this to `Z_OK`).
/// * **`Z_NEED_DICT`** — the stream needs a preset dictionary this one-shot
///   helper cannot supply; map to [`ZlibError::DataError`] (C: `Z_DATA_ERROR`).
/// * **`Z_OK` / `Z_BUF_ERROR` (a non-terminal stop)** — `inflate` returned
///   without reaching end-of-stream. C distinguishes "output too small" from
///   "incomplete input" by inspecting the count of *unused input*
///   (`err == Z_BUF_ERROR && len == 0`). Because a safe-Rust decoder may
///   consume / buffer its input differently from C, this port makes the
///   equivalent decision from the **output** side, which is independent of the
///   input-buffering strategy: a fully decoded stream *always* reports
///   `Z_STREAM_END` (true even for an exact-fit buffer and for an empty
///   payload), so a non-terminal stop means either —
///   - the output buffer is completely full (`produced == dest_len`, with a
///     non-empty buffer): more output is still pending, so the destination was
///     too small → [`ZlibError::BufError`] (C returns the raw `Z_BUF_ERROR`); or
///   - the output buffer still has room: `inflate` consumed all the input it
///     could yet did not finish — a truncated, empty, or otherwise incomplete
///     stream → [`ZlibError::DataError`] (the C `len == 0` branch).
///
///   For every stream stock zlib can actually emit this yields exactly the same
///   classification as the C unused-input test.
/// * **any other code** — a genuine failure (`Z_DATA_ERROR` for corrupt data,
///   `Z_MEM_ERROR`, `Z_STREAM_ERROR`, …) passed straight through, mirroring C's
///   trailing `: err`. [`ZlibError::from_c_int`] recognises every negative zlib
///   error; the defensive `unwrap_or` keeps the mapping total for any value
///   that is somehow outside the known set.
#[inline]
fn map_result(
    code: i32,
    consumed: usize,
    produced: usize,
    dest_len: usize,
) -> Result<(usize, usize), ZlibError> {
    match code {
        c if c == Z_STREAM_END => Ok((produced, consumed)),
        c if c == Z_NEED_DICT => Err(ZlibError::DataError),
        c if c == Z_OK || c == Z_BUF_ERROR => {
            if produced == dest_len && dest_len != 0 {
                // The output buffer is completely full yet the stream did not
                // end: more output is pending, so the destination was too small.
                Err(ZlibError::BufError)
            } else {
                // Output space remained, so `inflate` stopped for lack of input
                // — a truncated, empty, or otherwise incomplete zlib stream
                // (the C `len == 0` case).
                Err(ZlibError::DataError)
            }
        }
        other => Err(ZlibError::from_c_int(other).unwrap_or(ZlibError::DataError)),
    }
}

// ===========================================================================
// C-semantics core — used by the FFI shim (`crate::ffi`).
// ===========================================================================

/// Decompress `source` into `dest`, returning `Ok((produced, consumed))` on a
/// fully decoded stream or a mapped [`ZlibError`] otherwise.
///
/// This is the C-semantics core behind the `uncompress2`/`uncompress2_z` FFI
/// shims: it honours the C in/out contract by reporting **both** how many bytes
/// were written to `dest` (`produced`) and how many bytes of `source` were
/// consumed (`consumed`). `dest` must be large enough to hold the entire
/// decompressed output, whose size the caller is expected to know in advance
/// (`uncompr.c` L11-L20).
///
/// The decoder is a zlib-wrapped (RFC 1950) [`Inflate`] created with the default
/// window (`DEF_WBITS` = 15), exactly matching C `inflateInit`. It is owned
/// locally and torn down by `Drop` (the RAII analogue of C `inflateEnd`) when
/// this function returns.
///
/// # Errors
///
/// * [`ZlibError::DataError`] — the input was corrupt, truncated, empty, or
///   otherwise an incomplete zlib stream, or it requested a preset dictionary.
/// * [`ZlibError::BufError`] — `dest` was too small to hold the full output.
/// * Any other [`ZlibError`] reported by [`Inflate::new`] or [`Inflate::inflate`]
///   (e.g. [`ZlibError::MemError`]) is propagated unchanged.
#[inline]
pub fn uncompress2_to_buf(dest: &mut [u8], source: &[u8]) -> Result<(usize, usize), ZlibError> {
    // Record the output buffer length before the borrow is handed to
    // `inflate`; it drives the "destination too small" vs "incomplete stream"
    // decision (see [`map_result`]).
    let dest_len = dest.len();

    // `Inflate::new()` == C `inflateInit(&stream)`: a zlib-wrapped decoder with
    // the default 32 KiB window. Initialisation failure (e.g. `Z_MEM_ERROR`)
    // propagates via `?`, mirroring C's `if (err != Z_OK) return err;`.
    let mut inflate = Inflate::new()?;

    // A single call decodes as much as possible from the whole input into the
    // whole output. `flush` is the RAW C `i32` `Z_NO_FLUSH`, and the returned
    // triple is the RAW `(c_code, consumed, produced)` contract of the engine.
    let (code, consumed, produced) = inflate.inflate(source, dest, Z_NO_FLUSH);

    // `inflate` drops here == C `inflateEnd(&stream)` (RAII).
    map_result(code, consumed, produced, dest_len)
}

/// Decompress `source` into `dest`, returning `Ok((produced, consumed))`.
///
/// Behaviourally identical to [`uncompress2_to_buf`]; it exists so the FFI shim
/// can wrap the by-value-`sourceLen` C entry points (`uncompress` /
/// `uncompress_z`) under a matching name. In the safe core there is no
/// pointer/by-value distinction, so the two share a single implementation.
///
/// # Errors
///
/// See [`uncompress2_to_buf`].
#[inline]
pub fn uncompress_to_buf(dest: &mut [u8], source: &[u8]) -> Result<(usize, usize), ZlibError> {
    uncompress2_to_buf(dest, source)
}

// ===========================================================================
// Idiomatic public API — re-exported by the crate root via `lib.rs`.
// ===========================================================================

/// Decompress `source` into `dest`, returning the number of bytes written.
///
/// `dest` must be large enough to hold the **entire** decompressed output. As
/// with C `uncompress`, the uncompressed size is not carried in the stream: the
/// compressor is expected to have recorded it and transmitted it out of band
/// (`uncompr.c` L11-L20).
///
/// This is the idiomatic counterpart of C `uncompress`; it is re-exported at
/// the crate root as `zlib_rs::uncompress`.
///
/// # Errors
///
/// * [`ZlibError::DataError`] if `source` is corrupt, truncated, empty, or an
///   otherwise incomplete zlib stream.
/// * [`ZlibError::BufError`] if `dest` is too small to hold the full output.
///
/// # Examples
///
/// ```
/// use zlib_rs::uncompress;
///
/// let packed = [
///     0x78, 0x9c, 0xab, 0xca, 0xc9, 0x4c, 0xd2, 0x2d, 0x2a, 0x06, 0x00, 0x0b, 0x58, 0x02, 0xc4,
/// ];
/// let mut out = [0u8; 7];
/// let produced = uncompress(&mut out, &packed).unwrap();
/// assert_eq!(&out[..produced], b"zlib-rs");
/// ```
#[inline]
pub fn uncompress(dest: &mut [u8], source: &[u8]) -> Result<usize, ZlibError> {
    uncompress2_to_buf(dest, source).map(|(produced, _consumed)| produced)
}

/// Decompress `source` into `dest`, returning `(produced, consumed)`.
///
/// Like [`uncompress`], but also reports how many input bytes were consumed —
/// useful when `source` may contain trailing data after the zlib stream (the
/// first unused byte is `source[consumed..]`). `dest` must be large enough to
/// hold the entire decompressed output.
///
/// This is the idiomatic counterpart of C `uncompress2`; it is re-exported at
/// the crate root as `zlib_rs::uncompress2`.
///
/// # Errors
///
/// See [`uncompress`].
///
/// # Examples
///
/// ```
/// use zlib_rs::uncompress2;
///
/// let packed = [
///     0x78, 0x9c, 0xab, 0xca, 0xc9, 0x4c, 0xd2, 0x2d, 0x2a, 0x06, 0x00, 0x0b, 0x58, 0x02, 0xc4,
/// ];
/// let mut out = [0u8; 16];
/// let (produced, consumed) = uncompress2(&mut out, &packed).unwrap();
/// assert_eq!(&out[..produced], b"zlib-rs");
/// assert_eq!(consumed, packed.len());
/// ```
#[inline]
pub fn uncompress2(dest: &mut [u8], source: &[u8]) -> Result<(usize, usize), ZlibError> {
    uncompress2_to_buf(dest, source)
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    // `use super::*` re-imports the parent module's own `use` items —
    // `ZlibError`, `Inflate`, and the `Z_*` constants it already imports — so
    // only the additional constants exercised by the `map_result` tests need to
    // be brought in here.
    use super::*;
    use crate::constants::{Z_DATA_ERROR, Z_MEM_ERROR, Z_STREAM_ERROR};

    // Canonical RFC 1950 zlib streams produced by stock zlib at level 6. Using
    // streams emitted by reference zlib — rather than this crate's own
    // `compress` — directly exercises the hard requirement that the decoder
    // accept ANY canonical-zlib stream (AAP §0.6.7) and keeps these tests
    // self-contained within this file's declared dependency set.

    /// Decompressed payload of [`SMALL_PACKED`].
    const SMALL_ORIG: &[u8] = b"zlib-rs";

    /// `zlib.compress(b"zlib-rs", 6)`.
    const SMALL_PACKED: [u8; 15] = [
        0x78, 0x9c, 0xab, 0xca, 0xc9, 0x4c, 0xd2, 0x2d, 0x2a, 0x06, 0x00, 0x0b, 0x58, 0x02, 0xc4,
    ];

    /// `zlib.compress(LARGE_ORIG, 6)`, where `LARGE_ORIG` is the 284-byte
    /// payload formed by repeating [`LARGE_PHRASE`] four times.
    const LARGE_PACKED: [u8; 77] = [
        0x78, 0x9c, 0xf3, 0x48, 0xcd, 0xc9, 0xc9, 0xd7, 0x51, 0xc8, 0x80, 0x50, 0x1e, 0xae, 0x3e,
        0x3e, 0xfe, 0x0a, 0x55, 0x39, 0x99, 0x49, 0x8a, 0x0a, 0x21, 0x19, 0xa9, 0x0a, 0x85, 0xa5,
        0x99, 0xc9, 0xd9, 0x0a, 0x49, 0x45, 0xf9, 0xe5, 0x79, 0x0a, 0x69, 0xf9, 0x15, 0x0a, 0x59,
        0xa5, 0xb9, 0x05, 0xc5, 0x0a, 0xf9, 0x65, 0xa9, 0x45, 0x0a, 0x25, 0x40, 0xe9, 0x9c, 0xc4,
        0xaa, 0x4a, 0x85, 0x94, 0xfc, 0x74, 0x3d, 0x05, 0x8f, 0xe1, 0x68, 0x0c, 0x00, 0xeb, 0xe5,
        0x61, 0x35,
    ];

    /// The 71-byte base phrase repeated four times to build the 284-byte
    /// payload encoded by [`LARGE_PACKED`].
    const LARGE_PHRASE: &[u8] =
        b"Hello, hello, HELLO zlib! The quick brown fox jumps over the lazy dog. ";

    // ---- map_result: exhaustive, dependency-free branch coverage ----------

    #[test]
    fn map_stream_end_is_ok_with_counts() {
        // Z_STREAM_END -> Ok((produced, consumed)); C maps this to Z_OK.
        assert_eq!(map_result(Z_STREAM_END, 15, 7, 32), Ok((7, 15)));
    }

    #[test]
    fn map_need_dict_is_data_error() {
        assert_eq!(map_result(Z_NEED_DICT, 2, 0, 32), Err(ZlibError::DataError));
    }

    #[test]
    fn map_ok_output_full_is_buf_error() {
        // Non-terminal Z_OK with the output buffer full == destination too small.
        assert_eq!(map_result(Z_OK, 3, 5, 5), Err(ZlibError::BufError));
    }

    #[test]
    fn map_ok_output_room_is_data_error() {
        // Non-terminal Z_OK with output room left == incomplete stream.
        assert_eq!(map_result(Z_OK, 10, 4, 32), Err(ZlibError::DataError));
    }

    #[test]
    fn map_buf_error_output_full_is_buf_error() {
        // Output buffer full -> destination too small (C returns raw Z_BUF_ERROR).
        assert_eq!(map_result(Z_BUF_ERROR, 3, 5, 5), Err(ZlibError::BufError));
    }

    #[test]
    fn map_buf_error_output_room_is_data_error() {
        // Output space remained -> incomplete stream (the C `len == 0` branch).
        assert_eq!(
            map_result(Z_BUF_ERROR, 15, 7, 32),
            Err(ZlibError::DataError)
        );
    }

    #[test]
    fn map_empty_destination_is_data_error() {
        // A zero-length destination is never treated as "full": a non-terminal
        // stop with `dest_len == 0` is an incomplete stream (e.g. empty input).
        assert_eq!(map_result(Z_BUF_ERROR, 0, 0, 0), Err(ZlibError::DataError));
    }

    #[test]
    fn map_other_codes_pass_through() {
        // The trailing `: err` branch — every remaining code is returned as-is.
        assert_eq!(
            map_result(Z_DATA_ERROR, 1, 0, 32),
            Err(ZlibError::DataError)
        );
        assert_eq!(map_result(Z_MEM_ERROR, 0, 0, 32), Err(ZlibError::MemError));
        assert_eq!(
            map_result(Z_STREAM_ERROR, 0, 0, 32),
            Err(ZlibError::StreamError)
        );
        // Defensive fallback keeps the mapping total for an out-of-range code.
        assert_eq!(map_result(999, 0, 0, 32), Err(ZlibError::DataError));
    }

    // ---- End-to-end: real `Inflate` over canonical zlib streams -----------

    #[test]
    fn round_trip_small() {
        let mut out = [0u8; 32];
        let produced = uncompress(&mut out, &SMALL_PACKED).unwrap();
        assert_eq!(&out[..produced], SMALL_ORIG);
    }

    #[test]
    fn round_trip_large() {
        let mut out = [0u8; 512];
        let produced = uncompress(&mut out, &LARGE_PACKED).unwrap();
        // The payload is the base phrase repeated four times (284 bytes).
        assert_eq!(produced, 284);
        for (i, &byte) in out[..produced].iter().enumerate() {
            assert_eq!(byte, LARGE_PHRASE[i % LARGE_PHRASE.len()]);
        }
    }

    #[test]
    fn uncompress2_reports_produced_and_consumed() {
        let mut out = [0u8; 32];
        let (produced, consumed) = uncompress2(&mut out, &SMALL_PACKED).unwrap();
        assert_eq!(produced, SMALL_ORIG.len());
        assert_eq!(consumed, SMALL_PACKED.len());
    }

    #[test]
    fn uncompress_to_buf_is_an_alias_of_uncompress2_to_buf() {
        let mut a = [0u8; 32];
        let mut b = [0u8; 32];
        assert_eq!(
            uncompress_to_buf(&mut a, &SMALL_PACKED),
            uncompress2_to_buf(&mut b, &SMALL_PACKED),
        );
    }

    #[test]
    fn truncated_stream_is_data_error() {
        // Drop the last five bytes (the Adler-32 trailer plus one payload byte).
        // The decoder consumes the whole truncated input without reaching
        // Z_STREAM_END, so the result is a data error (incomplete stream).
        let truncated = &LARGE_PACKED[..LARGE_PACKED.len() - 5];
        let mut out = [0u8; 512];
        assert_eq!(uncompress(&mut out, truncated), Err(ZlibError::DataError));
    }

    #[test]
    fn destination_too_small_is_buf_error() {
        // `SMALL_PACKED` decodes to seven bytes; a one-byte buffer cannot hold
        // it, so input remains and the result is a buffer error.
        let mut out = [0u8; 1];
        assert_eq!(
            uncompress(&mut out, &SMALL_PACKED),
            Err(ZlibError::BufError)
        );
    }

    #[test]
    fn empty_input_is_data_error() {
        let mut out = [0u8; 32];
        assert_eq!(uncompress(&mut out, &[]), Err(ZlibError::DataError));
    }

    #[test]
    fn garbage_input_is_data_error() {
        // A header whose `(CMF * 256 + FLG) % 31 != 0` check fails is rejected
        // outright by `inflate` with Z_DATA_ERROR.
        let mut out = [0u8; 32];
        assert_eq!(
            uncompress(&mut out, &[0xff, 0xff, 0xff, 0xff, 0xff, 0xff]),
            Err(ZlibError::DataError),
        );
    }
}
