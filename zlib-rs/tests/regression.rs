//! Regression conformance suite — a faithful Rust port of the canonical zlib
//! test driver `test/example.c`.
//!
//! This is the primary **functional-conformance gate** for the `zlib-rs` core.
//! Every one of the ten regression functions from `test/example.c` is ported
//! to an idiomatic Rust `#[test]`, preserving each behavioral assertion of the
//! reference suite:
//!
//! | C function (`test/example.c`) | Rust test                       | Exercises                                            |
//! |-------------------------------|---------------------------------|------------------------------------------------------|
//! | `test_compress`               | [`test_compress`]               | one-shot `compress` / `uncompress` round-trip        |
//! | `test_gzio`                   | [`test_gzio`] (gz-io)           | gzip-container (`.gz`) file round-trip               |
//! | `test_deflate`                | [`test_deflate`]                | streaming compressor, forced 1-byte buffers          |
//! | `test_inflate`                | [`test_inflate`]                | streaming decompressor, forced 1-byte buffers        |
//! | `test_large_deflate`          | [`test_large_deflate`]          | `deflate_params` mid-stream level/strategy switches  |
//! | `test_large_inflate`          | [`test_large_inflate`]          | large decompression, `total_out` accounting          |
//! | `test_flush`                  | (helper for [`test_sync`])      | `Z_FULL_FLUSH` flush point + deliberate corruption   |
//! | `test_sync`                   | [`test_sync`]                   | `inflate` sync recovery after a damaged block        |
//! | `test_dict_deflate`           | [`test_dict_deflate`]           | preset-dictionary compression + Adler-32 stream ID   |
//! | `test_dict_inflate`           | [`test_dict_inflate`]           | preset-dictionary `Z_NEED_DICT` handshake            |
//!
//! Plus [`version_string`], the port of the `zlibVersion()` guard in C `main`.
//!
//! # Binding conventions
//!
//! This is an **external-crate integration test**: it reaches the library only
//! through its public API (`use zlib_rs::...;`), never private internals. It
//! uses the standard libtest harness and is written entirely in safe Rust
//! (`#![forbid(unsafe_code)]`). The C idioms `CHECK_ERR(err, msg)` / `exit(1)`
//! become `assert!` / `assert_eq!` / `.expect(msg)`, and the C `printf` debug
//! output is dropped.
//!
//! ## Streaming-engine slice adapter
//!
//! The C API mutates `next_in` / `next_out` / `avail_in` / `avail_out` in place;
//! the Rust engine instead takes an `input: &[u8]` plus an `output: &mut [u8]`
//! per call and reports how much it consumed and produced. The two cursor-driven
//! helpers [`deflate_stepped`] and [`inflate_stepped`] emulate the C loops —
//! including the deliberately pathological "force small buffers"
//! (`avail_in = avail_out = 1`) stepping that stresses the engines'
//! resumability across `avail_out == 0` and partial-input boundaries.
//!
//! Note the two engine entry points return their `(consumed, produced)` pair in
//! **opposite tuple positions**: `deflate` yields `(result, consumed, produced)`
//! while `inflate` yields `(consumed, produced, result)`. The adapters
//! destructure each in its own order.
//!
//! ## Note on `test_gzio`
//!
//! The C `test_gzio` exercises the buffered `gz*` FILE API (`gzopen`, `gzputc`,
//! `gzputs`, `gzprintf`, `gzseek`, `gzgetc`, `gzungetc`, `gzgets`, `gztell`,
//! `gzclose`). In `zlib-rs` that entire buffered layer is `pub(crate)` and has
//! no public `GzFile` handle yet — by design, the public handle and the `gz*`
//! entry points are reconstructed in the `libz-rs-sys` FFI shim (see the module
//! docs on `zlib_rs::gz`). An external integration test therefore cannot reach
//! that API without violating the "public API only" rule. To still honor the
//! reference test's intent — *read/write of `.gz` files* — [`test_gzio`] ports a
//! complete **gzip-container round-trip through a real temporary file** built
//! exclusively on the public streaming engine (a gzip-wrapped `deflate`, an
//! on-disk `.gz` file, then an auto-detecting `inflate`). It is gated behind the
//! same `gz-io` feature as the C `gz*` family.

#![forbid(unsafe_code)]

