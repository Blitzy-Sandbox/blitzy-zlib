//! The aliasing-sensitive boundary paths, in the one shape a Rust-validity checker can see.
//!
//! # Why this file exists separately from the rest of `tests/`
//!
//! Every other test in this crate asks whether the library computes the right answer. This
//! one asks whether the *borrows* it forms while computing it are legal, which is a different
//! question with a different instrument: an ordinary run -- and AddressSanitizer with it --
//! can be green while the code is undefined, because Rust's aliasing rules are violated by
//! the *existence* of two conflicting borrows and not by any observable misbehaviour. Only an
//! interpreter that models those rules, which on this toolchain means Miri, can refute them.
//!
//! So the paths gathered here are exactly the ones where this crate hands the safe core a
//! borrow of caller memory that some *other* caller pointer also covers:
//!
//! | Path | Overlapping pair | The mechanism that keeps it sound |
//! |---|---|---|
//! | `compress2_z`, `compress2`, `uncompress2_z`, `uncompress2` | `source` inside `dest` | `zlib_rs::read_buf::OneShotSink` -- the destination is borrowed one round at a time, so no borrow of it is live when the next input window is copied |
//! | `inflateBack` | `next_in` inside the caller's window | `zlib_rs::infback::InflateBackWindow` -- the window is borrowed for one write cycle at a time, for the same reason |
//! | `deflate`, `deflateParams`, `inflate` with a `gz_header` field inside `next_out` | header field inside the output range | `crate::types::OutputScratch` -- only the overlapping span moves, in windows of `OVERLAP_STAGE_BYTES`, each committed before the next is filled |
//!
//! Each case is driven with **more than one stage window** of input, because a single window
//! is precisely the case a wrong design still gets right: the first copy happens before the
//! first borrow either way, and it is the second and later copies -- taken while a call-long
//! borrow would still have been live -- that are undefined. `OVERLAP_STAGE_BYTES` is 1 KiB,
//! so every fixture below is a few KiB: enough for several refills, small enough that Miri
//! interprets the whole file in a reasonable time.
//!
//! # How to run it
//!
//!     cargo test  -p libz-rs-sys --test alias_overlap
//!     cargo +nightly miri test -p libz-rs-sys --test alias_overlap
//!
//! The second command is the one that matters, and `.github/workflows/rust.yml`'s
//! `miri-facade` job runs it.
//!
//! The header cases carry two further assertions that the aliasing question does not reach, and
//! that belong here because this is where the staging machinery is exercised: the hidden
//! allocation is a **constant** rather than anything the caller chooses -- neither its
//! `avail_out` nor its `name_max` -- and a refused allocation is reported as `Z_MEM_ERROR`
//! rather than as an inconsistent stream. Both are measured through an injected
//! `zalloc`/`zfree` pair, the instrument `test/infcover.c` builds with `mem_setup`/`mem_limit`.
//!
//! # ★ The answers are checked too, not just the borrows
//!
//! A test that only proved "no undefined behaviour" would pass just as happily over a
//! function that returned nothing useful. Every case below compares its result against the
//! same operation performed over *disjoint* buffers, so the assertion is that staging changed
//! neither the status, nor the counts, nor a byte of the output -- with the one documented
//! exception of level 0, where `deflate_stored` sizes its blocks from the `avail_in` it is
//! handed and a windowed feed therefore chooses different block boundaries. That case is
//! asserted as a round trip instead, which is what has to hold when the byte choice is inside
//! the space C leaves undefined.

// Assertions panic, which library code may not do -- but a test that cannot fail is not a
// test. Indexing is likewise used deliberately: every index below is a constant or a length
// the same statement produced, so an out-of-range one is the defect under inspection. The
// cast lints are relaxed because a test drives the C ABI, whose lengths are `uInt`, `uLong`
// and `c_int`: every conversion below acts on a fixture length this file chose, all of them a
// few kilobytes, so none can lose information on any target this workspace builds for.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used
)]

use core::ffi::c_int;
use core::mem::size_of;

use z::{
    compress2, compress2_z, inflateBack, inflateBackEnd, inflateBackInit_, uncompress2,
    uncompress2_z, z_stream, zlibVersion,
};

/// Bytes one staged chunk carries; `crate::types::OVERLAP_STAGE_BYTES`, which is
/// crate-private and so restated here.
///
/// It is not merely copied: `spans_several_stage_windows` asserts that the fixtures really do
/// exceed it several times over, so a future change to the stage size makes this file fail
/// rather than quietly degrade into a single-window test.
const STAGE_BYTES: usize = 1024;

/// `Z_OK`.
const OK: c_int = 0;
/// `Z_STREAM_END`.
const STREAM_END: c_int = 1;
/// `Z_BUF_ERROR`.
const BUF_ERROR: c_int = -5;
/// `Z_DEFAULT_COMPRESSION`.
const DEFAULT_LEVEL: c_int = -1;

/// A payload that is several stage windows long and compresses to far less than itself.
///
/// Both properties are load-bearing. The length is what forces the refill loop past its first
/// iteration; the compressibility is what makes the destination cursor overtake the unread
/// input inside one call, so the overlap is genuine rather than nominal.
fn payload() -> Vec<u8> {
    b"hello, hello! this run repeats so that it compresses to very little. ".repeat(64)
}

/// A payload that DEFLATE cannot shrink, so that its compressed form is itself several stage
/// windows long.
///
/// The one-shot cases want a compressible payload, because there the overlap has to be
/// genuine inside a single call. `inflateBack`'s staged region is the *compressed* stream, so
/// there the requirement inverts: the stream is what must span several windows, and only
/// incompressible input produces one. Generated from a fixed linear congruential sequence so
/// the fixture is deterministic and needs no corpus file.
fn incompressible(len: usize) -> Vec<u8> {
    let mut state = 0x2545_f491_4f6c_dd1d_u64;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 33) as u8
        })
        .collect()
}

/// The payload compressed out of two distinct allocations: the answer every overlapping call
/// below has to reproduce.
fn reference(level: c_int) -> Vec<u8> {
    let source = payload();
    let mut dest = vec![0_u8; source.len() * 2 + 64];
    let mut dest_len = dest.len();
    // SAFETY: `dest` and `source` are two distinct live allocations and the lengths passed
    // are their own.
    let status = unsafe {
        compress2_z(
            dest.as_mut_ptr(),
            &mut dest_len,
            source.as_ptr(),
            source.len(),
            level,
        )
    };
    assert_eq!(status, OK, "the disjoint reference call must succeed");
    dest.truncate(dest_len);
    dest
}

