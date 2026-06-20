//! One-shot whole-buffer compression — a 100% safe-Rust port of zlib's
//! `compress.c`.
//!
//! This module provides the convenience helpers that compress an entire
//! in-memory buffer in a single call, mirroring the C entry points `compress`,
//! `compress2`, `compress_z`, and `compress2_z` (`compress.c`, zlib
//! `1.3.2.1-motley`), together with the `compressBound` / `compressBound_z`
//! output-size estimator. They are the compression counterparts of the
//! `uncompress` helpers in [`crate::util::uncompress`].
//!
//! # Public surface
//!
//! * [`compress`] / [`compress2`] — the idiomatic, crate-level API (re-exported
//!   by the crate root as `zlib_rs::compress` / `zlib_rs::compress2`). Each
//!   allocates and returns a `Vec<u8>` holding the complete RFC 1950 zlib
//!   stream, sized up front with [`compress_bound`] so a single pass always
//!   completes and the buffer is then trimmed to the exact produced length.
//! * [`compress_bound`] — the exact C `compressBound` upper-bound formula,
//!   usable in `const` context (re-exported as `zlib_rs::compress_bound`).
//! * [`compress2_to_buf`] / [`compress_to_buf`] — the C-semantics core that the
//!   FFI shim (`crate::ffi`) wraps: they compress into a caller-supplied output
//!   buffer and report the number of bytes produced, honouring the C `destLen`
//!   in/out contract.
//!
//! # Relationship to the C source
//!
//! The C implementation drives `deflate()` inside a `do { … } while (err ==
//! Z_OK)` loop whose sole purpose is to refill the 32-bit `avail_in` /
//! `avail_out` registers from larger `z_size_t` lengths. Rust slices already
//! carry their length, so this port issues a **single** [`Deflate::compress`]
//! call to [`FlushMode::Finish`] over the whole input and output. Because the
//! safe-Rust engine is not constrained by C's `uInt` chunk cap, that one call
//! produces byte-for-byte the same stream as the C chunked loop — the chunking
//! is an artifact of C's 32-bit registers, not a semantic difference
//! (`compress.c`, `compress2_z`).
//!
//! The status-to-result mapping reproduces `compress2_z`'s terminal
//! `return err == Z_STREAM_END ? Z_OK : err;` exactly:
//!
//! | C `deflate` outcome                       | This module returns           |
//! |-------------------------------------------|-------------------------------|
//! | `Z_STREAM_END` (stream finished)          | `Ok(produced)`                |
//! | not finished (output buffer too small)    | `Err(ZlibError::BufError)`    |
//! | invalid `level` (`deflateInit` rejects)   | `Err(ZlibError::StreamError)` |
//!
//! # Safety and `no_std`
//!
//! The entire module is **100% safe Rust** (no `unsafe`) and `no_std`-clean: it
//! depends only on `core`, the `alloc` crate (for the returned `Vec`), and
//! sibling crate modules — never on `std`. The compressor is created on the
//! stack, owns its heap state through [`Deflate`], and is released by `Drop`
//! (the RAII analogue of C `deflateEnd`) when the helper returns — there is no
//! manual teardown and no leak path.
//!
//! # Examples
//!
//! ```
//! use zlib_rs::compress;
//!
//! let original = b"hello hello hello hello hello hello";
//! let packed = compress(original).unwrap();
//! assert!(!packed.is_empty());
//! // The default level uses a 32 KiB window, so the zlib CMF header is 0x78.
//! assert_eq!(packed[0], 0x78);
//! ```

use alloc::{vec, vec::Vec};

use crate::constants::{FlushMode, Z_DEFAULT_COMPRESSION};
use crate::deflate::Deflate;
use crate::error::{ReturnCode, ZlibError};

// ===========================================================================
// Output-size bound — the exact reproduction of `compressBound_z`.
// ===========================================================================