use zlib_rs::checksum::adler32;
use zlib_rs::constants::{
    Flush, Strategy, Z_BEST_COMPRESSION, Z_BEST_SPEED, Z_DEFAULT_COMPRESSION, Z_NO_COMPRESSION,
    ZLIB_VERNUM, ZLIB_VERSION,
};
use zlib_rs::deflate::{
    deflate, deflate_end, deflate_init, deflate_params, deflate_set_dictionary,
};
use zlib_rs::error::ReturnCode;
use zlib_rs::inflate::{InflateState, inflate};
use zlib_rs::stream::ZStream;
use zlib_rs::util::{compress, compress_bound, uncompress, zlib_version};

// Imports used only by the gzip-container round-trip in `test_gzio`, which is
// itself gated behind `gz-io`. Gating the imports too keeps the default-feature
// build free of "unused import" warnings under `clippy -D warnings`.
#[cfg(feature = "gz-io")]
use zlib_rs::constants::{DEF_MEM_LEVEL, MAX_WBITS, Z_DEFLATED};
#[cfg(feature = "gz-io")]
use zlib_rs::deflate::deflate_init2;

// ---------------------------------------------------------------------------
// Shared fixtures (byte-for-byte identical to `test/example.c`)
// ---------------------------------------------------------------------------

/// The reference payload. The repeated "hello" deliberately stresses the LZ77
/// match finder (C comment: *"hello world" would be more standard, but the
/// repeated "hello" stresses the compression code better*).
const HELLO: &[u8] = b"hello, hello!";

/// What the C suite actually compresses: `strlen(hello) + 1 == 14` bytes,
/// **including the trailing NUL**. Dropping the NUL would change both the
/// checksums and the recovered-string comparison, so it is preserved here.
const HELLO_Z: &[u8] = b"hello, hello!\0";

/// The preset dictionary. C uses `sizeof(dictionary) == 6` for
/// `char dictionary[] = "hello"`, i.e. the five characters **plus** the NUL;
/// the `deflateSetDictionary` call passes all six bytes.
const DICTIONARY: &[u8] = b"hello\0";

/// Decompressed-buffer size used by the "large" tests (`uncomprLen` in C `main`).
const UNCOMPR_LEN: usize = 20_000;

/// Compressed-buffer size used by the "large" tests (`comprLen == 3 * uncomprLen`).
const COMPR_LEN: usize = 3 * UNCOMPR_LEN;

/// Mirrors the C `strcpy((char*)uncompr, "garbage")` pre-fill: seeds an output
/// buffer with recognizable junk so a failure to overwrite is caught. The
/// byte-exact assertions below make this cosmetic, but it keeps the port honest.
fn prefill_garbage(buf: &mut [u8]) {
    const GARBAGE: &[u8] = b"garbage";
    let n = GARBAGE.len().min(buf.len());
    buf[..n].copy_from_slice(&GARBAGE[..n]);
}

// ---------------------------------------------------------------------------
// Streaming-engine driving helpers
// ---------------------------------------------------------------------------

/// Compress all of `src` through an already-initialized deflate `strm`, forcing
/// **one input byte and one output byte per call** — the Rust analogue of the C
/// `test_deflate` loop that sets `avail_in = avail_out = 1`.
///
/// Phase 1 feeds the input one byte at a time under [`Flush::NoFlush`] until it
/// is fully consumed; phase 2 then drains the compressor one output byte at a
/// time under [`Flush::Finish`] until it reports [`ReturnCode::StreamEnd`]. The
/// output vector grows on demand. The caller owns stream teardown
/// ([`deflate_end`]); this helper only drives the engine and returns the exact
/// compressed bytes.
fn deflate_stepped(strm: &mut ZStream, src: &[u8], cap: usize) -> Vec<u8> {
    let mut out = vec![0u8; cap.max(1)];
    let mut in_pos = 0usize;
    let mut out_pos = 0usize;

    // Phase 1: 1 byte in / 1 byte out, NoFlush, until all input is consumed.
    while in_pos < src.len() {
        if out_pos == out.len() {
            out.resize(out.len() * 2, 0);
        }
        let (result, consumed, produced) = deflate(
            strm,
            &src[in_pos..in_pos + 1],
            &mut out[out_pos..out_pos + 1],
            Flush::NoFlush,
        );
        result.expect("deflate (Z_NO_FLUSH)");
        in_pos += consumed;
        out_pos += produced;
        // The engine must always make progress with a free output byte and a
        // pending input byte; bail out defensively rather than spin forever.
        if consumed == 0 && produced == 0 {
            break;
        }
    }

    // Phase 2: drain and finish, still one output byte at a time.
    loop {
        if out_pos == out.len() {
            out.resize(out.len() * 2, 0);
        }
        let (result, _consumed, produced) =
            deflate(strm, &[], &mut out[out_pos..out_pos + 1], Flush::Finish);
        out_pos += produced;
        if result.expect("deflate (Z_FINISH)") == ReturnCode::StreamEnd {
            break;
        }
    }

    out.truncate(out_pos);
    out
}

