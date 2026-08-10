// The panic-prone family -- `unwrap_used`, `expect_used`, `indexing_slicing`, `panic` -- is
// denied for every target of this package through `[lints] workspace = true`.  That policy is
// right for `src/**`, where a panic would abort a C caller's process, and wrong here: a test
// asserts, and a failing assertion panics.  `clippy.toml`'s `allow-*-in-tests` keys reach
// `#[test]` bodies only, so the file-scope helpers below still need this, and
// `indexing_slicing` has no in-tests key at all.  Nothing else is relaxed.
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used
)]

//! `inflate_back` over storage the library may write but never read.
//!
//! # Why this suite lives in the facade's package and not the core's
//!
//! [`zlib_rs::read_buf::OutputRegion::write_only`] is the shape the C boundary needs.
//! `infback.c` L25-L64 writes nothing into the caller's window before decoding into it, and
//! `test/infcover.c` L475 hands `inflateBack` a stack array that was never initialised -- so
//! there is no `&mut [u8]` to be had at the boundary and none may be manufactured by filling,
//! which would destroy bytes a C caller expects to keep. The region therefore carries a
//! high-water mark and an [`zlib_rs::read_buf::InitView`]: a `fn(&[MaybeUninit<u8>]) -> &[u8]`
//! the *caller* supplies, applied only to slots the region has itself written.
//!
//! Supplying one is a `MaybeUninit<u8>`-to-`u8` reinterpretation, and that is an `unsafe`
//! operation. `crates/zlib-rs` carries `#![forbid(unsafe_code)]` and cannot express it -- which
//! is exactly why the view is a caller-supplied function pointer rather than something the core
//! does for itself -- and the core's own integration suites carry `#![forbid(unsafe_code)]` too,
//! so they cannot express it either. **This package is the one place in the workspace that may**:
//! it is the C ABI boundary, and `src/types.rs`'s `init_view()` is where the shipped facade
//! performs the same reinterpretation for every `(next_out, avail_out)` pair a C caller presents.
//!
//! So these two tests moved here from `crates/zlib-rs/tests/infback.rs` rather than being
//! deleted or rewritten with a filled window: the property they assert -- that the write-only
//! shape decodes identically to an initialised one, and that the short-window guard survives the
//! change of shape -- is a property of the *boundary*, and it is asserted from the crate that
//! owns the boundary. `crates/zlib-rs/tests/infback.rs` keeps the other forty-odd `inflateBack`
//! assertions, all of which need no `unsafe` at all.
//!
//! # What is asserted
//!
//! 1. [`a_write_only_window_decodes_identically_to_an_initialised_one`] -- parity over one
//!    stream: same status, same output bytes, same unused input, same callback traffic. The
//!    storage shape is a boundary concern and must not reach the decoder's decisions.
//! 2. [`a_short_write_only_window_is_refused`] -- a window shorter than `1 << windowBits` is
//!    rejected with `Z_STREAM_ERROR` in the write-only form too. C would proceed with
//!    `left == 0` and spin inside `ROOM()` for ever; `zlib.h` L1203 names the status.
//!
//! Both run under the same fixtures the core's suite uses, transcribed here so this file stands
//! alone -- it links `zlib_rs` directly and needs nothing from `libz_rs_sys`'s exported surface,
//! so it compiles and runs under every feature configuration of this package, including
//! `--no-default-features`.

use core::mem::MaybeUninit;

use zlib_rs::allocate::GlobalAllocator;
use zlib_rs::error::ReturnCode;
use zlib_rs::infback::{
    inflate_back, inflate_back_end, inflate_back_init, inflate_back_into, InflateBackInput,
    InflateBackOutput, OutputFailure,
};
use zlib_rs::read_buf::{InitView, OutputRegion};

// ---------------------------------------------------------------------------
// Fixtures, transcribed from the C harness
// ---------------------------------------------------------------------------

/// The window size `test/infcover.c` uses for every `inflateBack` call.
///
/// `cover_back()` declares `unsigned char win[32768]` (L475) and passes `windowBits = 15`
/// (L486), and `1 << 15` is 32768 -- exactly the size `infback.c` L22-L23 demands.
const HARNESS_WINDOW_LEN: usize = 32768;

/// The `windowBits` the harness uses throughout, and the largest the interface allows.
const HARNESS_WINDOW_BITS: i32 = 15;

