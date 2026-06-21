//! `tests/interop.rs` — the flate2 (canonical C zlib) byte-identical ORACLE.
//!
//! This integration test is the project's **binary-compatibility oracle**. It
//! pins `zlib-rs` to the behaviour of canonical C zlib by linking the
//! `flate2` dev-dependency in its `zlib` configuration — i.e. flate2 delegates
//! to the system `libz` through `libz-sys`, so every reference value produced
//! here comes from the *real* C implementation, not a second Rust port.
//!
//! It enforces the user constraint *"Output must be binary-compatible with
//! zlib-produced streams"* (AAP §0.7.2) and the bit-exact-parity contract
//! (AAP §0.6.7) along four axes:
//!
//! * **Group A — byte-identical compression.** At the default strategy and a
//!   fixed `(level, windowBits = 15, memLevel = 8)` the DEFLATE output of zlib
//!   is deterministic, so `zlib-rs` must reproduce it *byte for byte*. These
//!   are exact-equality assertions, the strongest form of the contract.
//! * **Group B — `zlib-rs` reads what C zlib writes.** Every framing flate2 can
//!   emit (zlib, raw, gzip) must round-trip back through `zlib_rs::Inflate`.
//! * **Group C — C zlib reads what `zlib-rs` writes.** The mirror of Group B:
//!   flate2's decoders must accept `zlib-rs`'s zlib- and raw-framed output.
//! * **Groups D–F — strategies, checksums and flush interop.** Strategy-varied
//!   streams must remain C-zlib-decodable; the Adler-32 / CRC-32 engines must
//!   match C zlib and the published known-answer values; and mid-stream flush
//!   boundaries must produce C-zlib-readable streams.
//!
//! Conceptually these cases derive from the round-trip, large-buffer, flush,
//! strategy and checksum exercises in the upstream `test/example.c`, but here
//! every expectation is validated against the C-zlib reference rather than a
//! hand-written golden value.
//!
//! ## Framing / FFI note
//!
//! `zlib-rs`'s `#[no_mangle] extern "C"` drop-in symbols (`compress`, `crc32`,
//! …) live behind the non-default `capi` feature, so a plain `cargo test` does
//! **not** export them. That is essential here: it lets the crate link cleanly
//! alongside the C `libz` that `flate2`'s `zlib` backend pulls in, with no
//! duplicate-symbol clash. This test therefore never enables `capi`.
//!
//! Run with: `cargo test --test interop`.

// This oracle is entirely safe Rust; the crate-wide contract is "no unsafe in
// the test/validation surface". Forbidding it here makes any accidental
// introduction a hard compile error.
#![forbid(unsafe_code)]

use flate2::{Compress, Compression, FlushCompress, Status};
use std::io::Read;
// `Write` is only needed to drive the flate2 `GzEncoder` in the gzip-framed
// interop tests, which are themselves gated on the `gzip` feature; importing it
// unconditionally would be an unused import in a `gzip`-off build.
#[cfg(feature = "gzip")]
use std::io::Write;

use zlib_rs::{
    Deflate, FlushMode, Inflate, Strategy, Z_BUF_ERROR, Z_NO_FLUSH, Z_OK, Z_STREAM_END, adler32,
    compress2, crc32,
};

// ===========================================================================
// Shared corpus
// ===========================================================================

/// A small, deterministic corpus exercising distinct compressor behaviours:
///
/// * `hello` — the upstream `test/example.c` payload; its repeated `"hello"`
///   stresses short back-references.
/// * `repeated_a` — 5 000 identical bytes; collapses to a handful of
///   long-distance matches and exercises run-length behaviour.
/// * `lorem` — natural-language prose with mixed match lengths.
/// * `json` — structured, punctuation-heavy text typical of real payloads.
/// * `binary_ramp` — a non-textual 0..=255 byte ramp repeated several times,
///   exercising the literal/length code mix on binary data.
/// * `empty` — the degenerate zero-length input (valid header + trailer only).
///
/// Each entry is `(name, bytes)`; the name is woven into assertion messages so
/// a failure pinpoints the offending fixture.
fn corpus() -> Vec<(&'static str, Vec<u8>)> {
    let lorem = b"Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do \
                  eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim \
                  ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut \
                  aliquip ex ea commodo consequat."
        .to_vec();

    let json = br#"{"name":"zlib-rs","version":"1.3.2","drop_in":true,"levels":[0,1,2,3,4,5,6,7,8,9],"nested":{"a":true,"b":null,"c":["x","y","z"]},"note":"hello, hello!"}"#
        .to_vec();

    let mut binary_ramp = Vec::with_capacity(256 * 8);
    for _ in 0..8 {
        binary_ramp.extend((0u16..=255).map(|b| b as u8));
    }

    vec![
        ("hello", b"hello, hello!".to_vec()),
        ("repeated_a", vec![b'a'; 5000]),
        ("lorem", lorem),
        ("json", json),
        ("binary_ramp", binary_ramp),
        ("empty", Vec::new()),
    ]
}

