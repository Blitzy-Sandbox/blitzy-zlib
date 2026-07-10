//! Canonical zlib regression driver — a faithful Rust port of `test/example.c`,
//! the reference zlib exerciser shipped with the C library.
//!
//! This integration test operationalizes the "official zlib test vectors"
//! conformance requirement (AAP §0.6.4 / §0.7.1). It reproduces the ten
//! `example.c` helper functions as independent `#[test]`s driving the public
//! [`zlib_rs`] API:
//!
//! * [`version_check`] — the version guard from `example.c`'s `main`.
//! * [`test_compress`] — one-call [`compress`] / [`uncompress`] round-trip.
//! * [`test_gzio`] — gzip file I/O (gated behind the `gz-io` feature, mirroring
//!   the C `NO_GZCOMPRESS` guard around `test_gzio`).
//! * [`test_deflate`] / [`test_inflate`] — small-buffer streaming with the
//!   `example.c` "force 1-byte `avail_in`/`avail_out`" loops.
//! * [`test_large_deflate`] / [`test_large_inflate`] — large-buffer streaming
//!   with mid-stream `deflateParams` level/strategy switches; the decisive
//!   assertion is `total_out == 50000` on inflate.
//! * [`test_flush`] / [`test_sync`] — `Z_FULL_FLUSH`, deliberate first-block
//!   corruption, and `inflateSync` recovery.
//! * [`test_dict_deflate`] / [`test_dict_inflate`] — preset-dictionary
//!   handshake, verifying the dictionary Adler-32 id round-trips.
//!
//! Beyond the faithful `example.c` port, two regression guards derived from
//! documented `deflate.c` / `inflate.c` behavior strengthen coverage (AAP
//! §0.4.1 lists `inflate.c` / `deflate.c` as this file's regression sources):
//!
//! * [`test_deflate_bound_is_upper_bound`] — `deflateBound` must be a genuine
//!   upper limit on the compressed size, even for incompressible input; a
//!   single `Z_FINISH` pass into a buffer sized to exactly the bound must
//!   finish without running out of space.
//! * [`test_window_wrap_round_trip`] — data larger than the 32 KiB sliding
//!   window must round-trip byte-for-byte, exercising the circular-window wrap
//!   in both the deflate and inflate engines with small output windows.
//!
//! All ops bind to the idiomatic `zlib_rs` API (no FFI, no `unsafe`). The
//! streaming API is slice-based: each `deflate`/`inflate` call takes an `input`
//! and `output` slice and reports `consumed`/`produced`, while the running
//! `total_in`/`total_out`/`adler` counters live on the [`ZStream`]. The C
//! `next_in`/`avail_in` cursor loops are therefore reproduced by advancing local
//! offsets by the reported `consumed`/`produced`.

use zlib_rs::constants::{Z_FINISH, Z_FULL_FLUSH, Z_NO_FLUSH};
use zlib_rs::deflate::{
    deflate, deflate_bound, deflate_end, deflate_init, deflate_params, deflate_set_dictionary,
};
use zlib_rs::inflate::{inflate, inflate_end, inflate_init, inflate_set_dictionary, inflate_sync};
use zlib_rs::{
    ReturnCode, Strategy, Z_BEST_COMPRESSION, Z_BEST_SPEED, Z_DEFAULT_COMPRESSION,
    Z_NO_COMPRESSION, ZLIB_VERNUM, ZLIB_VERSION, ZStream, compress, compress_bound, uncompress,
    zlibCompileFlags, zlibVersion,
};

// ===========================================================================
// Shared fixtures — mirror `example.c` byte-for-byte.
// ===========================================================================

/// The canonical `example.c` payload: `hello[] = "hello, hello!"`. The C driver
/// compresses `strlen(hello) + 1` bytes, i.e. it INCLUDES the trailing NUL, for
/// a total of 14 bytes. The repeated "hello" deliberately stresses the coder.
/// We therefore carry the explicit trailing NUL so every length matches C.
const HELLO: &[u8] = b"hello, hello!\0";

/// The preset dictionary: `example.c` declares `dictionary[] = "hello"` and
/// passes `sizeof(dictionary)`, which is 6 bytes INCLUDING the terminating NUL.
/// We use the 6-byte form so the dictionary Adler-32 id and the resulting
/// compressed bytes match reference zlib exactly.
const DICTIONARY: &[u8] = b"hello\0";