/// Decompress all of `comp` through an already-initialized inflate `strm`,
/// forcing **one input byte and one output byte per call** — the Rust analogue
/// of the C `test_inflate` loop that sets `avail_in = avail_out = 1`.
///
/// Runs under [`Flush::NoFlush`] until the engine reports
/// [`ReturnCode::StreamEnd`]; the output vector grows on demand. The caller owns
/// stream teardown ([`ZStream::end`]); this helper returns the recovered bytes.
fn inflate_stepped(strm: &mut ZStream, comp: &[u8], cap: usize) -> Vec<u8> {
    let mut out = vec![0u8; cap.max(1)];
    let mut in_pos = 0usize;
    let mut out_pos = 0usize;

    loop {
        if out_pos == out.len() {
            out.resize(out.len() * 2, 0);
        }
        let in_chunk: &[u8] = if in_pos < comp.len() {
            &comp[in_pos..in_pos + 1]
        } else {
            &[]
        };
        let (consumed, produced, result) = inflate(
            strm,
            in_chunk,
            &mut out[out_pos..out_pos + 1],
            Flush::NoFlush,
        );
        in_pos += consumed;
        out_pos += produced;
        if result.expect("inflate (Z_NO_FLUSH)") == ReturnCode::StreamEnd {
            break;
        }
        // Defensive no-progress guard (e.g. a truncated stream): without this a
        // malformed input with no remaining bytes could loop forever.
        if consumed == 0 && produced == 0 {
            break;
        }
    }

    out.truncate(out_pos);
    out
}

// ===========================================================================
// Phase A — test_compress: one-shot round-trip            [example.c L66-85]
// ===========================================================================

/// Port of C `test_compress`: compress the 14-byte `HELLO_Z` with the one-shot
/// helper, then decompress it and confirm the recovered bytes match exactly.
#[test]
fn test_compress() {
    // Compress into a `compress_bound`-sized buffer (C uses `comprLen`).
    let mut compr = vec![0u8; compress_bound(HELLO_Z.len())];
    let compressed_len = compress(&mut compr, HELLO_Z).expect("compress");
    compr.truncate(compressed_len);
    assert!(!compr.is_empty(), "compress produced no output");

    // Recover into a buffer pre-filled with "garbage" (C: `strcpy(uncompr,...)`).
    let mut uncompr = vec![0u8; HELLO_Z.len() + 8];
    prefill_garbage(&mut uncompr);
    let recovered_len = uncompress(&mut uncompr, &compr).expect("uncompress");

    assert_eq!(recovered_len, HELLO_Z.len(), "uncompressed length mismatch");
    assert_eq!(&uncompr[..recovered_len], HELLO_Z, "bad uncompress");
}

// ===========================================================================
// Phase B — test_gzio: gzip-container file round-trip      [example.c L90-165]
// ===========================================================================

