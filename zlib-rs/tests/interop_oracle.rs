//! All-tuple **byte-for-byte** oracle: `zlib-rs` vs canonical C zlib for every
//! `(level, strategy, windowBits)` combination.
//!
//! # Why this file exists (and why it is the one test that uses FFI)
//!
//! [`tests/interop.rs`](interop) is the primary interop suite and is written in
//! pure safe Rust (`#![forbid(unsafe_code)]`): it uses the high-level [`flate2`]
//! API as the C-zlib oracle. But `flate2`'s public `Compress` API exposes only
//! the compression **level** (and `windowBits`) — it has **no** way to select a
//! deflate **strategy** (`Z_FILTERED` / `Z_HUFFMAN_ONLY` / `Z_RLE` / `Z_FIXED`).
//! Consequently `interop.rs` can only prove byte-for-byte equality for the
//! *default* strategy; for non-default strategies and the full `windowBits`
//! range it falls back to round-trip / cross-decode (validity) checks.
//!
//! Project rule **Invariant 1** requires **bit-identical** output for *every*
//! `(level, strategy, windowBits)` tuple. Proving that demands an oracle that
//! can set the strategy, which only the low-level C `deflateInit2_` entry point
//! offers. This file therefore calls the raw [`libz_sys`] C-zlib bindings
//! directly and compares the produced bytes against the `zlib-rs` engine across
//! the complete tuple matrix — the strongest possible bit-exactness assertion.
//!
//! ## Unsafe policy
//!
//! Every other file under `tests/` (including `interop.rs`) is safe-Rust and
//! carries `#![forbid(unsafe_code)]`. This file is the **single, deliberate
//! exception**: a thin C-zlib oracle whose only `unsafe` is the unavoidable
//! invocation of the C `deflateInit2_` / `deflate` / `deflateEnd` FFI. Each
//! `unsafe` block carries a `// SAFETY:` justification. This does **not** weaken
//! the core's guarantee — the `zlib-rs` crate itself remains
//! `#![forbid(unsafe_code)]`; this is dev-only test scaffolding, never shipped,
//! exactly like the `flate2`/`libz-sys` oracle dependency it builds on.
//!
//! # Scope of the matrix
//!
//! * **Containers** — zlib (`windowBits` 9..=15) and raw DEFLATE
//!   (`windowBits` -15..=-9). The gzip container is intentionally excluded from
//!   *whole-stream* byte equality because its 10-byte header carries an MTIME
//!   and OS/XFL byte that legitimately differ between producers; the gzip
//!   DEFLATE **payload** equals the raw-DEFLATE bytes for the same
//!   `(level, strategy, windowBits & 15)`, so the raw matrix already proves the
//!   gzip payload's bit-exactness for every strategy (and `interop.rs` checks
//!   the gzip payload directly for the default strategy).
//! * **Levels** — 0..=9 (level 0 = stored; strategy is moot but still asserted
//!   identical).
//! * **Strategies** — all five: `Default`, `Filtered`, `HuffmanOnly`, `Rle`,
//!   `Fixed`.
//! * **memLevel / method** — fixed at `DEF_MEM_LEVEL` (8) and `Z_DEFLATED`,
//!   matching `zlib-rs`'s engine and `flate2`'s defaults.
//!
//! The whole file requires `std` (the `libz-sys` link + `Vec`), so it is gated
//! with `#![cfg(feature = "std")]`; under `--no-default-features` the test crate
//! is simply empty.

#![cfg(feature = "std")]

use core::ffi::{c_int, c_void};
use core::mem;

use libz_sys::{
    Z_DEFAULT_STRATEGY, Z_DEFLATED, Z_FILTERED, Z_FINISH, Z_FIXED, Z_HUFFMAN_ONLY, Z_OK, Z_RLE,
    Z_STREAM_END, deflate, deflateEnd, deflateInit2_, uInt, voidpf, z_stream, zlibVersion,
};

use zlib_rs::constants::{DEF_MEM_LEVEL, Flush, Strategy};
use zlib_rs::deflate::{deflate as zrs_deflate, deflate_init2};
use zlib_rs::error::ReturnCode;
use zlib_rs::stream::ZStream;

// ===========================================================================
// Output sizing
// ===========================================================================