/// Deterministic pseudo-random bytes via a 64-bit LCG, so the large-buffer
/// tests are reproducible without pulling in an RNG dependency at runtime.
/// (The `rand` dev-dependency exists for property tests elsewhere; this oracle
/// stays dependency-light and fully deterministic.)
fn pseudo_random(len: usize, seed: u64) -> Vec<u8> {
    let mut state = seed;
    let mut out = Vec::with_capacity(len);
    for _ in 0..len {
        // Numerical Recipes LCG constants; we take a high byte for better
        // bit-mixing than the low-order bits provide.
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        out.push((state >> 33) as u8);
    }
    out
}

// ===========================================================================
// flate2 (canonical C zlib) reference helpers
// ===========================================================================

/// Compress `data` with flate2's canonical C-zlib backend, selecting the
/// framing via `zlib_header` (`true` → RFC 1950 zlib wrapper; `false` → raw
/// RFC 1951 DEFLATE). The compressor uses zlib's **default strategy**,
/// `windowBits = 15` and `memLevel = 8` — exactly the configuration `zlib-rs`
/// uses for `compress2` and `Deflate::with_options(level, ±15, 8, Default)` —
/// which is the precondition for byte-identical output (AAP §0.6.7).
///
/// `compress_vec` writes into the *spare capacity* of the output `Vec` and
/// never grows it itself, so the loop reserves room up front and expands on the
/// (here unreachable) chance a single `Finish` call cannot complete. The full
/// remaining input is re-presented each iteration starting at `total_in`,
/// mirroring zlib's `next_in` advance.
fn flate2_compress(data: &[u8], level: u32, zlib_header: bool) -> Vec<u8> {
    let mut comp = Compress::new(Compression::new(level), zlib_header);
    // `data.len()/2 + 64` strictly dominates zlib's deflateBound plus wrapper
    // overhead for every corpus size, so the first `Finish` call completes.
    let mut out: Vec<u8> = Vec::with_capacity(data.len() + data.len() / 2 + 64);
    loop {
        if out.len() == out.capacity() {
            out.reserve(out.capacity().max(64));
        }
        let consumed = comp.total_in() as usize;
        let status = comp
            .compress_vec(&data[consumed..], &mut out, FlushCompress::Finish)
            .expect("flate2 (C zlib) compression must not fail");
        match status {
            Status::StreamEnd => return out,
            // Not finished: the only reason with full input presented is a full
            // output buffer, so make room and continue.
            Status::Ok | Status::BufError => out.reserve(out.capacity().max(64)),
        }
    }
}

/// Compress `data` into the RFC 1950 **zlib wrapper** via C zlib at `level`.
fn flate2_zlib_compress(data: &[u8], level: u32) -> Vec<u8> {
    flate2_compress(data, level, true)
}

/// Compress `data` into a **raw** RFC 1951 DEFLATE stream via C zlib at `level`.
fn flate2_raw_compress(data: &[u8], level: u32) -> Vec<u8> {
    flate2_compress(data, level, false)
}

/// Decompress a complete RFC 1950 zlib-wrapped `stream` with C zlib.
fn flate2_zlib_decompress(stream: &[u8]) -> Vec<u8> {
    let mut dec = flate2::read::ZlibDecoder::new(stream);
    let mut out = Vec::new();
    dec.read_to_end(&mut out)
        .expect("flate2 (C zlib) zlib inflate must succeed");
    out
}

/// Decompress a complete raw RFC 1951 DEFLATE `stream` with C zlib.
fn flate2_raw_decompress(stream: &[u8]) -> Vec<u8> {
    let mut dec = flate2::read::DeflateDecoder::new(stream);
    let mut out = Vec::new();
    dec.read_to_end(&mut out)
        .expect("flate2 (C zlib) raw inflate must succeed");
    out
}