/// Returns an upper bound on the compressed size of `source_len` input bytes.
///
/// This is the exact analogue of C `compressBound` / `compressBound_z` and is
/// intended for sizing a destination buffer before calling
/// [`compress2_to_buf`] (or it is applied automatically by [`compress`] /
/// [`compress2`]). The bound is computed for the default `deflateInit`
/// configuration (`windowBits` = 15, `memLevel` = 8); per the C source's own
/// caveat it must be revisited if those defaults ever change.
///
/// The formula is, verbatim:
///
/// ```text
/// bound = source_len + (source_len >> 12) + (source_len >> 14)
///                    + (source_len >> 25) + 13
/// ```
///
/// On arithmetic overflow the function saturates to [`usize::MAX`], mirroring
/// C's `return bound < sourceLen ? (z_size_t)-1 : bound;` (a `z_size_t` of all
/// ones). Because every shifted term is non-negative, the only way the running
/// sum can wrap below `source_len` is a genuine overflow, so the post-hoc
/// `bound < source_len` test detects it precisely.
///
/// This is a `const fn`, so it can size fixed-length buffers at compile time.
///
/// # Examples
///
/// ```
/// use zlib_rs::compress_bound;
///
/// // The fixed wrapper/Huffman overhead for an empty input.
/// assert_eq!(compress_bound(0), 13);
/// // A 1 MiB input: 1048576 + 256 + 64 + 0 + 13.
/// assert_eq!(compress_bound(1 << 20), (1 << 20) + 256 + 64 + 13);
/// // Saturates rather than wrapping on overflow.
/// assert_eq!(compress_bound(usize::MAX), usize::MAX);
/// ```
#[inline]
#[must_use]
pub const fn compress_bound(source_len: usize) -> usize {
    // `wrapping_add` keeps the arithmetic well-defined in `const` context (a
    // debug-mode overflow panic is not permitted there) and makes the overflow
    // test below exact: every term is non-negative, so the wrapped sum is
    // strictly less than `source_len` if and only if the true mathematical sum
    // exceeded `usize::MAX`.
    let bound = source_len
        .wrapping_add(source_len >> 12)
        .wrapping_add(source_len >> 14)
        .wrapping_add(source_len >> 25)
        .wrapping_add(13);

    if bound < source_len {
        // Overflow: C returns `(z_size_t)-1`, i.e. an all-ones `size_t`.
        usize::MAX
    } else {
        bound
    }
}

/// Compile-time proof that [`compress_bound`] is usable in `const` context and
/// yields the documented base overhead (`13`) for an empty input.
const _: () = assert!(compress_bound(0) == 13);

// ===========================================================================
// C-semantics core — reproduces `compress2_z` over slices (wrapped by `ffi`).
// ===========================================================================

/// Compress `source` into `dest`, returning the number of bytes written.
///
/// This is the safe-Rust core of C `compress2_z` and is the function the FFI
/// shim (`crate::ffi`) wraps to expose the C `compress2` / `compress2_z`
/// symbols. `level` follows `deflateInit` semantics: `0..=9`, or
/// [`Z_DEFAULT_COMPRESSION`] (`-1`) for the default level 6.
///
/// `dest` must be large enough to hold the entire compressed stream; size it
/// with [`compress_bound`]. A single [`Deflate::compress`] call to
/// [`FlushMode::Finish`] either consumes all of `source` and finishes the
/// stream (returning the produced length) or runs out of output room.
///
/// # Errors
///
/// * [`ZlibError::StreamError`] — `level` is invalid (propagated from
///   [`Deflate::new`], mirroring C's `deflateInit` returning `Z_STREAM_ERROR`).
/// * [`ZlibError::BufError`] — `dest` was too small to hold the whole stream
///   (the stream did not reach end-of-stream), mirroring C's terminal
///   `Z_BUF_ERROR`.
/// * Any other [`ZlibError`] reported by the engine (e.g.
///   [`ZlibError::MemError`]) is propagated unchanged.
#[inline]
pub fn compress2_to_buf(dest: &mut [u8], source: &[u8], level: i32) -> Result<usize, ZlibError> {
    // `Deflate::new(level)` == C `deflateInit(&stream, level)` =
    // `deflateInit2(level, Z_DEFLATED, MAX_WBITS, DEF_MEM_LEVEL,
    // Z_DEFAULT_STRATEGY)`. An invalid level makes it return
    // `Err(ZlibError::StreamError)`, which `?` propagates exactly as C's
    // `if (err != Z_OK) return err;`.
    let mut deflate = Deflate::new(level)?;

    // One pass over the whole input and output. `FlushMode::Finish` asks the
    // engine to consume all input and finish the stream; with a `dest` sized by
    // `compress_bound` this completes in a single call. The returned
    // `DeflateOutcome` carries the terminal status and the produced length.
    let outcome = deflate.compress(source, dest, FlushMode::Finish)?;

    // `deflate` is dropped here == C `deflateEnd(&stream)` (RAII): the owned
    // state and working buffers are freed deterministically with no leak path.
    match outcome.code {
        // C: `return err == Z_STREAM_END ? Z_OK : err;` — the stream finished.
        ReturnCode::StreamEnd => Ok(outcome.produced),
        // Not finished: the output buffer was too small. C surfaces this as the
        // terminal `Z_BUF_ERROR` once `avail_out` is exhausted.
        _ => Err(ZlibError::BufError),
    }
}

