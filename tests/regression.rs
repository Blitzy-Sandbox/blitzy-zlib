//! Regression suite — a 1:1 port of the C `test/example.c` driver.
//!
//! This integration test reproduces **every** behaviour exercised by zlib's
//! canonical `test/example.c` example/regression program, validated against the
//! idiomatic `zlib_rs` public API. The 10 C test functions (`test_compress`,
//! `test_gzio`, `test_deflate`, `test_inflate`, `test_large_deflate`,
//! `test_large_inflate`, `test_flush`, `test_sync`, `test_dict_deflate`,
//! `test_dict_inflate`) plus the `main()` version check are consolidated into
//! eight `#[test]` functions, pairing the natural deflate/inflate halves while
//! preserving every observable behaviour (AAP §0.4.1 — "10 baseline regression
//! cases"; tech-spec testing-parity floor).
//!
//! The C driver's `CHECK_ERR(err, msg)` macro ("abort unless the call returned
//! `Z_OK`") is ported as Rust assertions: `?` propagation on the `Result`-typed
//! idiomatic calls, and `assert_eq!`/`assert!` on the raw inflate return codes.
//!
//! # Fixture / NUL semantics (faithful to `example.c`)
//!
//! C computes the message length as `strlen(hello) + 1` — i.e. it **includes**
//! the trailing NUL — and the dictionary length as `sizeof(dictionary)`, which
//! likewise includes the NUL. To reproduce byte-for-byte behaviour (so that
//! round-trips, the dictionary Adler-32 / `Z_NEED_DICT` handshake, and the
//! `gzseek` offsets all line up exactly), this port treats the fixtures as the
//! exact byte slices including the trailing NUL: [`HELLO`] is 14 bytes and
//! [`DICTIONARY`] is 6 bytes. Decompressed output is compared against the full
//! byte slice; where C uses `strcmp` (which stops at the NUL) the equivalent
//! C-string view is checked explicitly.
//!
//! # Deflate / inflate API asymmetry (do not conflate the two)
//!
//! The `zlib_rs` compressor and decompressor expose deliberately different
//! shapes, mirrored faithfully here:
//!
//! * [`Deflate::compress`] takes a typed [`FlushMode`] enum and returns
//!   `Result<DeflateOutcome, ZlibError>`, whose `.code` is a [`ReturnCode`]
//!   (`ReturnCode::StreamEnd` once a `Finish` stream is complete) and whose
//!   `.consumed` / `.produced` report the byte counts for the call.
//! * [`Inflate::inflate`] takes a **raw `i32`** flush (`Z_NO_FLUSH`, `Z_FINISH`)
//!   and returns a **raw `(i32, usize, usize)`** = `(code, consumed, produced)`;
//!   the running totals live on the public `Inflate::total_in` / `total_out`
//!   fields. Codes are compared against the raw `Z_*` constants.

#![forbid(unsafe_code)]

use zlib_rs::constants::*;
use zlib_rs::{
    Deflate, Inflate, ReturnCode, ZlibError, adler32, compress_bound, compress2, uncompress,
    uncompress2,
};

// ===========================================================================
// Fixtures — byte-identical to `test/example.c`.
// ===========================================================================

/// The test message, **including its trailing NUL** (C: `hello[] = "hello,
/// hello!"` driven by `strlen(hello) + 1` = 14 bytes). The repeated "hello"
/// deliberately stresses the LZ77 match finder.
const HELLO: &[u8] = b"hello, hello!\0";

/// The preset dictionary, **including its trailing NUL** (C: `dictionary[] =
/// "hello"` driven by `sizeof(dictionary)` = 6 bytes).
const DICTIONARY: &[u8] = b"hello\0";

/// Decompression scratch-buffer size (C: `uncomprLen = 20000`).
const UNCOMPR_LEN: usize = 20000;

/// Compression scratch-buffer size (C: `comprLen = 3 * uncomprLen` = 60000).
const COMPR_LEN: usize = 3 * UNCOMPR_LEN;

// ===========================================================================
// Shared helper.
// ===========================================================================

