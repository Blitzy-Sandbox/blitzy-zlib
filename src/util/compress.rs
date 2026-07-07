//! One-call, buffer-to-buffer compression helpers — the safe-Rust port of the
//! C `compress.c` (zlib `1.3.2.1-motley`).
//!
//! This module provides the three "one-shot" entry points that compress a
//! complete source buffer into a caller-provided destination buffer in a single
//! call, without the caller having to drive the streaming [`deflate`] engine
//! directly:
//!
//! * [`compress`] — compress at the library default level.
//! * [`compress2`] — compress at a caller-chosen level.
//! * [`compress_bound`] (and its C-named alias [`compressBound`]) — compute the
//!   worst-case compressed size so callers can size the destination buffer.
//!
//! # Relationship to the C originals
//!
//! The C library splits each helper into a `size_t`-generic `_z` variant and a
//! `uLong` wrapper that merely narrows the width (`compress2_z`/`compress2`,
//! `compress_z`/`compress`, `compressBound_z`/`compressBound`). Rust's [`usize`]
//! unifies C's `size_t`/`uLong` split, so this module exposes a single
//! `usize`-based function per helper. The raw-pointer `extern "C"` shims that
//! reproduce the exact C signatures live separately in `src/ffi/util.rs` and are
//! not this module's concern.
//!
//! # Fidelity
//!
//! The compression loop in [`compress2`] is a faithful transcription of C
//! `compress2_z` (`compress.c` L24-L66): input and output are offered to the
//! engine in `u32::MAX`-sized chunks, [`FlushMode::NoFlush`] is used while input
//! remains and [`FlushMode::Finish`] once every byte has been handed over, and
//! the terminal `Z_STREAM_END` is remapped to success. Preserving this exact
//! multi-call `deflate` sequence is what keeps the emitted stream byte-identical
//! to reference zlib. [`compress_bound`] reproduces the sizing formula bit-for-
//! bit — including the saturate-to-maximum overflow behavior — because callers
//! pre-allocate their output buffers against it (AAP §0.6.4, §0.7.1).
//!
//! # Safety and portability
//!
//! There is **zero `unsafe`** in this module and no dependency on `std`: it
//! operates purely over slices (`&[u8]` / `&mut [u8]`) and drives the safe
//! [`deflate`] engine. It is `no_std` + `alloc` compatible (the engine performs
//! its own allocation through the stream's allocator). Targets Rust 2024
//! edition, MSRV 1.85.0.

use crate::constants::{FlushMode, Z_DEFAULT_COMPRESSION};
use crate::deflate::{deflate, deflate_end, deflate_init};
use crate::error::ReturnCode;
use crate::stream::ZStream;

/// Returns an upper bound on the compressed size of `source_len` bytes.
///
/// This is the idiomatic, `usize`-based port of C `compressBound_z`
/// (`compress.c` L91-L99). The bound is computed as
///
/// ```text
/// source_len + (source_len >> 12) + (source_len >> 14) + (source_len >> 25) + 13
/// ```
///
/// which reserves enough room for the worst-case DEFLATE expansion (a stream of
/// incompressible data emitted as stored blocks) plus the zlib wrapper's header
/// and trailer. The result is **bit-exact** with reference zlib for every input,
/// so a caller that sizes its destination buffer to `compress_bound(n)` and then
/// calls [`compress`] / [`compress2`] on `n` bytes is guaranteed enough space
/// (AAP §0.6.4, §0.7.1).
///
/// # Overflow
///
/// The three right-shift terms can only shrink `source_len`, so the sole
/// overflow risk is the final accumulation near the top of the `usize` range. C
/// detects this with `bound < sourceLen ? (z_size_t)-1 : bound` (unsigned
/// wraparound yields the all-ones sentinel). This port reproduces that semantic
/// exactly: the additions are chained through [`usize::checked_add`], and any
/// overflow saturates the result to [`usize::MAX`].
///
/// # Examples
///
/// For an empty input the bound is exactly the `13`-byte overhead reserve, and
/// the top of the range saturates rather than wrapping:
///
/// ```text
/// compress_bound(0)          == 13
/// compress_bound(100_000)    == 100_043   // 100000 + 24 + 6 + 0 + 13
/// compress_bound(usize::MAX) == usize::MAX // overflow saturates
/// ```
#[must_use]
pub fn compress_bound(source_len: usize) -> usize {
    source_len
        .checked_add(source_len >> 12)
        .and_then(|bound| bound.checked_add(source_len >> 14))
        .and_then(|bound| bound.checked_add(source_len >> 25))
        .and_then(|bound| bound.checked_add(13))
        .unwrap_or(usize::MAX)
}