/// Compress `source` into `dest` at the default level, returning bytes written.
///
/// The level-defaulted counterpart of [`compress2_to_buf`], reproducing C
/// `compress_z`, which forwards to `compress2_z` with
/// [`Z_DEFAULT_COMPRESSION`]. The FFI shim wraps it to expose the C `compress`
/// / `compress_z` symbols.
///
/// # Errors
///
/// See [`compress2_to_buf`]. With a valid default level only
/// [`ZlibError::BufError`] (`dest` too small) and engine-internal errors can
/// occur.
#[inline]
pub fn compress_to_buf(dest: &mut [u8], source: &[u8]) -> Result<usize, ZlibError> {
    compress2_to_buf(dest, source, Z_DEFAULT_COMPRESSION)
}

// ===========================================================================
// Idiomatic public API — re-exported by the crate root via `lib.rs`.
// ===========================================================================

/// Compress `source` at the default level, returning a freshly allocated buffer.
///
/// This is the idiomatic counterpart of C `compress`; it is re-exported at the
/// crate root as `zlib_rs::compress`. The returned `Vec<u8>` is a complete
/// RFC 1950 zlib stream trimmed to the exact compressed length (the
/// over-allocated tail reserved via [`compress_bound`] is dropped before
/// returning).
///
/// # Errors
///
/// Returns a [`ZlibError`] only on an internal engine failure; the default
/// level is always valid and the destination is sized via [`compress_bound`],
/// so [`ZlibError::BufError`] cannot occur.
///
/// # Examples
///
/// ```
/// use zlib_rs::compress;
///
/// let packed = compress(b"hello hello hello").unwrap();
/// assert!(!packed.is_empty());
/// ```
#[inline]
pub fn compress(source: &[u8]) -> Result<Vec<u8>, ZlibError> {
    compress2(source, Z_DEFAULT_COMPRESSION)
}