/// Drive `d` to the end of the stream with [`FlushMode::Finish`], appending the
/// compressed bytes into `out` starting at `*out_pos` and advancing the cursor.
///
/// `input` is fed (its remaining tail each iteration) and then drained with
/// empty input until [`ReturnCode::StreamEnd`] is observed — the idiomatic
/// equivalent of C's `do { deflate(&s, Z_FINISH); } while (err != Z_STREAM_END)`
/// loop. The iteration count is bounded by the output capacity purely as an
/// anti-hang safety net; a correct stream always finishes well within it.
fn deflate_to_end(
    d: &mut Deflate,
    input: &[u8],
    out: &mut [u8],
    out_pos: &mut usize,
) -> Result<(), ZlibError> {
    let mut in_pos = 0usize;
    for _ in 0..=out.len() {
        let outcome = d.compress(&input[in_pos..], &mut out[*out_pos..], FlushMode::Finish)?;
        in_pos += outcome.consumed;
        *out_pos += outcome.produced;
        if outcome.code == ReturnCode::StreamEnd {
            return Ok(());
        }
    }
    panic!("deflate did not reach StreamEnd within the output-buffer bound");
}

// ===========================================================================
// 1. Version identity — `example.c` `main()` version check.
// ===========================================================================

/// Mirrors the version sanity check at the top of `example.c`'s `main()`
/// (`zlibVersion()` vs `ZLIB_VERSION`, plus the `ZLIB_VERNUM` print). The
/// frozen baseline is `1.3.2.1-motley` / `0x1321`.
#[test]
fn version_smoke() {
    assert_eq!(zlib_rs::util::zlib_version(), "1.3.2.1-motley");
    assert_eq!(zlib_rs::util::zlib_version(), ZLIB_VERSION);
    assert_eq!(zlib_rs::util::zlib_version_num(), ZLIB_VERNUM);
    assert_eq!(ZLIB_VERNUM, 0x1321);
}

// ===========================================================================
// 2. `compress()` / `uncompress()` round-trip — C `test_compress`.
// ===========================================================================

/// Port of `test_compress`: one-shot compress then uncompress of [`HELLO`],
/// asserting the restored buffer equals the original 14 bytes (and that the
/// C-string view matches "hello, hello!").
#[test]
fn compress_uncompress_roundtrip() -> Result<(), ZlibError> {
    let compressed = compress2(HELLO, Z_DEFAULT_COMPRESSION)?;
    assert!(
        !compressed.is_empty(),
        "compressed output must be non-empty"
    );

    let mut restored = vec![0u8; UNCOMPR_LEN];
    let produced = uncompress(&mut restored, &compressed)?;

    // Full byte-slice comparison (includes the trailing NUL, 14 bytes).
    assert_eq!(
        produced,
        HELLO.len(),
        "uncompress must restore all 14 bytes"
    );
    assert_eq!(
        &restored[..produced],
        HELLO,
        "round-trip must reproduce HELLO exactly"
    );
    // C compares with `strcmp` (stops at the NUL); verify that view too.
    assert_eq!(
        &restored[..produced - 1],
        &HELLO[..HELLO.len() - 1],
        "the C-string portion must equal \"hello, hello!\""
    );
    Ok(())
}

// ===========================================================================
// 3. `compress_bound` + multi-level one-shot round-trips.
//
// Extra coverage split-out (project-guide lists tests/regression.rs = 8 fns).
// Exercises the `compress2` (Vec) / `uncompress2` ((produced, consumed)) /
// `compress_bound` family that `example.c`'s compress test implies.
// ===========================================================================

/// Validate [`compress_bound`] and round-trip [`HELLO`] through every explicit
/// compression level plus [`Z_DEFAULT_COMPRESSION`], proving the bound holds and
/// `uncompress2` reports the full consumed length each time.
#[test]
fn compress_bound_and_levels() -> Result<(), ZlibError> {
    // Empty input carries the fixed 13-byte wrapper/Huffman overhead.
    assert_eq!(compress_bound(0), 13);
    assert!(
        compress_bound(HELLO.len()) >= HELLO.len(),
        "the bound must cover the payload"
    );

    for level in [
        Z_NO_COMPRESSION,
        Z_BEST_SPEED,
        5,
        Z_BEST_COMPRESSION,
        Z_DEFAULT_COMPRESSION,
    ] {
        let compressed = compress2(HELLO, level)?;
        assert!(
            compressed.len() <= compress_bound(HELLO.len()),
            "level {level}: output must be within compress_bound"
        );

        let mut restored = vec![0u8; UNCOMPR_LEN];
        let (produced, consumed) = uncompress2(&mut restored, &compressed)?;
        assert_eq!(
            consumed,
            compressed.len(),
            "level {level}: uncompress2 must consume the whole stream"
        );
        assert_eq!(
            &restored[..produced],
            HELLO,
            "level {level}: round-trip must reproduce HELLO"
        );
    }
    Ok(())
}