/// C-named alias for [`compress_bound`].
///
/// zlib publishes this helper as `compressBound`; the crate root re-exports this
/// camelCase spelling so C-familiar callers (and the FFI layer) can use the
/// exact name from `zlib.h`. It is a thin, allocation-free forward to
/// [`compress_bound`] and returns an identical value for every input.
#[allow(non_snake_case)]
#[must_use]
pub fn compressBound(source_len: usize) -> usize {
    compress_bound(source_len)
}

/// Compresses `source` into `dest` at the given compression `level`, returning
/// the number of bytes written to `dest`.
///
/// Faithful port of C `compress2_z` (`compress.c` L24-L66). `level` has the same
/// meaning as in `deflateInit`: `0` ([`Z_NO_COMPRESSION`]) through `9`
/// ([`Z_BEST_COMPRESSION`]), or `-1` ([`Z_DEFAULT_COMPRESSION`]) to request the
/// library default. `dest` must be at least [`compress_bound(source.len())`]
/// bytes for the call to be guaranteed to succeed.
///
/// Empty inputs are valid: an empty `source` still produces a complete zlib
/// stream (header, one empty block, and the Adler-32 trailer), so `dest` must
/// have room for at least those bytes. An empty `dest` therefore yields
/// [`ReturnCode::BufError`] — matching the C behavior — because not even the
/// two-byte header fits.
///
/// [`compress_bound(source.len())`]: compress_bound
/// [`Z_NO_COMPRESSION`]: crate::constants::Z_NO_COMPRESSION
/// [`Z_BEST_COMPRESSION`]: crate::constants::Z_BEST_COMPRESSION
///
/// # Errors
///
/// * [`ReturnCode::StreamError`] — `level` is outside the valid set
///   (`-1` or `0..=9`); reported by the deflate initializer.
/// * [`ReturnCode::BufError`] — `dest` was too small to hold the complete
///   compressed stream.
///
/// (`ReturnCode::MemError` is possible in principle if the engine cannot
/// allocate its working buffers, mirroring C `Z_MEM_ERROR`.)
pub fn compress2(dest: &mut [u8], source: &[u8], level: i32) -> Result<usize, ReturnCode> {
    // Create a fresh stream and initialize the deflate engine at `level`. An
    // invalid `level` is rejected here (C `deflateInit` → `Z_STREAM_ERROR`); the
    // idiomatic `ZlibError` is remapped to its integer-equivalent `ReturnCode`.
    let mut strm = ZStream::new();
    deflate_init(&mut strm, level).map_err(ReturnCode::from)?;

    // The engine's per-call I/O width is C `uInt` == `u32`, so — exactly as C
    // `compress2_z` does — the loop offers the input and output to `deflate` in
    // `u32::MAX`-sized chunks. Bookkeeping mirrors the C locals:
    //   * `source_len` / `left`      — input / output not yet offered
    //     (C `sourceLen` / `left`);
    //   * `avail_in` / `avail_out`   — the currently offered window sizes
    //     (C `stream.avail_in` / `stream.avail_out`);
    //   * `in_pos` / `out_pos`       — absolute bytes consumed / produced so far
    //     (C `stream.next_in - source` / `stream.next_out - dest`).
    let max = u32::MAX as usize;
    let mut source_len = source.len();
    let mut left = dest.len();
    let mut avail_in: usize = 0;
    let mut avail_out: usize = 0;
    let mut in_pos: usize = 0;
    let mut out_pos: usize = 0;

    let code = loop {
        // Refill the offered output window once the previous one is exhausted.
        if avail_out == 0 {
            avail_out = left.min(max);
            left -= avail_out;
        }
        // Refill the offered input window once the previous one is exhausted.
        if avail_in == 0 {
            avail_in = source_len.min(max);
            source_len -= avail_in;
        }

        // C: `deflate(&stream, sourceLen ? Z_NO_FLUSH : Z_FINISH)`. Once every
        // byte has been moved into the offered window (`source_len == 0`), the
        // engine is told this is the final input via `Z_FINISH`.
        let flush = if source_len != 0 {
            FlushMode::NoFlush
        } else {
            FlushMode::Finish
        };

        // Offer exactly the current windows; the engine advances by the returned
        // `consumed`/`produced` counts (it never exceeds the slice lengths).
        let outcome = deflate(
            &mut strm,
            &source[in_pos..in_pos + avail_in],
            &mut dest[out_pos..out_pos + avail_out],
            flush.as_c_int(),
        );

        // Advance the absolute cursors and shrink the offered windows by the
        // amounts actually processed. These subtractions cannot underflow:
        // `consumed <= avail_in` and `produced <= avail_out` by construction.
        in_pos += outcome.consumed;
        avail_in -= outcome.consumed;
        out_pos += outcome.produced;
        avail_out -= outcome.produced;

        // C loops `while (err == Z_OK)`; every other code is terminal. When the
        // output is exhausted mid-`Z_FINISH`, the engine returns `Z_BUF_ERROR`,
        // which both breaks the loop and becomes the reported error below.
        if outcome.code != ReturnCode::Ok {
            break outcome.code;
        }
    };

    // Release the engine. RAII (`Drop` on `strm`) would already free the state,
    // but calling `deflate_end` mirrors C `deflateEnd` and is safe: it clears the
    // state, so the later `Drop` on `strm` is a no-op (no double free). C ignores
    // this return value, and so do we.
    let _ = deflate_end(&mut strm);

    // C: `return err == Z_STREAM_END ? Z_OK : err`, with the produced-byte count
    // (`next_out - dest` == `out_pos`) reported to the caller on success.
    if code == ReturnCode::StreamEnd {
        Ok(out_pos)
    } else {
        Err(code)
    }
}