// ===========================================================================
// zlib-rs subject helpers (the implementation under test)
// ===========================================================================

/// Compress `data` with `zlib-rs` into a **raw** RFC 1951 DEFLATE stream
/// (`window_bits = -15`), `memLevel = 8`, at `level` and `strategy`.
///
/// The output buffer is sized from [`Deflate::bound`] (plus a small margin), so
/// the single `Finish` call always completes; the result is truncated to the
/// bytes actually produced. Returns the finished stream.
fn zlibrs_raw_compress(data: &[u8], level: i32, strategy: Strategy) -> Vec<u8> {
    let mut def = Deflate::with_options(level, -15, 8, strategy)
        .expect("zlib-rs raw Deflate options must be valid");
    let mut out = vec![0u8; def.bound(data.len() as u64) as usize + 64];
    let outcome = def
        .compress(data, &mut out, FlushMode::Finish)
        .expect("zlib-rs raw compression must not fail");
    assert_eq!(
        outcome.code,
        zlib_rs::ReturnCode::StreamEnd,
        "raw Deflate did not reach StreamEnd in a single Finish call"
    );
    assert_eq!(
        outcome.consumed,
        data.len(),
        "raw Deflate did not consume all input"
    );
    out.truncate(outcome.produced);
    out
}

/// Compress `data` with `zlib-rs` using an explicit `window_bits` framing
/// (e.g. `31` for gzip) at `level` and the default strategy. Used only by the
/// gzip-encode interop test, so it shares that test's `gzip` feature gate.
#[cfg(feature = "gzip")]
fn zlibrs_compress_framed(data: &[u8], level: i32, window_bits: i32) -> Vec<u8> {
    let mut def = Deflate::with_options(level, window_bits, 8, Strategy::Default)
        .expect("zlib-rs framed Deflate options must be valid");
    let mut out = vec![0u8; def.bound(data.len() as u64) as usize + 64];
    let outcome = def
        .compress(data, &mut out, FlushMode::Finish)
        .expect("zlib-rs framed compression must not fail");
    assert_eq!(
        outcome.code,
        zlib_rs::ReturnCode::StreamEnd,
        "framed Deflate did not reach StreamEnd in a single Finish call"
    );
    out.truncate(outcome.produced);
    out
}

/// Decompress `stream` with `zlib-rs` using the given `window_bits` overload
/// (`15` zlib, `-15` raw, `31` gzip, `47` auto-detect), returning the decoded
/// bytes.
///
/// Drives the streaming [`Inflate::inflate`] loop with `Z_NO_FLUSH`, advancing
/// the input cursor by the consumed count and the output by the produced count
/// each call (zlib's `next_in`/`next_out` advance), growing the scratch buffer
/// only if the caller under-estimated `expected_len`. Terminates on
/// `Z_STREAM_END`; a stall (no input consumed and no output produced) also
/// breaks so a malformed fixture can never hang the suite.
fn zlibrs_inflate(stream: &[u8], window_bits: i32, expected_len: usize) -> Vec<u8> {
    let mut inf =
        Inflate::with_window_bits(window_bits).expect("zlib-rs Inflate windowBits must be valid");
    let mut result: Vec<u8> = Vec::with_capacity(expected_len);
    // A scratch slice large enough to absorb the whole payload in one pass for
    // the common case; the loop still copes if it is too small.
    let mut scratch = vec![0u8; expected_len.max(64)];
    let mut in_pos = 0usize;
    loop {
        let (code, consumed, produced) = inf.inflate(&stream[in_pos..], &mut scratch, Z_NO_FLUSH);
        in_pos += consumed;
        result.extend_from_slice(&scratch[..produced]);
        match code {
            Z_STREAM_END => break,
            Z_OK | Z_BUF_ERROR => {
                if consumed == 0 && produced == 0 {
                    // No forward progress is possible; stop rather than spin.
                    break;
                }
            }
            other => panic!("zlib-rs inflate failed with return code {other}"),
        }
    }
    result
}

// ===========================================================================
// Group A — byte-identical compressed output: zlib-rs vs flate2 / C zlib
//
// The strongest form of the binary-compatibility contract (AAP §0.6.7,
// §0.7.2): at the default strategy and matched (level, windowBits = 15,
// memLevel = 8) zlib's DEFLATE output is deterministic, so zlib-rs MUST
// reproduce it byte for byte across the whole corpus.
// ===========================================================================