/// The string `example.c` writes into the output buffer before every
/// decompression (`strcpy((char*)uncompr, "garbage")`) to guarantee the buffer
/// is overwritten and never read while uninitialized.
const GARBAGE: &[u8] = b"garbage";

/// Size of the (zero-filled) plaintext buffer — `uncomprLen` in `example.c`.
const UNCOMPR_LEN: usize = 20000;

/// Size of the compressed buffer — `comprLen = 3 * uncomprLen` in `example.c`.
const COMPR_LEN: usize = 3 * UNCOMPR_LEN;

// ===========================================================================
// Shared helpers — factor the fixtures that `example.c`'s `main` threads
// between paired tests, while keeping every `#[test]` independently runnable.
// ===========================================================================

/// Small-buffer deflate of [`HELLO`] at `Z_DEFAULT_COMPRESSION`, forcing 1-byte
/// `avail_in`/`avail_out` windows exactly as C `test_deflate` does. Returns the
/// compressed stream for reuse by the inflate-side tests.
fn deflate_hello() -> Vec<u8> {
    let mut strm = ZStream::new();
    deflate_init(&mut strm, Z_DEFAULT_COMPRESSION).expect("deflateInit");

    let len = HELLO.len();
    let mut compr = vec![0u8; COMPR_LEN];
    let mut in_off = 0usize;
    let mut out_off = 0usize;

    // `Z_NO_FLUSH` loop: feed one byte in / allow one byte out per iteration
    // until all input is consumed (C: `while (total_in != len && total_out <
    // comprLen)` with `avail_in = avail_out = 1`).
    while (strm.total_in as usize) != len && (strm.total_out as usize) < COMPR_LEN {
        let in_end = (in_off + 1).min(len);
        let out_end = (out_off + 1).min(compr.len());
        let outcome = deflate(
            &mut strm,
            &HELLO[in_off..in_end],
            &mut compr[out_off..out_end],
            Z_NO_FLUSH,
        );
        assert_eq!(outcome.code, ReturnCode::Ok, "deflate Z_NO_FLUSH");
        in_off += outcome.consumed;
        out_off += outcome.produced;
    }

    // `Z_FINISH` loop, still with 1-byte output windows, until `Z_STREAM_END`.
    loop {
        let out_end = (out_off + 1).min(compr.len());
        let outcome = deflate(&mut strm, &[], &mut compr[out_off..out_end], Z_FINISH);
        out_off += outcome.produced;
        if outcome.code == ReturnCode::StreamEnd {
            break;
        }
        assert_eq!(outcome.code, ReturnCode::Ok, "deflate Z_FINISH");
    }

    deflate_end(&mut strm).expect("deflateEnd");
    compr.truncate(out_off);
    compr
}