/// A single-pass output capacity that is a valid upper bound for DEFLATE output
/// under **any** strategy and `windowBits`.
///
/// `deflateBound`/`compressBound` (the basis of [`zlib_rs::util::compress_bound`])
/// assumes the *default* strategy. It is **not** a valid bound for `Z_FIXED`,
/// which is forced to emit fixed-Huffman blocks and cannot fall back to stored
/// blocks, so incompressible input can expand to roughly 1.125x (literals
/// 144..=255 cost 9 bits each). `1.5x + 128` comfortably covers that worst case
/// plus the zlib/gzip wrapper and per-block framing, so both the C oracle and
/// the `zlib-rs` engine always finish in a single pass for every tuple in the
/// matrix. This affects only buffer *capacity*, never the produced bytes that
/// the byte-for-byte assertion compares.
fn single_pass_capacity(input_len: usize) -> usize {
    input_len + input_len / 2 + 128
}

// ===========================================================================
// Strategy mapping
// ===========================================================================

/// Map the `zlib-rs` [`Strategy`] enum to the C `Z_*` strategy integer so the
/// oracle and the engine are driven with the identical parameter.
fn c_strategy(strategy: Strategy) -> c_int {
    match strategy {
        Strategy::Default => Z_DEFAULT_STRATEGY,
        Strategy::Filtered => Z_FILTERED,
        Strategy::HuffmanOnly => Z_HUFFMAN_ONLY,
        Strategy::Rle => Z_RLE,
        Strategy::Fixed => Z_FIXED,
    }
}

/// Human-readable strategy label for assertion messages.
fn strategy_name(strategy: Strategy) -> &'static str {
    match strategy {
        Strategy::Default => "Default",
        Strategy::Filtered => "Filtered",
        Strategy::HuffmanOnly => "HuffmanOnly",
        Strategy::Rle => "Rle",
        Strategy::Fixed => "Fixed",
    }
}

// ===========================================================================
// C-zlib oracle (the only FFI in the entire `tests/` tree)
// ===========================================================================

// The C runtime's allocator, already linked into this test binary through the
// `libz-sys` dependency. We forward C zlib's allocation requests to it so the
// oracle's `z_stream` can be constructed with *valid* (non-null) `zalloc`/
// `zfree` function pointers. `libz-sys` types those two fields as non-nullable
// `extern "C" fn`s, so zero-initializing them would be undefined behavior; a
// real allocator pair is the correct, well-defined alternative. The choice of
// allocator is orthogonal to the DEFLATE algorithm, so the compressed output is
// byte-for-byte identical to zlib's built-in `zcalloc`/`zcfree`.
unsafe extern "C" {
    fn malloc(size: usize) -> *mut c_void;
    fn free(ptr: *mut c_void);
}

/// C-ABI allocation callback handed to the oracle's `z_stream`. Honors zlib's
/// `items * size` contract by forwarding to the C runtime `malloc`.
unsafe extern "C" fn oracle_zalloc(_opaque: voidpf, items: uInt, size: uInt) -> voidpf {
    // SAFETY: `malloc` is the C runtime allocator linked into this binary. The
    // byte count is zlib's documented `items * size`; the returned pointer is
    // handed straight back to zlib and released exactly once via `oracle_zfree`
    // during `deflateEnd`.
    unsafe { malloc(items as usize * size as usize) }
}

/// Counterpart to [`oracle_zalloc`]: releases a block it previously returned.
unsafe extern "C" fn oracle_zfree(_opaque: voidpf, address: voidpf) {
    // SAFETY: `address` was produced by `oracle_zalloc` (C `malloc`) and is
    // freed exactly once when zlib invokes `zfree` during `deflateEnd`.
    unsafe { free(address) }
}