/// Assert that `zlib_rs::compress2` (RFC 1950 zlib wrapper) is byte-identical
/// to C zlib over the entire corpus at the given `level`.
fn assert_zlib_byte_identical(level: i32) {
    for (name, data) in corpus() {
        let subject = compress2(&data, level).expect("zlib-rs compress2 must succeed");
        let reference = flate2_zlib_compress(&data, level as u32);
        assert_eq!(
            subject,
            reference,
            "zlib-wrapper output differs from C zlib for fixture '{name}' at level {level} \
             (subject {} bytes, reference {} bytes)",
            subject.len(),
            reference.len()
        );
    }
}

/// Assert that `zlib-rs` raw DEFLATE is byte-identical to C zlib over the whole
/// corpus at the given `level` and the default strategy.
fn assert_raw_byte_identical(level: i32) {
    for (name, data) in corpus() {
        let subject = zlibrs_raw_compress(&data, level, Strategy::Default);
        let reference = flate2_raw_compress(&data, level as u32);
        assert_eq!(
            subject,
            reference,
            "raw DEFLATE output differs from C zlib for fixture '{name}' at level {level} \
             (subject {} bytes, reference {} bytes)",
            subject.len(),
            reference.len()
        );
    }
}

#[test]
fn zlib_wrapper_byte_identical_level_0() {
    // Z_NO_COMPRESSION — stored blocks; the wrapper plus stored framing must
    // match exactly.
    assert_zlib_byte_identical(0);
}

#[test]
fn zlib_wrapper_byte_identical_level_1() {
    // Z_BEST_SPEED — the deflate_fast path.
    assert_zlib_byte_identical(1);
}

#[test]
fn zlib_wrapper_byte_identical_level_6() {
    // The default level — the deflate_slow (lazy-match) path.
    assert_zlib_byte_identical(6);
}

#[test]
fn zlib_wrapper_byte_identical_level_9() {
    // Z_BEST_COMPRESSION — maximum lazy matching / chain length.
    assert_zlib_byte_identical(9);
}

#[test]
fn zlib_wrapper_byte_identical_default_level() {
    // Z_DEFAULT_COMPRESSION (-1) must resolve to level 6 internally, so its
    // output equals C zlib at an explicit level 6.
    for (name, data) in corpus() {
        let subject = compress2(&data, zlib_rs::Z_DEFAULT_COMPRESSION)
            .expect("zlib-rs compress2 at default level must succeed");
        let reference = flate2_zlib_compress(&data, 6);
        assert_eq!(
            subject, reference,
            "Z_DEFAULT_COMPRESSION did not resolve to level 6 for fixture '{name}'"
        );
    }
}

#[test]
fn raw_deflate_byte_identical_level_1() {
    assert_raw_byte_identical(1);
}

#[test]
fn raw_deflate_byte_identical_level_6() {
    assert_raw_byte_identical(6);
}

#[test]
fn raw_deflate_byte_identical_level_9() {
    assert_raw_byte_identical(9);
}

// ===========================================================================
// Group B — zlib-rs decodes flate2 / C-zlib output ("we read what C zlib writes")
//
// Every framing C zlib can emit must round-trip back through zlib_rs::Inflate,
// covering the windowBits overloading ranges (15 zlib, -15 raw, 31/47 gzip).
// ===========================================================================

#[test]
fn inflate_flate2_zlib_stream() {
    // C zlib writes the RFC 1950 wrapper; zlib-rs reads it at windowBits = 15.
    for (name, data) in corpus() {
        for level in [0u32, 1, 6, 9] {
            let stream = flate2_zlib_compress(&data, level);
            let decoded = zlibrs_inflate(&stream, 15, data.len());
            assert_eq!(
                decoded, data,
                "zlib-rs failed to inflate C-zlib zlib stream for '{name}' at level {level}"
            );
        }
    }
}

#[test]
fn inflate_flate2_raw_stream() {
    // C zlib writes raw RFC 1951; zlib-rs reads it at windowBits = -15.
    for (name, data) in corpus() {
        for level in [1u32, 6, 9] {
            let stream = flate2_raw_compress(&data, level);
            let decoded = zlibrs_inflate(&stream, -15, data.len());
            assert_eq!(
                decoded, data,
                "zlib-rs failed to inflate C-zlib raw stream for '{name}' at level {level}"
            );
        }
    }
}