/// Large-buffer deflate with dynamic level/strategy changes — the stream built
/// by C `test_large_deflate`. It compresses a 20000-byte zero-filled buffer,
/// switches to `Z_NO_COMPRESSION` and feeds back `uncomprLen/2` bytes of
/// already-compressed data, switches to `Z_BEST_COMPRESSION` + `Z_FILTERED` and
/// feeds the full plaintext again, then finishes. The greedy-consumption check
/// from C is asserted inline. The returned stream decompresses to exactly
/// `2 * UNCOMPR_LEN + UNCOMPR_LEN / 2 == 50000` bytes.
fn large_deflate_stream() -> Vec<u8> {
    let uncompr = vec![0u8; UNCOMPR_LEN];
    let mut compr = vec![0u8; COMPR_LEN];
    let mut strm = ZStream::new();
    deflate_init(&mut strm, Z_BEST_SPEED).expect("deflateInit");
    let mut out_off = 0usize;

    // Step 1: one greedy `Z_NO_FLUSH` over the full input. The mostly-zero
    // buffer compresses tightly, so all input must be consumed in one call.
    let o1 = deflate(&mut strm, &uncompr, &mut compr[out_off..], Z_NO_FLUSH);
    assert_eq!(o1.code, ReturnCode::Ok, "large deflate step 1");
    assert_eq!(o1.consumed, UNCOMPR_LEN, "deflate not greedy");
    out_off += o1.produced;

    // Step 2: switch to no compression and feed back `uncomprLen/2` bytes of the
    // data produced so far. In C `avail_in == 0` at the `deflateParams` call, so
    // the internal `Z_BLOCK` flush here is handed an empty input slice. The C
    // driver ignores the `deflateParams` return value, so we only guard against
    // a hard `StreamError` (a genuinely invalid parameter combination).
    let feedback = compr[..UNCOMPR_LEN / 2].to_vec();
    let p1 = deflate_params(
        &mut strm,
        &[],
        &mut compr[out_off..],
        Z_NO_COMPRESSION,
        Strategy::Default,
    );
    assert_ne!(p1.code, ReturnCode::StreamError, "deflateParams 1");
    out_off += p1.produced;

    let o2 = deflate(&mut strm, &feedback, &mut compr[out_off..], Z_NO_FLUSH);
    assert_eq!(o2.code, ReturnCode::Ok, "large deflate step 2");
    assert_eq!(o2.consumed, feedback.len(), "feedback not fully consumed");
    out_off += o2.produced;

    // Step 3: switch back to best compression + filtered strategy and feed the
    // full plaintext again.
    let p2 = deflate_params(
        &mut strm,
        &[],
        &mut compr[out_off..],
        Z_BEST_COMPRESSION,
        Strategy::Filtered,
    );
    assert_ne!(p2.code, ReturnCode::StreamError, "deflateParams 2");
    out_off += p2.produced;

    let o3 = deflate(&mut strm, &uncompr, &mut compr[out_off..], Z_NO_FLUSH);
    assert_eq!(o3.code, ReturnCode::Ok, "large deflate step 3");
    assert_eq!(o3.consumed, UNCOMPR_LEN, "third input not fully consumed");
    out_off += o3.produced;

    // Step 4: finish the stream.
    loop {
        let outcome = deflate(&mut strm, &[], &mut compr[out_off..], Z_FINISH);
        out_off += outcome.produced;
        if outcome.code == ReturnCode::StreamEnd {
            break;
        }
        assert_eq!(outcome.code, ReturnCode::Ok, "large deflate finish");
    }

    deflate_end(&mut strm).expect("deflateEnd");
    compr.truncate(out_off);
    compr
}

/// Deflate with a full flush followed by deliberate corruption of the first
/// compressed block — the stream built by C `test_flush`. The first 3 bytes of
/// [`HELLO`] are deflated with `Z_FULL_FLUSH`, `compr[3]` is incremented to
/// damage that block, and the remaining `len - 3` bytes are finished. Returns
/// the (corrupted) stream together with its exact compressed length.
fn deflate_full_flush_corrupted() -> (Vec<u8>, usize) {
    let len = HELLO.len();
    let mut strm = ZStream::new();
    deflate_init(&mut strm, Z_DEFAULT_COMPRESSION).expect("deflateInit");
    let mut compr = vec![0u8; COMPR_LEN];
    let mut out_off = 0usize;

    // Deflate the first 3 bytes with `Z_FULL_FLUSH` (ample output → all 3
    // consumed and the block emitted).
    let o1 = deflate(&mut strm, &HELLO[..3], &mut compr[out_off..], Z_FULL_FLUSH);
    assert_eq!(o1.code, ReturnCode::Ok, "deflate Z_FULL_FLUSH");
    assert_eq!(o1.consumed, 3, "full-flush should consume 3 bytes");
    out_off += o1.produced;

    // Force an error in the first compressed block (C: `compr[3]++`). The full
    // flush emits well over 4 bytes, so index 3 lies inside the first block and
    // ahead of the second block's output region (`compr[out_off..]`).
    assert!(
        out_off > 3,
        "first block must exceed 3 bytes to corrupt compr[3]"
    );
    compr[3] = compr[3].wrapping_add(1);

    // Deflate the remaining `len - 3` bytes with `Z_FINISH`; accept
    // `Z_STREAM_END` (C: `if (err != Z_STREAM_END) CHECK_ERR(err, ...)`).
    let o2 = deflate(&mut strm, &HELLO[3..len], &mut compr[out_off..], Z_FINISH);
    out_off += o2.produced;
    if o2.code != ReturnCode::StreamEnd {
        assert_eq!(o2.code, ReturnCode::Ok, "deflate Z_FINISH (flush test)");
        loop {
            let outcome = deflate(&mut strm, &[], &mut compr[out_off..], Z_FINISH);
            out_off += outcome.produced;
            if outcome.code == ReturnCode::StreamEnd {
                break;
            }
            assert_eq!(
                outcome.code,
                ReturnCode::Ok,
                "deflate Z_FINISH loop (flush test)"
            );
        }
    }

    deflate_end(&mut strm).expect("deflateEnd");
    compr.truncate(out_off);
    (compr, out_off)
}