#[test]
fn spans_several_stage_windows() {
    assert!(
        payload().len() > 4 * STAGE_BYTES,
        "the fixture must exercise the refill loop, not only its first iteration"
    );
}

/// `compress2_z` over one buffer: `source` and `dest` are the same address.
#[test]
fn compress2_z_in_place_over_several_windows() {
    let source = payload();
    let expected = reference(DEFAULT_LEVEL);

    let mut shared = vec![0_u8; source.len() * 2 + 64];
    shared[..source.len()].copy_from_slice(&source);
    let base = shared.as_mut_ptr();
    let mut dest_len = shared.len();
    // SAFETY: one live allocation, aliased on purpose. Each input window is copied into the
    // bounded stage while no borrow of the destination exists, and the destination is
    // borrowed for one round at a time, so the two never coexist.
    let status = unsafe { compress2_z(base, &mut dest_len, base, source.len(), DEFAULT_LEVEL) };

    assert_eq!(status, OK);
    assert_eq!(
        &shared[..dest_len],
        &expected[..],
        "staging the input must not change a byte of the output"
    );
}

/// `compress2`, the `uLong` spelling, with the input at an offset inside the destination.
#[test]
fn compress2_partially_overlapping_over_several_windows() {
    let source = payload();
    let expected = reference(DEFAULT_LEVEL);

    let offset = 3 * STAGE_BYTES / 2;
    let mut shared = vec![0_u8; offset + source.len() * 2 + 64];
    shared[offset..offset + source.len()].copy_from_slice(&source);
    let base = shared.as_mut_ptr();
    let mut dest_len = shared.len() as z::uLong;
    // SAFETY: `base` and `base + offset` lie in the same live allocation, whose extent is
    // the length reported for the destination; the ranges overlap deliberately.
    let status = unsafe {
        let middle = base.add(offset);
        compress2(
            base,
            &mut dest_len,
            middle,
            source.len() as z::uLong,
            DEFAULT_LEVEL,
        )
    };

    assert_eq!(status, OK);
    let produced = dest_len as usize;
    assert_eq!(
        &shared[..produced],
        &expected[..],
        "staging the input must not change a byte of the output"
    );
}

/// `uncompress2_z` decoding a stream that lies in its own destination.
#[test]
fn uncompress2_z_in_place_over_several_windows() {
    let source = payload();
    let stream = reference(DEFAULT_LEVEL);

    let mut shared = vec![0_u8; source.len() + stream.len() + 64];
    shared[..stream.len()].copy_from_slice(&stream);
    let base = shared.as_mut_ptr();
    let mut produced = shared.len();
    let mut consumed = stream.len();
    // SAFETY: one live allocation, aliased on purpose, whose extent is the length reported
    // for the destination.
    let status = unsafe { uncompress2_z(base, &mut produced, base, &mut consumed) };

    assert_eq!(status, OK);
    assert_eq!(consumed, stream.len(), "the whole stream must be consumed");
    assert_eq!(produced, source.len());
    assert_eq!(&shared[..produced], &source[..]);
}

/// `uncompress2`, the `uLong` spelling, decoding in place.
#[test]
fn uncompress2_in_place_over_several_windows() {
    let source = payload();
    let stream = reference(DEFAULT_LEVEL);

    let mut shared = vec![0_u8; source.len() + stream.len() + 64];
    shared[..stream.len()].copy_from_slice(&stream);
    let base = shared.as_mut_ptr();
    let mut produced = shared.len() as z::uLong;
    let mut consumed = stream.len() as z::uLong;
    // SAFETY: as above.
    let status = unsafe { uncompress2(base, &mut produced, base, &mut consumed) };

    assert_eq!(status, OK);
    assert_eq!(produced as usize, source.len());
    assert_eq!(&shared[..produced as usize], &source[..]);
}

/// Level 0 in place: the one configuration whose *bytes* a windowed feed may change, so the
/// round trip is what is asserted.
#[test]
fn stored_blocks_round_trip_in_place() {
    let source = payload();

    let mut shared = vec![0_u8; source.len() * 2 + 64];
    shared[..source.len()].copy_from_slice(&source);
    let base = shared.as_mut_ptr();
    let mut dest_len = shared.len();
    // SAFETY: one live allocation, aliased on purpose.
    let status = unsafe { compress2_z(base, &mut dest_len, base, source.len(), 0) };
    assert_eq!(status, OK);
    let stored = shared[..dest_len].to_vec();

    let mut back = vec![0_u8; source.len() + 64];
    let mut produced = back.len();
    let mut consumed = stored.len();
    // SAFETY: two distinct live allocations, each described by its own length.
    let status = unsafe {
        uncompress2_z(
            back.as_mut_ptr(),
            &mut produced,
            stored.as_ptr(),
            &mut consumed,
        )
    };
    assert_eq!(status, OK);
    assert_eq!(&back[..produced], &source[..]);
}

// ---------------------------------------------------------------------------
// inflateBack: the caller's input living inside the caller's window
// ---------------------------------------------------------------------------

/// What `in()` and `out()` share for the duration of one [`inflateBack`] call.
struct Back {
    /// Where in the caller's window the staged input begins.
    input_at: usize,
    /// How much of it is still to be served.
    input_left: usize,
    /// The window itself, which is also where the input sits.
    window: Vec<u8>,
    /// Everything `out()` was handed, in order.
    collected: Vec<u8>,
}

/// C's `in_func`: hands back the whole remaining staged region, from inside the window.
///
/// One call rather than a drip-feed, because the point is to make `inflateBack`'s own
/// staging do the chunking: the facade copies the region a window at a time, and it is those
/// later copies -- taken while a call-long borrow of the window would still be live -- that
/// this file exists to check.
unsafe extern "C" fn pull(desc: *mut core::ffi::c_void, buf: *mut *const u8) -> core::ffi::c_uint {
    // SAFETY: `desc` is the `&mut Back` this test passed to `inflateBack`, live for the whole
    // call, and nothing else borrows it while this callback runs.
    let back = unsafe { &mut *desc.cast::<Back>() };
    if back.input_left == 0 {
        return 0;
    }
    let served = back.input_left;
    // SAFETY: `input_at + input_left` is inside `window`, which the fixture established.
    unsafe {
        *buf = back.window.as_ptr().add(back.input_at);
    }
    back.input_left = 0;
    served as core::ffi::c_uint
}