/// Port of C `test_gzio`, adapted to the public API (see the module-level note
/// on `test_gzio`): perform a full gzip-container (`.gz`) round-trip through a
/// real temporary file, built on the public streaming engine.
///
/// 1. **Write phase** — gzip-wrap `HELLO_Z` with `deflate_init2`
///    (`windowBits == MAX_WBITS + 16` selects the gzip container) and write the
///    bytes to a uniquely-named temporary `.gz` file (mirroring `gzopen("wb")`
///    … `gzclose`).
/// 2. **Read phase** — read the file back and decompress it with an
///    auto-detecting `inflate` (`windowBits == MAX_WBITS + 32`), mirroring
///    `gzopen("rb")` … `gzread` … `gzclose`. The recovered bytes must equal the
///    original payload.
///
/// Gated behind `gz-io` exactly like the C `gz*` family (which `#ifdef`s out
/// under `NO_GZCOMPRESS`). `gz-io` implies both `std` and `gzip`, so the gzip
/// `windowBits` paths and the `std::fs` file I/O are always available here.
#[cfg(feature = "gz-io")]
#[test]
fn test_gzio() {
    use std::io::{Read, Write};

    // A process-unique path so parallel test binaries never collide.
    let path = std::env::temp_dir().join(format!("zlib_rs_example_{}.gz", std::process::id()));

    // --- Write phase: produce a gzip stream and persist it to disk. ---
    let gz_bytes = {
        let mut strm = ZStream::new();
        deflate_init2(
            &mut strm,
            Z_DEFAULT_COMPRESSION,
            Z_DEFLATED,
            MAX_WBITS + 16, // +16 => gzip container (RFC 1952)
            DEF_MEM_LEVEL,
            Strategy::Default,
        )
        .expect("deflateInit2 (gzip)");
        let bytes = deflate_stepped(&mut strm, HELLO_Z, compress_bound(HELLO_Z.len()) + 64);
        deflate_end(&mut strm).expect("deflateEnd");
        bytes
    };
    assert!(!gz_bytes.is_empty(), "gzip deflate produced no output");

    {
        let mut file = std::fs::File::create(&path).expect("create .gz file");
        file.write_all(&gz_bytes).expect("write .gz file");
        file.flush().expect("flush .gz file");
    }

    // --- Read phase: read the file back and decompress it. ---
    let mut file_bytes = Vec::new();
    {
        let mut file = std::fs::File::open(&path).expect("open .gz file");
        file.read_to_end(&mut file_bytes).expect("read .gz file");
    }
    assert_eq!(
        file_bytes, gz_bytes,
        ".gz bytes must survive a file round-trip"
    );

    let recovered = {
        let mut strm = ZStream::new();
        // +32 => automatic zlib/gzip header detection.
        strm.set_inflate_state(
            InflateState::new(MAX_WBITS + 32).expect("inflateInit2 (auto-detect)"),
        );
        let bytes = inflate_stepped(&mut strm, &file_bytes, HELLO_Z.len() + 16);
        strm.end().expect("inflateEnd");
        bytes
    };
    assert_eq!(
        recovered.as_slice(),
        HELLO_Z,
        "gzip round-trip must recover the original payload"
    );

    // Best-effort cleanup; a leftover temp file must never fail the test.
    let _ = std::fs::remove_file(&path);
}

// ===========================================================================
// Phase C — test_deflate + test_inflate: streaming, 1-byte buffers
//                                                          [example.c L172-241]
// ===========================================================================

/// Compress `HELLO_Z` through the streaming compressor with the forced
/// 1-byte-in / 1-byte-out stepping of C `test_deflate`. Returned to the caller
/// so `test_deflate` and `test_inflate` are each self-contained (libtest may run
/// them in any order / in parallel, so they cannot share a buffer the way the C
/// `main` threads `compr` between functions).
fn deflate_hello_small() -> Vec<u8> {
    let mut strm = ZStream::new();
    deflate_init(&mut strm, Z_DEFAULT_COMPRESSION).expect("deflateInit");
    let compr = deflate_stepped(&mut strm, HELLO_Z, compress_bound(HELLO_Z.len()) + 64);
    deflate_end(&mut strm).expect("deflateEnd");
    compr
}

/// Port of C `test_deflate`: drive the compressor with one-byte buffers and
/// confirm it emits a stream that round-trips. (The authoritative recovered-data
/// assertion lives in [`test_inflate`]; C `test_deflate` itself only checks the
/// per-call return codes, which `deflate_stepped` already asserts.)
#[test]
fn test_deflate() {
    let compr = deflate_hello_small();
    assert!(!compr.is_empty(), "deflate produced no output");

    // Sanity round-trip so a silently-wrong stream is caught here too.
    let mut strm = ZStream::new();
    strm.set_inflate_state(InflateState::new_default().expect("inflateInit"));
    let recovered = inflate_stepped(&mut strm, &compr, HELLO_Z.len() + 16);
    strm.end().expect("inflateEnd");
    assert_eq!(
        recovered.as_slice(),
        HELLO_Z,
        "deflate output does not round-trip"
    );
}

