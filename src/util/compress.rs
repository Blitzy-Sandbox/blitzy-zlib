//! One-shot DEFLATE compression helpers — the safe-Rust port of zlib's
//! `compress.c` (AAP §0.4.1).
//!
//! This module provides the convenience entry points for compressing a buffer
//! in a single call, mirroring the C `compress`, `compress2`, and
//! `compressBound` functions:
//!
//! | This module                       | C source        | Role                                        |
//! |-----------------------------------|-----------------|---------------------------------------------|
//! | [`compress`] / [`compress2`]      | `compress.c`    | idiomatic: compress into a fresh [`Vec<u8>`]|
//! | [`compress_to_buf`] / [`compress2_to_buf`] | `compress.c` | C-semantics core: compress into a caller buffer |
//! | [`compress_bound`]                | `compress.c`    | upper bound on the compressed size          |
//!
//! All four compression helpers drive [`crate::deflate::Deflate`], the safe
//! DEFLATE engine, with a single [`FlushMode::Finish`] call. Because the engine
//! reproduces C zlib's LZ77 match selection and Huffman code construction
//! exactly, the emitted bytes are **byte-identical to C zlib** for the same
//! input, level, strategy, and window (AAP §0.7.1). The single-call `Finish`
//! path is equivalent to C `compress2`'s chunked loop: C only chunks because
//! its `avail_*` counters are 32-bit `uInt`s, which is an artifact of the C ABI
//! rather than a difference in the produced stream.
//!
//! # Public surface
//!
//! `src/lib.rs` re-exports [`compress`], [`compress2`], and [`compress_bound`]
//! as the crate's public API (`zlib_rs::{compress, compress2, compress_bound}`),
//! and `src/ffi.rs` builds its `extern "C"` `compress` / `compress2` /
//! `compressBound` shims on top of the C-semantics cores defined here.
//!
//! # Constraints
//!
//! * **100% safe Rust** — there is no `unsafe` in this file (AAP §0.6.2).
//! * **`no_std`-clean** — `core` + `alloc` only; the idiomatic API returns an
//!   `alloc`-backed [`Vec<u8>`] and never touches `std`.
//! * **Exact `compressBound` formula** with overflow saturation (AAP §0.6.1).

use crate::constants::{FlushMode, Z_DEFAULT_COMPRESSION};
use crate::deflate::Deflate;
use crate::error::{ReturnCode, ZlibError};
use alloc::vec::Vec;

/// Upper bound on the number of bytes required to hold the DEFLATE-compressed
/// form of `source_len` input bytes — the safe-Rust port of C `compressBound`.
///
/// The bound is computed by the exact zlib formula
///
/// ```text
/// source_len + (source_len >> 12) + (source_len >> 14) + (source_len >> 25) + 13
/// ```
///
/// which accounts for the worst-case per-block expansion of incompressible data
/// (the 5-byte stored-block headers emitted roughly every 16 KiB / 64 KiB) plus
/// the constant zlib wrapper and final-block overhead. The result is suitable as
/// the output-buffer size for a single-call [`compress`] / [`compress2_to_buf`]:
/// a buffer of this size is always large enough for the whole stream, so the
/// compressor finishes in one call.
///
/// On arithmetic overflow the function saturates to [`usize::MAX`], matching C's
/// `compressBound`, which returns `(z_size_t)-1` in that case. The internal
/// additions use [`usize::wrapping_add`] so the overflow is well-defined and the
/// final `bound < source_len` test reliably detects it.
///
/// This is a `const fn`, so it can size buffers in `const`/`static` contexts.
///
/// # Examples
///
/// ```
/// use zlib_rs::util::compress::compress_bound;
///
/// assert_eq!(compress_bound(0), 13);
/// // The bound is always at least as large as the input.
/// assert!(compress_bound(1_000) >= 1_000);
/// ```
#[must_use]
pub const fn compress_bound(source_len: usize) -> usize {
    // bound = source_len + (source_len>>12) + (source_len>>14) + (source_len>>25) + 13
    // `wrapping_add` makes any overflow wrap (rather than panic in debug), so
    // the `bound < source_len` comparison below detects it exactly as C's
    // `bound < sourceLen ? (z_size_t)-1 : bound`.
    let bound = source_len
        .wrapping_add(source_len >> 12)
        .wrapping_add(source_len >> 14)
        .wrapping_add(source_len >> 25)
        .wrapping_add(13);
    if bound < source_len {
        // Overflowed: saturate to the maximum, mirroring C's `(z_size_t)-1`.
        usize::MAX
    } else {
        bound
    }
}