/// C's `out_func`: collects the bytes and reports success, which for `out()` is zero.
unsafe extern "C" fn push(
    desc: *mut core::ffi::c_void,
    buf: *mut u8,
    len: core::ffi::c_uint,
) -> c_int {
    // SAFETY: as `pull`.
    let back = unsafe { &mut *desc.cast::<Back>() };
    if len != 0 {
        // SAFETY: `inflateBack` hands `out()` a prefix of the window it was given, so `len`
        // bytes are readable at `buf`.
        let written = unsafe { core::slice::from_raw_parts(buf, len as usize) };
        back.collected.extend_from_slice(written);
    }
    0
}

/// A chunk `in()` publishes from inside the window is DECLINED, and this pins that
/// behaviour so the staged case below cannot be mistaken for it.
///
/// ★ The refusal is the callback contract's, not an implementation limit. The window is the
/// output buffer (`zlib.h` L1141-L1146) and `in()`'s bytes must survive "until `in()` is
/// called again or until `inflateBack()` returns" (`infback.c` L172-L174); a chunk the
/// decoder itself overwrites cannot satisfy the second. `Z_BUF_ERROR` with the input channel
/// is the answer, and it arrives without reading a byte of the chunk.
#[test]
fn a_callback_chunk_inside_the_window_is_declined() {
    let source = incompressible(4 * STAGE_BYTES);
    let raw = raw_deflate(&source);
    assert!(
        raw.len() > 2 * STAGE_BYTES,
        "the staged region must span several windows, got {}",
        raw.len()
    );

    // A 32 KiB window, with the raw stream placed inside it at an offset that leaves room
    // for the decoder to write over the front of it.
    let window_bits = 15;
    let window_len = 1_usize << window_bits;
    let input_at = window_len - raw.len();
    let mut window = vec![0_u8; window_len];
    window[input_at..].copy_from_slice(&raw);

    let mut back = Back {
        input_at,
        input_left: raw.len(),
        window,
        collected: Vec::new(),
    };

    // SAFETY: `z_stream` is a `#[repr(C)]` aggregate of scalars and raw pointers, for which
    // an all-zero bit pattern is a valid value -- it is exactly the state `memset`-ing a
    // `z_stream` gives a C caller, and the one `zlib.h` L85-L86 describes as "Z_NULL" hooks.
    let mut strm: z_stream = unsafe { core::mem::zeroed() };
    // SAFETY: `strm` is a live zeroed stream and the window is live for `window_len` bytes;
    // `zlibVersion` is this library's own `'static` string.
    let status = unsafe {
        inflateBackInit_(
            &mut strm,
            window_bits,
            back.window.as_mut_ptr(),
            zlibVersion(),
            size_of::<z_stream>() as c_int,
        )
    };
    assert_eq!(status, OK, "inflateBackInit_ must accept the window");

    // Nothing is staged through `next_in`/`avail_in` on the first case, so that the ordinary
    // callback path is covered too; the second case below stages it.
    let desc = core::ptr::from_mut(&mut back).cast::<core::ffi::c_void>();
    // SAFETY: `strm` is the state `inflateBackInit_` produced, the two callbacks match the
    // C signatures, and `desc` is live for the call.
    let status = unsafe { inflateBack(&mut strm, Some(pull), desc, Some(push), desc) };
    assert_eq!(
        status, BUF_ERROR,
        "a chunk inside the window must be declined, not decoded"
    );
    assert!(
        back.collected.is_empty(),
        "nothing may be produced from a declined chunk"
    );

    // SAFETY: `strm` still holds the state, and nothing borrows the window.
    let status = unsafe { inflateBackEnd(&mut strm) };
    assert_eq!(status, OK);
}

/// The same, with the region delivered through `(next_in, avail_in)` rather than through
/// `in()`, which is the path `crate::infback`'s staged adapter serves.
#[test]
fn inflate_back_with_staged_input_inside_the_window() {
    let source = incompressible(4 * STAGE_BYTES);
    let raw = raw_deflate(&source);
    assert!(
        raw.len() > 2 * STAGE_BYTES,
        "the staged region must span several windows, got {}",
        raw.len()
    );

    let window_bits = 15;
    let window_len = 1_usize << window_bits;
    let input_at = window_len - raw.len();
    let mut window = vec![0_u8; window_len];
    window[input_at..].copy_from_slice(&raw);

    let mut back = Back {
        input_at,
        input_left: 0,
        window,
        collected: Vec::new(),
    };

    // SAFETY: `z_stream` is a `#[repr(C)]` aggregate of scalars and raw pointers, for which
    // an all-zero bit pattern is a valid value -- it is exactly the state `memset`-ing a
    // `z_stream` gives a C caller, and the one `zlib.h` L85-L86 describes as "Z_NULL" hooks.
    let mut strm: z_stream = unsafe { core::mem::zeroed() };
    // SAFETY: as above.
    let status = unsafe {
        inflateBackInit_(
            &mut strm,
            window_bits,
            back.window.as_mut_ptr(),
            zlibVersion(),
            size_of::<z_stream>() as c_int,
        )
    };
    assert_eq!(status, OK);

    // SAFETY: the pointer is inside the live window and the count is the extent placed
    // there, so the stream describes a readable region -- one that overlaps the window on
    // purpose.
    strm.next_in = unsafe { back.window.as_mut_ptr().add(input_at) };
    strm.avail_in = raw.len() as z::uInt;

    let desc = core::ptr::from_mut(&mut back).cast::<core::ffi::c_void>();
    // SAFETY: as above; `in()` will report exhaustion because `input_left` is zero.
    let status = unsafe { inflateBack(&mut strm, Some(pull), desc, Some(push), desc) };
    assert_eq!(
        status, STREAM_END,
        "the staged region must decode to its end"
    );
    assert_eq!(back.collected, source);

    // SAFETY: as above.
    let status = unsafe { inflateBackEnd(&mut strm) };
    assert_eq!(status, OK);
}

