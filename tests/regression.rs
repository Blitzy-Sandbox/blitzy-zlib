//! Regression suite — a 1:1 behavioral port of the C `test/example.c` driver.
//!
//! `test/example.c` is zlib's canonical usage example and smoke test. It
//! exercises the full public surface through ten `test_*` functions plus a
//! version check in `main`. This file reproduces every one of those behaviors
//! against the idiomatic `zlib_rs` API (AAP §0.4.1 — "10 baseline regression
//! cases"; the testing-parity floor requires at least the API coverage of
//! example.c's ten functions).
//!
//! Run with `cargo test --test regression`.
//!
//! # Behavior map (C `test_*` → Rust `#[test]`)
//!
//! | C function (example.c)             | Rust test                                  |
//! |------------------------------------|--------------------------------------------|
//! | `test_compress`                    | [`compress_uncompress_roundtrip`]          |
//! | `test_gzio`                        | [`gzio_read_write`] (gated `gz-io`)        |
//! | `test_deflate`                     | [`deflate_small_buffers`]                  |
//! | `test_inflate`                     | [`inflate_small_buffers`]                  |
//! | `test_large_deflate` + `_inflate`  | [`large_deflate_inflate_with_param_changes`] |
//! | `test_flush` + `test_sync`         | [`flush_then_sync_recovery`]               |
//! | `test_dict_deflate` + `_inflate`   | [`dictionary_deflate_inflate`]             |
//! | `main` version check               | [`version_smoke`]                          |
//!
//! The C `CHECK_ERR(err, msg)` macro asserts a call returned `Z_OK`; it is
//! ported as `.expect(msg)` / `assert_eq!`.
//!
//! # API asymmetry (faithfully honored here)
//!
//! * [`Deflate::compress`] takes a [`FlushMode`] **enum** and returns
//!   `Result<DeflateOutcome, _>` whose `.code` is a [`ReturnCode`].
//! * [`Inflate::inflate`] takes a **raw `i32`** flush and returns a raw
//!   `(code, consumed, produced)` triple. Its codes are compared against the
//!   `Z_*` integer constants ([`Z_OK`], [`Z_STREAM_END`], [`Z_NEED_DICT`]).
//!
//! # NUL semantics (reproduced byte-for-byte from example.c)
//!
//! C uses `strlen(hello) + 1` (14 bytes, **including** the terminating NUL) for
//! the payload and `sizeof(dictionary)` (6 bytes, including the NUL) for the
//! dictionary. To keep round-trips and the dictionary id identical to C, the
//! trailing NUL is part of the fixtures: [`HELLO`] and [`DICTIONARY`] are the
//! exact byte slices below, NUL included, and decompressed output is compared
//! against the full byte slice.

#![forbid(unsafe_code)]

use zlib_rs::constants::*;
use zlib_rs::util::{compress2, uncompress};
use zlib_rs::{Deflate, Inflate, ReturnCode, adler32, zlib_version, zlib_version_num};

// ===========================================================================
// Fixtures — exact values from example.c
// ===========================================================================

/// The compression payload. example.c: `hello[] = "hello, hello!"` and the
/// processed length is `strlen(hello) + 1`, so the trailing NUL is included
/// (14 bytes). The repeated "hello" deliberately stresses the LZ77 matcher.
const HELLO: &[u8] = b"hello, hello!\0";

/// The preset dictionary. example.c: `dictionary[] = "hello"` consumed with
/// `sizeof(dictionary)` (6 bytes, NUL included).
const DICTIONARY: &[u8] = b"hello\0";

/// Decompression scratch size (`uncomprLen` in example.c).
const UNCOMPR_LEN: usize = 20000;

/// Compression scratch size (`comprLen = 3 * uncomprLen` in example.c).
const COMPR_LEN: usize = 3 * UNCOMPR_LEN;

// ===========================================================================
// Helpers
// ===========================================================================