/// Deflate [`HELLO`] with a preset dictionary at `Z_BEST_COMPRESSION` — the
/// stream built by C `test_dict_deflate`. Returns the compressed stream and the
/// dictionary Adler-32 id captured from `strm.adler` immediately after
/// `deflateSetDictionary` (C: `dictId = c_stream.adler`).
fn deflate_with_dict() -> (Vec<u8>, u32) {
    let mut strm = ZStream::new();
    deflate_init(&mut strm, Z_BEST_COMPRESSION).expect("deflateInit");
    deflate_set_dictionary(&mut strm, DICTIONARY).expect("deflateSetDictionary");
    let dict_id = strm.adler;

    let mut compr = vec![0u8; COMPR_LEN];
    let outcome = deflate(&mut strm, HELLO, &mut compr, Z_FINISH);
    assert_eq!(
        outcome.code,
        ReturnCode::StreamEnd,
        "deflate should report Z_STREAM_END"
    );
    let produced = outcome.produced;

    deflate_end(&mut strm).expect("deflateEnd");
    compr.truncate(produced);
    (compr, dict_id)
}

// ===========================================================================
// Tests — one per `example.c` function.
// ===========================================================================

/// Port of the version guard in `example.c`'s `main`: the linked
/// `zlibVersion()` must share its first character with the compile-time
/// `ZLIB_VERSION`, and here (single crate, no dynamic linking) must equal it
/// exactly. Also pins the `ZLIB_VERSION` / `ZLIB_VERNUM` constants and confirms
/// `zlibCompileFlags()` returns without panicking.
#[test]
fn version_check() {
    let linked = zlibVersion();
    assert_eq!(
        linked.as_bytes()[0],
        ZLIB_VERSION.as_bytes()[0],
        "incompatible zlib version"
    );
    assert_eq!(linked, ZLIB_VERSION, "different zlib version linked");
    assert_eq!(ZLIB_VERSION, "1.3.2.1-motley");
    assert_eq!(ZLIB_VERNUM, 0x1321);

    // `zlibCompileFlags()` must simply return (its exact value is platform- and
    // configuration-dependent).
    let _flags = zlibCompileFlags();
}

/// Port of C `test_compress`: compress [`HELLO`] with the one-call [`compress`],
/// clobber the output buffer with "garbage", then [`uncompress`] and assert the
/// recovered bytes equal [`HELLO`] (including the trailing NUL).
#[test]
fn test_compress() {
    let source = HELLO;
    let bound = compress_bound(source.len());
    let mut compr = vec![0u8; bound];
    let compressed_len = compress(&mut compr, source).expect("compress");
    compr.truncate(compressed_len);

    let mut uncompr = vec![0u8; UNCOMPR_LEN];
    uncompr[..GARBAGE.len()].copy_from_slice(GARBAGE);
    let recovered = uncompress(&mut uncompr, &compr).expect("uncompress");

    assert_eq!(&uncompr[..recovered], HELLO, "bad uncompress");
}

/// Port of C `test_deflate`: drive the small-buffer deflate loop and assert it
/// produces a non-empty stream. The byte-exact round-trip is asserted by
/// [`test_inflate`], which consumes the very same helper output.
#[test]
fn test_deflate() {
    let compr = deflate_hello();
    assert!(!compr.is_empty(), "deflate produced no output");
}