/// `source` as a raw DEFLATE stream, which is the only container `inflateBack` accepts
/// (`zlib.h` L1141-L1144).
fn raw_deflate(source: &[u8]) -> Vec<u8> {
    // SAFETY: `z_stream` is a `#[repr(C)]` aggregate of scalars and raw pointers, for which
    // an all-zero bit pattern is a valid value -- it is exactly the state `memset`-ing a
    // `z_stream` gives a C caller, and the one `zlib.h` L85-L86 describes as "Z_NULL" hooks.
    let mut strm: z_stream = unsafe { core::mem::zeroed() };
    // SAFETY: `strm` is a live zeroed stream; `-15` selects a raw 32 KiB-window deflate.
    let status = unsafe {
        z::deflateInit2_(
            &mut strm,
            DEFAULT_LEVEL,
            8,
            -15,
            8,
            0,
            zlibVersion(),
            size_of::<z_stream>() as c_int,
        )
    };
    assert_eq!(status, OK, "deflateInit2_ must accept a raw configuration");

    let mut out = vec![0_u8; source.len() * 2 + 64];
    strm.next_in = source.as_ptr().cast_mut();
    strm.avail_in = source.len() as z::uInt;
    strm.next_out = out.as_mut_ptr();
    strm.avail_out = out.len() as z::uInt;
    // SAFETY: both regions are live for the counts just assigned and they are distinct
    // allocations.
    let status = unsafe { z::deflate(&mut strm, 4) };
    assert_eq!(status, STREAM_END, "the whole input must be compressed");
    let produced = out.len() - strm.avail_out as usize;
    // SAFETY: `strm` still holds the compressor's state.
    let status = unsafe { z::deflateEnd(&mut strm) };
    assert_eq!(status, OK);

    out.truncate(produced);
    out
}

// ---------------------------------------------------------------------------
// A `gz_header` field inside the caller's output range
// ---------------------------------------------------------------------------
//
// ★ The third row of this file's table, and the one whose staging is *not* optional. The three
// header field views are `Cell` slices over the caller's own memory and the header states write
// through them, so a field inside `(next_out, avail_out)` puts a shared `Cell` borrow and an
// exclusive output borrow over the same byte -- undefined whether or not either is used.
// `crate::types::OutputScratch` answers it by moving only the bytes a field covers onto storage
// this library owns, in windows of `OVERLAP_STAGE_BYTES`, and writing the head before the span
// and the tail after it straight into the caller's buffer.
//
// The fixtures below place the span so that **all three** kinds of round run in one call -- a
// capped head, several staged windows, and an uncapped tail -- because a span shorter than one
// window is precisely the case a design with a forgotten cursor still gets right.

/// Where the overlapping field sits inside the caller's output range.
const FIELD_AT: usize = STAGE_BYTES;

/// The field's extent, chosen so the span covers three staging windows rather than one.
const FIELD_BYTES: usize = 2 * STAGE_BYTES + 500;

/// `Z_MEM_ERROR`.
const MEM_ERROR: c_int = -4;

/// `Z_FINISH`.
const FINISH: c_int = 4;

/// The fixture the header-overlap cases decompress: long enough that its decompressed form
/// runs past the overlapping span, so the *tail* segment -- the caller's own buffer again,
/// uncapped -- is reached as well as the head and the windows.
fn header_payload() -> Vec<u8> {
    payload().repeat(2)
}

/// An allocator that records what a stream asked it for, and can be told to refuse.
///
/// The same instrument `test/infcover.c` builds with `mem_setup`/`mem_limit`, in the shape this
/// file needs: `spent` is what one call cost, so two calls differing only in whether staging ran
/// give the staging block's size by subtraction, and `refusing` reproduces the one state that
/// makes an entry point report a memory failure.
///
/// ★ The outstanding blocks are held in a side table rather than in a header written ahead of
/// each one. A prefix header is the obvious C shape and it is *undefined* in Rust: the pointer
/// handed to the stream would carry provenance over the block alone, so recovering the header by
/// subtracting from it reads outside that provenance -- which is exactly what the checker this
/// file exists for reports. Keeping the original pointer in the table preserves its tag, and
/// searching the table on release costs nothing at these sizes.
///
/// ★ Every fixture below reaches its tracker **only** through the raw pointer it installed in
/// `opaque`, never through the local that owns it. Writing through the owning place -- the obvious
/// `tracker.refusing = true` -- invalidates every pointer derived from it, so the next `zalloc`
/// the library performs would be reading through a dead tag. That, too, is a violation the
/// checker this file exists for reports, and it is why the value lives behind
/// [`Box::into_raw`] and is reclaimed only once the last library call has returned.
struct Tracker {
    /// Total bytes handed out over this stream's life.
    spent: usize,
    /// The largest single request.
    largest: usize,
    /// The blocks still outstanding, as the pointer handed out and its byte count. A non-empty
    /// table at the end of a session is a leak.
    live: Vec<(*mut u8, usize)>,
    /// When true every request is refused, exactly as `mem_limit` does.
    refusing: bool,
}

impl Tracker {
    const fn new() -> Self {
        Self {
            spent: 0,
            largest: 0,
            live: Vec::new(),
            refusing: false,
        }
    }
}

/// The alignment every tracked block is given: what `malloc` would provide, and enough for the
/// `u16` and `u32` arrays a deflate state carves out of one byte block.
const TRACK_ALIGN: usize = 16;

/// `zalloc` for [`Tracker`].
///
/// # Safety
///
/// `opaque` must address a live [`Tracker`] that outlives every block obtained through it.
unsafe extern "C" fn track_alloc(opaque: z::voidpf, items: z::uInt, size: z::uInt) -> z::voidpf {
    // SAFETY: this function's contract; the stream hands back the `opaque` it was given.
    let tracker = unsafe { &mut *opaque.cast::<Tracker>() };
    let bytes = items as usize * size as usize;
    if tracker.refusing || bytes == 0 {
        return core::ptr::null_mut();
    }
    let Ok(layout) = std::alloc::Layout::from_size_align(bytes, TRACK_ALIGN) else {
        return core::ptr::null_mut();
    };
    // SAFETY: the layout has a non-zero size, which is `alloc`'s only requirement.
    let block = unsafe { std::alloc::alloc(layout) };
    if block.is_null() {
        return core::ptr::null_mut();
    }
    tracker.spent += bytes;
    tracker.largest = tracker.largest.max(bytes);
    tracker.live.push((block, bytes));
    block.cast()
}