/// Port of C `test_inflate`: decompress the 1-byte-stepped stream produced by
/// [`deflate_hello_small`] using one-byte buffers, and confirm the recovered
/// bytes equal `HELLO_Z` (C pre-fills "garbage" then `strcmp`s against `hello`).
#[test]
fn test_inflate() {
    let compr = deflate_hello_small();

    let mut strm = ZStream::new();
    strm.set_inflate_state(InflateState::new_default().expect("inflateInit"));
    let recovered = inflate_stepped(&mut strm, &compr, HELLO_Z.len() + 16);
    strm.end().expect("inflateEnd");

    assert_eq!(recovered.as_slice(), HELLO_Z, "bad inflate");
}

// ===========================================================================
// Phase D — test_large_deflate + test_large_inflate: deflate_params mid-stream
//                                                          [example.c L246-333]
// ===========================================================================

/// Port of C `test_large_deflate`: the canonical `deflateParams` test. It feeds
/// three input segments while switching compression level/strategy mid-stream:
///
/// 1. `UNCOMPR_LEN` zero bytes in a **single** `deflate(Z_NO_FLUSH)` call — and
///    asserts the engine consumed them all (the C *"deflate not greedy"* guard:
///    `avail_in == 0`).
/// 2. switch to `Z_NO_COMPRESSION` / `Strategy::Default`, then feed
///    `UNCOMPR_LEN / 2` bytes taken from the already-produced compressed output
///    (C feeds `compr`).
/// 3. switch to `Z_BEST_COMPRESSION` / `Strategy::Filtered`, then feed
///    `UNCOMPR_LEN` zero bytes again.
///
/// then `deflate(Z_FINISH)` must report [`ReturnCode::StreamEnd`]. The exact
/// three-segment sequence and the greedy assertion are preserved verbatim.
///
/// Note: the public `deflate_params` takes the current input/output slices
/// (because a mid-stream parameter change may have to flush a pending block) and
/// returns `(result, consumed, produced)`; it is called here with empty input
/// (`avail_in == 0` at each switch point) and the remaining output window, and
/// its produced byte count is folded into `out_pos`.
fn large_deflate() -> Vec<u8> {
    let mut strm = ZStream::new();
    deflate_init(&mut strm, Z_BEST_SPEED).expect("deflateInit");

    // The C `compr` buffer is `calloc`-zeroed; mirror that exactly, because
    // segment #2 feeds bytes taken from this buffer.
    let mut compr = vec![0u8; COMPR_LEN];
    let mut out_pos = 0usize;

    // The C `uncompr` buffer is also `calloc`-zeroed and "compresses very well".
    let zeros = vec![0u8; UNCOMPR_LEN];

    // --- Segment #1: 20_000 zeros in ONE call; assert the engine is greedy. ---
    {
        let (result, consumed, produced) =
            deflate(&mut strm, &zeros, &mut compr[out_pos..], Flush::NoFlush);
        result.expect("deflate (segment #1)");
        assert_eq!(
            consumed, UNCOMPR_LEN,
            "deflate not greedy: did not consume all input in a single call"
        );
        out_pos += produced;
    }

    // --- Switch to stored (no compression); may flush the pending block. ---
    {
        let (result, _consumed, produced) = deflate_params(
            &mut strm,
            Z_NO_COMPRESSION,
            Strategy::Default,
            &[],
            &mut compr[out_pos..],
        );
        result.expect("deflateParams (-> Z_NO_COMPRESSION)");
        out_pos += produced;
    }

    // --- Segment #2: feed 10_000 bytes taken from the produced output. ---
    // Copy them out first: a single buffer cannot be borrowed as both the
    // input and the output of one `deflate` call.
    {
        let input2: Vec<u8> = compr[..UNCOMPR_LEN / 2].to_vec();
        let (result, _consumed, produced) =
            deflate(&mut strm, &input2, &mut compr[out_pos..], Flush::NoFlush);
        result.expect("deflate (segment #2)");
        out_pos += produced;
    }

    // --- Switch back to best compression + filtered; may flush again. ---
    {
        let (result, _consumed, produced) = deflate_params(
            &mut strm,
            Z_BEST_COMPRESSION,
            Strategy::Filtered,
            &[],
            &mut compr[out_pos..],
        );
        result.expect("deflateParams (-> Z_BEST_COMPRESSION / filtered)");
        out_pos += produced;
    }

    // --- Segment #3: 20_000 zeros again. ---
    {
        let (result, _consumed, produced) =
            deflate(&mut strm, &zeros, &mut compr[out_pos..], Flush::NoFlush);
        result.expect("deflate (segment #3)");
        out_pos += produced;
    }

    // --- Finish: must report StreamEnd. ---
    {
        let (result, _consumed, produced) =
            deflate(&mut strm, &[], &mut compr[out_pos..], Flush::Finish);
        assert_eq!(
            result.expect("deflate (Z_FINISH)"),
            ReturnCode::StreamEnd,
            "deflate should report Z_STREAM_END"
        );
        out_pos += produced;
    }

    deflate_end(&mut strm).expect("deflateEnd");
    compr.truncate(out_pos);
    compr
}