/// Port of C `test_inflate`: inflate the small-buffer deflate output with forced
/// 1-byte `avail_in`/`avail_out` windows in a `Z_NO_FLUSH` loop until
/// `Z_STREAM_END`, then assert the recovered bytes equal [`HELLO`].
#[test]
fn test_inflate() {
    let compr = deflate_hello();

    let mut strm = ZStream::new();
    inflate_init(&mut strm).expect("inflateInit");
    let mut uncompr = vec![0u8; UNCOMPR_LEN];
    uncompr[..GARBAGE.len()].copy_from_slice(GARBAGE);
    let mut in_off = 0usize;
    let mut out_off = 0usize;

    while (strm.total_out as usize) < UNCOMPR_LEN && (strm.total_in as usize) < compr.len() {
        let in_end = (in_off + 1).min(compr.len());
        let out_end = (out_off + 1).min(uncompr.len());
        let outcome = inflate(
            &mut strm,
            &compr[in_off..in_end],
            &mut uncompr[out_off..out_end],
            Z_NO_FLUSH,
        );
        in_off += outcome.consumed;
        out_off += outcome.produced;
        if outcome.code == ReturnCode::StreamEnd {
            break;
        }
        assert_eq!(outcome.code, ReturnCode::Ok, "inflate");
    }

    inflate_end(&mut strm).expect("inflateEnd");
    assert_eq!(&uncompr[..out_off], HELLO, "bad inflate");
}

/// Port of C `test_large_deflate`: build the large, level-switching stream and
/// assert it is non-empty. The greedy-consumption invariant is checked inside
/// [`large_deflate_stream`]; the decompressed-length invariant is checked by
/// [`test_large_inflate`].
#[test]
fn test_large_deflate() {
    let compr = large_deflate_stream();
    assert!(!compr.is_empty(), "large deflate produced no output");
}

/// Port of C `test_large_inflate`: inflate the level-switching stream, discarding
/// the output, in a `Z_NO_FLUSH` loop until `Z_STREAM_END`. The decisive
/// assertion is `total_out == 2 * UNCOMPR_LEN + UNCOMPR_LEN / 2 == 50000`.
#[test]
fn test_large_inflate() {
    let compr = large_deflate_stream();

    let mut strm = ZStream::new();
    inflate_init(&mut strm).expect("inflateInit");
    let mut uncompr = vec![0u8; UNCOMPR_LEN];
    uncompr[..GARBAGE.len()].copy_from_slice(GARBAGE);
    let mut in_off = 0usize;

    loop {
        // Reuse the whole output buffer each iteration to discard the output.
        let outcome = inflate(&mut strm, &compr[in_off..], &mut uncompr, Z_NO_FLUSH);
        in_off += outcome.consumed;
        if outcome.code == ReturnCode::StreamEnd {
            break;
        }
        assert_eq!(outcome.code, ReturnCode::Ok, "large inflate");
    }

    inflate_end(&mut strm).expect("inflateEnd");
    assert_eq!(
        strm.total_out,
        (2 * UNCOMPR_LEN + UNCOMPR_LEN / 2) as u64,
        "bad large inflate total_out"
    );
}

/// Port of C `test_flush`: build the full-flush + corrupted-first-block stream
/// and assert its length is consistent and large enough to contain the damaged
/// block. The recovery is exercised by [`test_sync`].
#[test]
fn test_flush() {
    let (compr, compr_len) = deflate_full_flush_corrupted();
    assert_eq!(compr.len(), compr_len, "reported length must match buffer");
    assert!(
        compr_len > 4,
        "flush stream must contain a corrupted first block"
    );
}

/// Port of C `test_sync`: from the corrupted full-flush stream, inflate only the
/// 2-byte zlib header, call [`inflate_sync`] to skip the damaged first block,
/// then finish. The first block held "hel" (now lost); the recovered tail is
/// therefore `&HELLO[3..]` (`"lo, hello!\0"`), which C reconstructs by printing
/// `"hel"` ahead of it.
#[test]
fn test_sync() {
    let (compr, compr_len) = deflate_full_flush_corrupted();

    let mut strm = ZStream::new();
    inflate_init(&mut strm).expect("inflateInit");
    let mut uncompr = vec![0u8; UNCOMPR_LEN];
    uncompr[..GARBAGE.len()].copy_from_slice(GARBAGE);

    // Read just the zlib header (C: `avail_in = 2`).
    let o1 = inflate(&mut strm, &compr[..2], &mut uncompr, Z_NO_FLUSH);
    assert_eq!(o1.code, ReturnCode::Ok, "inflate header");
    assert_eq!(o1.consumed, 2, "header should consume 2 bytes");
    let mut in_off = o1.consumed;
    let out_off = o1.produced;

    // Skip the damaged block: `inflateSync` scans for the flush marker.
    let (sync_code, sync_consumed) = inflate_sync(&mut strm, &compr[in_off..compr_len]);
    assert_eq!(sync_code, ReturnCode::Ok, "inflateSync");
    in_off += sync_consumed;

    // Finish decoding the intact tail.
    let o2 = inflate(
        &mut strm,
        &compr[in_off..compr_len],
        &mut uncompr[out_off..],
        Z_FINISH,
    );
    assert_eq!(
        o2.code,
        ReturnCode::StreamEnd,
        "inflate should report Z_STREAM_END"
    );
    let total_out = out_off + o2.produced;

    inflate_end(&mut strm).expect("inflateEnd");
    assert_eq!(
        &uncompr[..total_out],
        &HELLO[3..],
        "bad inflateSync recovery"
    );
}