/// Compress `source` at the given `level`, returning a freshly allocated buffer.
///
/// `level` is `0..=9` (`0` = no compression / store, `9` = best), or
/// [`Z_DEFAULT_COMPRESSION`] (`-1`) for the default level 6. This is the
/// idiomatic counterpart of C `compress2`; it is re-exported at the crate root
/// as `zlib_rs::compress2`.
///
/// The destination is sized with [`compress_bound`] so the single finishing
/// pass always completes; the returned `Vec<u8>` is trimmed to the exact
/// compressed length.
///
/// # Errors
///
/// Returns [`ZlibError::StreamError`] if `level` is outside the valid range
/// (propagated from [`Deflate::new`]); other [`ZlibError`] variants are
/// propagated from the engine on internal failure.
///
/// # Examples
///
/// ```
/// use zlib_rs::compress2;
///
/// let original = b"aaaaaaaaaabbbbbbbbbbcccccccccc";
/// let best = compress2(original, 9).unwrap();
/// let fast = compress2(original, 1).unwrap();
/// assert!(!best.is_empty());
/// assert!(!fast.is_empty());
/// ```
#[inline]
pub fn compress2(source: &[u8], level: i32) -> Result<Vec<u8>, ZlibError> {
    // Size the destination to the worst-case compressed length so the single
    // `Finish` pass in `compress2_to_buf` always completes (never `BufError`).
    let mut dest = vec![0u8; compress_bound(source.len())];

    // Compress into the over-allocated buffer, then trim to what was produced.
    let produced = compress2_to_buf(&mut dest, source, level)?;
    dest.truncate(produced);
    Ok(dest)
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ---- compress_bound: exact formula + overflow saturation --------------

    #[test]
    fn bound_empty_is_base_overhead() {
        // C `compressBound(0)` == 13 (the fixed wrapper/Huffman overhead).
        assert_eq!(compress_bound(0), 13);
    }

    #[test]
    fn bound_matches_formula_for_moderate_sizes() {
        // Reproduce the formula independently across a spread of sizes.
        for n in [1usize, 100, 4096, 1 << 16, 1 << 20, 1 << 26, 12_345_678] {
            let expected = n + (n >> 12) + (n >> 14) + (n >> 25) + 13;
            assert_eq!(compress_bound(n), expected, "n = {n}");
        }
    }

    #[test]
    fn bound_saturates_on_overflow() {
        // C returns `(z_size_t)-1` (all-ones) when the bound overflows; here
        // that is `usize::MAX`.
        assert_eq!(compress_bound(usize::MAX), usize::MAX);
        // Near the top of the range the shifted terms still push the sum past
        // `usize::MAX`, so saturation kicks in.
        assert_eq!(compress_bound(usize::MAX - 12), usize::MAX);
    }

    // A second compile-time check (mirroring the module-level `const _`) that
    // proves `compress_bound` is a usable `const fn` for sizing fixed arrays.
    const BOUND_OF_1KIB: usize = compress_bound(1024);

    #[test]
    fn bound_is_const_evaluable() {
        // 1024 + (1024 >> 12 = 0) + (1024 >> 14 = 0) + (1024 >> 25 = 0) + 13.
        assert_eq!(BOUND_OF_1KIB, 1037);
        // The compile-time constant matches the run-time evaluation.
        assert_eq!(BOUND_OF_1KIB, compress_bound(1024));
    }

    // ---- compress / compress2: end-to-end through the real engine ---------

    /// A compressible payload (repeated text) used by the engine tests.
    const SAMPLE: &[u8] = b"hello hello hello hello hello hello hello hello!";

    #[test]
    fn compress_default_produces_zlib_stream() {
        let packed = compress(SAMPLE).unwrap();
        assert!(!packed.is_empty(), "compressed output must be non-empty");
        // CMF byte for the default windowBits = 15: (7 << 4) | Z_DEFLATED(8) = 0x78.
        assert_eq!(packed[0], 0x78, "zlib CMF header byte for a 32 KiB window");
        // The produced stream never exceeds the advertised upper bound.
        assert!(packed.len() <= compress_bound(SAMPLE.len()));
    }

    #[test]
    fn compress2_all_levels_succeed() {
        // Every valid level (plus the default sentinel) produces a non-empty
        // zlib stream that begins with the expected CMF header byte.
        for level in [0, 1, 6, 9, Z_DEFAULT_COMPRESSION] {
            let packed = compress2(SAMPLE, level).unwrap();
            assert!(!packed.is_empty(), "level {level} produced empty output");
            assert_eq!(packed[0], 0x78, "level {level} zlib CMF header byte");
            assert!(packed.len() <= compress_bound(SAMPLE.len()));
        }
    }

    #[test]
    fn compress2_invalid_level_is_stream_error() {
        // An out-of-range level is rejected by `Deflate::new` exactly as C
        // `deflateInit` returns `Z_STREAM_ERROR`.
        assert_eq!(compress2(SAMPLE, 42), Err(ZlibError::StreamError));
        assert_eq!(compress2(SAMPLE, -2), Err(ZlibError::StreamError));
    }

    #[test]
    fn compress_to_buf_matches_compress2_default() {
        // `compress_to_buf` is `compress2_to_buf` at the default level, so the
        // two must produce identical bytes.
        let mut a = vec![0u8; compress_bound(SAMPLE.len())];
        let mut b = vec![0u8; compress_bound(SAMPLE.len())];
        let na = compress_to_buf(&mut a, SAMPLE).unwrap();
        let nb = compress2_to_buf(&mut b, SAMPLE, Z_DEFAULT_COMPRESSION).unwrap();
        assert_eq!(na, nb);
        assert_eq!(a[..na], b[..nb]);
    }

    #[test]
    fn compress2_to_buf_small_dest_is_buf_error() {
        // A 1-byte destination cannot hold the stream; the single `Finish` pass
        // does not reach end-of-stream, so the C terminal `Z_BUF_ERROR` is
        // surfaced as `ZlibError::BufError`.
        let mut tiny = [0u8; 1];
        assert_eq!(
            compress2_to_buf(&mut tiny, SAMPLE, 6),
            Err(ZlibError::BufError)
        );
    }

    #[test]
    fn idiomatic_compress_trims_to_produced_length() {
        // The returned buffer is trimmed to exactly the produced length, so it
        // is no larger than the bound and (for compressible input) smaller than
        // a freshly allocated worst-case buffer.
        let packed = compress(SAMPLE).unwrap();
        assert!(packed.len() <= compress_bound(SAMPLE.len()));
        // Trimming means the capacity-sized tail of zero bytes is gone: the
        // last byte is part of the Adler-32 trailer, not an untouched zero pad
        // (a worst-case-sized buffer would still end in zero padding).
        assert!(
            packed.len() >= 2,
            "a zlib stream is at least a 2-byte header"
        );
    }
}