/// Deflate `input` while forcing one byte of input and one byte of output per
/// `deflate` call — the safe-Rust analogue of example.c's `test_deflate`
/// "force small buffers" stress (`avail_in = avail_out = 1`). Returns the full
/// zlib-wrapped compressed stream.
fn deflate_one_byte_at_a_time(input: &[u8]) -> Vec<u8> {
    let mut deflater = Deflate::new(Z_DEFAULT_COMPRESSION).expect("deflateInit");
    let mut out = vec![0u8; COMPR_LEN];
    let mut in_pos = 0usize;
    let mut out_pos = 0usize;

    // Phase 1: feed every input byte, one input/one output byte at a time, with
    // Z_NO_FLUSH (mirrors C `while (total_in != len && total_out < comprLen)`).
    while in_pos < input.len() && out_pos < out.len() {
        let outcome = deflater
            .compress(
                &input[in_pos..in_pos + 1],
                &mut out[out_pos..out_pos + 1],
                FlushMode::NoFlush,
            )
            .expect("deflate");
        in_pos += outcome.consumed;
        out_pos += outcome.produced;
        assert!(
            outcome.consumed != 0 || outcome.produced != 0,
            "deflate made no progress with 1-byte buffers"
        );
    }

    // Phase 2: finish the stream, still one output byte at a time, until the
    // engine reports Z_STREAM_END (mirrors the C `for (;;)` finish loop).
    loop {
        let outcome = deflater
            .compress(&[], &mut out[out_pos..out_pos + 1], FlushMode::Finish)
            .expect("deflate");
        out_pos += outcome.produced;
        if outcome.code == ReturnCode::StreamEnd {
            break;
        }
        assert!(
            outcome.produced != 0,
            "deflate made no progress while finishing with a 1-byte buffer"
        );
    }

    out.truncate(out_pos);
    out
}

/// Inflate `compressed` while forcing one byte of input and one byte of output
/// per `inflate` call — the safe-Rust analogue of example.c's `test_inflate`
/// "force small buffers" stress. Returns the decompressed bytes.
fn inflate_one_byte_at_a_time(compressed: &[u8]) -> Vec<u8> {
    let mut inflater = Inflate::new().expect("inflateInit");
    let mut out = vec![0u8; UNCOMPR_LEN];
    let mut in_pos = 0usize;
    let mut out_pos = 0usize;

    while out_pos < out.len() && in_pos < compressed.len() {
        let (code, consumed, produced) = inflater.inflate(
            &compressed[in_pos..in_pos + 1],
            &mut out[out_pos..out_pos + 1],
            Z_NO_FLUSH,
        );
        in_pos += consumed;
        out_pos += produced;
        if code == Z_STREAM_END {
            break;
        }
        assert_eq!(code, Z_OK, "inflate");
        assert!(
            consumed != 0 || produced != 0,
            "inflate made no progress with 1-byte buffers"
        );
    }

    out.truncate(out_pos);
    out
}

/// Inflate an entire zlib-wrapped stream using ample buffers, returning the
/// decompressed bytes. Drives `inflate` with `Z_NO_FLUSH` until
/// `Z_STREAM_END`.
fn inflate_whole(compressed: &[u8]) -> Vec<u8> {
    let mut inflater = Inflate::new().expect("inflateInit");
    let mut out = vec![0u8; UNCOMPR_LEN];
    let mut in_pos = 0usize;
    let mut out_pos = 0usize;

    loop {
        let (code, consumed, produced) =
            inflater.inflate(&compressed[in_pos..], &mut out[out_pos..], Z_NO_FLUSH);
        in_pos += consumed;
        out_pos += produced;
        if code == Z_STREAM_END {
            break;
        }
        assert_eq!(code, Z_OK, "inflate");
        assert!(consumed != 0 || produced != 0, "inflate made no progress");
    }

    out.truncate(out_pos);
    out
}

// ===========================================================================
// Tests
// ===========================================================================