// ===========================================================================
// 4. Streaming deflate + inflate with forced 1-byte buffers.
//    Consolidates C `test_deflate` (#3) and `test_inflate` (#4).
// ===========================================================================

/// Port of `test_deflate` + `test_inflate`: deflate [`HELLO`] feeding a single
/// input byte and draining a single output byte at a time (`avail_in =
/// avail_out = 1`), then inflate the result the same way, asserting the
/// round-trip reproduces [`HELLO`]. This stresses partial-progress handling.
#[test]
fn deflate_inflate_small_buffers() -> Result<(), ZlibError> {
    // ---- deflate side (C test_deflate) ----
    let mut d = Deflate::new(Z_DEFAULT_COMPRESSION)?;
    let mut compressed = vec![0u8; COMPR_LEN];
    let mut in_pos = 0usize;
    let mut out_pos = 0usize;

    // Phase 1: Z_NO_FLUSH, one input byte / one output byte per call until all
    // of HELLO is consumed.
    while in_pos < HELLO.len() {
        let outcome = d.compress(
            &HELLO[in_pos..in_pos + 1],
            &mut compressed[out_pos..out_pos + 1],
            FlushMode::NoFlush,
        )?;
        in_pos += outcome.consumed;
        out_pos += outcome.produced;
    }

    // Phase 2: Z_FINISH, still one output byte at a time, until the stream ends.
    loop {
        let outcome = d.compress(
            &[],
            &mut compressed[out_pos..out_pos + 1],
            FlushMode::Finish,
        )?;
        out_pos += outcome.produced;
        if outcome.code == ReturnCode::StreamEnd {
            break;
        }
    }
    let compressed_len = out_pos;

    // ---- inflate side (C test_inflate) ----
    let mut inf = Inflate::new()?;
    let mut restored = vec![0u8; UNCOMPR_LEN];
    let mut in_pos = 0usize;
    let mut out_pos = 0usize;

    while out_pos < restored.len() && in_pos < compressed_len {
        let (code, consumed, produced) = inf.inflate(
            &compressed[in_pos..in_pos + 1],
            &mut restored[out_pos..out_pos + 1],
            Z_NO_FLUSH,
        );
        in_pos += consumed;
        out_pos += produced;
        if code == Z_STREAM_END {
            break;
        }
        assert_eq!(
            code, Z_OK,
            "inflate returned an unexpected code (consumed={consumed}, produced={produced})"
        );
    }

    assert_eq!(
        &restored[..out_pos],
        HELLO,
        "small-buffer round-trip must reproduce HELLO"
    );
    Ok(())
}

// ===========================================================================
// 5. Large-buffer deflate with mid-stream parameter changes + large inflate.
//    Consolidates C `test_large_deflate` (#5, the deflateParams test) and
//    `test_large_inflate` (#6).
// ===========================================================================