/// `zfree` for [`Tracker`].
///
/// # Safety
///
/// `opaque` must address the same live [`Tracker`], and `address` must be a block
/// [`track_alloc`] returned and has not yet freed.
unsafe extern "C" fn track_free(opaque: z::voidpf, address: z::voidpf) {
    if address.is_null() {
        return;
    }
    // SAFETY: this function's contract.
    let tracker = unsafe { &mut *opaque.cast::<Tracker>() };
    let wanted = address.cast::<u8>();
    let found = tracker
        .live
        .iter()
        .position(|(block, _)| core::ptr::eq(*block, wanted));
    let Some(index) = found else {
        panic!("zfree was handed a block this allocator did not produce, or a double free");
    };
    let (block, bytes) = tracker.live.swap_remove(index);
    let layout = std::alloc::Layout::from_size_align(bytes, TRACK_ALIGN).unwrap();
    // SAFETY: `block` is the pointer `track_alloc` obtained from `alloc` with this exact layout,
    // it has not been released before -- the table entry is removed above -- and the table
    // preserved its provenance over the whole block.
    unsafe {
        std::alloc::dealloc(block, layout);
    }
}

/// What one `deflate` or `inflate` call produced, and what it cost.
struct Outcome {
    /// The status the entry point returned.
    status: c_int,
    /// The bytes it produced, truncated to the count it reported.
    produced: Vec<u8>,
    /// `total_in` on return.
    consumed: z::uLong,
    /// Bytes the stream's allocator handed out over the whole session.
    spent: usize,
}

/// `payload` as a gzip stream whose NAME field is `name`.
fn gzip_stream(payload: &[u8], name: &[u8]) -> Vec<u8> {
    // SAFETY: an all-zero `z_stream` is the `Z_NULL`-hooked state `zlib.h` L85-L86 describes.
    let mut strm: z_stream = unsafe { core::mem::zeroed() };
    // SAFETY: `strm` is a live zeroed stream; `31` selects a 32 KiB-window gzip wrapper.
    let status = unsafe {
        z::deflateInit2_(
            &mut strm,
            DEFAULT_LEVEL,
            8,
            31,
            8,
            0,
            zlibVersion(),
            size_of::<z_stream>() as c_int,
        )
    };
    assert_eq!(status, OK, "deflateInit2_ must accept a gzip configuration");

    let mut field = name.to_vec();
    field.push(0);
    // SAFETY: an all-zero `gz_header` is the state `zlib.h` L118-L133 describes for a writer
    // that installs no fields; `name` is the only one this fixture sets.
    let mut head: z::gz_header = unsafe { core::mem::zeroed() };
    head.name = field.as_mut_ptr();
    // SAFETY: `strm` holds a compressor's state and `head` outlives the calls that emit it.
    assert_eq!(unsafe { z::deflateSetHeader(&mut strm, &mut head) }, OK);

    let mut out = vec![0_u8; payload.len() * 2 + 4096];
    strm.next_in = payload.as_ptr().cast_mut();
    strm.avail_in = payload.len() as z::uInt;
    strm.next_out = out.as_mut_ptr();
    strm.avail_out = out.len() as z::uInt;
    // SAFETY: three distinct live allocations, each for the count assigned to it.
    let status = unsafe { z::deflate(&mut strm, FINISH) };
    assert_eq!(status, STREAM_END, "the whole fixture must be compressed");
    let produced = out.len() - strm.avail_out as usize;
    // SAFETY: `strm` still holds the compressor's state.
    assert_eq!(unsafe { z::deflateEnd(&mut strm) }, OK);
    out.truncate(produced);
    out
}

/// Inflates `gz`, with `head.name` either inside the output range at `(at, capacity)` or in its
/// own allocation.
///
/// `refuse_staging` turns the allocator down *after* the state is built, so the only request
/// left to refuse is the staging block -- which is what `test/infcover.c`'s `mem_limit` does to
/// reach a `Z_MEM_ERROR` path deliberately.
///
/// `feed` is how many `inflate` calls the stream is delivered in. It exists so that a *cost*
/// comparison can be apples to apples: a decode that reaches the end of the stream inside one
/// core call never needs the 32 KiB sliding window, whereas one that spans several calls does,
/// and a staged decode spans several by construction. Feeding the reference in two calls makes
/// both sides pay for the window, so the difference between their totals is the staging block
/// and nothing else.
fn inflate_with_field(
    gz: &[u8],
    out_len: usize,
    field: Option<(usize, usize)>,
    refuse_staging: bool,
    feed: usize,
) -> Outcome {
    let tracker_ptr = Box::into_raw(Box::new(Tracker::new()));
    // SAFETY: an all-zero `z_stream` is a valid one; the three hook members are then assigned.
    let mut strm: z_stream = unsafe { core::mem::zeroed() };
    strm.zalloc = Some(track_alloc);
    strm.zfree = Some(track_free);
    strm.opaque = tracker_ptr.cast();
    // SAFETY: `strm` is live and its hooks address a `Tracker` that outlives this function;
    // `31` selects a gzip wrapper.
    let status =
        unsafe { z::inflateInit2_(&mut strm, 31, zlibVersion(), size_of::<z_stream>() as c_int) };
    assert_eq!(status, OK, "inflateInit2_ must accept a gzip configuration");

    let mut out = vec![0_u8; out_len];
    let mut away = vec![0_u8; field.map_or(64, |(_, capacity)| capacity)];
    // One raw pointer, from which both the output and the overlapping field are derived: taking
    // `as_mut_ptr` twice would retag the first, and this file is meant to be run under a
    // checker that says so.
    let base = out.as_mut_ptr();

    // SAFETY: an all-zero `gz_header` is the state a reader starts from -- every member
    // `inflateGetHeader` consults is assigned below.
    let mut head: z::gz_header = unsafe { core::mem::zeroed() };
    if let Some((at, capacity)) = field {
        assert!(
            at + capacity <= out_len,
            "the field must be inside the output"
        );
        // SAFETY: the assertion above keeps the whole field inside the same live allocation the
        // output occupies, and `base` is that allocation's own pointer. That overlap is the
        // arrangement under test.
        head.name = unsafe { base.add(at) };
        head.name_max = capacity as z::uInt;
    } else {
        head.name = away.as_mut_ptr();
        head.name_max = away.len() as z::uInt;
    }
    // SAFETY: `strm` holds a decompressor's state, and `head` and every buffer it points at
    // outlive the call below.
    assert_eq!(unsafe { z::inflateGetHeader(&mut strm, &mut head) }, OK);

    if refuse_staging {
        // SAFETY: `tracker_ptr` came from `Box::into_raw` and has not been reclaimed. Reaching
        // the value through this pointer rather than through an owning place is what keeps the
        // pointer the stream holds valid -- see [`Tracker`].
        unsafe {
            (*tracker_ptr).refusing = true;
        }
    }

    strm.next_out = base;
    strm.avail_out = out_len as z::uInt;
    let step = gz.len().div_ceil(feed.max(1));
    let mut status = OK;
    let mut offset = 0;
    while offset < gz.len() {
        let take = step.min(gz.len() - offset);
        // SAFETY: `offset + take <= gz.len()`, so the result is inside `gz`'s own allocation.
        strm.next_in = unsafe { gz.as_ptr().add(offset) }.cast_mut();
        strm.avail_in = take as z::uInt;
        // SAFETY: the input is its own live allocation for `avail_in` bytes and the output is
        // `out`'s for what is left of `out_len`; the field, when it overlaps, is inside the
        // latter, which is the arrangement `OutputScratch` exists to serve.
        status = unsafe { z::inflate(&mut strm, 0) };
        offset += take - strm.avail_in as usize;
        if status != OK {
            break;
        }
    }
    let produced = out_len - strm.avail_out as usize;
    let consumed = strm.total_in;
    // SAFETY: `strm` still holds the decompressor's state.
    assert_eq!(unsafe { z::inflateEnd(&mut strm) }, OK);

    // SAFETY: `tracker_ptr` came from `Box::into_raw`, has not been reclaimed, and the last call
    // that could reach it has returned.
    let tracker = unsafe { Box::from_raw(tracker_ptr) };
    assert!(
        tracker.live.is_empty(),
        "every block must go back to the allocator"
    );

    out.truncate(produced);
    Outcome {
        status,
        produced: out,
        consumed,
        spent: tracker.spent,
    }
}

