#![no_main]
//! Inflate robustness harness.
//!
//! Feeds arbitrary, attacker-controlled bytes to the one-call zlib-framed
//! decoder [`zlib_rs::uncompress`]. Decoding untrusted input must reject
//! malformed streams with an error code and must NEVER panic, read out of
//! bounds, or over-allocate. Any panic here is a memory-safety or robustness
//! bug in the inflate state machine.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // A fixed 64 KiB output ceiling: decoding untrusted input must stay bounded.
    // Every outcome (Ok, or a Data/Buf/Stream error) is acceptable — the only
    // failure this harness catches is a panic / UB inside the decoder.
    let mut out = vec![0u8; 1 << 16];
    let _ = zlib_rs::uncompress(&mut out, data);
});