/// Compresses `source` into `dest` at the library default compression level,
/// returning the number of bytes written to `dest`.
///
/// Faithful port of C `compress_z` / `compress` (`compress.c` L77-L85): a
/// convenience wrapper that forwards to [`compress2`] with
/// [`Z_DEFAULT_COMPRESSION`]. As with [`compress2`], `dest` must be at least
/// [`compress_bound(source.len())`] bytes to be guaranteed sufficient.
///
/// [`compress_bound(source.len())`]: compress_bound
///
/// # Errors
///
/// Returns the same errors as [`compress2`]: [`ReturnCode::BufError`] if `dest`
/// is too small (a bad `level` cannot occur here, as the level is fixed).
pub fn compress(dest: &mut [u8], source: &[u8]) -> Result<usize, ReturnCode> {
    compress2(dest, source, Z_DEFAULT_COMPRESSION)
}

#[cfg(test)]
mod tests {
    use super::*;

    // The round-trip tests decompress with `flate2` (its pure-Rust
    // `miniz_oxide` backend) to prove the emitted stream is valid zlib. `std` is
    // available under `cfg(test)` even though the crate is `no_std`, so `Vec`,
    // `vec!`, and `std::io` may be used freely here.
    use flate2::read::ZlibDecoder;
    use std::io::Read;

    // ---------------------------------------------------------------------
    // compress_bound / compressBound  (Phase 1 — bit-exact sizing contract)
    // ---------------------------------------------------------------------

    #[test]
    fn compress_bound_zero_is_thirteen() {
        // Empty input still needs the fixed 13-byte overhead reserve.
        assert_eq!(compress_bound(0), 13);
    }

    #[test]
    fn compress_bound_mid_size_is_exact() {
        // 100000 + (100000>>12 = 24) + (100000>>14 = 6) + (100000>>25 = 0) + 13.
        assert_eq!(compress_bound(100_000), 100_043);
    }

    #[test]
    fn compress_bound_saturates_on_overflow() {
        // C returns (z_size_t)-1 when the sum wraps; the Rust port saturates.
        assert_eq!(compress_bound(usize::MAX), usize::MAX);
        // The neighborhood just below MAX also saturates (the +13 overflows).
        assert_eq!(compress_bound(usize::MAX - 5), usize::MAX);
    }

    #[test]
    fn compress_bound_is_monotonic_and_covers_source() {
        let mut prev = 0usize;
        for &n in &[0usize, 1, 13, 1024, 65_536, 100_000, 1_000_000, 16_000_000] {
            let bound = compress_bound(n);
            // Never smaller than the source plus the 13-byte reserve.
            assert!(bound >= n + 13, "bound {bound} must be >= {n} + 13");
            // Non-decreasing in the source length.
            assert!(bound >= prev, "bound {bound} must be >= previous {prev}");
            prev = bound;
        }
    }

    #[test]
    fn compress_bound_camelcase_alias_is_identical() {
        for &n in &[0usize, 1, 1000, 100_000, 1_000_000, usize::MAX] {
            assert_eq!(compressBound(n), compress_bound(n));
        }
    }

    // ---------------------------------------------------------------------
    // Round-trip helpers
    // ---------------------------------------------------------------------

    /// Decompresses a complete zlib stream with `flate2`, returning the bytes.
    fn inflate_with_flate2(compressed: &[u8]) -> Vec<u8> {
        let mut decoder = ZlibDecoder::new(compressed);
        let mut out = Vec::new();
        decoder
            .read_to_end(&mut out)
            .expect("compress2 must emit a valid zlib stream");
        out
    }