// The span must exercise more than one staging window, and must be preceded by a head segment
// written straight into the caller's buffer. Both are properties of the constants alone, so they
// are checked when this file is compiled rather than when it is run.
const _: () = assert!(FIELD_BYTES > 2 * STAGE_BYTES);
const _: () = assert!(FIELD_AT > 0);

#[test]
fn the_header_field_fixture_spans_several_staging_windows() {
    assert!(
        header_payload().len() > FIELD_AT + FIELD_BYTES + STAGE_BYTES,
        "the span must be followed by a tail segment written straight to the caller"
    );
}

/// ★ **M-12.** `inflateGetHeader` with `name` inside `next_out`, over a span several windows
/// long: every staged round has to land where the decoder would have written it.
#[test]
fn inflate_header_field_inside_the_output_over_several_windows() {
    let source = header_payload();
    let gz = gzip_stream(&source, b"a-name-that-is-read-back");
    let out_len = source.len() + 64;

    let disjoint = inflate_with_field(&gz, out_len, None, false, 1);
    let staged = inflate_with_field(&gz, out_len, Some((FIELD_AT, FIELD_BYTES)), false, 1);

    assert_eq!(
        disjoint.status, STREAM_END,
        "the reference call must finish"
    );
    assert_eq!(
        disjoint.produced, source,
        "the reference call must recover the fixture"
    );
    assert_eq!(
        staged.status, disjoint.status,
        "staging must not change the status"
    );
    assert_eq!(
        staged.consumed, disjoint.consumed,
        "staging must not change how much input was consumed"
    );
    assert_eq!(
        staged.produced, disjoint.produced,
        "every staged round must land at the address the decoder would have written"
    );
}

/// ★ **M-13.** The staging block is a constant, not the caller's `avail_out` and not its
/// `name_max`.
#[test]
fn the_staging_block_is_bounded_by_a_constant() {
    let source = header_payload();
    let gz = gzip_stream(&source, b"n");

    // Two shapes whose only difference from the reference is the field's placement, and whose
    // two caller-chosen numbers differ from each other by a factor of sixteen. If either number
    // reached the allocation, the two costs below would differ by roughly that factor. The
    // output range is larger again, so `avail_out` is a third distinct number and cannot be
    // mistaken for either.
    let out_len = 128 * 1024;
    let small = 4 * 1024;
    let large = 64 * 1024;

    // The reference is fed in two calls so that it, too, pays for the sliding window: see
    // `inflate_with_field`'s `feed`. What is left between the totals is the staging block.
    let disjoint = inflate_with_field(&gz, out_len, None, false, 2);
    let modest = inflate_with_field(&gz, out_len, Some((STAGE_BYTES, small)), false, 1);
    let generous = inflate_with_field(&gz, out_len, Some((STAGE_BYTES, large)), false, 1);
    let roomy = inflate_with_field(&gz, out_len * 2, Some((STAGE_BYTES, large)), false, 1);

    for outcome in [&modest, &generous] {
        assert_eq!(outcome.status, disjoint.status);
        assert_eq!(outcome.produced, disjoint.produced);
        let extra = outcome.spent.saturating_sub(disjoint.spent);
        assert!(
            extra <= STAGE_BYTES,
            "staging cost {extra} bytes on top of the reference's {}, which must not exceed \
             the {STAGE_BYTES}-byte window",
            disjoint.spent
        );
    }
    assert_eq!(
        modest.spent, generous.spent,
        "a sixteen-fold larger advertised field must not cost one byte more"
    );
    assert_eq!(
        roomy.spent, generous.spent,
        "a twice-as-large `avail_out` must not cost one byte more either"
    );
    assert_eq!(
        roomy.produced, disjoint.produced,
        "the larger output range must recover the same bytes"
    );
}

/// ★ **M-13.** A refused staging allocation is reported as a memory failure, which is what
/// `zlib.h` L515-L516 lists among `inflate`'s own results.
#[test]
fn inflate_reports_a_refused_staging_allocation_as_a_memory_error() {
    let source = header_payload();
    let gz = gzip_stream(&source, b"n");
    let out_len = source.len() + 64;

    let refused = inflate_with_field(&gz, out_len, Some((FIELD_AT, FIELD_BYTES)), true, 1);
    assert_eq!(
        refused.status, MEM_ERROR,
        "a refused staging block must be reported as Z_MEM_ERROR, not as an inconsistent stream"
    );
    assert_eq!(
        refused.produced,
        Vec::<u8>::new(),
        "the refusal must happen before a byte is produced"
    );
}