/// Port of `test_large_deflate` + `test_large_inflate`: compress a 20000-byte
/// run of zeros at [`Z_BEST_SPEED`], switch compression parameters twice
/// mid-stream via [`Deflate::params`] (the `deflateParams` equivalent), then
/// decode the whole stream in a loop and assert the total decompressed size is
/// `2 * UNCOMPR_LEN + UNCOMPR_LEN / 2` = 50000 bytes.
#[test]
fn large_deflate_inflate_with_param_changes() -> Result<(), ZlibError> {
    // ---- deflate side (C test_large_deflate) ----
    let mut d = Deflate::with_options(Z_BEST_SPEED, MAX_WBITS, DEF_MEM_LEVEL, Strategy::Default)?;
    let zeros = vec![0u8; UNCOMPR_LEN]; // mostly-zero input compresses very well
    let mut compr = vec![0u8; COMPR_LEN];
    let mut out_pos = 0usize;

    // Stage 1: a single Z_NO_FLUSH call must consume ALL input (C asserts
    // `avail_in == 0` afterwards — the "deflate not greedy" check).
    let stage1 = d.compress(&zeros, &mut compr[out_pos..], FlushMode::NoFlush)?;
    assert_eq!(
        stage1.consumed, UNCOMPR_LEN,
        "deflate must consume all input in one call (not greedy)"
    );
    out_pos += stage1.produced;

    // Switch to "no compression" mid-stream. Any pending block is flushed into
    // the output buffer; advance the cursor by what `params` produced. The
    // strategy argument is a raw `i32` here (== C `deflateParams`).
    out_pos += d.params(Z_NO_COMPRESSION, Z_DEFAULT_STRATEGY, &mut compr[out_pos..])?;

    // Feed UNCOMPR_LEN / 2 bytes of already-emitted data (arbitrary content).
    // C aliases the output buffer as input here; Rust forbids aliasing a slice
    // mutably and immutably, so snapshot the leading bytes first.
    let already_emitted: Vec<u8> = compr[..UNCOMPR_LEN / 2].to_vec();
    let stage2 = d.compress(&already_emitted, &mut compr[out_pos..], FlushMode::NoFlush)?;
    assert_eq!(
        stage2.consumed,
        UNCOMPR_LEN / 2,
        "all fed bytes must be consumed"
    );
    out_pos += stage2.produced;

    // Switch back to best compression with the filtered strategy and re-feed the
    // zero buffer.
    out_pos += d.params(Z_BEST_COMPRESSION, Z_FILTERED, &mut compr[out_pos..])?;
    let stage3 = d.compress(&zeros, &mut compr[out_pos..], FlushMode::NoFlush)?;
    assert_eq!(stage3.consumed, UNCOMPR_LEN, "all zeros must be consumed");
    out_pos += stage3.produced;

    // Finish the stream.
    deflate_to_end(&mut d, &[], &mut compr, &mut out_pos)?;
    let compressed_len = out_pos;

    // ---- inflate side (C test_large_inflate) ----
    // Decode in a loop, discarding the output each iteration; the running total
    // is what we assert against.
    let mut inf = Inflate::new()?;
    let mut scratch = vec![0u8; UNCOMPR_LEN];
    let mut in_pos = 0usize;
    let mut guard = 0u32;
    loop {
        let (code, consumed, _produced) =
            inf.inflate(&compr[in_pos..compressed_len], &mut scratch, Z_NO_FLUSH);
        in_pos += consumed;
        if code == Z_STREAM_END {
            break;
        }
        assert_eq!(code, Z_OK, "large inflate returned an unexpected code");
        guard += 1;
        assert!(guard < 100, "large inflate failed to terminate");
    }

    let expected_total = (2 * UNCOMPR_LEN + UNCOMPR_LEN / 2) as u64;
    assert_eq!(
        inf.total_out, expected_total,
        "large_inflate total_out must be 50000"
    );
    Ok(())
}

// ===========================================================================
// 6. Full-flush + deliberate corruption, then `inflateSync` recovery.
//    Consolidates C `test_flush` (#7) and `test_sync` (#8).
// ===========================================================================