/// Port of example.c `test_compress`: a `compress` / `uncompress` round-trip of
/// the `HELLO` payload using the one-shot convenience helpers.
#[test]
fn compress_uncompress_roundtrip() {
    // C: `compress(compr, &comprLen, hello, len)` at Z_DEFAULT_COMPRESSION via
    // the default `compress`. We use `compress2` with the explicit level so the
    // intent is visible; the re-exported one-shot returns an owned `Vec`.
    let compr = compress2(HELLO, Z_DEFAULT_COMPRESSION).expect("compress");

    // C: `uncompress(uncompr, &uncomprLen, compr, comprLen)` then
    // `strcmp((char*)uncompr, "hello, hello!")`. `uncompress` writes into a
    // caller-sized buffer and returns the produced length.
    let mut uncompr = vec![0u8; UNCOMPR_LEN];
    let n = uncompress(&mut uncompr, &compr).expect("uncompress");

    assert_eq!(
        &uncompr[..n],
        HELLO,
        "uncompress did not reproduce the input"
    );
}

/// Port of example.c `test_deflate` (the streaming, 1-byte-buffer half of the
/// deflate↔inflate pair). Deflates `HELLO` one byte at a time, then verifies a
/// whole-buffer inflate reproduces it exactly.
#[test]
fn deflate_small_buffers() {
    let compr = deflate_one_byte_at_a_time(HELLO);
    let uncompr = inflate_whole(&compr);
    assert_eq!(uncompr, HELLO, "deflate (1-byte buffers) round-trip");
}

/// Port of example.c `test_inflate` (the streaming, 1-byte-buffer inflate).
/// Compresses `HELLO`, then inflates it one byte at a time and checks the
/// result.
#[test]
fn inflate_small_buffers() {
    let compr = compress2(HELLO, Z_DEFAULT_COMPRESSION).expect("compress");
    let uncompr = inflate_one_byte_at_a_time(&compr);
    assert_eq!(uncompr, HELLO, "inflate (1-byte buffers) round-trip");
}

/// Port of the version check in example.c `main`, which prints and validates
/// `zlibVersion()` against `ZLIB_VERSION`. The crate self-identifies as the
/// `1.3.2.1-motley` baseline with version number `0x1321`.
#[test]
fn version_smoke() {
    assert_eq!(zlib_version(), ZLIB_VERSION, "zlibVersion string");
    assert_eq!(zlib_version(), "1.3.2.1-motley", "expected zlib baseline");
    assert_eq!(zlib_version_num(), ZLIB_VERNUM, "ZLIB_VERNUM");
    assert_eq!(zlib_version_num(), 0x1321, "expected version number");
}