    /// Compresses `data` at `level` into a `compress_bound`-sized buffer, then
    /// verifies it decompresses back to `data`.
    fn assert_round_trip(level: i32, data: &[u8]) {
        let mut buf = vec![0u8; compress_bound(data.len())];
        let produced = compress2(&mut buf, data, level)
            .unwrap_or_else(|err| panic!("compress2 at level {level} failed: {err:?}"));
        let restored = inflate_with_flate2(&buf[..produced]);
        assert_eq!(restored, data, "round-trip mismatch at level {level}");
    }

    /// Deterministic, effectively-incompressible bytes (a simple LCG), so the
    /// tests need no `rand` dependency yet still exercise the stored-block path.
    fn pseudo_random(len: usize) -> Vec<u8> {
        let mut out = vec![0u8; len];
        let mut state: u32 = 0x1234_5678;
        for byte in &mut out {
            state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            *byte = (state >> 24) as u8;
        }
        out
    }

    // ---------------------------------------------------------------------
    // compress2 — round-trip across representative inputs and every level
    // ---------------------------------------------------------------------

    #[test]
    fn compress2_round_trips_all_inputs_and_levels() {
        let empty: Vec<u8> = Vec::new();
        let small = b"hello, zlib-rs one-call compression!".to_vec();
        let compressible = vec![b'A'; 50_000]; // long run — compresses tiny
        let incompressible = pseudo_random(40_000); // ~stored size

        for &level in &[0i32, 1, 6, 9, Z_DEFAULT_COMPRESSION] {
            assert_round_trip(level, &empty);
            assert_round_trip(level, &small);
            assert_round_trip(level, &compressible);
            assert_round_trip(level, &incompressible);
        }
    }

    #[test]
    fn compress2_highly_compressible_shrinks() {
        // A 50 KiB single-byte run must compress to far fewer bytes at level 9.
        let data = vec![b'Q'; 50_000];
        let mut buf = vec![0u8; compress_bound(data.len())];
        let produced = compress2(&mut buf, &data, 9).expect("compress2 failed");
        assert!(produced < data.len() / 10, "expected strong compression");
        assert_eq!(inflate_with_flate2(&buf[..produced]), data);
    }

    #[test]
    fn compress2_empty_input_with_adequate_dest_succeeds() {
        // 16 >= compress_bound(0) == 13, so the header + empty block + trailer fit.
        let mut buf = [0u8; 16];
        let produced = compress2(&mut buf, &[], 6).expect("empty-input compress2 failed");
        assert!(
            produced > 0,
            "an empty input still emits header + block + trailer"
        );
        assert_eq!(inflate_with_flate2(&buf[..produced]), Vec::<u8>::new());
    }

    // ---------------------------------------------------------------------
    // Error paths
    // ---------------------------------------------------------------------

    #[test]
    fn compress2_too_small_dest_yields_buf_error() {
        let data = vec![b'Z'; 4096];
        // One byte cannot even hold the two-byte zlib header.
        let mut tiny = [0u8; 1];
        assert_eq!(compress2(&mut tiny, &data, 6), Err(ReturnCode::BufError));
    }

    #[test]
    fn compress2_invalid_level_yields_stream_error() {
        let data = b"some data to compress";
        let mut buf = [0u8; 64];
        // Above the valid 0..=9 range.
        assert_eq!(compress2(&mut buf, data, 42), Err(ReturnCode::StreamError));
        // Below the range and not the Z_DEFAULT_COMPRESSION (-1) sentinel.
        assert_eq!(compress2(&mut buf, data, -2), Err(ReturnCode::StreamError));
    }

    // ---------------------------------------------------------------------
    // compress — the default-level convenience wrapper
    // ---------------------------------------------------------------------

    #[test]
    fn compress_default_level_round_trips() {
        let data = b"the quick brown fox jumps over the lazy dog. ".repeat(200);
        let mut buf = vec![0u8; compress_bound(data.len())];
        let produced = compress(&mut buf, &data).expect("compress failed");
        assert_eq!(inflate_with_flate2(&buf[..produced]), data);
    }

    #[test]
    fn compress_matches_compress2_default_level() {
        // `compress` must be exactly `compress2(.., Z_DEFAULT_COMPRESSION)`.
        let data = b"determinism check: compress == compress2(-1)".repeat(64);
        let mut buf_a = vec![0u8; compress_bound(data.len())];
        let mut buf_b = vec![0u8; compress_bound(data.len())];
        let na = compress(&mut buf_a, &data).unwrap();
        let nb = compress2(&mut buf_b, &data, Z_DEFAULT_COMPRESSION).unwrap();
        assert_eq!(na, nb);
        assert_eq!(buf_a[..na], buf_b[..nb], "identical bytes expected");
    }
}