/// The byte a caller-supplied window is pre-filled with in the initialised arm.
///
/// Deliberately not zero: a window that arrives zeroed cannot distinguish "the decoder wrote a
/// zero here" from "nothing was written here", which is the whole distinction this file is
/// about. `0xa5` is `test/infcover.c`'s own choice for its instrumented allocator (L46).
const WINDOW_SENTINEL: u8 = 0xa5;

/// `"hello, hello!"` raw-deflated at level 6: one fixed-Huffman block with a match.
///
/// The payload `test/example.c` L35 uses throughout, because the repeated `hello` exercises the
/// match machinery. Verified against the C implementation.
const HELLO_FIXED: &[u8] = &[
    0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0xd7, 0x51, 0xc8, 0x00, 0x51, 0x8a, 0x00,
];

/// What [`HELLO_FIXED`] decodes to: thirteen bytes, with no terminating NUL.
const HELLO: &[u8] = b"hello, hello!";

/// The reinterpretation the shipped facade supplies as a function pointer.
///
/// Sound here for the same reason it is sound in `src/types.rs`: [`OutputRegion`] raises its
/// high-water mark only in its write methods and clamps every read to it, so the view is never
/// reached with a slot nothing has been stored into.
fn view() -> InitView {
    fn shared(slots: &[MaybeUninit<u8>]) -> &[u8] {
        // SAFETY: `MaybeUninit<u8>` is `repr(transparent)` over `u8`, so the cast changes only
        // the type's initialisation promise and not the layout, the length or the memory. Every
        // slot handed here lies inside the region's written prefix, which is what discharges
        // that promise: `OutputRegion` clamps every read to the high-water mark it raises in
        // its own write methods, so no unwritten slot can reach this call.
        unsafe { &*(core::ptr::from_ref(slots) as *const [u8]) }
    }
    InitView::new(shared)
}

/// Returns a caller-supplied window of `len` bytes, pre-filled with [`WINDOW_SENTINEL`].
fn sentinel_window(len: usize) -> Vec<u8> {
    vec![WINDOW_SENTINEL; len]
}

/// Write-only storage of `len` slots, holding nothing at all.
fn uninitialised_window(len: usize) -> Vec<MaybeUninit<u8>> {
    let mut slots: Vec<MaybeUninit<u8>> = Vec::with_capacity(len);
    slots.resize_with(len, MaybeUninit::uninit);
    slots
}

// ---------------------------------------------------------------------------
// The two callbacks
// ---------------------------------------------------------------------------

/// An input source that hands out fixed-size pieces and then reports failure.
///
/// `test/infcover.c` L460's `pull()` hands out one byte per call and returns zero once it runs
/// out. The piece size selects the decode path: pieces smaller than six bytes keep `have` below
/// the `have >= 6 && left >= 258` threshold at `infback.c` L424, so the fast path never fires.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Pieces<'i> {
    /// The pieces still to hand out, in order.
    queue: Vec<&'i [u8]>,

    /// How far through [`Self::queue`] the source has got.
    next: usize,

    /// How many times the decoder asked.
    calls: usize,
}

impl<'i> Pieces<'i> {
    /// Splits `stream` into runs of `size` bytes, the last possibly shorter.
    ///
    /// # Panics
    ///
    /// If `size` is zero, which would describe a source that never makes progress.
    fn new(stream: &'i [u8], size: usize) -> Self {
        assert!(size > 0, "a zero-byte piece would never make progress");
        Self {
            queue: stream.chunks(size).collect(),
            next: 0,
            calls: 0,
        }
    }
}

impl<'i> InflateBackInput<'i> for Pieces<'i> {
    /// The next queued piece, or [`None`] once the queue is empty -- which, on the input side of
    /// the contract, is failure. `in()` returns a count, so zero means "no bytes, and no more".
    fn next_chunk(&mut self) -> Option<&'i [u8]> {
        self.calls += 1;
        let piece = self.queue.get(self.next).copied();
        if piece.is_some() {
            self.next += 1;
        }
        piece
    }
}