/// Port of example.c `test_large_deflate` + `test_large_inflate`: a large,
/// mostly-zero buffer is deflated with **mid-stream parameter changes**
/// (the `deflateParams` exercise), then inflated while discarding output, with
/// the cumulative `total_out` asserted exactly.
///
/// The three input segments fed to deflate total `2 * UNCOMPR_LEN +
/// UNCOMPR_LEN / 2` = 50000 bytes, so the decoder's `total_out` must land on
/// exactly that value.
///
/// One faithful divergence from C: example.c feeds the *output* buffer back in
/// as the middle input (an in-place overlapping read/write that safe Rust
/// forbids). We instead feed a separate buffer whose prefix is a snapshot of
/// the compressed bytes produced so far — only the byte **count** affects the
/// asserted `total_out`, so the result is identical.
#[test]
fn large_deflate_inflate_with_param_changes() {
    let mut deflater =
        Deflate::with_options(Z_BEST_SPEED, MAX_WBITS, DEF_MEM_LEVEL, Strategy::Default)
            .expect("deflateInit2");

    let zeros = vec![0u8; UNCOMPR_LEN];
    let mut compr = vec![0u8; COMPR_LEN];
    let mut out_pos = 0usize;

    // Segment 1: the all-zero buffer at Z_BEST_SPEED. It is so compressible
    // that a single call must swallow the whole input — example.c's
    // "deflate not greedy" check (`avail_in == 0` afterwards).
    let outcome = deflater
        .compress(&zeros, &mut compr[out_pos..], FlushMode::NoFlush)
        .expect("deflate");
    out_pos += outcome.produced;
    assert_eq!(outcome.consumed, UNCOMPR_LEN, "deflate not greedy");

    // Switch to "stored" mode (no compression) and flush the current block.
    let outcome = deflater
        .params(
            &[],
            &mut compr[out_pos..],
            Z_NO_COMPRESSION,
            Z_DEFAULT_STRATEGY,
        )
        .expect("deflateParams (-> no compression)");
    out_pos += outcome.produced;

    // Segment 2: feed UNCOMPR_LEN/2 bytes of "already compressed" data. We
    // mirror C by snapshotting the compressed prefix into a fresh buffer; the
    // remainder stays zero (C reads from a calloc'd buffer, so its tail is
    // zero too). Content is irrelevant to the byte count we assert later.
    let mut middle = vec![0u8; UNCOMPR_LEN / 2];
    let snap = out_pos.min(middle.len());
    middle[..snap].copy_from_slice(&compr[..snap]);
    let outcome = deflater
        .compress(&middle, &mut compr[out_pos..], FlushMode::NoFlush)
        .expect("deflate");
    out_pos += outcome.produced;
    assert_eq!(
        outcome.consumed,
        UNCOMPR_LEN / 2,
        "stored segment fully consumed"
    );

    // Switch back to maximum compression with the filtered strategy.
    let outcome = deflater
        .params(&[], &mut compr[out_pos..], Z_BEST_COMPRESSION, Z_FILTERED)
        .expect("deflateParams (-> best/filtered)");
    out_pos += outcome.produced;

    // Segment 3: the all-zero buffer again, now at Z_BEST_COMPRESSION.
    let outcome = deflater
        .compress(&zeros, &mut compr[out_pos..], FlushMode::NoFlush)
        .expect("deflate");
    out_pos += outcome.produced;
    assert_eq!(
        outcome.consumed, UNCOMPR_LEN,
        "third segment fully consumed"
    );

    // Finish the stream.
    let outcome = deflater
        .compress(&[], &mut compr[out_pos..], FlushMode::Finish)
        .expect("deflate");
    out_pos += outcome.produced;
    assert_eq!(
        outcome.code,
        ReturnCode::StreamEnd,
        "deflate should report stream end"
    );
    compr.truncate(out_pos);

    // test_large_inflate: decompress in a loop, discarding the output each
    // iteration, and assert the cumulative output length.
    let mut inflater = Inflate::new().expect("inflateInit");
    let mut scratch = vec![0u8; UNCOMPR_LEN];
    let mut in_pos = 0usize;
    loop {
        let (code, consumed, produced) =
            inflater.inflate(&compr[in_pos..], &mut scratch, Z_NO_FLUSH);
        in_pos += consumed;
        if code == Z_STREAM_END {
            break;
        }
        assert_eq!(code, Z_OK, "large inflate");
        assert!(consumed != 0 || produced != 0, "large inflate stalled");
    }

    let expected = (2 * UNCOMPR_LEN + UNCOMPR_LEN / 2) as u64;
    assert_eq!(inflater.total_out, expected, "bad large inflate total_out");
}