/// gzip framing is only compiled when the `gzip` feature is enabled (default),
/// mirroring the C `#ifdef GZIP` guard.
#[cfg(feature = "gzip")]
#[test]
fn inflate_flate2_gzip_stream() {
    // C zlib writes the RFC 1952 gzip wrapper; zlib-rs reads it both at the
    // explicit gzip windowBits (31) and via auto-detection (47).
    for (name, data) in corpus() {
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), Compression::new(6));
        enc.write_all(&data).expect("flate2 gzip write");
        let stream = enc.finish().expect("flate2 gzip finish");

        let decoded_explicit = zlibrs_inflate(&stream, 31, data.len());
        assert_eq!(
            decoded_explicit, data,
            "zlib-rs (windowBits = 31) failed to inflate C-zlib gzip stream for '{name}'"
        );

        let decoded_auto = zlibrs_inflate(&stream, 47, data.len());
        assert_eq!(
            decoded_auto, data,
            "zlib-rs (windowBits = 47 auto-detect) failed to inflate C-zlib gzip stream for '{name}'"
        );
    }
}

// ===========================================================================
// Group C — flate2 / C zlib decodes zlib-rs output ("C zlib reads what we write")
//
// The mirror of Group B: every stream zlib-rs emits must be accepted by the
// canonical C decoder.
// ===========================================================================

#[test]
fn flate2_inflates_zlibrs_zlib() {
    // zlib-rs writes the RFC 1950 wrapper; C zlib reads it back.
    for (name, data) in corpus() {
        for level in [0i32, 1, 6, 9] {
            let stream = compress2(&data, level).expect("zlib-rs compress2 must succeed");
            let decoded = flate2_zlib_decompress(&stream);
            assert_eq!(
                decoded, data,
                "C zlib failed to inflate zlib-rs zlib stream for '{name}' at level {level}"
            );
        }
    }
}

#[test]
fn flate2_inflates_zlibrs_raw() {
    // zlib-rs writes raw RFC 1951; C zlib reads it back.
    for (name, data) in corpus() {
        for level in [1i32, 6, 9] {
            let stream = zlibrs_raw_compress(&data, level, Strategy::Default);
            let decoded = flate2_raw_decompress(&stream);
            assert_eq!(
                decoded, data,
                "C zlib failed to inflate zlib-rs raw stream for '{name}' at level {level}"
            );
        }
    }
}

// ===========================================================================
// Group D — strategy variations cross-decode through C zlib
//
// Strategy-varied streams need not be byte-identical to the default-strategy
// reference, so the robust invariant asserted here is cross-decodability: C
// zlib must accept every strategy zlib-rs can emit, and zlib-rs must read its
// own output back (a self round-trip), proving the stream is well-formed
// RFC 1951.
// ===========================================================================

/// Compress `STRATEGY_CORPUS` with `zlib-rs` at `strategy`, then assert both
/// C zlib (the oracle) and `zlib-rs` itself decode it back to the original.
fn assert_strategy_crossdecode(strategy: Strategy, label: &str) {
    // A payload with both runs and structure so each strategy has something to
    // act on (RLE runs, filtered deltas, Huffman-only literals).
    let samples: [Vec<u8>; 3] = [
        b"aaaaaabbbbbbccccccddddddeeeeeeffffff112233 abracadabra abracadabra!".to_vec(),
        vec![b'z'; 1500],
        pseudo_random(2048, 0x5151_5151_2727_2727),
    ];
    for (idx, data) in samples.iter().enumerate() {
        let stream = zlibrs_raw_compress(data, 6, strategy);

        // Oracle: canonical C zlib must decode the strategy-varied stream.
        let via_flate2 = flate2_raw_decompress(&stream);
        assert_eq!(
            &via_flate2, data,
            "C zlib failed to decode zlib-rs {label} stream (sample {idx})"
        );

        // Self round-trip: zlib-rs reads back its own strategy-varied stream.
        let via_zlibrs = zlibrs_inflate(&stream, -15, data.len());
        assert_eq!(
            &via_zlibrs, data,
            "zlib-rs failed to round-trip its own {label} stream (sample {idx})"
        );
    }
}