/// Port of `test_flush` + `test_sync`: deflate the first 3 bytes of [`HELLO`]
/// with [`FlushMode::FullFlush`], corrupt the first compressed block, finish the
/// stream, then recover past the damaged block with [`Inflate::sync`]
/// (`inflateSync`). The known-good prefix "hel" prepended to the recovered tail
/// must reconstruct the full message.
#[test]
fn flush_then_sync_recovery() -> Result<(), ZlibError> {
    // ---- deflate side (C test_flush) ----
    let mut d = Deflate::new(Z_DEFAULT_COMPRESSION)?;
    let mut compr = vec![0u8; COMPR_LEN];
    let mut out_pos = 0usize;

    // Deflate "hel" with a FULL_FLUSH: emits a flush marker AND resets the
    // window so the following block is self-contained (and thus decodable after
    // the first block is skipped).
    let head = d.compress(&HELLO[..3], &mut compr[out_pos..], FlushMode::FullFlush)?;
    assert_eq!(
        head.consumed, 3,
        "full-flush must consume the 3 prefix bytes"
    );
    out_pos += head.produced;

    // Force an error in the first compressed block (C: `compr[3]++`).
    compr[3] = compr[3].wrapping_add(1);

    // Deflate the remaining bytes ("lo, hello!\0") and finish.
    deflate_to_end(&mut d, &HELLO[3..], &mut compr, &mut out_pos)?;
    let damaged_len = out_pos;

    // ---- inflate side (C test_sync) ----
    let mut inf = Inflate::new()?;
    let mut restored = vec![0u8; UNCOMPR_LEN];

    // Feed just the 2-byte zlib header.
    let (code_header, consumed_header, _produced) =
        inf.inflate(&compr[..2], &mut restored, Z_NO_FLUSH);
    assert_eq!(code_header, Z_OK, "header-only inflate should return Z_OK");
    let mut in_pos = consumed_header;

    // Skip the damaged region: scan the rest for the next flush marker.
    let (code_sync, sync_consumed) = inf.sync(&compr[in_pos..damaged_len]);
    assert_eq!(
        code_sync, Z_OK,
        "inflateSync should locate the flush marker"
    );
    in_pos += sync_consumed;

    // Finish decoding from just past the marker (the self-contained block).
    let mut out_pos = 0usize;
    for _ in 0..=damaged_len {
        let (code, consumed, produced) = inf.inflate(
            &compr[in_pos..damaged_len],
            &mut restored[out_pos..],
            Z_FINISH,
        );
        in_pos += consumed;
        out_pos += produced;
        if code == Z_STREAM_END {
            break;
        }
        assert_eq!(
            code, Z_OK,
            "sync-recovery inflate returned an unexpected code"
        );
        if consumed == 0 && produced == 0 {
            break; // no progress — avoid spinning (bounded by the for-loop too)
        }
    }

    // C prints "hel%s" — i.e. the known prefix "hel" plus the recovered tail.
    let mut reconstructed = b"hel".to_vec();
    reconstructed.extend_from_slice(&restored[..out_pos]);
    assert!(
        reconstructed.starts_with(b"hel"),
        "recovered text must begin with \"hel\""
    );
    assert_eq!(
        reconstructed, HELLO,
        "inflateSync recovery must reconstruct the full message"
    );
    Ok(())
}

// ===========================================================================
// 7. Preset-dictionary deflate + inflate (the `Z_NEED_DICT` handshake).
//    Consolidates C `test_dict_deflate` (#9) and `test_dict_inflate` (#10).
// ===========================================================================

/// Port of `test_dict_deflate` + `test_dict_inflate`: compress [`HELLO`] with a
/// preset [`DICTIONARY`], then decompress, observing the [`Z_NEED_DICT`]
/// handshake — the decompressor's reported `adler` must equal the dictionary's
/// Adler-32, after which supplying the dictionary lets decoding complete and
/// reproduce [`HELLO`].
#[test]
fn dictionary_deflate_inflate() -> Result<(), ZlibError> {
    // ---- deflate side (C test_dict_deflate) ----
    let mut d = Deflate::new(Z_BEST_COMPRESSION)?;
    d.set_dictionary(DICTIONARY)?;
    // C captures `dictId = c_stream.adler` after deflateSetDictionary. The crate
    // does not expose the stream's adler on `Deflate`, so compute the same value
    // directly: the Adler-32 of the dictionary seeded with 1.
    let dict_id = adler32(1, DICTIONARY);

    let mut compr = vec![0u8; COMPR_LEN];
    let mut out_pos = 0usize;
    deflate_to_end(&mut d, HELLO, &mut compr, &mut out_pos)?;
    let dict_compressed = compr[..out_pos].to_vec();

    // ---- inflate side (C test_dict_inflate) ----
    let mut inf = Inflate::new()?;
    let mut restored = vec![0u8; UNCOMPR_LEN];
    let mut in_pos = 0usize;
    let mut out_pos = 0usize;
    let mut saw_need_dict = false;

    for _ in 0..=dict_compressed.len() + 4 {
        let (code, consumed, produced) = inf.inflate(
            &dict_compressed[in_pos..],
            &mut restored[out_pos..],
            Z_NO_FLUSH,
        );
        in_pos += consumed;
        out_pos += produced;

        if code == Z_STREAM_END {
            break;
        }
        if code == Z_NEED_DICT {
            assert_eq!(
                inf.adler, dict_id,
                "Z_NEED_DICT must report the dictionary's Adler-32"
            );
            inf.set_dictionary(DICTIONARY)?;
            saw_need_dict = true;
            continue;
        }
        assert_eq!(code, Z_OK, "inflate-with-dict returned an unexpected code");
    }

    assert!(saw_need_dict, "the Z_NEED_DICT handshake must occur");
    assert_eq!(
        &restored[..out_pos],
        HELLO,
        "dictionary round-trip must reproduce HELLO"
    );
    Ok(())
}