/// Port of example.c `test_flush` + `test_sync`: deflate a few bytes with
/// `Z_FULL_FLUSH`, deliberately corrupt the first compressed block, finish the
/// stream, then recover with `inflateSync` and decode the remainder.
///
/// After `inflateSync` skips the damaged leading block, the decoder yields the
/// tail of the payload (`HELLO[3..]`); prepending the three skipped bytes
/// (`"hel"`, exactly what C prints via `"hel%s"`) reconstructs `HELLO`.
#[test]
fn flush_then_sync_recovery() {
    // --- test_flush: produce a deliberately damaged stream ---
    let mut deflater = Deflate::new(Z_DEFAULT_COMPRESSION).expect("deflateInit");
    let mut compr = vec![0u8; COMPR_LEN];
    let mut out_pos = 0usize;

    // Deflate the first three bytes and force a block boundary.
    let outcome = deflater
        .compress(&HELLO[..3], &mut compr[out_pos..], FlushMode::FullFlush)
        .expect("deflate");
    out_pos += outcome.produced;

    // Force an error inside the first compressed block.
    compr[3] = compr[3].wrapping_add(1);

    // Deflate the remainder and finish. C only fails this call on a negative
    // return code, so a graceful completion (Z_STREAM_END) is the norm.
    let outcome = deflater
        .compress(&HELLO[3..], &mut compr[out_pos..], FlushMode::Finish)
        .expect("deflate");
    out_pos += outcome.produced;
    let compr_len = out_pos;

    // --- test_sync: recover past the damaged block ---
    let mut inflater = Inflate::new().expect("inflateInit");
    let mut out = vec![0u8; UNCOMPR_LEN];

    // Read just the two-byte zlib header.
    let (code, consumed_hdr, _produced) = inflater.inflate(&compr[..2], &mut out, Z_NO_FLUSH);
    assert_eq!(code, Z_OK, "inflate (header)");
    assert_eq!(consumed_hdr, 2, "zlib header should be fully consumed");

    // Skip the damaged part. `sync` returns (code, bytes consumed from input).
    let (sync_code, sync_consumed) = inflater.sync(&compr[2..compr_len]);
    assert_eq!(sync_code, Z_OK, "inflateSync");

    // Decode whatever survives after the sync point.
    let mut in_pos = 2 + sync_consumed;
    let mut out_pos = 0usize;
    loop {
        let (code, consumed, produced) =
            inflater.inflate(&compr[in_pos..compr_len], &mut out[out_pos..], Z_FINISH);
        in_pos += consumed;
        out_pos += produced;
        if code == Z_STREAM_END {
            break;
        }
        if code == Z_DATA_ERROR {
            // C explicitly tolerates this (the recovered stream's check value
            // no longer matches); the payload bytes are already produced.
            break;
        }
        assert_eq!(code, Z_OK, "inflate finish after sync");
        assert!(consumed != 0 || produced != 0, "inflate stalled after sync");
    }

    // The recovered bytes are the tail HELLO[3..]; "hel" was in the skipped
    // block. Reassembling the two halves must reproduce the whole payload.
    let mut recovered = Vec::with_capacity(HELLO.len());
    recovered.extend_from_slice(b"hel");
    recovered.extend_from_slice(&out[..out_pos]);
    assert_eq!(
        recovered, HELLO,
        "inflateSync recovery should reconstruct the payload"
    );
}

/// Port of example.c `test_dict_deflate` + `test_dict_inflate`: deflate with a
/// preset dictionary, then decompress through the `Z_NEED_DICT` handshake.
///
/// The dictionary id the decoder reports is the Adler-32 of the dictionary
/// (what C reads from `c_stream.adler` after `deflateSetDictionary`). The crate
/// exposes no deflate-side `adler` accessor, so we recompute it directly with
/// [`adler32`] — an identical value by construction.
#[test]
fn dictionary_deflate_inflate() {
    // --- test_dict_deflate ---
    let mut deflater = Deflate::new(Z_BEST_COMPRESSION).expect("deflateInit");
    deflater
        .set_dictionary(DICTIONARY)
        .expect("deflateSetDictionary");

    // The id the inflate side will demand (Adler-32 of the dictionary, seed 1).
    let dict_id = adler32(1, DICTIONARY);

    let mut compr = vec![0u8; COMPR_LEN];
    let outcome = deflater
        .compress(HELLO, &mut compr, FlushMode::Finish)
        .expect("deflate");
    assert_eq!(
        outcome.code,
        ReturnCode::StreamEnd,
        "deflate should report stream end"
    );
    let compr_len = outcome.produced;

    // --- test_dict_inflate ---
    let mut inflater = Inflate::new().expect("inflateInit");
    let mut out = vec![0u8; UNCOMPR_LEN];
    let mut in_pos = 0usize;
    let mut out_pos = 0usize;

    loop {
        let (code, consumed, produced) =
            inflater.inflate(&compr[in_pos..compr_len], &mut out[out_pos..], Z_NO_FLUSH);
        in_pos += consumed;
        out_pos += produced;

        if code == Z_STREAM_END {
            break;
        }
        if code == Z_NEED_DICT {
            // The stream must reference exactly our dictionary.
            assert_eq!(inflater.adler, dict_id, "unexpected dictionary id");
            inflater
                .set_dictionary(DICTIONARY)
                .expect("inflateSetDictionary");
            continue;
        }
        assert_eq!(code, Z_OK, "inflate with dictionary");
        assert!(consumed != 0 || produced != 0, "inflate stalled");
    }

    assert_eq!(&out[..out_pos], HELLO, "dictionary round-trip");
}