/// Compress `data` with canonical **C zlib** at the exact `(level, windowBits,
/// strategy)` tuple, in a single `Z_FINISH` pass, and return the produced bytes.
///
/// This is the byte-for-byte reference. `method`/`memLevel` are fixed at
/// `Z_DEFLATED`/`DEF_MEM_LEVEL` to match the `zlib-rs` engine exactly.
fn c_zlib_deflate(data: &[u8], level: i32, window_bits: i32, strategy: c_int) -> Vec<u8> {
    // `z_stream`'s `zalloc`/`zfree` are *non-nullable* `extern "C" fn` pointers
    // in `libz-sys`, so zero-initializing the whole struct (`mem::zeroed`) is
    // undefined behavior. Construct it explicitly instead: install the trivial
    // C-runtime allocator pair above and clear every data field. No `unsafe` is
    // needed for the construction itself.
    let mut strm = z_stream {
        next_in: core::ptr::null_mut(),
        avail_in: 0,
        total_in: 0,
        next_out: core::ptr::null_mut(),
        avail_out: 0,
        total_out: 0,
        msg: core::ptr::null_mut(),
        state: core::ptr::null_mut(),
        zalloc: oracle_zalloc,
        zfree: oracle_zfree,
        opaque: core::ptr::null_mut(),
        data_type: 0,
        adler: 0,
        reserved: 0,
    };

    // SAFETY: `zlibVersion()` returns a pointer to a static NUL-terminated C
    // string owned by the linked zlib; we only forward it to `deflateInit2_`.
    let version = unsafe { zlibVersion() };

    // SAFETY: `strm` is a freshly zeroed, valid stream; `version` is zlib's own
    // version string and `stream_size` is the true size of `z_stream`. All
    // scalar arguments are within the C ABI's `c_int` domain.
    let rc = unsafe {
        deflateInit2_(
            &mut strm,
            level as c_int,
            Z_DEFLATED,
            window_bits as c_int,
            DEF_MEM_LEVEL as c_int,
            strategy,
            version,
            mem::size_of::<z_stream>() as c_int,
        )
    };
    assert_eq!(rc, Z_OK, "C deflateInit2_ failed (rc={rc})");

    // One output buffer large enough to finish in a single call under any
    // strategy (see `single_pass_capacity` — `deflateBound` alone is not a valid
    // bound for `Z_FIXED`).
    let mut out = vec![0u8; single_pass_capacity(data.len())];

    // Point the stream at the input and output slices. `next_in` is typed
    // `*mut` by the C ABI even though `deflate` only reads it; `cast_mut` is the
    // idiomatic, provenance-preserving way to satisfy the signature.
    strm.next_in = data.as_ptr().cast_mut();
    strm.avail_in = data.len() as u32;
    strm.next_out = out.as_mut_ptr();
    strm.avail_out = out.len() as u32;

    // SAFETY: `next_in`/`next_out` reference the live `data`/`out` buffers above
    // with matching `avail_in`/`avail_out`; `out` is sized past `deflateBound`,
    // so a single `Z_FINISH` call completes the stream.
    let rc = unsafe { deflate(&mut strm, Z_FINISH) };
    assert_eq!(
        rc, Z_STREAM_END,
        "C deflate(Z_FINISH) did not finish in one pass (rc={rc})"
    );

    let produced = out.len() - strm.avail_out as usize;

    // SAFETY: `strm` was initialized by `deflateInit2_` and is freed exactly
    // once here; after this the stream must not be reused.
    let rc = unsafe { deflateEnd(&mut strm) };
    assert_eq!(rc, Z_OK, "C deflateEnd failed (rc={rc})");

    out.truncate(produced);
    out
}

/// Compress `data` with the **`zlib-rs`** streaming engine at the same
/// `(level, windowBits, strategy)` tuple, returning the produced bytes. Mirrors
/// the driver used by `tests/interop.rs` and `crate::util::compress`.
fn zrs_deflate_engine(data: &[u8], level: i32, window_bits: i32, strategy: Strategy) -> Vec<u8> {
    let mut strm = ZStream::new();
    deflate_init2(
        &mut strm,
        level,
        Z_DEFLATED,
        window_bits,
        DEF_MEM_LEVEL,
        strategy,
    )
    .expect("zlib-rs deflate_init2 should succeed");

    let mut out = vec![0u8; single_pass_capacity(data.len())];
    let mut in_pos = 0usize;
    let mut out_pos = 0usize;

    loop {
        let (rc, consumed, produced) = zrs_deflate(
            &mut strm,
            &data[in_pos..],
            &mut out[out_pos..],
            Flush::Finish,
        );
        in_pos += consumed;
        out_pos += produced;

        match rc {
            Ok(ReturnCode::StreamEnd) => break,
            Ok(_) => assert!(
                consumed != 0 || produced != 0,
                "zlib-rs deflate stalled with no progress (output buffer too small?)"
            ),
            Err(e) => panic!("zlib-rs deflate engine error: {e:?}"),
        }
    }

    out.truncate(out_pos);
    out
}

// ===========================================================================
// Representative inputs
// ===========================================================================