#[test]
fn strategy_filtered_crossdecode() {
    assert_strategy_crossdecode(Strategy::Filtered, "Z_FILTERED");
}

#[test]
fn strategy_huffman_only_crossdecode() {
    assert_strategy_crossdecode(Strategy::HuffmanOnly, "Z_HUFFMAN_ONLY");
}

#[test]
fn strategy_rle_crossdecode() {
    assert_strategy_crossdecode(Strategy::Rle, "Z_RLE");
}

#[test]
fn strategy_fixed_crossdecode() {
    assert_strategy_crossdecode(Strategy::Fixed, "Z_FIXED");
}

// ===========================================================================
// Group E — checksum cross-validation against C zlib / known-answer values
// ===========================================================================

/// CRC-32 (IEEE) of `data`, seeded fresh, via the flate2 reference.
fn flate2_crc(data: &[u8]) -> u32 {
    let mut crc = flate2::Crc::new();
    crc.update(data);
    crc.sum()
}

#[test]
fn crc32_matches_flate2() {
    // The canonical CRC-32 check value: crc32(0, "123456789") == 0xCBF43926.
    const CRC_KAT: u32 = 0xcbf4_3926;
    assert_eq!(
        crc32(0, b"123456789"),
        CRC_KAT,
        "zlib-rs crc32 disagreed with the published known-answer value"
    );
    assert_eq!(
        flate2_crc(b"123456789"),
        CRC_KAT,
        "flate2 reference crc32 disagreed with the published known-answer value"
    );

    // zlib-rs must equal the C-zlib-derived reference across the corpus and a
    // few edge inputs (empty, single byte, long run, binary).
    let mut inputs: Vec<Vec<u8>> = corpus().into_iter().map(|(_, d)| d).collect();
    inputs.push(Vec::new());
    inputs.push(b"a".to_vec());
    inputs.push(vec![0u8; 1024]);
    inputs.push(pseudo_random(4096, 0xDEAD_BEEF_CAFE_F00D));
    for data in &inputs {
        assert_eq!(
            crc32(0, data),
            flate2_crc(data),
            "zlib-rs crc32 disagreed with C-zlib reference for a {}-byte input",
            data.len()
        );
    }

    // Incremental update must equal the one-shot CRC of the concatenation
    // (the property the gzip trailer relies on).
    let whole = b"the quick brown fox jumps over the lazy dog";
    let (head, tail) = whole.split_at(17);
    let incremental = crc32(crc32(0, head), tail);
    assert_eq!(
        incremental,
        crc32(0, whole),
        "zlib-rs crc32 is not consistent under incremental update"
    );
    assert_eq!(
        incremental,
        flate2_crc(whole),
        "zlib-rs incremental crc32 disagreed with the C-zlib reference"
    );
}

#[test]
fn adler32_matches_reference() {
    // The published Adler-32 known-answer: adler32(1, "123456789") == 0x091E01DE.
    assert_eq!(
        adler32(1, b"123456789"),
        0x091e_01de,
        "zlib-rs adler32 disagreed with the published known-answer value"
    );

    // The empty-input identity: the seed value is returned unchanged.
    assert_eq!(
        adler32(1, b""),
        1,
        "adler32 of empty input must be the seed"
    );

    // Incremental update must equal the one-shot Adler-32 of the concatenation
    // (the property the zlib trailer relies on).
    let whole = b"the quick brown fox jumps over the lazy dog";
    let (head, tail) = whole.split_at(11);
    assert_eq!(
        adler32(adler32(1, head), tail),
        adler32(1, whole),
        "zlib-rs adler32 is not consistent under incremental update"
    );
}

// ===========================================================================
// Group F — flush-mode interop: mid-stream flush boundaries stay C-zlib-readable
// ===========================================================================