/// Port of C `test_dict_deflate`: build a dictionary-compressed stream and assert
/// it is non-empty with a non-zero dictionary id. The dictionary handshake is
/// exercised end-to-end by [`test_dict_inflate`].
#[test]
fn test_dict_deflate() {
    let (compr, dict_id) = deflate_with_dict();
    assert!(!compr.is_empty(), "dictionary deflate produced no output");
    assert_ne!(dict_id, 0, "dictionary id (adler) should be set");
}

/// Port of C `test_dict_inflate`: inflate the dictionary-compressed stream. On
/// `Z_NEED_DICT` the required dictionary id (`strm.adler`) must equal the id
/// captured on the deflate side; supplying [`DICTIONARY`] then lets decoding
/// complete. The recovered bytes must equal [`HELLO`].
#[test]
fn test_dict_inflate() {
    let (compr, dict_id) = deflate_with_dict();

    let mut strm = ZStream::new();
    inflate_init(&mut strm).expect("inflateInit");
    let mut uncompr = vec![0u8; UNCOMPR_LEN];
    uncompr[..GARBAGE.len()].copy_from_slice(GARBAGE);
    let mut in_off = 0usize;
    let mut out_off = 0usize;

    loop {
        let outcome = inflate(
            &mut strm,
            &compr[in_off..],
            &mut uncompr[out_off..],
            Z_NO_FLUSH,
        );
        in_off += outcome.consumed;
        out_off += outcome.produced;
        match outcome.code {
            ReturnCode::StreamEnd => break,
            ReturnCode::NeedDict => {
                assert_eq!(strm.adler, dict_id, "unexpected dictionary");
                inflate_set_dictionary(&mut strm, DICTIONARY).expect("inflateSetDictionary");
            }
            ReturnCode::Ok => {}
            other => panic!("inflate with dict: {other:?}"),
        }
    }

    inflate_end(&mut strm).expect("inflateEnd");
    assert_eq!(&uncompr[..out_off], HELLO, "bad inflate with dict");
}

// ===========================================================================
// Additional regression guards — beyond the `example.c` port, derived from
// documented `deflate.c` / `inflate.c` behavior (AAP §0.4.1). Both are
// deterministic, dependency-free, and green in BOTH the default and
// `--no-default-features` configurations.
// ===========================================================================

/// Regression guard derived from `deflate.c`'s `deflateBound`: the reported
/// bound must be a *true* upper limit on the compressed size for any input,
/// including incompressible data — an underestimated bound historically caused
/// output-buffer overruns. A worst-case (deterministic pseudo-random) buffer is
/// deflated in a single `Z_FINISH` pass into an output buffer sized to exactly
/// [`deflate_bound`]; the stream must finish (`Z_STREAM_END`) without exhausting
/// that space, and the result must round-trip back to the original bytes.
///
/// Uses only the safe streaming API and a deterministic input, so it is stable
/// in both the default and `--no-default-features` configurations and needs no
/// C toolchain.
#[test]
fn test_deflate_bound_is_upper_bound() {
    // Deterministic, incompressible input via a xorshift PRNG (reproducible so
    // the guard never flakes and needs no external entropy source).
    let mut state: u32 = 0x1234_5678;
    let mut data = vec![0u8; 8192];
    for byte in data.iter_mut() {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        *byte = (state >> 24) as u8;
    }

    let mut strm = ZStream::new();
    deflate_init(&mut strm, Z_BEST_COMPRESSION).expect("deflateInit");

    // Query `deflateBound` on the initialized stream, exactly as a C caller
    // would before allocating its output buffer.
    let bound = deflate_bound(&strm, data.len());
    assert!(
        bound >= data.len(),
        "bound must cover at least the input length"
    );

    // Output buffer sized to EXACTLY the reported bound — no slack whatsoever.
    let mut compr = vec![0u8; bound];
    let outcome = deflate(&mut strm, &data, &mut compr, Z_FINISH);
    assert_eq!(
        outcome.code,
        ReturnCode::StreamEnd,
        "deflate must finish within exactly deflate_bound bytes"
    );
    assert_eq!(outcome.consumed, data.len(), "all input must be consumed");
    assert!(
        outcome.produced <= bound,
        "produced ({}) must not exceed the bound ({bound})",
        outcome.produced
    );
    deflate_end(&mut strm).expect("deflateEnd");

    // The worst-case stream must decompress back to the exact input.
    let mut back = vec![0u8; data.len()];
    let recovered = uncompress(&mut back, &compr[..outcome.produced]).expect("uncompress");
    assert_eq!(
        &back[..recovered],
        &data[..],
        "worst-case round-trip mismatch"
    );
}