/// Compress `source` into the caller-provided `dest` buffer at the given
/// `level`, returning the number of bytes written — the C-semantics core that
/// mirrors C `compress2` (`compress2_z`).
///
/// `level` follows `deflateInit` semantics: `0..=9`, or
/// [`Z_DEFAULT_COMPRESSION`] (`-1`) which resolves to level 6. The output is the
/// zlib (RFC 1950) wrapped stream, identical to C `compress2`.
///
/// `dest` should be sized with [`compress_bound`]; given such a buffer the whole
/// stream is produced in a single [`FlushMode::Finish`] call. This function is
/// the building block for the `extern "C"` `compress2` shim in `src/ffi.rs`,
/// which converts the returned [`Result`] into the C integer return code.
///
/// # Errors
///
/// * [`ZlibError::StreamError`] — `level` is outside `-1..=9` (propagated from
///   [`Deflate::new`]).
/// * [`ZlibError::BufError`] — `dest` was too small to hold the complete stream
///   (the compressor did not reach `StreamEnd`), matching C `compress2`
///   returning a code other than `Z_STREAM_END`.
///
/// # Examples
///
/// ```
/// use zlib_rs::util::compress::compress2_to_buf;
///
/// let data = b"abracadabra abracadabra";
/// // `compress_bound(23) == 36`, so a 64-byte buffer is ample for one call.
/// let mut dest = [0u8; 64];
/// let n = compress2_to_buf(&mut dest, data, 6).unwrap();
/// assert!(n >= 2);
/// assert_eq!(dest[0], 0x78); // zlib CMF byte (windowBits 15)
/// ```
pub fn compress2_to_buf(dest: &mut [u8], source: &[u8], level: i32) -> Result<usize, ZlibError> {
    // `deflateInit(&stream, level)` — invalid levels surface as StreamError.
    let mut deflate = Deflate::new(level)?;
    // Single `Z_FINISH` over the whole input/output. For a `compress_bound`-sized
    // `dest` this completes the stream in one call (StreamEnd); for a short
    // `dest` it stops with the buffer full (code != StreamEnd).
    let outcome = deflate.compress(source, dest, FlushMode::Finish)?;
    match outcome.code {
        // C: `err == Z_STREAM_END ? Z_OK : err` — completed successfully.
        ReturnCode::StreamEnd => Ok(outcome.produced),
        // Not finished => the output buffer was too small.
        _ => Err(ZlibError::BufError),
    }
    // `deflate` is dropped here — the RAII equivalent of C `deflateEnd`.
}

/// Compress `source` into `dest` at the default compression level, returning the
/// number of bytes written — the C-semantics core that mirrors C `compress`
/// (`compress_z`, i.e. [`compress2_to_buf`] with [`Z_DEFAULT_COMPRESSION`]).
///
/// See [`compress2_to_buf`] for buffer-sizing guidance and the full error
/// contract.
///
/// # Errors
///
/// [`ZlibError::BufError`] if `dest` is too small to hold the complete stream.
/// (The default level is always valid, so `StreamError` cannot occur here.)
pub fn compress_to_buf(dest: &mut [u8], source: &[u8]) -> Result<usize, ZlibError> {
    compress2_to_buf(dest, source, Z_DEFAULT_COMPRESSION)
}

/// Compress `source` at `level`, returning a freshly allocated buffer holding
/// the zlib (RFC 1950) stream — the idiomatic Rust counterpart of C
/// `compress2`.
///
/// `level` is `0..=9` or [`Z_DEFAULT_COMPRESSION`] (`-1`, resolving to level 6).
/// The output buffer is sized up front with [`compress_bound`], so the whole
/// stream is produced in a single call and is then trimmed to the exact
/// compressed length — this path can never return [`ZlibError::BufError`].
///
/// # Errors
///
/// [`ZlibError::StreamError`] if `level` is outside `-1..=9`.
///
/// # Examples
///
/// ```
/// use zlib_rs::util::compress::compress2;
///
/// let packed = compress2(b"hello hello hello hello", 9).unwrap();
/// assert!(!packed.is_empty());
/// assert_eq!(packed[0], 0x78);
/// ```
pub fn compress2(source: &[u8], level: i32) -> Result<Vec<u8>, ZlibError> {
    // Size the destination so a single `Finish` always completes the stream.
    let mut dest = alloc::vec![0u8; compress_bound(source.len())];
    let produced = compress2_to_buf(&mut dest, source, level)?;
    // Trim the over-allocated tail down to the exact compressed length.
    dest.truncate(produced);
    Ok(dest)
}

/// Compress `source` at the default compression level, returning a freshly
/// allocated buffer holding the zlib (RFC 1950) stream — the idiomatic Rust
/// counterpart of C `compress`.
///
/// Equivalent to [`compress2`] with [`Z_DEFAULT_COMPRESSION`]. Because the
/// default level is always valid and the output is sized via [`compress_bound`],
/// this function is effectively infallible for any in-memory input; it returns
/// a [`Result`] for symmetry with [`compress2`] and the rest of the API.
///
/// # Errors
///
/// This function does not fail for valid in-memory input; the [`Result`] mirrors
/// [`compress2`]'s signature.
///
/// # Examples
///
/// ```
/// use zlib_rs::util::compress::compress;
///
/// let packed = compress(b"hello hello hello").unwrap();
/// assert!(!packed.is_empty());
/// ```
pub fn compress(source: &[u8]) -> Result<Vec<u8>, ZlibError> {
    compress2(source, Z_DEFAULT_COMPRESSION)
}