// ===========================================================================
// 8. gzip file read/write — C `test_gzio`.
//    Gated by `gz-io` (a default feature): the gz file layer is unavailable
//    without it. Every other test uses always-available APIs.
// ===========================================================================

/// Port of `test_gzio`: write a `.gz` file via the byte-oriented `gz*` API
/// (`gzputc` / `gzputs` / `gzprintf` / a forward `gzseek` that zero-fills), then
/// read it back exercising `gzread` / `gzseek` / `gztell` / `gzgetc` /
/// `gzungetc` / `gzgets`. A unique temp path keeps parallel test processes from
/// colliding, and the file is removed on completion.
#[cfg(feature = "gz-io")]
#[test]
fn gzio_read_write() {
    use std::path::PathBuf;
    use zlib_rs::gz::{
        gzclose, gzgetc, gzgets, gzopen, gzprintf, gzputc, gzputs, gzread, gzseek, gztell, gzungetc,
    };

    // Unique per-process temp path (parallel clones must not collide).
    let mut path: PathBuf = std::env::temp_dir();
    path.push(format!("zlib_rs_regression_{}.gz", std::process::id()));

    // ---- write side ----
    {
        let mut file = gzopen(&path, "wb").expect("gzopen(wb) should succeed");
        assert_eq!(
            gzputc(&mut file, i32::from(b'h')),
            i32::from(b'h'),
            "gzputc must echo the written byte"
        );
        assert_eq!(gzputs(&mut file, b"ello"), 4, "gzputs must write 4 bytes");
        assert_eq!(
            gzprintf(&mut file, format_args!(", {}!", "hello")),
            8,
            "gzprintf must write 8 bytes"
        );
        // Forward seek in write mode adds one zero byte (C: `gzseek(file, 1L,
        // SEEK_CUR)`), making the payload exactly "hello, hello!\0" (14 bytes).
        gzseek(&mut file, 1, SEEK_CUR);
        assert_eq!(gzclose(Some(file)), Z_OK, "gzclose(wb) should succeed");
    }

    // ---- read side ----
    {
        let mut file = gzopen(&path, "rb").expect("gzopen(rb) should succeed");
        let mut buf = vec![0u8; UNCOMPR_LEN];

        // gzread returns the full 14-byte payload and it equals HELLO.
        let n = gzread(&mut file, &mut buf);
        assert_eq!(n, HELLO.len() as i32, "gzread must return 14");
        assert_eq!(&buf[..n as usize], HELLO, "gzread payload must equal HELLO");

        // Seek back 8 bytes → logical position 6 (the space after the comma).
        let pos = gzseek(&mut file, -8, SEEK_CUR);
        assert_eq!(pos, 6, "gzseek(-8, SEEK_CUR) must land at position 6");
        assert_eq!(gztell(&file), 6, "gztell must agree with gzseek");

        // gzgetc reads the space at position 6, then gzungetc pushes it back.
        assert_eq!(
            gzgetc(&mut file),
            i32::from(b' '),
            "gzgetc must read a space"
        );
        assert_eq!(
            gzungetc(i32::from(b' '), &mut file),
            i32::from(b' '),
            "gzungetc must echo the pushed-back byte"
        );

        // gzgets reads " hello!" — the Rust API returns the raw byte count
        // copied (which includes the payload's embedded trailing NUL), so derive
        // the C-string length the way C's `strlen` would (== 7).
        let written = gzgets(&mut file, &mut buf);
        let cstr_len = buf[..written]
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(written);
        assert_eq!(cstr_len, 7, "gzgets C-string length must be 7");
        assert_eq!(
            &buf[..cstr_len],
            &HELLO[6..13],
            "gzgets must yield \" hello!\""
        );

        assert_eq!(gzclose(Some(file)), Z_OK, "gzclose(rb) should succeed");
    }

    // ---- cleanup ----
    let _ = std::fs::remove_file(&path);
}