/// Regression guard for the 32 KiB circular sliding window shared by `deflate.c`
/// and `inflate.c`. Input larger than one window with long-range structure
/// forces back-references whose distances approach and cross the window
/// boundary; off-by-one errors in the wrap bookkeeping (a historical class of
/// zlib bugs) corrupt the output. A 96 KiB (three-window) deterministic buffer
/// is round-tripped through streaming deflate and streaming inflate — the latter
/// with deliberately small (4 KiB) output windows to stress the window-copy
/// paths — and must reproduce the input byte-for-byte.
///
/// Deterministic and dependency-free, so it is stable under both the default and
/// `--no-default-features` configurations.
#[test]
fn test_window_wrap_round_trip() {
    // 96 KiB == three full 32 KiB windows. The pattern is low-entropy yet
    // non-trivial (a slow drift over a periodic base), so it yields long
    // back-references that span multiple window widths.
    const N: usize = 96 * 1024;
    let mut data = vec![0u8; N];
    for (i, byte) in data.iter_mut().enumerate() {
        *byte = ((i / 61) as u8).wrapping_add((i % 251) as u8);
    }

    // ---- streaming deflate at the default level ----
    let mut strm = ZStream::new();
    deflate_init(&mut strm, Z_DEFAULT_COMPRESSION).expect("deflateInit");
    let mut compr = vec![0u8; compress_bound(N)];
    let out = deflate(&mut strm, &data, &mut compr, Z_FINISH);
    assert_eq!(out.code, ReturnCode::StreamEnd, "deflate must finish");
    assert_eq!(out.consumed, N, "all input must be consumed");
    let compr_len = out.produced;
    deflate_end(&mut strm).expect("deflateEnd");

    // ---- streaming inflate with small output windows ----
    // Small output windows force inflate to flush its output across the 32 KiB
    // window boundary repeatedly, exercising the wrap-around copy logic.
    let mut strm = ZStream::new();
    inflate_init(&mut strm).expect("inflateInit");
    let mut out_buf = vec![0u8; N];
    let mut in_off = 0usize;
    let mut out_off = 0usize;
    loop {
        let out_end = (out_off + 4096).min(out_buf.len());
        let outcome = inflate(
            &mut strm,
            &compr[in_off..compr_len],
            &mut out_buf[out_off..out_end],
            Z_NO_FLUSH,
        );
        in_off += outcome.consumed;
        out_off += outcome.produced;
        if outcome.code == ReturnCode::StreamEnd {
            break;
        }
        assert_eq!(outcome.code, ReturnCode::Ok, "inflate across window wrap");
        // Guarantee forward progress so a stalled stream fails fast instead of
        // spinning forever.
        assert!(
            outcome.consumed != 0 || outcome.produced != 0,
            "inflate made no progress across the window boundary"
        );
    }
    inflate_end(&mut strm).expect("inflateEnd");

    assert_eq!(
        out_off, N,
        "decompressed length must equal the input length"
    );
    assert_eq!(out_buf, data, "window-wrap round-trip corrupted the data");
}

// ===========================================================================
// gzip file I/O — gated behind `gz-io` (mirrors the C `NO_GZCOMPRESS` guard).
// ===========================================================================

