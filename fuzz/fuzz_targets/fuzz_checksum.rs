#![no_main]
//! Checksum invariant harness (Adler-32 and CRC-32).
//!
//! For any input split into two chunks, three computations must agree:
//!   1. one-shot over the whole buffer,
//!   2. incremental (chunk `a`, then chunk `b` seeded by `a`'s result), and
//!   3. `*_combine` folding the two independently-computed running checksums.
//! Divergence is a correctness bug; a panic is a robustness bug. These are the
//! exact algebraic properties zlib consumers rely on for streaming checksums.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let split = data.len() / 2;
    let (a, b) = data.split_at(split);

    // Adler-32 (seed 1, per RFC 1950).
    let adler_one = zlib_rs::adler32(1, data);
    let adler_inc = zlib_rs::adler32(zlib_rs::adler32(1, a), b);
    assert_eq!(adler_one, adler_inc, "adler32 incremental mismatch");
    let adler_comb = zlib_rs::adler32_combine(
        zlib_rs::adler32(1, a),
        zlib_rs::adler32(1, b),
        b.len() as i64,
    );
    assert_eq!(adler_one, adler_comb, "adler32_combine mismatch");

    // CRC-32 (seed 0, per RFC 1952).
    let crc_one = zlib_rs::crc32(0, data);
    let crc_inc = zlib_rs::crc32(zlib_rs::crc32(0, a), b);
    assert_eq!(crc_one, crc_inc, "crc32 incremental mismatch");
    let crc_comb =
        zlib_rs::crc32_combine(zlib_rs::crc32(0, a), zlib_rs::crc32(0, b), b.len() as i64);
    assert_eq!(crc_one, crc_comb, "crc32_combine mismatch");
});
