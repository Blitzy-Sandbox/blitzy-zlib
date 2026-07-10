#![no_main]
//! C FFI deflate -> inflate round-trip harness.
//!
//! Exercises the raw pointer / `z_stream` boundary directly: an arbitrary
//! payload is compressed through the `extern "C"` [`deflate`] path and then
//! decompressed through the `extern "C"` [`inflate`] path. The result must
//! equal the original input byte-for-byte. This stresses the handle lifecycle
//! (`*Init_` / `*End`), the `next_in`/`next_out`/`avail_*` pointer arithmetic,
//! and the `Box::into_raw`/`from_raw` opaque-state plumbing at the boundary.

use core::ffi::{c_char, c_int, c_uint};

use libfuzzer_sys::fuzz_target;
use zlib_rs::ffi::{
    deflate, deflateEnd, deflateInit_, inflate, inflateEnd, inflateInit_, z_stream,
};

// zlib return codes / flush modes (ABI-stable integer values).
const Z_OK: c_int = 0;
const Z_STREAM_END: c_int = 1;
const Z_FINISH: c_int = 4;
const DEFAULT_LEVEL: c_int = 6;

fuzz_target!(|data: &[u8]| {
    let version = b"1\0".as_ptr() as *const c_char;
    let size = core::mem::size_of::<z_stream>() as c_int;

    // ---- Compress through the C ABI deflate path. -------------------------
    // SAFETY: all-zero `z_stream` is the C `memset` idiom; every field is a
    // valid zero (null pointers, `None` hooks, zero counters).
    let mut ds: z_stream = unsafe { core::mem::zeroed() };
    // SAFETY: `ds` is a valid owned `z_stream`; `version`/`size` are what the C
    // API expects; `DEFAULT_LEVEL` is in range.
    if unsafe { deflateInit_(&mut ds, DEFAULT_LEVEL, version, size) } != Z_OK {
        return;
    }

    // 1.5x + 128 comfortably exceeds the zlib deflate bound for any input.
    let mut comp = vec![0u8; data.len() + data.len() / 2 + 128];
    ds.next_in = data.as_ptr();
    ds.avail_in = data.len() as c_uint;
    ds.next_out = comp.as_mut_ptr();
    ds.avail_out = comp.len() as c_uint;
    // SAFETY: `next_in`/`next_out` point at the live buffers with matching
    // `avail_*` counts; a single `Z_FINISH` completes in one call given the
    // over-sized output buffer.
    let dret = unsafe { deflate(&mut ds, Z_FINISH) };
    let produced = comp.len() - ds.avail_out as usize;
    // SAFETY: `ds` was initialized above; reclaim its internal state.
    let dend = unsafe { deflateEnd(&mut ds) };
    // The output buffer is deliberately oversized (`data.len() + data.len() / 2
    // + 128` comfortably exceeds the zlib deflate bound for any input), so a
    // single `Z_FINISH` MUST finish the stream in one call. A non-`Z_STREAM_END`
    // result — or a `deflateEnd` that does not return `Z_OK` — is therefore a
    // genuine defect in the crate's OWN FFI deflate lifecycle/compression of
    // self-produced data, not an artifact of untrusted fuzz input, so surface it
    // to libFuzzer instead of swallowing it. (The only silent early return is
    // the `deflateInit_` init/OOM failure above, where no live stream exists.)
    assert_eq!(dret, Z_STREAM_END, "ffi deflate did not finish in one shot");
    assert_eq!(dend, Z_OK, "ffi deflateEnd did not return Z_OK");

    // ---- Decompress through the C ABI inflate path. -----------------------
    // SAFETY: as above.
    let mut is: z_stream = unsafe { core::mem::zeroed() };
    // SAFETY: valid owned `z_stream`; version/size as the C API expects.
    if unsafe { inflateInit_(&mut is, version, size) } != Z_OK {
        return;
    }

    // `max(1)` avoids a zero-length output buffer on the empty-payload edge.
    let mut back = vec![0u8; data.len().max(1)];
    is.next_in = comp.as_ptr();
    is.avail_in = produced as c_uint;
    is.next_out = back.as_mut_ptr();
    is.avail_out = back.len() as c_uint;
    // SAFETY: `next_in`/`next_out` point at the live buffers with matching
    // `avail_*` counts.
    let iret = unsafe { inflate(&mut is, Z_FINISH) };
    let got = back.len() - is.avail_out as usize;
    // SAFETY: `is` was initialized above; reclaim its internal state.
    unsafe {
        inflateEnd(&mut is);
    }

    assert_eq!(iret, Z_STREAM_END, "ffi inflate did not reach stream end");
    assert_eq!(got, data.len(), "ffi round-trip length mismatch");
    assert_eq!(&back[..got], data, "ffi round-trip content mismatch");
});