/// Port of C `test_large_deflate`: run [`large_deflate`] (which embeds the
/// greedy and StreamEnd assertions) and confirm it produced a stream.
#[test]
fn test_large_deflate() {
    let compr = large_deflate();
    assert!(!compr.is_empty(), "large deflate produced no output");
}

/// Port of C `test_large_inflate`: inflate the [`large_deflate`] stream,
/// discarding the output into a reused `UNCOMPR_LEN` buffer, and confirm the
/// total decompressed length is `2 * UNCOMPR_LEN + UNCOMPR_LEN / 2` (the three
/// fed segments: 20_000 + 10_000 + 20_000 == 50_000).
#[test]
fn test_large_inflate() {
    let compr = large_deflate();

    let mut strm = ZStream::new();
    strm.set_inflate_state(InflateState::new_default().expect("inflateInit"));

    // Reused output window — the produced bytes are intentionally discarded; the
    // assertion is purely on the cumulative `total_out`.
    let mut uncompr = vec![0u8; UNCOMPR_LEN];
    let mut in_pos = 0usize;

    loop {
        let (consumed, produced, result) =
            inflate(&mut strm, &compr[in_pos..], &mut uncompr, Flush::NoFlush);
        in_pos += consumed;
        match result.expect("large inflate") {
            ReturnCode::StreamEnd => break,
            _ => {
                if consumed == 0 && produced == 0 {
                    break; // defensive no-progress guard
                }
            }
        }
    }

    strm.end().expect("inflateEnd");
    assert_eq!(
        strm.total_out,
        (2 * UNCOMPR_LEN + UNCOMPR_LEN / 2) as u64,
        "bad large inflate total_out"
    );
}

// ===========================================================================
// Phase E — test_flush + test_sync: full flush, corruption, sync recovery
//                                                          [example.c L338-409]
// ===========================================================================

/// Port of C `test_flush`: compress the first three bytes of `HELLO_Z` ("hel")
/// with [`Flush::FullFlush`] — which emits a flush point (an empty stored block,
/// `00 00 FF FF`) that `inflate` can resynchronize to — then **deliberately
/// corrupt a byte of that first compressed block** (C: `compr[3]++`), then
/// compress the remaining bytes with [`Flush::Finish`]. Returns the corrupted
/// stream for [`test_sync`] to recover from.
fn deflate_with_full_flush_corrupted() -> Vec<u8> {
    let mut strm = ZStream::new();
    deflate_init(&mut strm, Z_DEFAULT_COMPRESSION).expect("deflateInit");

    let mut compr = vec![0u8; COMPR_LEN];
    let mut out_pos = 0usize;

    // Compress "hel" (the first 3 bytes) with a full flush.
    {
        let (result, _consumed, produced) = deflate(
            &mut strm,
            &HELLO_Z[..3],
            &mut compr[out_pos..],
            Flush::FullFlush,
        );
        result.expect("deflate (Z_FULL_FLUSH)");
        out_pos += produced;
    }

    // Force an error in the first compressed block (byte-for-byte: `compr[3]++`).
    // The full flush above guarantees `out_pos > 3`, so this byte lies within the
    // already-emitted first block and is not overwritten by the finish below.
    assert!(out_pos > 3, "full flush must emit more than 3 bytes");
    compr[3] = compr[3].wrapping_add(1);

    // Compress the remaining 11 bytes ("lo, hello!\0") and finish.
    {
        let (result, _consumed, produced) = deflate(
            &mut strm,
            &HELLO_Z[3..],
            &mut compr[out_pos..],
            Flush::Finish,
        );
        assert_eq!(
            result.expect("deflate (Z_FINISH)"),
            ReturnCode::StreamEnd,
            "deflate should report Z_STREAM_END"
        );
        out_pos += produced;
    }

    deflate_end(&mut strm).expect("deflateEnd");
    compr.truncate(out_pos);
    compr
}