/// An output sink that accepts everything and keeps it.
///
/// C's `push(Z_NULL, ..)`, which `return desc != Z_NULL` evaluates to zero for
/// (`test/infcover.c` L467) -- and zero, on the output side, means success. Keeping the bytes is
/// what lets these tests assert that the two decodes agree rather than merely that both were
/// uncomplaining.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct Collector {
    /// Everything handed over, concatenated in the order it arrived.
    written: Vec<u8>,

    /// How many times `out()` ran.
    calls: usize,
}

impl InflateBackOutput for Collector {
    /// Always [`Ok`], which is C's `out()` returning zero: success. Note the inversion against
    /// [`Pieces::next_chunk`].
    ///
    /// # Errors
    ///
    /// Never. The signature is the trait's.
    fn write_out(&mut self, data: &[u8]) -> Result<(), OutputFailure> {
        self.calls += 1;
        self.written.extend_from_slice(data);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// The tests
// ---------------------------------------------------------------------------

/// A window that arrives as write-only storage decodes to exactly the same bytes.
///
/// The assertion is parity with the slice form over the same stream: same status, same output,
/// same unused input, same callback traffic. That is the property the write-only shape has to
/// preserve -- the storage shape is a boundary concern and must not reach the decoder's
/// decisions.
#[test]
fn a_write_only_window_decodes_identically_to_an_initialised_one() {
    // The reference run, over an ordinary initialised window.
    let mut initialised = sentinel_window(HARNESS_WINDOW_LEN);
    let mut state = inflate_back_init(HARNESS_WINDOW_BITS, GlobalAllocator).expect("init");
    let mut expected_sink = Collector::default();
    let mut expected_source = Pieces::new(HELLO_FIXED, 3);
    let expected = inflate_back(
        &mut state,
        &mut initialised,
        None,
        &mut expected_source,
        &mut expected_sink,
    );
    assert_eq!(expected.code, ReturnCode::STREAM_END);
    assert_eq!(expected_sink.written, HELLO);
    assert_eq!(inflate_back_end(&mut state), ReturnCode::OK);

    // The same stream again, over storage that holds nothing.
    let mut slots = uninitialised_window(HARNESS_WINDOW_LEN);
    let mut state = inflate_back_init(HARNESS_WINDOW_BITS, GlobalAllocator).expect("init");
    let mut sink = Collector::default();
    let mut source = Pieces::new(HELLO_FIXED, 3);
    let actual = inflate_back_into(
        &mut state,
        OutputRegion::write_only(&mut slots, view()),
        None,
        &mut source,
        &mut sink,
    );

    assert_eq!(actual.code, expected.code, "same status");
    assert_eq!(sink.written, expected_sink.written, "same output bytes");
    assert_eq!(actual.next_in, expected.next_in, "same unused input");
    assert_eq!(source.calls, expected_source.calls, "same callback traffic");
    assert_eq!(sink.calls, expected_sink.calls, "same number of out() runs");
    assert_eq!(inflate_back_end(&mut state), ReturnCode::OK);
}

/// A window shorter than `1 << windowBits` is refused in the write-only form too.
///
/// `inflate_back`'s window check narrows the region to exactly `state.wsize` and reports
/// `Z_STREAM_ERROR` when it cannot -- C would proceed with `left == 0` and spin inside `ROOM()`
/// for ever (`zlib.h` L1203 names the status). The narrowing goes through
/// `OutputRegion::into_prefix`, so it is worth asserting that the guard survived the change of
/// shape.
#[test]
fn a_short_write_only_window_is_refused() {
    let mut slots = uninitialised_window(HARNESS_WINDOW_LEN - 1);
    let mut state = inflate_back_init(HARNESS_WINDOW_BITS, GlobalAllocator).expect("init");
    let mut sink = Collector::default();
    let mut source = Pieces::new(HELLO_FIXED, 3);
    let result = inflate_back_into(
        &mut state,
        OutputRegion::write_only(&mut slots, view()),
        Some(HELLO_FIXED),
        &mut source,
        &mut sink,
    );

    assert_eq!(result.code, ReturnCode::STREAM_ERROR);
    assert_eq!(
        result.next_in,
        Some(HELLO_FIXED),
        "a refusal before the loop hands the staged input straight back"
    );
    assert!(sink.written.is_empty(), "and emits nothing");
    assert_eq!(source.calls, 0, "and never calls in()");
    assert_eq!(inflate_back_end(&mut state), ReturnCode::OK);
}
