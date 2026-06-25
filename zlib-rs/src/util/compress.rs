//! One-shot in-memory buffer compression (`compress.c`).
//!
//! This module is the safe-Rust port of the C `compress.c` translation unit. It
//! provides the three "convenience" entry points that compress an entire source
//! buffer into a destination buffer in a single call, without the caller ever
//! touching the streaming [`ZStream`] machinery directly:
//!
//! * [`compress_bound`] — the output-sizing helper (`compressBound` /
//!   `compressBound_z`, `compress.c` lines 91-99).
//! * [`compress2`] — compress at a caller-chosen level (`compress2` /
//!   `compress2_z`, `compress.c` lines 24-74).
//! * [`compress`] — compress at the library default level (`compress` /
//!   `compress_z`, `compress.c` lines 77-85).
//!
//! Together they back the public `compress` / `compress2` / `compressBound`
//! symbols that the thin `libz-rs-sys` FFI shim re-exposes with the exact C ABI.
//!
//! # Slice model vs. the C chunking loop
//!
//! The C `compress2_z` body runs a `do { … } while (err == Z_OK)` loop whose
//! sole purpose is to feed the compressor in `uInt`-sized (32-bit) chunks,
//! because the C `z_stream` `avail_in` / `avail_out` counters are only 32 bits
//! wide. Rust slices carry a full [`usize`] length, so [`crate::deflate::deflate`]
//! receives the *entire* input and output in one call. The loop therefore
//! collapses to "drive the compressor with [`Flush::Finish`] until it reports
//! [`ReturnCode::StreamEnd`]", with the iteration retained only as a safety net
//! for the (theoretical) case where the engine needs more than one pass.
//!
//! # Memory ownership (RAII)
//!
//! The C code pairs an explicit `deflateInit` with an explicit `deflateEnd` to
//! allocate and free the compressor state. Here the state lives in a [`Box`]
//! owned by the [`ZStream`], so dropping the local stream at the end of the
//! function performs the `deflateEnd` cleanup automatically — there is no manual
//! end call, and the leak / double-free classes are unrepresentable.
//!
//! [`Box`]: alloc::boxed::Box
//!
//! # Errors and the C return mapping
//!
//! Every helper returns [`Result<usize, ZlibError>`](crate::error::ZlibError);
//! the `Ok` payload is the number of bytes written to `dest`. This mirrors the C
//! contract `return err == Z_STREAM_END ? Z_OK : err;`: a finished stream is
//! success, and any negative status (`Z_STREAM_ERROR`, `Z_BUF_ERROR`,
//! `Z_MEM_ERROR`) is propagated unchanged. The C NULL / `destLen` validation is
//! subsumed by the slice model — Rust slices are non-null and carry their own
//! length, so those checks have no analogue here.
//!
//! # `no_std` and safety
//!
//! The module needs heap allocation (the compressor state) but not the standard
//! library, so it references only `core` / `alloc` and compiles unchanged under
//! `#![no_std]` (`cargo build --no-default-features`). It contains no `unsafe`,
//! consistent with the crate-wide `#![forbid(unsafe_code)]`.

use crate::constants::{Flush, Z_DEFAULT_COMPRESSION};
use crate::deflate;
use crate::error::{ReturnCode, ZlibError};
use crate::stream::ZStream;

/// Returns an upper bound on the compressed size of `source_len` bytes.
///
/// This mirrors the C `compressBound()` / `compressBound_z()`
/// (`compress.c` lines 91-99), which computes
///
/// ```text
/// bound = source_len + (source_len >> 12) + (source_len >> 14)
///                     + (source_len >> 25) + 13
/// ```
///
/// and saturates to `(z_size_t)-1` if that sum overflows. This is the
/// *wrap-inclusive* `+ 13` form appropriate to the one-shot [`compress2`]
/// path, which always wraps its output in a zlib (RFC 1950) container at the
/// default `windowBits` / `memLevel`. It is deliberately distinct from — and
/// smaller in scope than — the general `deflateBound` formula, which accounts
/// for arbitrary wrappers and headers and therefore lives with the compressor
/// in [`crate::deflate`].
///
/// # Overflow
///
/// The computation uses [`usize::saturating_add`] rather than `+`. Plain
/// addition panics on overflow in debug builds, whereas saturating to
/// [`usize::MAX`] reproduces the C behavior of returning `(z_size_t)-1` on
/// overflow without panicking. Saturating at any intermediate step is
/// equivalent to the C overflow check, because a partial sum can only saturate
/// once it has already exceeded [`usize::MAX`], which guarantees the full sum
/// would have overflowed as well.
#[must_use]
pub fn compress_bound(source_len: usize) -> usize {
    source_len
        .saturating_add(source_len >> 12)
        .saturating_add(source_len >> 14)
        .saturating_add(source_len >> 25)
        .saturating_add(13)
}