/// Port of C `test_sync`: recover from the corrupted stream produced by
/// [`deflate_with_full_flush_corrupted`].
///
/// Read just the two-byte zlib header, then call `inflate`'s sync routine to
/// skip the damaged first block forward to the full-flush marker, then finish
/// decoding the (intact) second block. The C suite's hard guarantees — which
/// this port preserves — are exactly: the header read succeeds, the sync
/// succeeds, and the final `inflate` reports [`ReturnCode::StreamEnd`].
///
/// The data before the flush point ("hel") is irrecoverably lost; what survives
/// is the second block, `"lo, hello!\0"` (`HELLO_Z[3..]`). The C code prints
/// `"hel%s"`, literally prepending the known-lost "hel" to the recovered tail to
/// reconstruct the original string — so the recovered bytes here are the tail,
/// not a "hel" prefix.
#[test]
fn test_sync() {
    let comp = deflate_with_full_flush_corrupted();

    let mut strm = ZStream::new();
    strm.set_inflate_state(InflateState::new_default().expect("inflateInit"));

    // Read just the 2-byte zlib header (C: `avail_in = 2`).
    let mut scratch = vec![0u8; UNCOMPR_LEN];
    {
        let (consumed, _produced, result) =
            inflate(&mut strm, &comp[..2], &mut scratch, Flush::NoFlush);
        result.expect("inflate (zlib header)");
        assert_eq!(consumed, 2, "should consume exactly the 2-byte zlib header");
    }

    // Sync over the remaining input: skip the damaged block to the flush marker.
    let rest = &comp[2..];
    let sync_consumed = {
        let state = strm.inflate_state_mut().expect("inflate state");
        let (consumed, result) = state.sync(rest);
        result.expect("inflateSync");
        consumed
    };

    // Finish decoding from just past the recovered flush marker.
    let after_marker = &rest[sync_consumed..];
    let mut recovered = vec![0u8; UNCOMPR_LEN];
    let (_consumed, produced, result) =
        inflate(&mut strm, after_marker, &mut recovered, Flush::Finish);
    assert_eq!(
        result.expect("inflate (Z_FINISH after sync)"),
        ReturnCode::StreamEnd,
        "inflate should report Z_STREAM_END"
    );
    strm.end().expect("inflateEnd");

    // The recovered bytes are the second block: "lo, hello!\0" == HELLO_Z[3..].
    assert_eq!(
        &recovered[..produced],
        &HELLO_Z[3..],
        "inflateSync should recover the post-flush-point tail"
    );
}

// ===========================================================================
// Phase F — test_dict_deflate + test_dict_inflate: preset dictionary
//                                                          [example.c L414-491]
// ===========================================================================

/// Port of C `test_dict_deflate`: compress `HELLO_Z` with the preset dictionary
/// `DICTIONARY` set before any data is fed. Returns the compressed stream and
/// the dictionary's Adler-32 stream ID (C: `dictId = c_stream.adler`, captured
/// immediately after `deflateSetDictionary`).
fn deflate_with_dict() -> (Vec<u8>, u32) {
    let mut strm = ZStream::new();
    deflate_init(&mut strm, Z_BEST_COMPRESSION).expect("deflateInit");

    deflate_set_dictionary(&mut strm, DICTIONARY).expect("deflateSetDictionary");
    // The stream's running checksum now holds the Adler-32 of the dictionary;
    // for a zlib stream this is the dictionary identifier emitted in the header.
    let dict_id = strm.adler;

    let mut compr = vec![0u8; compress_bound(HELLO_Z.len()) + 64];
    let (result, _consumed, produced) = deflate(&mut strm, HELLO_Z, &mut compr[..], Flush::Finish);
    assert_eq!(
        result.expect("deflate (Z_FINISH)"),
        ReturnCode::StreamEnd,
        "deflate should report Z_STREAM_END"
    );
    deflate_end(&mut strm).expect("deflateEnd");

    compr.truncate(produced);
    (compr, dict_id)
}