// ---------------------------------------------------------------------------
// The compressor's side of the same overlap
// ---------------------------------------------------------------------------
//
// ★ `deflate` only *reads* its header fields, but a read through a `Cell` view is still
// incompatible with an exclusive borrow of the same byte, so the span moves for the same reason.
// What differs is the ordering the fixture must respect: a gzip header is emitted before any
// compressed data, so a field placed *beyond* the header's own length is fully consumed before
// the write cursor arrives at it. That is what makes the emitted bytes fully determined here and
// therefore comparable, byte for byte, against a run whose field lives in its own allocation.

/// The NAME field's visible length in the compressor fixture, chosen so the range it occupies --
/// the terminator included, per `deflate.c` L1158-L1160 -- is exactly two staging windows.
const DEFLATE_NAME_LEN: usize = 2 * STAGE_BYTES - 1;

/// Where that field sits inside the caller's output range: past the `10 + name + NUL` bytes the
/// gzip header itself occupies, so the compressor has finished reading the name before its write
/// cursor reaches it.
const DEFLATE_FIELD_AT: usize = 3 * STAGE_BYTES;

/// Compresses `source` into a gzip stream, with the NAME field either inside the output range at
/// `DEFLATE_FIELD_AT` or in its own allocation.
fn deflate_with_field(
    source: &[u8],
    out_len: usize,
    inside: bool,
    refuse_staging: bool,
) -> Outcome {
    let tracker_ptr = Box::into_raw(Box::new(Tracker::new()));
    // SAFETY: an all-zero `z_stream` is a valid one; the three hook members are then assigned.
    let mut strm: z_stream = unsafe { core::mem::zeroed() };
    strm.zalloc = Some(track_alloc);
    strm.zfree = Some(track_free);
    strm.opaque = tracker_ptr.cast();
    // SAFETY: `strm` is live and its hooks address a `Tracker` that outlives this function;
    // `31` selects a 32 KiB-window gzip wrapper.
    let status = unsafe {
        z::deflateInit2_(
            &mut strm,
            DEFAULT_LEVEL,
            8,
            31,
            8,
            0,
            zlibVersion(),
            size_of::<z_stream>() as c_int,
        )
    };
    assert_eq!(status, OK, "deflateInit2_ must accept a gzip configuration");

    let mut out = vec![0_u8; out_len];
    let mut away = vec![b'n'; DEFLATE_NAME_LEN + 1];
    away[DEFLATE_NAME_LEN] = 0;
    // One raw pointer, from which both the output and the overlapping field are derived.
    let base = out.as_mut_ptr();
    if inside {
        assert!(
            DEFLATE_FIELD_AT + DEFLATE_NAME_LEN < out_len,
            "the field, terminator included, must be inside the output"
        );
        // SAFETY: the assertion above keeps the whole field, terminator included, inside `out`'s
        // own allocation, and `away`'s bytes are the field's content.
        unsafe {
            core::ptr::copy_nonoverlapping(
                away.as_ptr(),
                base.add(DEFLATE_FIELD_AT),
                DEFLATE_NAME_LEN + 1,
            );
        }
    }
    // SAFETY: an all-zero `gz_header` is the state a writer starts from; `name` is the only
    // field this fixture installs.
    let mut head: z::gz_header = unsafe { core::mem::zeroed() };
    head.name = if inside {
        // SAFETY: as above -- inside `out`'s allocation, which is the overlap under test.
        unsafe { base.add(DEFLATE_FIELD_AT) }
    } else {
        away.as_mut_ptr()
    };
    // SAFETY: `strm` holds a compressor's state and `head`, with the buffer it points at,
    // outlives every call that emits it.
    assert_eq!(unsafe { z::deflateSetHeader(&mut strm, &mut head) }, OK);

    if refuse_staging {
        // SAFETY: as in `inflate_with_field` -- through the raw pointer, never the owning place.
        unsafe {
            (*tracker_ptr).refusing = true;
        }
    }

    strm.next_in = source.as_ptr().cast_mut();
    strm.avail_in = source.len() as z::uInt;
    strm.next_out = base;
    strm.avail_out = out_len as z::uInt;
    // SAFETY: the input is its own live allocation for `avail_in` bytes and the output is
    // `out`'s for `out_len`; the header field, when it overlaps, is inside the latter.
    let status = unsafe { z::deflate(&mut strm, FINISH) };
    let produced = out_len - strm.avail_out as usize;
    let consumed = strm.total_in;
    // SAFETY: `strm` still holds the compressor's state.
    let end = unsafe { z::deflateEnd(&mut strm) };
    assert!(end == OK || refuse_staging, "the teardown must succeed");

    // SAFETY: `tracker_ptr` came from `Box::into_raw`, has not been reclaimed, and the last call
    // that could reach it has returned.
    let tracker = unsafe { Box::from_raw(tracker_ptr) };
    assert!(
        tracker.live.is_empty(),
        "every block must go back to the allocator"
    );

    out.truncate(produced);
    Outcome {
        status,
        produced: out,
        consumed,
        spent: tracker.spent,
    }
}

// The field must sit beyond the header that carries it, or the compressor would be reading bytes
// it is concurrently writing and C's own result would be unspecified -- ten fixed bytes plus the
// name and its terminator. And the range it occupies must cover more than one staging window.
// Both follow from the constants alone, so they are checked at compile time.
const _: () = assert!(DEFLATE_FIELD_AT > 10 + DEFLATE_NAME_LEN + 1);
const _: () = assert!(DEFLATE_NAME_LEN + 1 == 2 * STAGE_BYTES);

/// ★ **M-12/M-13, the compressor.** `deflateSetHeader` with `name` inside `next_out`, over a span
/// two windows long: a capped head round, two staged rounds and an uncapped tail round must
/// together emit exactly what one unsegmented call emits.
#[test]
fn deflate_header_field_inside_the_output_over_several_windows() {
    // Incompressible, so the stream runs well past the span and the tail round is reached.
    let source = incompressible(8 * 1024);
    let out_len = 16 * 1024;

    let disjoint = deflate_with_field(&source, out_len, false, false);
    let staged = deflate_with_field(&source, out_len, true, false);

    assert_eq!(
        disjoint.status, STREAM_END,
        "the reference call must finish"
    );
    assert!(
        disjoint.produced.len() > DEFLATE_FIELD_AT + 2 * STAGE_BYTES,
        "the reference must produce enough to reach past the span, not only into it"
    );
    assert_eq!(
        staged.status, disjoint.status,
        "staging must not change the status"
    );
    assert_eq!(
        staged.consumed, disjoint.consumed,
        "staging must not change how much input was consumed"
    );
    assert_eq!(
        staged.produced, disjoint.produced,
        "a segmented call must emit byte for byte what one unsegmented call emits"
    );
}