/// Compresses `source` into `dest` at the given compression `level`, returning
/// the number of bytes written to `dest`.
///
/// This mirrors the C `compress2()` / `compress2_z()`
/// (`compress.c` lines 24-74). `level` carries the same meaning as in
/// `deflateInit`: `0`-`9`, or [`Z_DEFAULT_COMPRESSION`] (`-1`) for the library
/// default. The compressor is driven to completion with [`Flush::Finish`] and
/// the resulting stream is a zlib (RFC 1950) container.
///
/// `dest` must be large enough to hold the compressed output; size it with
/// [`compress_bound`]. If `dest` is too small the compressor cannot make
/// progress and [`ZlibError::BufError`] is returned, matching the C
/// `Z_BUF_ERROR`.
///
/// # Errors
///
/// * [`ZlibError::StreamError`] — `level` is out of range (rejected by the
///   deflate initializer, exactly as the C code returns `Z_STREAM_ERROR`).
/// * [`ZlibError::BufError`] — `dest` was too small to hold the full output.
/// * [`ZlibError::MemError`] — the compressor state could not be allocated.
pub fn compress2(dest: &mut [u8], source: &[u8], level: i32) -> Result<usize, ZlibError> {
    // C `deflateInit(&stream, level)` with the default method / windowBits /
    // memLevel / strategy. RAII (the `Drop` on the `Box`ed state owned by
    // `strm`) replaces the explicit C `deflateEnd`. Init errors (an invalid
    // `level` becomes `Z_STREAM_ERROR`) propagate via `?`.
    let mut strm = ZStream::new();
    deflate::deflate_init(&mut strm, level)?;

    // Cursors into `source` (consumed input) and `dest` (produced output),
    // standing in for the C `next_in - source` / `next_out - dest` pointer
    // arithmetic. They only ever advance, and never past the slice length.
    let mut in_pos = 0usize;
    let mut out_pos = 0usize;

    // The whole input is available at once (the slice model), so the C
    // `sourceLen ? Z_NO_FLUSH : Z_FINISH` selector is always `Z_FINISH`.
    loop {
        let (rc, consumed, produced) = deflate::deflate(
            &mut strm,
            &source[in_pos..],
            &mut dest[out_pos..],
            Flush::Finish,
        );
        in_pos += consumed;
        out_pos += produced;

        match rc {
            // All input compressed and the trailer flushed: done. The C loop's
            // `while (err == Z_OK)` exits here and maps `Z_STREAM_END -> Z_OK`.
            Ok(ReturnCode::StreamEnd) => break,
            // Any engine error (`Z_STREAM_ERROR` / `Z_BUF_ERROR` / `Z_MEM_ERROR`)
            // is propagated unchanged, matching C `return … : err;`.
            Err(e) => return Err(e),
            // `Z_OK`: more work remains, so loop again with advanced cursors.
            // Guard against a stall — no input consumed *and* no output produced
            // means `dest` is too small to make progress (`Z_BUF_ERROR`).
            Ok(_) => {
                if consumed == 0 && produced == 0 {
                    return Err(ZlibError::BufError);
                }
            }
        }
    }

    // `strm` is dropped here, freeing the compressor state (RAII `deflateEnd`).
    Ok(out_pos)
}

/// Compresses `source` into `dest` at the library default compression level,
/// returning the number of bytes written to `dest`.
///
/// This mirrors the C `compress()` / `compress_z()` (`compress.c` lines 77-85),
/// which delegate to `compress2` with [`Z_DEFAULT_COMPRESSION`]. See
/// [`compress2`] for the full contract, sizing guidance, and error semantics.
///
/// # Errors
///
/// Propagates the errors documented on [`compress2`].
pub fn compress(dest: &mut [u8], source: &[u8]) -> Result<usize, ZlibError> {
    compress2(dest, source, Z_DEFAULT_COMPRESSION)
}

#[cfg(test)]
mod tests {
    use super::*;

    // The one-shot helpers allocate the compressor state via `Box`, so the
    // tests need an allocator; `alloc::vec!` keeps them `no_std`-compatible.

    #[test]
    fn compress_bound_zero_is_thirteen() {
        // 0 + (0>>12) + (0>>14) + (0>>25) + 13 == 13.
        assert_eq!(compress_bound(0), 13);
    }

    #[test]
    fn compress_bound_small_adds_only_constant() {
        // 100 + 0 + 0 + 0 + 13 == 113 (all the shifts are zero for small input).
        assert_eq!(compress_bound(100), 113);
    }

    #[test]
    fn compress_bound_saturates_without_panicking() {
        // C returns `(z_size_t)-1` on overflow; the saturating form returns
        // `usize::MAX` and, crucially, does not panic in debug builds.
        assert_eq!(compress_bound(usize::MAX), usize::MAX);
    }

    #[test]
    fn compress2_smoke_within_bound() {
        // A repetitive, highly compressible buffer.
        let source: &[u8] = b"zlib-rs zlib-rs zlib-rs zlib-rs zlib-rs zlib-rs!";
        let bound = compress_bound(source.len());
        let mut dest = alloc::vec![0u8; bound];

        let written = compress2(&mut dest, source, 6).expect("compress2 should succeed");
        assert!(written > 0, "compressed output must be non-empty");
        assert!(
            written <= bound,
            "compressed size {written} must not exceed bound {bound}"
        );
    }

    #[test]
    fn compress_default_level_smoke_within_bound() {
        let source: &[u8] = b"The quick brown fox jumps over the lazy dog.";
        let bound = compress_bound(source.len());
        let mut dest = alloc::vec![0u8; bound];

        let written = compress(&mut dest, source).expect("compress should succeed");
        assert!(written > 0);
        assert!(written <= bound);
    }

    #[test]
    fn compress2_empty_input_produces_valid_empty_stream() {
        // Compressing nothing still emits the zlib header + empty block +
        // Adler-32 trailer, which comfortably fits in `compress_bound(0)`.
        let source: &[u8] = b"";
        let bound = compress_bound(source.len());
        let mut dest = alloc::vec![0u8; bound];

        let written = compress2(&mut dest, source, 6).expect("empty compress should succeed");
        assert!(written > 0);
        assert!(written <= bound);
    }

    #[test]
    fn compress2_destination_too_small_is_buf_error() {
        // A zero-length destination cannot hold even the header, so the engine
        // makes no progress and the C `Z_BUF_ERROR` is reported.
        let source: &[u8] = b"some data that cannot fit anywhere";
        let mut dest: [u8; 0] = [];
        assert_eq!(compress2(&mut dest, source, 6), Err(ZlibError::BufError));
    }
}