#[cfg(test)]
mod tests {
    use super::{compress, compress_bound, compress_to_buf, compress2, compress2_to_buf};
    use crate::error::ZlibError;

    // Compile-time proof that `compress_bound` is a usable `const fn`.
    const _: () = assert!(compress_bound(0) == 13);

    #[test]
    fn compress_bound_zero_is_thirteen() {
        // Empty input still carries the constant wrapper/overhead term.
        assert_eq!(compress_bound(0), 13);
    }

    #[test]
    fn compress_bound_matches_exact_formula() {
        // For a moderate, non-overflowing size the result must equal the exact
        // zlib formula term-for-term.
        let n: usize = 1_000_000;
        let expected = n + (n >> 12) + (n >> 14) + (n >> 25) + 13;
        assert_eq!(compress_bound(n), expected);

        // Spot-check a few more sizes against the formula.
        for &n in &[1usize, 4096, 12_345, 1usize << 20, 1usize << 30] {
            let expected = n + (n >> 12) + (n >> 14) + (n >> 25) + 13;
            assert_eq!(compress_bound(n), expected, "mismatch at n={n}");
        }
    }

    #[test]
    fn compress_bound_saturates_on_overflow() {
        // C returns `(z_size_t)-1` when the bound overflows; we saturate.
        assert_eq!(compress_bound(usize::MAX), usize::MAX);
    }

    #[test]
    fn compress_default_round_trips_through_uncompress() {
        let original: &[u8] = b"the quick brown fox jumps over the lazy dog; the quick brown fox.";
        let packed = compress(original).expect("compress should succeed");
        assert!(!packed.is_empty());
        // zlib (RFC 1950) CMF byte for windowBits 15 is 0x78.
        assert_eq!(packed[0], 0x78);
        // The compressed form must be smaller than the original for this input.
        assert!(packed.len() < original.len());

        // True round-trip through the crate's own inflate engine.
        let mut restored = alloc::vec![0u8; original.len()];
        let produced =
            crate::util::uncompress(&mut restored, &packed).expect("uncompress should succeed");
        assert_eq!(produced, original.len());
        assert_eq!(&restored[..produced], original);
    }

    #[test]
    fn compress2_levels_round_trip() {
        let data: &[u8] = b"aaaaaaaaaabbbbbbbbbbccccccccccddddddddddeeeeeeeeee";
        for level in [0, 1, 6, 9] {
            let packed = compress2(data, level).expect("valid level compresses");
            assert!(!packed.is_empty(), "level {level} produced empty output");
            assert_eq!(packed[0], 0x78, "level {level} missing zlib header");

            let mut restored = alloc::vec![0u8; data.len()];
            let produced =
                crate::util::uncompress(&mut restored, &packed).expect("round-trip uncompress");
            assert_eq!(
                &restored[..produced],
                data,
                "level {level} round-trip mismatch"
            );
        }
    }

    #[test]
    fn compress2_invalid_level_is_stream_error() {
        // An out-of-range level is rejected by `Deflate::new` and propagated.
        let data: &[u8] = b"payload";
        assert_eq!(compress2(data, 42), Err(ZlibError::StreamError));
        assert_eq!(compress2(data, -2), Err(ZlibError::StreamError));
    }

    #[test]
    fn compress_to_buf_too_small_is_buf_error() {
        // A 1-byte output cannot hold even the 2-byte zlib header, so the core
        // reports BufError (incomplete) — mirroring C `compress2` returning a
        // code other than `Z_STREAM_END`.
        let data: &[u8] = b"some data that will not fit in a single byte";
        let mut tiny = [0u8; 1];
        assert_eq!(compress_to_buf(&mut tiny, data), Err(ZlibError::BufError));
    }

    #[test]
    fn compress2_to_buf_into_bound_sized_buffer_completes() {
        let data: &[u8] = b"compress into an exactly-bounded buffer of the right size";
        let mut dest = alloc::vec![0u8; compress_bound(data.len())];
        let produced =
            compress2_to_buf(&mut dest, data, 6).expect("a bound-sized buffer completes");
        assert!(produced >= 2 && produced <= dest.len());
        assert_eq!(dest[0], 0x78);

        // Confirm the produced prefix is a valid, decodable stream.
        let mut restored = alloc::vec![0u8; data.len()];
        let n = crate::util::uncompress(&mut restored, &dest[..produced]).unwrap();
        assert_eq!(&restored[..n], data);
    }

    #[test]
    fn compress_empty_input_round_trips() {
        // Compressing an empty buffer yields a valid (small) zlib stream that
        // inflates back to nothing.
        let packed = compress(b"").expect("empty input compresses");
        assert!(!packed.is_empty());
        assert_eq!(packed[0], 0x78);
        let mut restored = [0u8; 0];
        let n = crate::util::uncompress(&mut restored, &packed).expect("empty round-trip");
        assert_eq!(n, 0);
    }
}
