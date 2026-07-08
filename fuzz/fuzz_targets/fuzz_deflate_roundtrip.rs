#![no_main]
//! Deflate -> inflate round-trip harness.
//!
//! For an arbitrary payload and a fuzzer-chosen compression level, compressing
//! with [`zlib_rs::compress2`] and then decompressing with
//! [`zlib_rs::uncompress`] must reproduce the input byte-for-byte (RFC 1950 /
//! 1951 losslessness). A mismatch or a decode failure on our own output is a
//! correctness bug; a panic is a robustness bug.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Use the first byte to select a level in [-1, 9] (-1 == Z_DEFAULT_COMPRESSION,
    // 0 == Z_NO_COMPRESSION, ..., 9 == Z_BEST_COMPRESSION); the rest is payload.
    let (level, payload) = match data.split_first() {
        Some((&first, rest)) => (i32::from(first % 11) - 1, rest),
        None => (6, &[][..]),
    };

    // `compress_bound` is the exact zlib worst-case sizing, so this never errors
    // for lack of room.
    let mut compressed = vec![0u8; zlib_rs::compress_bound(payload.len())];
    let n = match zlib_rs::compress2(&mut compressed, payload, level) {
        Ok(n) => n,
        Err(_) => return,
    };

    // Decompress into a buffer sized to the exact original length. `max(1)`
    // avoids a zero-length output buffer on the empty-payload edge.
    let mut restored = vec![0u8; payload.len().max(1)];
    match zlib_rs::uncompress(&mut restored, &compressed[..n]) {
        Ok(m) => {
            assert_eq!(m, payload.len(), "round-trip length mismatch");
            assert_eq!(&restored[..m], payload, "round-trip content mismatch");
        }
        Err(e) => panic!("round-trip decode of self-produced stream failed: {e:?}"),
    }
});
