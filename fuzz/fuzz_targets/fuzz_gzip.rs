#![no_main]
//! Gzip / zlib auto-detect inflate harness (through the C FFI boundary).
//!
//! Drives arbitrary bytes through the `extern "C"` inflate state machine with
//! `windowBits = 47` (32 + 15 = auto-detect gzip *or* zlib framing). This
//! exercises the gzip header parser (magic, flags, EXTRA/NAME/COMMENT/HCRC
//! fields) on untrusted input. The decoder must reject malformed streams with
//! an error code and must never panic, read out of bounds, or loop unbounded.

use core::ffi::{c_char, c_int, c_uint};

use libfuzzer_sys::fuzz_target;
use zlib_rs::ffi::{inflate, inflateEnd, inflateInit2_, z_stream};

// zlib return codes / flush modes used here (ABI-stable integer values).
const Z_OK: c_int = 0;
const Z_NO_FLUSH: c_int = 0;
// windowBits 47 = 32 (auto-detect header) + 15 (max window): gzip or zlib.
const WBITS_AUTO: c_int = 47;

fuzz_target!(|data: &[u8]| {
    // SAFETY: an all-zero `z_stream` is the exact C `memset(&s, 0, sizeof s)`
    // idiom. Every field is a valid zero: null raw pointers, `None` allocator
    // hooks (the FFI init then installs the default allocator), zero counters.
    let mut strm: z_stream = unsafe { core::mem::zeroed() };

    let version = b"1\0".as_ptr() as *const c_char;
    let size = core::mem::size_of::<z_stream>() as c_int;

    // SAFETY: `strm` is a valid, owned, zeroed `z_stream`; `version` is a valid
    // NUL-terminated string whose first byte matches the library major version,
    // and `size` is the true `sizeof(z_stream)`. `inflateInit2_` performs no
    // hook allocation (the window is allocated lazily).
    if unsafe { inflateInit2_(&mut strm, WBITS_AUTO, version, size) } != Z_OK {
        return;
    }

    let mut out = [0u8; 4096];
    strm.next_in = data.as_ptr();
    strm.avail_in = data.len() as c_uint;

    // Drain the decoder, refilling the fixed output window each pass. `guard`
    // caps total work so a decompression-bomb input cannot dominate the run
    // (libFuzzer's own timeout would otherwise flag it as noise).
    let mut guard: u32 = 0;
    loop {
        strm.next_out = out.as_mut_ptr();
        strm.avail_out = out.len() as c_uint;
        // SAFETY: `strm` holds a live inflate state; `next_in`/`next_out` point
        // at the live local buffers with matching `avail_*` counts.
        let ret = unsafe { inflate(&mut strm, Z_NO_FLUSH) };
        // Stop on Z_STREAM_END (1) or any error (< 0); only Z_OK (0) continues.
        // Also stop once all input is consumed or the work budget is hit.
        if ret != Z_OK || strm.avail_in == 0 {
            break;
        }
        guard += 1;
        if guard > 4096 {
            break;
        }
    }

    // SAFETY: `strm` was initialized by `inflateInit2_`; `inflateEnd` reclaims
    // the internal state box.
    unsafe {
        inflateEnd(&mut strm);
    }
});