/// Compress `data` with `zlib-rs` (zlib wrapper, level 6), issuing `mid_flush`
/// at the half-way point and `Finish` at the end, concatenating the two output
/// segments into one stream. Exercises that the flush block boundary is
/// emitted in a C-zlib-compatible way.
fn zlibrs_flush_split(data: &[u8], mid_flush: FlushMode) -> Vec<u8> {
    let mut def = Deflate::with_options(6, 15, 8, Strategy::Default)
        .expect("zlib-rs Deflate options must be valid");
    let mut stream = Vec::new();
    let mid = data.len() / 2;

    let mut first = vec![0u8; def.bound(data.len() as u64) as usize + 64];
    let o1 = def
        .compress(&data[..mid], &mut first, mid_flush)
        .expect("zlib-rs mid-stream flush must not fail");
    stream.extend_from_slice(&first[..o1.produced]);

    let mut second = vec![0u8; def.bound(data.len() as u64) as usize + 64];
    let o2 = def
        .compress(&data[mid..], &mut second, FlushMode::Finish)
        .expect("zlib-rs finishing compress must not fail");
    assert_eq!(
        o2.code,
        zlib_rs::ReturnCode::StreamEnd,
        "stream did not finish after mid-stream flush"
    );
    stream.extend_from_slice(&second[..o2.produced]);
    stream
}

#[test]
fn sync_flush_interop() {
    // Z_SYNC_FLUSH mid-stream then Z_FINISH; C zlib must decode the whole
    // stream back to the original.
    let data = b"first chunk of data; second chunk of data; third chunk of data!";
    let stream = zlibrs_flush_split(data, FlushMode::SyncFlush);
    assert_eq!(
        flate2_zlib_decompress(&stream),
        data,
        "C zlib failed to decode a zlib-rs Z_SYNC_FLUSH stream"
    );
}

#[test]
fn full_flush_interop() {
    // Z_FULL_FLUSH mid-stream (resets the history) then Z_FINISH; C zlib must
    // still decode the whole stream back to the original.
    let data = b"alpha beta gamma delta epsilon; alpha beta gamma delta epsilon!";
    let stream = zlibrs_flush_split(data, FlushMode::FullFlush);
    assert_eq!(
        flate2_zlib_decompress(&stream),
        data,
        "C zlib failed to decode a zlib-rs Z_FULL_FLUSH stream"
    );
}

// ===========================================================================
// Extra fixed-corpus interop coverage
// ===========================================================================

#[test]
fn large_random_roundtrip_via_flate2() {
    // ~64 KiB of incompressible data exercises stored-block emission and
    // multi-window decoding in BOTH directions across the library boundary.
    let data = pseudo_random(64 * 1024, 0x0123_4567_89ab_cdef);

    // zlib-rs writes -> C zlib reads (zlib wrapper).
    let zlibrs_stream = compress2(&data, 6).expect("zlib-rs compress2 must succeed");
    assert_eq!(
        flate2_zlib_decompress(&zlibrs_stream),
        data,
        "C zlib failed to decode a large zlib-rs zlib stream"
    );

    // C zlib writes -> zlib-rs reads (zlib wrapper).
    let flate2_stream = flate2_zlib_compress(&data, 6);
    assert_eq!(
        zlibrs_inflate(&flate2_stream, 15, data.len()),
        data,
        "zlib-rs failed to decode a large C-zlib zlib stream"
    );

    // And the raw framing in both directions for the same payload.
    let zlibrs_raw = zlibrs_raw_compress(&data, 6, Strategy::Default);
    assert_eq!(
        flate2_raw_decompress(&zlibrs_raw),
        data,
        "C zlib failed to decode a large zlib-rs raw stream"
    );
    let flate2_raw = flate2_raw_compress(&data, 6);
    assert_eq!(
        zlibrs_inflate(&flate2_raw, -15, data.len()),
        data,
        "zlib-rs failed to decode a large C-zlib raw stream"
    );
}

/// gzip framing is gated on the `gzip` feature (default), mirroring `#ifdef
/// GZIP`.
#[cfg(feature = "gzip")]
#[test]
fn gzip_encode_zlibrs_decode_flate2() {
    // zlib-rs writes a gzip member (windowBits = 31); C zlib's GzDecoder reads
    // it back, proving the gzip header/trailer (CRC-32 + ISIZE) are emitted
    // exactly as canonical zlib expects.
    for (name, data) in corpus() {
        let stream = zlibrs_compress_framed(&data, 6, 31);
        let mut dec = flate2::read::GzDecoder::new(&stream[..]);
        let mut decoded = Vec::new();
        dec.read_to_end(&mut decoded)
            .expect("flate2 GzDecoder must decode a zlib-rs gzip member");
        assert_eq!(
            decoded, data,
            "C zlib failed to decode a zlib-rs gzip member for '{name}'"
        );
    }
}