/// Port of example.c `test_gzio`: exercise the gzip file layer end-to-end —
/// `gzopen`/`gzputc`/`gzputs`/`gzprintf`/`gzseek` on write, then
/// `gzread`/`gzseek`/`gztell`/`gzgetc`/`gzungetc`/`gzgets` on read.
///
/// Gated on the `gz-io` feature (the gzip file layer). A unique temp file is
/// used (parallel-clone safe) and removed afterwards — C hardcodes `"foo.gz"`,
/// which we avoid.
#[cfg(feature = "gz-io")]
#[test]
fn gzio_read_write() {
    use zlib_rs::gz::close::gzclose;
    use zlib_rs::gz::open::{gzopen, gzseek, gztell};
    use zlib_rs::gz::read::{gzgetc, gzgets, gzread, gzungetc};
    use zlib_rs::gz::write::{gzprintf, gzputc, gzputs};

    // Unique temp path so parallel runs never collide; cleaned up at the end.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let path = std::env::temp_dir().join(format!(
        "zlibrs_regression_example_{}_{}.gz",
        std::process::id(),
        nanos
    ));

    // --- write side ---
    {
        let mut file = gzopen(&path, "wb").expect("gzopen (write)");
        // Build "hello, hello!" one piece at a time: 'h', then "ello", then
        // ", hello!" via formatted output.
        gzputc(&mut file, i32::from(b'h'));
        assert_eq!(gzputs(&mut file, "ello"), 4, "gzputs");
        assert_eq!(
            gzprintf(&mut file, format_args!(", {}!", "hello")),
            8,
            "gzprintf"
        );
        // Seek one byte forward, writing a single zero byte — this is the
        // trailing NUL of HELLO (C: gzseek(file, 1L, SEEK_CUR)).
        gzseek(&mut file, 1, SEEK_CUR);
        assert_eq!(gzclose(Some(file)), Z_OK, "gzclose (write)");
    }

    // --- read side ---
    {
        let mut file = gzopen(&path, "rb").expect("gzopen (read)");
        let mut buf = vec![0u8; UNCOMPR_LEN];

        // gzread returns the full 14-byte payload (strlen + 1) and its content
        // is HELLO.
        let n = gzread(&mut file, &mut buf);
        assert_eq!(
            usize::try_from(n).expect("gzread length"),
            HELLO.len(),
            "gzread length"
        );
        assert_eq!(&buf[..HELLO.len()], HELLO, "gzread content");

        // Seek back 8 bytes (from EOF at 14) to position 6; gztell agrees.
        let pos = gzseek(&mut file, -8, SEEK_CUR);
        assert_eq!(pos, 6, "gzseek position");
        assert_eq!(gztell(&file), pos, "gztell");

        // Byte at position 6 is a space; push it back with gzungetc.
        assert_eq!(gzgetc(&mut file), i32::from(b' '), "gzgetc");
        assert_eq!(
            gzungetc(&mut file, i32::from(b' ')),
            i32::from(b' '),
            "gzungetc"
        );

        // gzgets now reads the ungot space followed by the rest of the line.
        // The gz layer returns the raw byte count (which includes the trailing
        // NUL); the C-string length up to that NUL is 7 (" hello!").
        let written = gzgets(&mut file, &mut buf);
        let s_len = buf[..written]
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(written);
        assert_eq!(s_len, 7, "gzgets C-string length");
        assert_eq!(
            &buf[..s_len],
            &HELLO[6..13],
            "gzgets content == \" hello!\""
        );

        assert_eq!(gzclose(Some(file)), Z_OK, "gzclose (read)");
    }

    // Remove the temp artifact; never leave files behind.
    let _ = std::fs::remove_file(&path);
}