/// Port of C `test_dict_deflate`: confirm the captured dictionary ID is the
/// Adler-32 of the dictionary and that a stream was produced. Cross-checks the
/// handshake value against `adler32(1, DICTIONARY)`.
#[test]
fn test_dict_deflate() {
    let (compr, dict_id) = deflate_with_dict();
    assert_eq!(
        dict_id,
        adler32(1, DICTIONARY),
        "dictId must equal adler32(1, dictionary)"
    );
    assert!(
        !compr.is_empty(),
        "dictionary-compressed stream must be non-empty"
    );
}

/// Port of C `test_dict_inflate`: decompress the dictionary-compressed stream.
/// Inflation first reports [`ReturnCode::NeedDict`]; at that point the stream's
/// reported checksum must equal the dictionary ID, after which the dictionary is
/// supplied via the inflate state and decoding continues to completion. The
/// recovered bytes must equal `HELLO_Z`.
#[test]
fn test_dict_inflate() {
    let (compr, dict_id) = deflate_with_dict();

    let mut strm = ZStream::new();
    strm.set_inflate_state(InflateState::new_default().expect("inflateInit"));

    let mut uncompr = vec![0u8; UNCOMPR_LEN];
    prefill_garbage(&mut uncompr);
    let mut in_pos = 0usize;
    let mut out_pos = 0usize;

    loop {
        let (consumed, produced, result) = inflate(
            &mut strm,
            &compr[in_pos..],
            &mut uncompr[out_pos..],
            Flush::NoFlush,
        );
        in_pos += consumed;
        out_pos += produced;
        match result.expect("inflate (with dictionary)") {
            ReturnCode::StreamEnd => break,
            ReturnCode::NeedDict => {
                assert_eq!(strm.adler, dict_id, "unexpected dictionary");
                let state = strm.inflate_state_mut().expect("inflate state");
                state
                    .set_dictionary(DICTIONARY)
                    .expect("inflateSetDictionary");
            }
            ReturnCode::Ok => {
                if consumed == 0 && produced == 0 {
                    break; // defensive no-progress guard
                }
            }
        }
    }

    strm.end().expect("inflateEnd");
    assert_eq!(&uncompr[..out_pos], HELLO_Z, "bad inflate with dict");
}

// ===========================================================================
// Phase G — version sanity                            [example.c main L497-513]
// ===========================================================================

/// Port of the C `main` version guard (`zlibVersion()[0] != myVersion[0]`):
/// confirm the runtime version string agrees with the compiled-in
/// `ZLIB_VERSION`, and that the pinned version literal and numeric form match
/// the values frozen in `zlib_rs::constants`.
#[test]
fn version_string() {
    let runtime = zlib_version();

    // C guards on the first character matching; we assert full equality too.
    assert_eq!(
        runtime.as_bytes().first(),
        ZLIB_VERSION.as_bytes().first(),
        "zlib version first-character mismatch"
    );
    assert_eq!(
        runtime, ZLIB_VERSION,
        "zlib_version() must equal ZLIB_VERSION"
    );

    // The exact values pinned by this port.
    assert_eq!(ZLIB_VERSION, "1.3.2.1-motley", "pinned ZLIB_VERSION");
    assert_eq!(ZLIB_VERNUM, 0x1321, "pinned ZLIB_VERNUM");

    // `HELLO` (without the NUL) is the human-readable payload; sanity-check the
    // relationship between the two fixtures so the constants cannot drift apart.
    assert_eq!(
        &HELLO_Z[..HELLO.len()],
        HELLO,
        "HELLO_Z must be HELLO + NUL"
    );
    assert_eq!(HELLO_Z[HELLO.len()], 0, "HELLO_Z must be NUL-terminated");
}