/// ★ **M-13, the compressor.** A refused staging allocation is a memory failure, not an
/// inconsistent stream.
#[test]
fn deflate_reports_a_refused_staging_allocation_as_a_memory_error() {
    let source = incompressible(8 * 1024);
    let refused = deflate_with_field(&source, 16 * 1024, true, true);
    assert_eq!(
        refused.status, MEM_ERROR,
        "a refused staging block must be reported as Z_MEM_ERROR"
    );
    assert_eq!(
        refused.consumed, 0,
        "the refusal must happen before a byte is consumed"
    );
    assert_eq!(
        refused.produced,
        Vec::<u8>::new(),
        "the refusal must happen before a byte is produced"
    );
}

/// ★ **M-13, `deflateParams`.** The same refusal, on the entry point that reaches the caller's
/// output through one internal `deflate(strm, Z_BLOCK)` (`deflate.c` L794).
///
/// Reaching the path takes three deliberate steps, all of them states a conforming caller can be
/// in: a first `deflate` that runs out of output space part-way through the gzip header, so the
/// status is still one that dereferences `gzhead`; a level change that selects a different
/// `configuration_table` function, so `deflate.c` L790-L791's condition holds and the internal
/// call really runs; and an output range with the header's `name` inside it.
///
/// The first step needs `memLevel` 1 rather than the default 8. `deflate.c` L1163-L1171 copies the
/// NAME field into `pending_buf` and only stops early when that buffer fills, and
/// `pending_buf_size` is `lit_bufs << (memLevel + 6)` -- 512 bytes at `memLevel` 1 against this
/// fixture's 2048-byte name, so the copy really does stop mid-field and leave the status in
/// `NAME_STATE`. At the default `memLevel` the whole name would fit and the status would already
/// have reached `BUSY_STATE`, where `gzhead` is no longer read and no staging is needed.
#[test]
fn deflate_params_reports_a_refused_staging_allocation_as_a_memory_error() {
    let source = incompressible(4 * 1024);
    let out_len = 16 * 1024;

    let tracker_ptr = Box::into_raw(Box::new(Tracker::new()));
    // SAFETY: an all-zero `z_stream` is a valid one; the hooks are then assigned.
    let mut strm: z_stream = unsafe { core::mem::zeroed() };
    strm.zalloc = Some(track_alloc);
    strm.zfree = Some(track_free);
    strm.opaque = tracker_ptr.cast();
    // SAFETY: `strm` is live and its hooks address a `Tracker` outliving this function; `31`
    // selects a gzip wrapper and `memLevel` 1 the small `pending_buf` this fixture needs.
    let status = unsafe {
        z::deflateInit2_(
            &mut strm,
            6,
            8,
            31,
            1,
            0,
            zlibVersion(),
            size_of::<z_stream>() as c_int,
        )
    };
    assert_eq!(status, OK);

    let mut out = vec![0_u8; out_len];
    let base = out.as_mut_ptr();
    let mut name = vec![b'n'; DEFLATE_NAME_LEN + 1];
    name[DEFLATE_NAME_LEN] = 0;
    // SAFETY: the field, terminator included, is inside `out`'s own allocation.
    unsafe {
        core::ptr::copy_nonoverlapping(
            name.as_ptr(),
            base.add(DEFLATE_FIELD_AT),
            DEFLATE_NAME_LEN + 1,
        );
    }
    // SAFETY: an all-zero `gz_header` is the writer's starting state.
    let mut head: z::gz_header = unsafe { core::mem::zeroed() };
    // SAFETY: `DEFLATE_FIELD_AT + DEFLATE_NAME_LEN < out_len` is asserted at compile time above,
    // so the field lies inside `out`'s own allocation -- the overlap under test.
    head.name = unsafe { base.add(DEFLATE_FIELD_AT) };
    // SAFETY: `strm` holds a compressor's state; `head` outlives every call below.
    assert_eq!(unsafe { z::deflateSetHeader(&mut strm, &mut head) }, OK);

    // A first call with too little room to finish the ten fixed header bytes, into a buffer of
    // its own. It leaves `last_flush` set, which is what `deflate.c` L791 tests, and leaves the
    // status in a header stage, which is what makes `gzhead` still live.
    let mut sliver = [0_u8; 5];
    strm.next_in = source.as_ptr().cast_mut();
    strm.avail_in = source.len() as z::uInt;
    strm.next_out = sliver.as_mut_ptr();
    strm.avail_out = sliver.len() as z::uInt;
    // SAFETY: two distinct live allocations, each for the count assigned to it; the header field
    // is in a third and is disjoint from both.
    assert_eq!(unsafe { z::deflate(&mut strm, 0) }, OK);

    // Now the overlapping output range, and an allocator that refuses.
    strm.next_out = base;
    strm.avail_out = out_len as z::uInt;
    // SAFETY: as in `inflate_with_field` -- through the raw pointer, never the owning place.
    unsafe {
        (*tracker_ptr).refusing = true;
    }
    // SAFETY: `strm` holds a compressor's state, its output range is `out`'s own allocation for
    // `out_len` bytes, and the header field lies inside it -- the arrangement under test.
    let status = unsafe { z::deflateParams(&mut strm, 1, 0) };
    assert_eq!(
        status, MEM_ERROR,
        "a refused staging block must be reported as Z_MEM_ERROR"
    );
    assert_eq!(
        strm.avail_out, out_len as z::uInt,
        "the refusal must happen before a byte is produced"
    );

    // SAFETY: as above.
    unsafe {
        (*tracker_ptr).refusing = false;
    }
    // SAFETY: `strm` still holds the compressor's state.
    assert_eq!(unsafe { z::deflateEnd(&mut strm) }, OK);

    // SAFETY: `tracker_ptr` came from `Box::into_raw`, has not been reclaimed, and the last call
    // that could reach it has returned.
    let tracker = unsafe { Box::from_raw(tracker_ptr) };
    assert!(
        tracker.live.is_empty(),
        "every block must go back to the allocator"
    );
}