/// Builds a unique temporary `.gz` path. Uniqueness combines the process id, an
/// optional `CLONE_INDEX` (set for parallel clones), and a high-resolution
/// timestamp so that concurrent test runs on the shared host never collide.
#[cfg(feature = "gz-io")]
fn unique_temp_path(tag: &str) -> std::path::PathBuf {
    use std::time::{SystemTime, UNIX_EPOCH};

    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let clone = std::env::var("CLONE_INDEX").unwrap_or_else(|_| "x".to_string());
    let name = format!("zlibrs_{tag}_{}_{clone}_{nanos}.gz", std::process::id());
    std::env::temp_dir().join(name)
}

/// Port of C `test_gzio`: write a `.gz` file with `gzputc`/`gzputs`/`gzprintf`
/// plus a 1-byte forward seek (adding a NUL), then reopen it and verify
/// `gzread`, `gzseek`/`gztell`, `gzgetc`, `gzungetc`, and `gzgets` all behave as
/// in `example.c`. The written payload reconstructs [`HELLO`] exactly.
///
/// Gated behind `gz-io` because the gz layer needs `std::fs`/`std::io`; this
/// mirrors the C `NO_GZCOMPRESS` guard that compiles `test_gzio` out when the gz
/// functions are unavailable.
#[cfg(feature = "gz-io")]
#[test]
fn test_gzio() {
    use zlib_rs::gz::{
        gzclose, gzgetc, gzgets, gzopen, gzprintf, gzputc, gzputs, gzread, gzseek, gztell, gzungetc,
    };

    // C `<stdio.h>` seek origin; the gz layer's own SEEK_* constants are private.
    const SEEK_CUR: i32 = 1;

    let len = HELLO.len() as i32; // 14
    let path = unique_temp_path("gzio");

    // ---- write ----
    {
        let mut file = gzopen(&path, "wb").expect("gzopen wb");
        assert_eq!(
            gzputc(&mut file, i32::from(b'h')),
            i32::from(b'h'),
            "gzputc"
        );
        assert_eq!(gzputs(&mut file, "ello"), 4, "gzputs");
        assert_eq!(
            gzprintf(&mut file, format_args!(", {}!", "hello")),
            8,
            "gzprintf"
        );
        // Add one zero byte via a 1-byte forward seek (C: `gzseek(file, 1L,
        // SEEK_CUR)`); the C driver ignores the returned position.
        let _ = gzseek(&mut file, 1, SEEK_CUR);
        assert_eq!(gzclose(file), 0, "gzclose (write)");
    }

    // ---- read ----
    let mut file = gzopen(&path, "rb").expect("gzopen rb");
    let mut uncompr = vec![0u8; UNCOMPR_LEN];
    uncompr[..GARBAGE.len()].copy_from_slice(GARBAGE);

    // The whole payload is `HELLO` (14 bytes including the seek-added NUL).
    let read = gzread(&mut file, &mut uncompr);
    assert_eq!(read, len, "gzread length");
    assert_eq!(&uncompr[..len as usize], HELLO, "bad gzread");

    // Seek back 8 bytes → absolute position 6; `gztell` must agree.
    let pos = gzseek(&mut file, -8, SEEK_CUR);
    assert_eq!(pos, 6, "gzseek position");
    assert_eq!(gztell(&file), pos, "gztell disagrees with gzseek");

    // Position 6 is the space between the two "hello"s.
    assert_eq!(gzgetc(&mut file), i32::from(b' '), "gzgetc");
    assert_eq!(
        gzungetc(i32::from(b' '), &mut file),
        i32::from(b' '),
        "gzungetc"
    );

    // `gzgets` reads the tail. Under C-string semantics `strlen` stops at the
    // NUL that lives in the stream at index 13, so the effective string is the
    // 7-byte `" hello!"` (== `&HELLO[6..13]`), matching C's `strlen == 7` and
    // `strcmp(uncompr, hello + 6) == 0` checks.
    let mut line = vec![0u8; UNCOMPR_LEN];
    let _written = gzgets(&mut file, &mut line).expect("gzgets");
    let cstr_len = line
        .iter()
        .position(|&b| b == 0)
        .expect("gzgets NUL-terminates the buffer");
    assert_eq!(cstr_len, 7, "gzgets C-string length after gzseek");
    assert_eq!(&line[..cstr_len], &HELLO[6..13], "bad gzgets after gzseek");

    assert_eq!(gzclose(file), 0, "gzclose (read)");

    // Best-effort cleanup of the temporary file.
    let _ = std::fs::remove_file(&path);
}