/// A diverse, deterministic set of inputs that exercise distinct compressor
/// behaviors: an empty buffer, natural-language text (literals + a non-trivial
/// Huffman tree), a highly repetitive buffer (long LZ77 matches), long
/// single-byte runs (the `Z_RLE` sweet spot), and pseudo-random bytes
/// (near-incompressible — stresses the stored/strategy decisions). Kept small so
/// the full level × strategy × windowBits matrix runs quickly.
fn oracle_inputs() -> Vec<(&'static str, Vec<u8>)> {
    // A natural-language paragraph with realistic redundancy.
    let text = b"The DEFLATE compressed data format combines the LZ77 algorithm \
                 with Huffman coding. Compressed data consists of a series of \
                 blocks, each prefixed by a three-bit header. This sentence \
                 repeats words like compressed, blocks, and Huffman to create \
                 matchable redundancy for the LZ77 stage."
        .to_vec();

    // Highly repetitive: many long back-references at every window size.
    let repetitive = b"abcabcabcabcabcdefdefdefdefdef0123456789"
        .iter()
        .cycle()
        .take(4096)
        .copied()
        .collect::<Vec<u8>>();

    // Long single-byte runs — the case `Z_RLE` is specifically tuned for.
    let mut runs = Vec::with_capacity(4096);
    for i in 0..256u32 {
        let byte = (i % 251) as u8;
        let run_len = 1 + (i % 17) as usize;
        runs.extend(core::iter::repeat_n(byte, run_len));
    }
    runs.truncate(4096);

    // Deterministic pseudo-random bytes (xorshift) — near-incompressible.
    let mut prng = Vec::with_capacity(2048);
    let mut state: u32 = 0x1234_5678;
    for _ in 0..2048 {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        prng.push((state & 0xff) as u8);
    }

    vec![
        ("empty", Vec::new()),
        ("text", text),
        ("repetitive", repetitive),
        ("runs", runs),
        ("pseudo_random", prng),
    ]
}

/// The five deflate strategies, in `Z_*` order.
const STRATEGIES: [Strategy; 5] = [
    Strategy::Default,
    Strategy::Filtered,
    Strategy::HuffmanOnly,
    Strategy::Rle,
    Strategy::Fixed,
];

/// Assert that the `zlib-rs` and C-zlib outputs are byte-for-byte identical for
/// a given tuple, with a diagnostic that pinpoints the first divergence.
fn assert_identical(
    container: &str,
    input_label: &str,
    level: i32,
    window_bits: i32,
    strategy: Strategy,
    zrs: &[u8],
    c: &[u8],
) {
    if zrs == c {
        return;
    }
    let first_diff = zrs
        .iter()
        .zip(c.iter())
        .position(|(a, b)| a != b)
        .unwrap_or_else(|| zrs.len().min(c.len()));
    panic!(
        "BIT-EXACTNESS MISMATCH vs C zlib\n  container={container} input={input_label} \
         level={level} windowBits={window_bits} strategy={}\n  zlib-rs len={} C len={} \
         first diff at byte {first_diff}",
        strategy_name(strategy),
        zrs.len(),
        c.len(),
    );
}

// ===========================================================================
// The all-tuple byte-for-byte matrices
// ===========================================================================

/// zlib container (`windowBits` 9..=15): `zlib-rs` output must be byte-for-byte
/// identical to C zlib for every `(input, level, strategy, windowBits)` tuple.
#[test]
fn oracle_zlib_full_tuple_matrix() {
    for (label, data) in oracle_inputs() {
        for window_bits in 9i32..=15 {
            for level in 0i32..=9 {
                for strategy in STRATEGIES {
                    let zrs = zrs_deflate_engine(&data, level, window_bits, strategy);
                    let c = c_zlib_deflate(&data, level, window_bits, c_strategy(strategy));
                    assert_identical("zlib", label, level, window_bits, strategy, &zrs, &c);
                }
            }
        }
    }
}

/// Raw DEFLATE (`windowBits` -15..=-9): `zlib-rs` output must be byte-for-byte
/// identical to C zlib for every `(input, level, strategy, windowBits)` tuple.
/// This raw matrix also proves the gzip-container DEFLATE **payload** is
/// bit-exact for every strategy (the gzip payload equals the raw output for the
/// same `windowBits & 15`).
#[test]
fn oracle_raw_full_tuple_matrix() {
    for (label, data) in oracle_inputs() {
        for window_bits in -15i32..=-9 {
            for level in 0i32..=9 {
                for strategy in STRATEGIES {
                    let zrs = zrs_deflate_engine(&data, level, window_bits, strategy);
                    let c = c_zlib_deflate(&data, level, window_bits, c_strategy(strategy));
                    assert_identical("raw", label, level, window_bits, strategy, &zrs, &c);
                }
            }
        }
    }
}
