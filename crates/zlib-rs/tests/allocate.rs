//! Integration tests for the safe core's allocator abstraction.
//!
//! This suite carries forward the single most valuable technique in zlib's own
//! test arsenal: the instrumented allocator of `test/infcover.c` (L56-L234),
//! which fills every block it hands out with the byte `0xa5` so that any code
//! path silently assuming zero-initialised memory is caught immediately, and
//! which audits its own use so that a leak, an out-of-order release or a release
//! of a block it never produced is reported rather than tolerated.
//!
//! # What is under test, and what is deliberately not
//!
//! The unit under test is `zlib_rs::allocate` -- the [`Allocator`] contract, the
//! blocks it hands out, and the way the deflate and inflate state types consume
//! it. Four properties are established here and nowhere else in the workspace:
//!
//! 1. **Hook routing.** Every block the core takes is returned to the allocator
//!    that produced it, in the reverse order of allocation, with nothing left
//!    over. That is `deflateEnd`'s `TRY_FREE` sequence (`deflate.c` L1300-L1306)
//!    observed from the outside.
//! 2. **The uninitialised-memory discipline.** Freshly allocated memory is not
//!    zeroed, and nothing in the port may assume it is. See the section comment
//!    on §3.2 below for why `0xa5` rather than `0x00` is the only value that can
//!    answer the question.
//! 3. **The default path.** An allocator with no caller hooks behaves exactly as
//!    C's substituted `zcalloc`/`zcfree` do (`zutil.c` L299-L308), and the choice
//!    of allocator is invisible in the compressed output.
//! 4. **Failure propagation.** A refused allocation surfaces as
//!    [`ReturnCode::MEM_ERROR`] -- never a panic, never an abort -- and leaves
//!    nothing behind. This is the `mem_limit` (`test/infcover.c` L176-L181)
//!    technique, which reaches error paths that fuzzing finds only by accident.
//!
//! Raw-pointer allocator plumbing -- an actual `(zalloc, zfree, opaque)` triple
//! of C function pointers -- is **not** tested here and cannot be: the core
//! carries `#![forbid(unsafe_code)]`, so it can neither hold nor call a C
//! function pointer. That work belongs to `crates/libz-rs-sys` and is tested
//! there. Accordingly this file contains **zero `unsafe`, zero FFI and zero
//! third-party imports**, and the tracking allocator it drives the core through
//! is a safe-Rust implementation of the same trait a facade allocator
//! implements.
//!
//! # Where the machinery lives
//!
//! `TrackingAllocator`, `check_err` and the deterministic corpus all live in
//! `tests/common/mod.rs` and are used from here rather than reimplemented, so
//! that every suite in this crate audits allocation the same way. That module
//! owns its own self-tests; §3.5 below repeats the four load-bearing ones from
//! *this* binary's point of view, because a tracker that silently stopped
//! detecting anomalies would quietly turn §3.1-§3.4 into assertions about
//! nothing.
//!
//! # Cost discipline
//!
//! Every case is bounded in both allocation count and allocation size, because
//! this is the suite that matters most under `cargo +nightly miri test -p
//! zlib-rs`, and Miri interprets each byte of a buffer fill individually. The
//! compact configuration used throughout -- `windowBits` 9 and `memLevel` 1 --
//! asks for 3072 bytes of working memory where the reference defaults ask for
//! 262144, and it exercises the same code paths harder: a 512-byte window slides
//! several times over a payload the default configuration would never slide over
//! at all. One case deliberately uses the reference defaults, and it is the only
//! one gated under Miri; the gate carries its reason at the test itself. One
//! further case, the corpus-wide sweep, is *reduced* rather than gated -- see
//! [`CORPUS_CLASSES_UNDER_MIRI`].

// The panic family is denied workspace-wide by `[workspace.lints.clippy]`, and
// `clippy.toml`'s `allow-unwrap-in-tests` / `allow-expect-in-tests` /
// `allow-panic-in-tests` keys relax that only *inside* a `#[test]` function.
// This file's helpers are file-scope functions shared by many tests, so the
// relaxation has to be stated here instead. Restructuring the helpers to return
// `Result` would be the wrong trade: a helper that cannot compress is a broken
// test, not a recoverable condition, and the panic names the step that broke.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

mod common;

use common::{check_err, corpus, TrackingAllocator};

use zlib_rs::allocate::{Allocator, GlobalAllocator, SENTINEL_FILL};
use zlib_rs::config::{DeflateConfig, InflateConfig, Method, Strategy};
use zlib_rs::deflate::{
    deflate, deflate_bound, deflate_end, deflate_init2, deflate_reset, DeflateStream, Flush,
};
use zlib_rs::error::ReturnCode;
use zlib_rs::inflate::state::{InflateState, LENS_LEN, WORK_LEN};
use zlib_rs::inflate::{
    inflate, inflate_copy, inflate_end, inflate_init2, inflate_reset, inflate_set_dictionary,
    InflateStream,
};

// ---------------------------------------------------------------------------
//  Fixtures and helpers
// ---------------------------------------------------------------------------

/// `windowBits` for the compact configuration: a 512-entry window.
///
/// The smallest value `deflateInit2_` uses as given. `zconf.h` L287 caps the other
/// end at 15, and a request of 8 does not survive: `deflate.c` L436 accepts it only
/// for a zlib-wrapped stream and L439 then rewrites it to 9, with the comment
/// "until 256-byte window bug fixed". Chosen so that the four working buffers total
/// 3072 bytes instead of the default configuration's 262144, which is what keeps
/// this suite fast under Miri.
const COMPACT_WINDOW_BITS: i32 = 9;

/// `memLevel` for the compact configuration: the minimum `zconf.h` L273 allows.
const COMPACT_MEM_LEVEL: i32 = 1;

/// Bytes the compact configuration's *first* buffer asks for.
///
/// `ZALLOC(strm, s->w_size, 2*sizeof(Byte))` (`deflate.c` L458), with
/// `w_size == 1 << 9`. Named because §3.4 arms a limit at exactly this figure to
/// let the window through and refuse everything after it.
const COMPACT_WINDOW_BYTES: usize = 1024;

/// Total working memory the compact configuration asks for, in bytes.
///
/// The four `ZALLOC` calls of `deflate.c` L458-L505 with `w_size == 512`,
/// `hash_size == 256` and `lit_bufsize == 128`: `1024` for the window, `1024` for
/// `prev`, `512` for `head` and `512` for the pending buffer.
const COMPACT_FOOTPRINT: usize = 3072;

/// `windowBits` for a raw stream with the smallest legal window.
///
/// The exact argument `test/infcover.c` L416 passes to `inflateInit2`, chosen
/// there for the same reason it is chosen here: a 256-byte window makes the
/// forced memory failures land on a predictable, tiny allocation.
const RAW_WINDOW_BITS: i32 = -8;

/// Bytes the lazily allocated window of a [`RAW_WINDOW_BITS`] stream asks for.
///
/// `ZALLOC(strm, 1U << state->wbits, sizeof(unsigned char))`
/// (`inflate.c` L260-L262) with `wbits == 8`.
const RAW_WINDOW_BYTES: usize = 256;

/// The longest corpus prefix [`the_allocator_is_invisible_across_every_corpus_class`]
/// compresses when it is running under Miri.
///
/// 512 bytes is the compact configuration's whole window, so a payload this long
/// still reaches the window's far edge. See [`corpus_classes_to_sweep`] for why
/// the length is only half of what has to be bounded.
const CORPUS_PREFIX_UNDER_MIRI: usize = 512;

/// The corpus classes [`the_allocator_is_invisible_across_every_corpus_class`]
/// visits when running under Miri.
///
/// ★ Under Miri the cost of that sweep is dominated by the number of *streams*, not
/// by the length of the payloads. Every stream fills its working buffers on
/// creation -- 3072 bytes for a compressor in the compact configuration and 512
/// for a decompressor's window -- and Miri interprets each of those bytes
/// individually. Visiting all eight classes through two allocators means
/// thirty-two streams, and interpreting that many buffer fills is expensive enough
/// to dominate the run even with every payload truncated to 512 bytes.
///
/// So the class list is bounded too, and these two are the ones chosen, because
/// they are the classes whose encoder decisions no *other* ungated test in this
/// file reaches: incompressible data is what selects a stored block over a Huffman
/// one (`trees.c` L1047 takes that branch when `stored_len + 4 <= opt_lenb`),
/// and binary data is what `detect_data_type` (`trees.c` L966) must classify as
/// binary rather than text. The literal and repetitive classes are already covered
/// ungated by §3.2 and by
/// [`the_default_and_injected_paths_produce_identical_output`].
///
/// Reducing the sweep rather than ignoring it outright is deliberate: an ignored
/// test proves nothing under the tool it was ignored for.
const CORPUS_CLASSES_UNDER_MIRI: [&str; 2] = ["incompressible", "binary"];

/// The named payloads [`the_allocator_is_invisible_across_every_corpus_class`]
/// sweeps: the whole corpus natively, [`CORPUS_CLASSES_UNDER_MIRI`] truncated to
/// [`CORPUS_PREFIX_UNDER_MIRI`] bytes under Miri.
///
/// The two classes are built directly rather than selected out of `corpus::all()`,
/// because `all()` *generates* every fixture it returns -- including the 33048-byte
/// window-crossing payload -- and generating one under the interpreter costs more
/// than compressing the 512 bytes that survive the truncation. Filtering after the
/// fact would pay that cost and then throw the result away.
fn corpus_sweep_classes() -> Vec<(&'static str, Vec<u8>)> {
    if !cfg!(miri) {
        return corpus::all();
    }

    let mut binary = corpus::binary();
    binary.truncate(CORPUS_PREFIX_UNDER_MIRI);
    let classes = vec![
        (
            CORPUS_CLASSES_UNDER_MIRI[0],
            corpus::incompressible(CORPUS_PREFIX_UNDER_MIRI),
        ),
        (CORPUS_CLASSES_UNDER_MIRI[1], binary),
    ];
    // The names are load-bearing: they appear in every assertion message, and a
    // typo would silently rename a class rather than fail.
    assert_eq!(
        classes.len(),
        CORPUS_CLASSES_UNDER_MIRI.len(),
        "the reduced sweep must cover every class it names"
    );
    classes
}

/// The compression configuration this suite uses wherever the configuration is
/// not itself the thing under test.
///
/// Level 6 is `Z_DEFAULT_COMPRESSION`'s resolved value (`deflate.c` L379-L383 via
/// `DEF_LEVEL`), so the lazy-matching `deflate_slow` path and the whole Huffman
/// coder are exercised; only the buffer sizes are reduced.
fn compact_config() -> DeflateConfig {
    DeflateConfig {
        level: 6,
        method: Method::Deflated,
        window_bits: COMPACT_WINDOW_BITS,
        mem_level: COMPACT_MEM_LEVEL,
        strategy: Strategy::Default,
    }
}

/// The byte footprint of one decoder state object.
///
/// This is C's `sizeof(struct inflate_state)`, the quantity `test/infcover.c`
/// L428 builds its allocation budget out of. It is computed rather than written
/// down because the figure is target- and layout-dependent; see
/// [`the_infcover_copy_budget_forces_a_memory_error`] for what depends on it.
///
/// The type argument matters only for the allocator type parameter, which
/// contributes one word; every other field is shared by all instantiations.
fn inflate_state_footprint() -> usize {
    size_of::<InflateState<'_, &TrackingAllocator>>()
}

/// Compresses `source` in one call and returns the complete stream.
///
/// The three steps a caller of the safe core performs, in the order the C API
/// performs them: `deflateInit2` (which internally ends with `deflateReset`),
/// then `deflate` with `Z_FINISH`, then `deflateEnd`. The reset is repeated here
/// because [`deflate_init2`] returns only the state half; the `z_stream` half --
/// and in particular the Adler-32 seed of 1 that `deflate.c` L667-L671 installs
/// -- has to be applied to the stream view, and [`deflate_reset`] is idempotent
/// on a freshly initialised state.
///
/// # Panics
///
/// If initialisation fails, if `deflate` does not report `Z_STREAM_END` on the
/// single `Z_FINISH` call, or if `deflateEnd` reports anything but `Z_OK`. The
/// output buffer is sized with [`deflate_bound`], which `zlib.h` L768-L777
/// guarantees is never an underestimate, so a short buffer is a defect rather
/// than an expected outcome.
fn deflate_through<'a, A>(source: &[u8], config: DeflateConfig, allocator: A) -> Vec<u8>
where
    A: Allocator<'a> + Copy,
{
    let mut state = deflate_init2(config, allocator).expect("deflate_init2");

    // `deflateBound` with no stream is the configuration-independent worst case:
    // `deflate.c` L876-L879 takes that branch when `deflateStateCheck` rejects the
    // stream, and returns the larger of the fixed-block and stored-block bounds
    // plus eighteen bytes of wrapper. That is exactly what a caller sizing a buffer
    // before initialisation gets, and `zlib.h` L771-L777 promises it is never an
    // underestimate. The allocator type has to be named because `None` carries no
    // state to infer it from; the branch taken never touches an allocator.
    let source_len = u64::try_from(source.len()).unwrap();
    let room = usize::try_from(deflate_bound::<A>(None, source_len)).unwrap();
    let mut packed = vec![0_u8; room];

    // Scoped so that the mutable borrow of `packed` ends before it is truncated.
    let produced = {
        let mut stream = DeflateStream::new(source, &mut packed);
        stream.apply_reset(deflate_reset(&mut state));
        let code = deflate(&mut state, &mut stream, Flush::Finish.as_raw());
        assert_eq!(
            code,
            ReturnCode::STREAM_END,
            "deflate(Z_FINISH) did not finish the stream: {code:?}"
        );
        stream.next_out
    };

    // `deflateEnd` reports `Z_DATA_ERROR` for a stream freed mid-compression
    // (`zlib.h` L373-L377); a finished stream must report `Z_OK`. `check_err` is
    // the `CHECK_ERR` macro of `test/example.c` L28-L33, which is how the C suite
    // spells this and which reports the numeric code a maintainer will look for.
    check_err(deflate_end(&mut state), "deflateEnd");

    packed.truncate(produced);
    packed
}

/// Decompresses `packed` in one call and returns the recovered bytes.
///
/// The inflate counterpart of [`deflate_through`]. `room` is the caller's output
/// budget, and it is required to be sufficient: a short buffer would make
/// `inflate` return `Z_OK` with output still pending rather than `Z_STREAM_END`,
/// so the `Z_STREAM_END` assertion below doubles as the check that `room` was
/// enough, and a caller that under-budgeted is told so at the call that did it.
///
/// # Panics
///
/// If initialisation fails, if `inflate` does not report `Z_STREAM_END`, or if
/// `inflateEnd` reports anything but `Z_OK`.
fn inflate_through<'a, A>(packed: &[u8], window_bits: i32, room: usize, allocator: A) -> Vec<u8>
where
    A: Allocator<'a> + Copy,
{
    let mut state =
        inflate_init2(InflateConfig::new(window_bits), allocator).expect("inflate_init2");
    let mut plain = vec![0_u8; room];

    let produced = {
        let mut stream = InflateStream::new(packed, &mut plain);
        stream.apply_reset(inflate_reset(&mut state));
        let code = inflate(&mut state, &mut stream, Flush::Finish.as_raw());
        assert_eq!(
            code,
            ReturnCode::STREAM_END,
            "inflate(Z_FINISH) did not finish the stream: {code:?}"
        );
        stream.next_out
    };

    check_err(inflate_end(&mut state), "inflateEnd");

    plain.truncate(produced);
    plain
}

/// Compresses and then decompresses `source` through `allocator`, returning both
/// halves.
///
/// The `.0` element is the compressed stream and the `.1` element the recovered
/// payload, so a caller can assert on byte identity of the former and on exact
/// recovery of the latter. Both stages share one allocator, which is what makes
/// this the shape §3.2 needs: a single tracker sees every block both a compressor
/// and a decompressor take.
///
/// # Panics
///
/// As [`deflate_through`] and [`inflate_through`].
fn round_trip_through<'a, A>(
    source: &[u8],
    config: DeflateConfig,
    allocator: A,
) -> (Vec<u8>, Vec<u8>)
where
    A: Allocator<'a> + Copy,
{
    let packed = deflate_through(source, config, allocator);
    // A decoder window at least as large as the encoder's is required by
    // RFC 1950; passing the encoder's own `windowBits` is the tightest legal
    // choice and keeps the decoder's lazy window as small as the encoder's.
    let plain = inflate_through(&packed, config.window_bits, source.len(), allocator);
    (packed, plain)
}

// ---------------------------------------------------------------------------
//  §3.1  The hook-routing contract
// ---------------------------------------------------------------------------
//
// Everything the core allocates it must give back, to the allocator it came
// from, in the reverse order it was taken. C states the order outright --
// `deflateEnd` frees "in reverse order of allocations" (`deflate.c`
// L1300-L1306) -- and `test/infcover.c` is what notices when the promise is
// broken: a release of anything other than the most recent block increments its
// `notlifo` counter (L136) and a release of a block it never handed out
// increments `rogue` (L150).
//
// The order is not pedantry in a Rust port. Rust drops the fields of a struct in
// *declaration* order, so a state type that declared its buffers in allocation
// order would release them in exactly the wrong one, and no ordinary test would
// notice.

/// A compressor's whole lifetime leaves the tracker with nothing outstanding.
///
/// `deflateInit2` takes four blocks (`deflate.c` L458-L505) and `deflateEnd`
/// returns all four (L1300-L1306). The three assertions are precisely the three
/// diagnostics `mem_done` prints at `test/infcover.c` L220-L227: bytes still
/// outstanding, releases that were not last-in-first-out, and releases of
/// addresses never handed out.
#[test]
fn a_compressor_returns_every_block_it_took() {
    let tracker = TrackingAllocator::new();

    let mut state = deflate_init2(compact_config(), &tracker).expect("deflate_init2");
    // All four buffers exist at this point, and the figure is exact rather than
    // approximate: `w_size * 2` + `w_size * 2` + `hash_size * 2` +
    // `lit_bufsize * LIT_BUFS` for the compact configuration.
    assert_eq!(
        tracker.total(),
        COMPACT_FOOTPRINT,
        "deflateInit2 did not take the four buffers of deflate.c L458-L505"
    );

    check_err(deflate_end(&mut state), "deflateEnd");

    assert_eq!(tracker.total(), 0, "bytes outstanding after deflateEnd");
    assert_eq!(tracker.not_lifo(), 0, "releases not LIFO after deflateEnd");
    assert_eq!(tracker.rogue(), 0, "unrecognised releases after deflateEnd");
    tracker.assert_clean();
}

/// A decompressor's whole lifetime leaves the tracker with nothing outstanding.
///
/// The inflate counterpart, and it has a different shape on purpose:
/// `inflateInit2` allocates **no** working memory in this port, because the
/// window is allocated lazily on first use (the allocation prefix of
/// `updatewindow`, `inflate.c` L256-L262) and the state object itself is the
/// facade's `ZALLOC(strm, 1, sizeof(inflate_state))` (`inflate.c` L197-L198)
/// rather than the core's. So the window is brought into existence deliberately,
/// through `inflateSetDictionary`, which is the same lever `test/infcover.c` L426
/// uses for the same purpose.
#[test]
fn a_decompressor_returns_its_lazily_allocated_window() {
    let tracker = TrackingAllocator::new();

    let mut state = inflate_init2(InflateConfig::new(RAW_WINDOW_BITS), &tracker)
        .expect("inflate_init2 with a raw 256-byte window");
    assert_eq!(
        tracker.total(),
        0,
        "inflateInit2 must not allocate: the window is lazy and the state block is the facade's"
    );

    // A raw stream accepts a dictionary at any point (`inflate.c` L1196-L1197
    // rejects only a wrapped stream outside `DICT` mode), and setting one runs
    // `updatewindow`, which is what allocates.
    check_err(
        inflate_set_dictionary(&mut state, corpus::DICTIONARY),
        "inflateSetDictionary on a raw stream",
    );
    assert_eq!(
        tracker.total(),
        RAW_WINDOW_BYTES,
        "the lazily allocated window is 1 << wbits bytes"
    );

    check_err(inflate_end(&mut state), "inflateEnd");

    assert_eq!(tracker.total(), 0, "bytes outstanding after inflateEnd");
    assert_eq!(tracker.not_lifo(), 0, "releases not LIFO after inflateEnd");
    assert_eq!(tracker.rogue(), 0, "unrecognised releases after inflateEnd");
    tracker.assert_clean();
}

/// Memory obtained from one allocator is never returned to another.
///
/// This is the evidence for the dependency-injection design: an
/// [`Allocator`] is a parameter of a state object, so a block cannot reach a
/// different allocator's release path. Two independent trackers each serve their
/// own compressor, the two lifetimes are interleaved so that both have live
/// blocks at the same moment, and then both are torn down.
///
/// A crossed release would be visible twice over: the tracker that received the
/// foreign block would report it as `rogue` (`test/infcover.c` L150) and the
/// tracker that produced it would report it as leaked (L220-L222). Asserting both
/// counters on both trackers is therefore a complete check, and it is the bug
/// class C can only detect after the fact -- in C a crossed free is heap
/// corruption, not an accounting note.
#[test]
fn blocks_never_cross_between_two_independent_allocators() {
    let first = TrackingAllocator::new();
    let second = TrackingAllocator::new();

    let mut first_state = deflate_init2(compact_config(), &first).expect("deflate_init2 on first");
    // Interleaved deliberately: both allocators have live blocks here, so a
    // mismatched release has something wrong to reach for.
    let mut second_state =
        deflate_init2(compact_config(), &second).expect("deflate_init2 on second");

    assert_eq!(first.total(), COMPACT_FOOTPRINT, "first allocator's blocks");
    assert_eq!(
        second.total(),
        COMPACT_FOOTPRINT,
        "second allocator's blocks"
    );

    // Torn down in the opposite order to construction, so the release sequence is
    // not merely the mirror image of the allocation sequence.
    assert_eq!(
        deflate_end(&mut first_state),
        ReturnCode::OK,
        "first deflateEnd"
    );
    assert_eq!(
        second.total(),
        COMPACT_FOOTPRINT,
        "second untouched by first"
    );
    assert_eq!(
        deflate_end(&mut second_state),
        ReturnCode::OK,
        "second deflateEnd"
    );

    assert_eq!(first.rogue(), 0, "first allocator received a foreign block");
    assert_eq!(
        second.rogue(),
        0,
        "second allocator received a foreign block"
    );
    assert_eq!(first.total(), 0, "first allocator leaked");
    assert_eq!(second.total(), 0, "second allocator leaked");

    first.assert_clean();
    second.assert_clean();
}

/// The high-water mark records the peak and never falls back.
///
/// `mem_high` (`test/infcover.c` L192-L197) exists so that a stream's peak
/// working-set size can be quoted after the stream is gone, which is what the
/// port's per-stream memory budget is measured against. That only works if the
/// figure is monotonic, so this asserts the property directly: it rises with the
/// second compressor, and a full teardown drops the outstanding total to zero
/// while leaving the peak standing.
#[test]
fn the_high_water_mark_is_monotonic_and_survives_teardown() {
    let tracker = TrackingAllocator::new();
    assert_eq!(tracker.high_water(), 0, "a fresh tracker has no history");

    let mut first = deflate_init2(compact_config(), &tracker).expect("first deflate_init2");
    let after_one = tracker.high_water();
    assert_eq!(after_one, COMPACT_FOOTPRINT, "peak after one compressor");
    assert!(
        after_one >= tracker.total(),
        "the peak can never be below the current total"
    );

    // A second live compressor doubles what is outstanding, so the peak must rise.
    let mut second = deflate_init2(compact_config(), &tracker).expect("second deflate_init2");
    let after_two = tracker.high_water();
    assert_eq!(
        after_two,
        COMPACT_FOOTPRINT * 2,
        "peak after two compressors"
    );
    assert!(after_two > after_one, "the peak did not rise");

    // Releasing one lowers the total but must not lower the peak. The younger
    // compressor goes first: a single tracker serving two streams sees one
    // interleaved release sequence, and ending the *older* stream first would
    // release blocks that are not the most recent and so would be reported as
    // non-LIFO -- correctly, but for a reason that has nothing to do with the
    // library. `test/infcover.c` never meets this because `mem_setup` gives every
    // stream its own zone (L158-L173).
    assert_eq!(
        deflate_end(&mut second),
        ReturnCode::OK,
        "second deflateEnd"
    );
    assert_eq!(
        tracker.total(),
        COMPACT_FOOTPRINT,
        "one compressor still live"
    );
    assert_eq!(tracker.high_water(), after_two, "the peak fell on release");

    assert_eq!(deflate_end(&mut first), ReturnCode::OK, "first deflateEnd");
    assert_eq!(tracker.total(), 0, "bytes outstanding after both ended");
    assert_eq!(
        tracker.high_water(),
        after_two,
        "the peak fell once everything was released"
    );

    // `finish` reports the same peak it has been reporting all along, which is
    // what makes it usable as a measurement rather than a log line.
    let report = tracker.finish();
    assert!(report.is_clean(), "unexpected anomaly: {report:?}");
    assert_eq!(report.high_water, after_two, "the report's peak disagrees");
}

// ---------------------------------------------------------------------------
//  §3.2  The uninitialised-memory discipline
// ---------------------------------------------------------------------------
//
// ★ WHY 0xa5 AND NOT 0x00 ★
//
// `test/infcover.c` L86-L87 fills every block it hands out with `0xa5`, and its
// comment says why: "fill memory with a non-zero value to make sure that the code
// isn't depending on zeros". That is the whole argument, and it is worth stating
// in full because the choice looks arbitrary and is not.
//
// A fill of `0x00` is worse than useless here -- it is actively misleading. If
// the allocator hands back zeros, then a code path that reads a buffer before
// writing it and a code path that correctly initialises the buffer first produce
// *the same answer*, so the test cannot distinguish "this code initialised the
// memory" from "the allocator happened to hand back zeros". The bug is not
// merely undetected; it is rendered undetectable. Every question of the form
// "does this code assume zeroed memory?" becomes unanswerable.
//
// A fill of any fixed non-zero byte restores the distinction: code that reads
// before writing now sees `0xa5` where it expected `0x00`, and the wrong answer
// propagates into the compressed output, the checksum or the recovered payload,
// where an ordinary equality assertion catches it.
//
// The question is live rather than theoretical, because the reference allocator
// really does not zero anything. The generic `zcalloc` (`zutil.c` L299-L303) is
//
//     return sizeof(uInt) > 2 ? (voidpf)malloc(items * size) :
//                               (voidpf)calloc(items, size);
//
// and `uInt` is `unsigned int`, four bytes wide on every target this port
// supports, so `sizeof(uInt) > 2` is always true and the branch taken is always
// **`malloc`**. `calloc` -- the only zeroing branch -- is reachable only on a
// 16-bit target. Default-allocated blocks therefore hold whatever the platform
// allocator left behind, and a caller-supplied hook is under no obligation to do
// better. The reference implementation is written accordingly and initialises
// what it needs explicitly: `deflate.c` L442 zeroes the state struct with
// `zmemzero`, and `deflate.c` L170-L173 clears the hash head array with
// `CLEAR_HASH`, precisely because neither arrives zeroed.
//
// `0xa5` specifically, rather than some other non-zero byte, is chosen for
// continuity with the C harness: a byte that turns up in a diff, a core dump or a
// failing assertion means the same thing to a maintainer of either
// implementation. It is exported from the core as `SENTINEL_FILL` so that the
// tracker here, the crate's own default allocator in debug builds, and any facade
// allocator all agree on it.

/// A block the tracker hands out is `0xa5` from end to end.
///
/// This test guards the guard. Every assertion in §3.2 and §3.4 rests on the
/// tracker actually filling with a non-zero byte; a regression that silently
/// zeroed instead would make the whole section pass while testing nothing at all.
/// So the fill is asserted directly, against a block taken from the tracker by
/// hand rather than through the library, and the sentinel's value is pinned to
/// the literal `0xa5` that `test/infcover.c` L87 writes.
#[test]
fn a_block_handed_out_by_the_tracker_is_filled_with_the_sentinel() {
    // Pinned against the C harness, not against itself: if `SENTINEL_FILL` ever
    // drifted from `memset(ptr, 0xa5, len)` the two implementations would stop
    // catching the same bug.
    assert_eq!(SENTINEL_FILL, 0xa5, "SENTINEL_FILL must be infcover's 0xa5");

    let tracker = TrackingAllocator::new();

    // `ZALLOC(strm, items, size)` keeps its two arguments separate all the way to
    // the hook (`zutil.h` L252-L253), and the block is their product
    // (`test/infcover.c` L76 computes `count * (size_t)size`).
    let bytes = tracker.allocate_bytes(8, 4).expect("allocate_bytes(8, 4)");
    assert_eq!(bytes.len(), 32, "items * size is the block length");
    assert!(
        bytes.as_slice().iter().all(|&byte| byte == SENTINEL_FILL),
        "a fresh byte block is not filled with the sentinel"
    );
    assert!(
        !bytes.as_slice().contains(&0),
        "a fresh byte block contains a zero: the fill has regressed to calloc semantics"
    );

    // The hash-chain shape, `ZALLOC(strm, items, sizeof(Pos))` (`deflate.c`
    // L459-L460). C's `memset` is bytewise and does not care what the block will
    // hold, so every byte of a `u16` block must carry the sentinel too, which
    // makes each element `0xa5a5`.
    let positions = tracker.allocate_u16s(6).expect("allocate_u16s(6)");
    assert_eq!(positions.len(), 6, "allocate_u16s counts elements");
    let expected = u16::from_ne_bytes([SENTINEL_FILL, SENTINEL_FILL]);
    assert!(
        positions.as_slice().iter().all(|&word| word == expected),
        "a fresh u16 block is not filled bytewise with the sentinel"
    );
    // Accounting stays in bytes, which is the unit `mem_alloc` records.
    assert_eq!(
        tracker.total(),
        32 + 6 * size_of::<u16>(),
        "the tracker accounts for both blocks in bytes"
    );

    // Released newest first, so the sequence is last-in-first-out.
    tracker.deallocate_u16s(&mut Some(positions));
    tracker.deallocate_bytes(&mut Some(bytes));
    tracker.assert_clean();
}

/// A full round trip through the `0xa5` allocator recovers the input exactly.
///
/// The payload is `test/example.c`'s own (L35): `"hello, hello!"` plus its
/// terminating NUL, fed as `strlen(hello) + 1`, whose repeated `"hello"` the C
/// comment says "stresses the compression code better".
///
/// This is the case the whole section exists for. A compressor and a decompressor
/// are both built on memory that is guaranteed *not* to be zeroed, and the
/// recovered bytes are compared against the original. Any place in the port that
/// reads a freshly allocated buffer before writing it -- the window, the hash
/// head array, the `prev` chain, the pending buffer or the decoder's window --
/// produces a wrong answer here and only here.
#[test]
fn a_round_trip_over_sentinel_filled_memory_recovers_the_input() {
    let tracker = TrackingAllocator::new();

    let (packed, plain) = round_trip_through(corpus::HELLO, compact_config(), &tracker);

    assert_eq!(
        plain,
        corpus::HELLO,
        "the round trip did not recover the payload byte for byte"
    );
    // A zlib-wrapped stream is a 2-byte header, the deflate data and a 4-byte
    // Adler-32 trailer, so anything this short would be a truncated stream rather
    // than a compressed one.
    assert!(
        packed.len() > 6,
        "the compressed stream is too short to be well formed: {} bytes",
        packed.len()
    );

    assert!(
        tracker.high_water() > 0,
        "the library did not allocate through the injected allocator at all"
    );
    tracker.assert_clean();
}

/// Sliding the window over sentinel-filled memory recovers the input exactly.
///
/// The previous case is 14 bytes long, so it never fills the window; this one is
/// long enough to slide a 512-byte window repeatedly. That matters because
/// sliding is where uninitialised memory is most dangerous: `fill_window`
/// (`deflate.c` L252) moves the window's upper half down and `slide_hash`
/// (`deflate.c` L187) rewrites both chain arrays, so a match finder that trusted
/// a stale or never-written entry would emit a distance pointing at memory that
/// holds nothing but `0xa5`. The decoder would then either reject the stream or
/// reproduce the fill byte, and the equality assertion catches both.
///
/// The payload is highly repetitive, which is deliberate: it maximises the number
/// of hash-chain hits, and therefore the number of window reads, per byte.
#[test]
fn sliding_the_window_over_sentinel_filled_memory_recovers_the_input() {
    // Just over twice the compact window, so the window slides more than once
    // while staying small enough to interpret quickly under Miri.
    let payload = corpus::repetitive(1200);
    let tracker = TrackingAllocator::new();

    let (packed, plain) = round_trip_through(&payload, compact_config(), &tracker);

    assert_eq!(
        plain, payload,
        "the round trip did not recover the sliding-window payload"
    );
    // Repetitive input is the easiest case there is, so a stream that did not
    // shrink dramatically means the match finder found nothing -- which is what a
    // hash chain read out of unwritten memory would look like.
    assert!(
        packed.len() < payload.len() / 4,
        "repetitive input did not compress: {} bytes from {}",
        packed.len(),
        payload.len()
    );
    tracker.assert_clean();
}

/// Neither the compressor nor the decompressor ever reports the fill byte.
///
/// A stronger statement than "the round trip worked": it checks that the *shape*
/// of the failure this section hunts is absent. `corpus::HELLO` contains no
/// `0xa5` byte, so if the fill leaked into the encoder's output through an
/// uninitialised read, the recovered payload would differ -- and if it leaked
/// into the decoder's window, the recovered payload would contain the sentinel.
/// Both are asserted, so a failure says which side leaked.
#[test]
fn the_fill_byte_never_reaches_the_payload() {
    assert!(
        !corpus::HELLO.contains(&SENTINEL_FILL),
        "the fixture must not contain the fill byte, or this test proves nothing"
    );

    let tracker = TrackingAllocator::new();
    let (_packed, plain) = round_trip_through(corpus::HELLO, compact_config(), &tracker);

    assert!(
        !plain.contains(&SENTINEL_FILL),
        "the allocator's fill byte reached the recovered payload"
    );
    assert_eq!(
        plain,
        corpus::HELLO,
        "the recovered payload is not the input"
    );
    tracker.assert_clean();
}

// ---------------------------------------------------------------------------
//  §3.3  The default path: both caller hooks absent
// ---------------------------------------------------------------------------
//
// `zlib.h` L149-L153 promises that "when `zalloc` and `zfree` are `Z_NULL` on
// entry to the initialization function, they are set to internal routines that
// use the standard library functions `malloc()` and `free()`". Each initialiser
// performs the substitution itself -- `deflate.c` L401-L414, `inflate.c`
// L183-L195 and `infback.c` L37-L49 all install `zcalloc`/`zcfree` from
// `zutil.c` L299-L308 and set `opaque` to the null pointer.
//
// In this port that substitution is not a run-time test against a null function
// pointer; it is a *type*. `GlobalAllocator` is the internal-routine path, and it
// is what a facade installs when it finds both hooks null, so exercising it here
// exercises the default path a caller who zeroes their `z_stream` gets. It
// reports `AllocatorId::GLOBAL` and `Opaque::NULL`, which is the null `opaque`
// C stores at `deflate.c` L406.
//
// The second, sharper property in this section is that the allocator must be
// *invisible*. A caller's choice of `zalloc` is a memory-management decision, and
// it may not perturb a single bit of the compressed stream. This is exactly the
// byte-identity requirement viewed from an unusual angle, and it is cheap to
// check here because the same input can be driven through two different
// allocators in one process.

/// A full init-use-end cycle works with no caller hooks at all.
///
/// The default configuration -- `deflateInit`'s own: level 6 resolved from
/// `Z_DEFAULT_COMPRESSION`, `MAX_WBITS`, `DEF_MEM_LEVEL` and
/// `Z_DEFAULT_STRATEGY` (`deflate.c` L379-L383) -- because this is the one case
/// where the *reference defaults* are the point rather than an expense: it is what
/// every caller who zeroes a `z_stream` and calls `deflateInit` actually gets.
///
/// Ignored under Miri, and only under Miri. The default configuration asks for
/// 262144 bytes of working memory against the compact configuration's 3072, and
/// every byte of the fill is interpreted individually, so this one test would cost
/// more than the whole of the rest of this file. Nothing is lost by the gate:
/// the code path is identical to
/// [`a_round_trip_over_sentinel_filled_memory_recovers_the_input`] and
/// [`the_default_and_injected_paths_produce_identical_output`], neither of which
/// is gated, so under Miri the path is still covered -- only the buffer sizes
/// differ. The `0xa5` cases and the anomaly-detection cases of §3.5 are never
/// gated.
#[test]
#[cfg_attr(miri, ignore)]
fn the_default_path_completes_a_round_trip_with_the_reference_defaults() {
    let config = DeflateConfig::new(6);
    assert_eq!(config.window_bits, 15, "deflateInit uses MAX_WBITS");
    assert_eq!(config.mem_level, 8, "deflateInit uses DEF_MEM_LEVEL");

    let (packed, plain) = round_trip_through(corpus::HELLO, config, GlobalAllocator);

    assert_eq!(
        plain,
        corpus::HELLO,
        "the default allocator did not survive a round trip"
    );
    assert!(packed.len() > 6, "the compressed stream is malformed");
}

/// The default path and the injected path produce identical output.
///
/// Same input, same configuration, two different allocators; the compressed
/// streams must match byte for byte and both must decompress to the original.
/// If they ever diverged, the divergence would have to come from the contents of
/// freshly allocated memory -- there is nothing else the two paths differ in --
/// which is the same defect §3.2 hunts, caught here by a different instrument.
///
/// The two allocators differ in more than identity, which is what makes the
/// comparison worth making: `GlobalAllocator` fills with the sentinel only in
/// debug builds and with zero in release builds, whereas the tracker fills with
/// the sentinel unconditionally. So in a release-profile run the two paths hand
/// the library genuinely different bytes, and the outputs must still agree.
#[test]
fn the_default_and_injected_paths_produce_identical_output() {
    let tracker = TrackingAllocator::new();
    let config = compact_config();

    let (default_packed, default_plain) =
        round_trip_through(corpus::HELLO, config, GlobalAllocator);
    let (injected_packed, injected_plain) = round_trip_through(corpus::HELLO, config, &tracker);

    assert_eq!(
        default_packed, injected_packed,
        "the choice of allocator changed the compressed bytes"
    );
    assert_eq!(
        default_plain, injected_plain,
        "the choice of allocator changed the recovered payload"
    );
    assert_eq!(
        default_plain,
        corpus::HELLO,
        "neither path recovered the input"
    );

    tracker.assert_clean();
}

/// The allocator is invisible across every payload class in the corpus.
///
/// One fixture cannot demonstrate allocator-independence, because a defect that
/// only manifests on, say, incompressible input -- where `deflate_stored` is
/// selected and the pending buffer is used differently -- would slip past a single
/// `"hello, hello!"`. So the comparison is repeated over the whole committed
/// corpus: the empty and one-byte edges, highly repetitive data, incompressible
/// data, text, binary and a payload that crosses the window. The fixture's name
/// travels into the assertion message so a failure says which class broke.
///
/// Under Miri the sweep is reduced rather than skipped: it visits
/// [`CORPUS_CLASSES_UNDER_MIRI`] and truncates each payload to
/// [`CORPUS_PREFIX_UNDER_MIRI`] bytes, which keeps it bounded while still
/// interpreting the two encoder paths no other ungated test in this file reaches.
#[test]
fn the_allocator_is_invisible_across_every_corpus_class() {
    let config = compact_config();
    let classes = corpus_sweep_classes();

    // A silent zero-iteration loop would make this test vacuous, which is exactly
    // the failure mode a configuration-dependent fixture list invites.
    assert!(
        classes.len() >= CORPUS_CLASSES_UNDER_MIRI.len(),
        "the sweep has no classes to visit"
    );

    for (name, payload) in classes {
        let tracker = TrackingAllocator::new();

        let (default_packed, default_plain) = round_trip_through(&payload, config, GlobalAllocator);
        let (injected_packed, injected_plain) = round_trip_through(&payload, config, &tracker);

        assert_eq!(
            default_packed, injected_packed,
            "compressed bytes differ between allocators for corpus class {name}"
        );
        assert_eq!(
            injected_plain, payload,
            "the injected allocator did not recover corpus class {name}"
        );
        assert_eq!(
            default_plain, payload,
            "the default allocator did not recover corpus class {name}"
        );

        tracker.assert_clean();
    }
}

// ---------------------------------------------------------------------------
//  §3.4  Allocation-failure propagation: the `mem_limit` analogue
// ---------------------------------------------------------------------------
//
// `zlib.h` L149 requires `zalloc` to "return Z_NULL if there is not enough memory
// for the object", and every allocating entry point has a `Z_MEM_ERROR` path for
// exactly that. Those paths are the least-travelled code in the library and among
// the most dangerous, because they run partway through construction, when some
// buffers exist and others do not.
//
// `test/infcover.c` reaches them deliberately rather than hoping: `mem_limit`
// (L176-L181) caps the total the zone will hand out, and `mem_alloc` then refuses
// any request that would breach the cap (L79-L80). `mem_limit(&strm, 1)` fails
// everything; `mem_limit(&strm, 0)` restores normal service. Fuzzing reaches these
// paths only by accident; this reaches them on demand, and it is the only way to
// test them at all.
//
// Three properties are asserted, in increasing order of difficulty:
//
//   1. A refused allocation surfaces as `Z_MEM_ERROR` -- a return value, never a
//      panic and never an abort. Every exported entry point of the shipped library
//      is `extern "C"`, so a panic escaping the core would abort a C caller's
//      process rather than unwind; the core therefore returns status codes.
//   2. A refused allocation leaks nothing. This is the hard one: a failure at the
//      second of four buffers must return the first before reporting, which is
//      `deflate.c` L508-L513 falling through to `deflateEnd`'s `TRY_FREE`
//      sequence. The tracker's outstanding-total check after a forced failure is
//      the proof.
//   3. A stream that has reported `Z_MEM_ERROR` is still safely tear-downable.
//
// ★ A note on what the core allocates, because it is not what C allocates ★
//
// C's `inflateInit2_` allocates the state object itself through the caller's hook
// (`ZALLOC(strm, 1, sizeof(inflate_state))`, `inflate.c` L197-L198). This port
// cannot: moving a Rust value into memory a caller's `zalloc` returned is a raw
// pointer write, so that step belongs to `crates/libz-rs-sys`. The core's
// `inflate_init2` therefore allocates *nothing at all*, and the window is
// allocated lazily on first use. Where a budget below depends on the state
// object's footprint being accounted for -- as `test/infcover.c` L428's does --
// this file performs that allocation explicitly through the tracker, exactly as
// the facade would, so the arithmetic behaves as it does in C. Each such case says
// so at the call.

/// A limit that refuses the very first request makes `deflateInit2` report
/// `Z_MEM_ERROR`.
///
/// A ceiling of one byte cannot satisfy any request the library makes -- the
/// smallest is 512 bytes even in the compact configuration -- so the window
/// allocation at `deflate.c` L458 fails and `deflate.c` L508-L513 reports
/// `Z_MEM_ERROR`. This is `mem_limit(&strm, 1)` (`test/infcover.c` L326).
///
/// Nothing was taken, so nothing can have leaked; the tracker is asserted pristine
/// to prove the failure path did not somehow record an allocation it never made.
#[test]
fn a_refused_first_allocation_makes_the_compressor_report_a_memory_error() {
    let tracker = TrackingAllocator::new();
    tracker.set_limit(1);

    let outcome = deflate_init2(compact_config(), &tracker);

    // `Result::unwrap_err` rather than a `match`: a successful initialisation here
    // would mean the limit was not honoured, and the panic names it.
    let code = outcome
        .map(|_| ())
        .expect_err("deflateInit2 succeeded under a one-byte allocation ceiling");
    assert_eq!(
        code,
        ReturnCode::MEM_ERROR,
        "a refused allocation must surface as Z_MEM_ERROR"
    );

    assert_eq!(tracker.total(), 0, "a refused request must record nothing");
    assert_eq!(
        tracker.high_water(),
        0,
        "a refused request must not move the high-water mark"
    );
    tracker.assert_clean();
}

/// A limit that refuses the very first request makes `inflate` report
/// `Z_MEM_ERROR`.
///
/// The decoder's counterpart, and the shape differs because the allocation does.
/// `inflate_init2` takes no memory, so the first request the *core* makes is the
/// lazy window inside `inflate` -- the allocation prefix of `updatewindow`,
/// `inflate.c` L256-L262. This is `test/infcover.c` L416-L423 line for line: a raw
/// `-8` stream, the two input bytes `63 00`, a single byte of output room, and
/// `mem_limit(&strm, 1)` before the call.
///
/// The single byte of output room is what forces the window into existence.
/// `inflate` only calls `updatewindow` when it leaves with output still
/// outstanding (`inflate.c` L1133-L1138); a call that consumes the whole stream
/// moves its output checkpoint forward at L1081 first and so needs no window at
/// all, which is why a complete one-shot decompression never allocates one.
#[test]
fn a_refused_window_allocation_makes_the_decompressor_report_a_memory_error() {
    let tracker = TrackingAllocator::new();

    let mut state = inflate_init2(InflateConfig::new(RAW_WINDOW_BITS), &tracker)
        .expect("inflate_init2 must not allocate, so it must succeed");

    // `mem_limit(&strm, 1)` -- armed after initialisation, exactly as the C
    // harness arms it, so that the failure lands on the window and nothing else.
    tracker.set_limit(1);

    // `strm.next_in = (void *)"\x63"` with `avail_in = 2` is the C string's two
    // bytes including its terminator: a fixed-Huffman block that produces output.
    let input = [0x63_u8, 0x00];
    let mut output = [0_u8; 1];
    let code = {
        let mut stream = InflateStream::new(&input, &mut output);
        stream.apply_reset(inflate_reset(&mut state));
        inflate(&mut state, &mut stream, Flush::NoFlush.as_raw())
    };
    assert_eq!(
        code,
        ReturnCode::MEM_ERROR,
        "the refused window allocation did not surface as Z_MEM_ERROR"
    );

    // Property 3: the stream survives the failure well enough to be torn down.
    // `inflateEnd` is documented to return `Z_OK` here (`inflate.c` L1155-L1165 has
    // no failure path for a live state), and it must not panic or produce a
    // release of anything the tracker did not hand out.
    tracker.set_limit(0);
    assert_eq!(
        inflate_end(&mut state),
        ReturnCode::OK,
        "inflateEnd after Z_MEM_ERROR"
    );

    assert_eq!(tracker.rogue(), 0, "teardown after Z_MEM_ERROR was rogue");
    assert_eq!(tracker.total(), 0, "teardown after Z_MEM_ERROR leaked");
    tracker.assert_clean();
}

/// A limit that permits the first buffer but refuses the second leaks nothing.
///
/// The property that actually matters, and the one no other test in the workspace
/// establishes. `deflateInit2` takes four buffers in a fixed order -- window,
/// `prev`, `head`, pending (`deflate.c` L458-L505) -- and a failure at any of them
/// must return the ones already taken before reporting. C does this by falling
/// through L508-L513 into `deflateEnd`'s `TRY_FREE` sequence; the `TRY_FREE` macro
/// (`zutil.h` L255) exists precisely because a partly built state has null
/// pointers among its live ones.
///
/// The ceiling is set to exactly the window's size, so the window is granted and
/// `prev` -- the very next request, and the same size -- is refused. A leak would
/// show up as a non-zero outstanding total; a high-water mark of exactly one window
/// confirms the failure really did happen at the second buffer rather than the
/// first.
#[test]
fn a_failure_partway_through_initialisation_releases_what_it_already_took() {
    let tracker = TrackingAllocator::new();

    // Exactly `ZALLOC(strm, s->w_size, 2*sizeof(Byte))` and not one byte more.
    tracker.set_limit(COMPACT_WINDOW_BYTES);

    let code = deflate_init2(compact_config(), &tracker)
        .map(|_| ())
        .expect_err("deflateInit2 succeeded with room for only its first buffer");
    assert_eq!(code, ReturnCode::MEM_ERROR, "the status code");

    assert_eq!(
        tracker.high_water(),
        COMPACT_WINDOW_BYTES,
        "the failure did not land on the second buffer: the window should have been granted"
    );
    assert_eq!(
        tracker.total(),
        0,
        "a failed initialisation leaked the buffers it had already taken"
    );
    assert_eq!(tracker.not_lifo(), 0, "the rollback was not in LIFO order");
    assert_eq!(tracker.rogue(), 0, "the rollback released a foreign block");
    tracker.assert_clean();
}

/// Every prefix of the four-buffer allocation sequence rolls back cleanly.
///
/// The previous case pins the second buffer; this one walks the whole sequence,
/// arming the ceiling just below each successive cumulative total so that the
/// failure lands on each of the four `ZALLOC` calls in turn. That is four distinct
/// rollback paths -- one buffer to return, then two, then three -- and each must
/// leave nothing outstanding.
///
/// The final iteration grants all four buffers and therefore succeeds, which is
/// the control: it proves the ceiling arithmetic is describing the real footprint
/// rather than accidentally refusing everything.
#[test]
fn every_prefix_of_the_allocation_sequence_rolls_back_cleanly() {
    // The cumulative totals after each of `deflate.c` L458, L459, L460 and L505,
    // for the compact configuration: window 1024, prev 1024, head 512, pending 512.
    let cumulative = [1024_usize, 2048, 2560, 3072];
    assert_eq!(
        cumulative[cumulative.len() - 1],
        COMPACT_FOOTPRINT,
        "the cumulative totals must add up to the configuration's footprint"
    );

    for (index, &granted) in cumulative.iter().enumerate() {
        let tracker = TrackingAllocator::new();
        // A ceiling of `granted` admits every request up to and including the
        // `index`-th and refuses the next one, if there is a next one.
        tracker.set_limit(granted);

        let outcome = deflate_init2(compact_config(), &tracker);

        if granted == COMPACT_FOOTPRINT {
            // The control: all four buffers fit exactly, so initialisation must
            // succeed and teardown must be clean.
            let mut state = outcome.expect("initialisation with an exact-fit ceiling must succeed");
            assert_eq!(tracker.total(), COMPACT_FOOTPRINT, "exact-fit footprint");
            assert_eq!(deflate_end(&mut state), ReturnCode::OK, "deflateEnd");
        } else {
            let code = outcome
                .map(|_| ())
                .expect_err("initialisation succeeded despite an insufficient ceiling");
            assert_eq!(
                code,
                ReturnCode::MEM_ERROR,
                "failure at buffer {} did not report Z_MEM_ERROR",
                index + 2
            );
            assert_eq!(
                tracker.high_water(),
                granted,
                "failure at buffer {} granted the wrong prefix",
                index + 2
            );
        }

        // True of both branches, which is the point: a rollback and a successful
        // teardown must both end with nothing outstanding.
        assert_eq!(
            tracker.total(),
            0,
            "a ceiling of {granted} bytes left memory outstanding"
        );
        tracker.assert_clean();
    }
}

/// The `test/infcover.c` L428 budget makes `inflateCopy` report `Z_MEM_ERROR`.
///
/// A documented reproduction of one specific assertion in the C coverage harness:
///
/// ```c
/// mem_limit(&strm, (sizeof(struct inflate_state) << 1) + 256);
/// /* ... */
/// ret = inflateCopy(&copy, &strm);            assert(ret == Z_MEM_ERROR);
/// ```
///
/// The budget is computed from `size_of` rather than written down, because the
/// footprint is layout- and target-dependent and a hard-coded byte count would
/// silently stop describing the same situation the moment the state changed shape.
///
/// ★ Why this only works if the state's footprint stays close to C's ★
///
/// The budget is deliberately tight: it grants room for two state objects plus one
/// 256-byte window, and `inflateCopy` needs two states *and two* windows. A port
/// whose state object were much smaller than C's would find the budget generous
/// and the copy would succeed; one whose state object were much larger would find
/// it insufficient earlier, and the failure would land somewhere else. The
/// footprint stays comparable because `src/inflate/state.rs` keeps `lens[320]`,
/// `work[288]` and `codes[1444]` as **inline arrays** -- exactly as `inflate.h`
/// L120-L122 declares them -- with only the window allocated separately. Moving
/// any of those three out into its own allocation would change both the footprint
/// and the number of requests, and this assertion is what would notice.
///
/// ★ Why the two state blocks are allocated by hand ★
///
/// C's `inflateInit2_` and `inflateCopy` each allocate their state object through
/// the caller's hook, so both appear in the zone's total and both count against
/// the ceiling. In this port those two allocations belong to the facade, so they
/// are performed here explicitly -- `allocate_bytes(1, footprint)` is
/// `ZALLOC(strm, 1, sizeof(inflate_state))` -- which makes the tracker's books
/// match C's and the budget behave as C's does. Without them the ceiling would be
/// almost 14 KiB of slack and the copy would succeed.
#[test]
fn the_infcover_copy_budget_forces_a_memory_error() {
    let footprint = inflate_state_footprint();

    // ★ The budget only bites because the state object is large, and it is large
    // because those three arrays are inline -- so that is checked rather than
    // assumed. `lens[320]` and `work[288]` are `unsigned short`, and
    // `codes[ENOUGH]` holds 1444 entries of four bytes, so the three together
    // account for 640 + 576 + 5776 = 6992 bytes; `inflate.h` L80-L81 describes the
    // whole struct as "approximately 7K bytes, not including the allocated sliding
    // window". A port that moved any of the three out into its own allocation would
    // fall below this floor, and the C budget would silently stop describing the
    // same situation.
    let inline_shorts = (LENS_LEN + WORK_LEN) * size_of::<u16>();
    assert!(
        footprint > inline_shorts,
        "the inflate state ({footprint} bytes) is no larger than its own \
         lens[] and work[] arrays ({inline_shorts} bytes)"
    );
    assert!(
        footprint >= 6 * 1024,
        "the inflate state has shrunk to {footprint} bytes, so \
         (sizeof(struct inflate_state) << 1) + 256 no longer reproduces the C \
         budget: the inline arrays have probably moved into their own allocations"
    );

    let tracker = TrackingAllocator::new();

    // `inflateInit2`'s own `ZALLOC(strm, 1, sizeof(inflate_state))`, which this
    // port's facade owns.
    let state_block = tracker
        .allocate_bytes(1, footprint)
        .expect("the state block must fit before any ceiling is armed");
    let mut state = inflate_init2(InflateConfig::new(RAW_WINDOW_BITS), &tracker)
        .expect("inflate_init2 with a raw 256-byte window");

    // `test/infcover.c` L426-L427: setting a dictionary on the raw stream is what
    // brings the 256-byte window into existence, and it must succeed.
    check_err(
        inflate_set_dictionary(&mut state, corpus::DICTIONARY),
        "inflateSetDictionary on the raw stream",
    );
    assert_eq!(
        tracker.total(),
        footprint + RAW_WINDOW_BYTES,
        "the source stream should hold exactly one state block and one window"
    );

    // `mem_limit(&strm, (sizeof(struct inflate_state) << 1) + 256)`.
    let budget = (footprint << 1) + RAW_WINDOW_BYTES;
    tracker.set_limit(budget);

    // `inflateCopy`'s own state allocation (`inflate.c` L1340-L1342). It fits
    // exactly: the outstanding total becomes the budget, and the ceiling refuses
    // only a request that would take the total *above* it (`test/infcover.c` L79).
    let copy_block = tracker
        .allocate_bytes(1, footprint)
        .expect("the copy's state block fits the budget exactly");
    assert_eq!(
        tracker.total(),
        budget,
        "the budget is now exhausted exactly"
    );

    // And so the copy's window -- 256 bytes it has no room for -- is refused.
    let code = inflate_copy(&state, &tracker)
        .map(|_| ())
        .expect_err("inflateCopy succeeded under the infcover budget");
    assert_eq!(
        code,
        ReturnCode::MEM_ERROR,
        "inflateCopy under the infcover budget must report Z_MEM_ERROR"
    );

    // The source is documented to survive untouched (`inflate.c` L1344-L1346 only
    // ever writes to the destination), which is what lets the C harness keep using
    // it afterwards -- and what lets this test tear it down normally.
    assert_eq!(
        tracker.total(),
        budget,
        "the failed copy disturbed the outstanding total"
    );

    // Teardown in strict reverse order of allocation, which is what keeps the
    // tracker's LIFO verdict clean: the copy's block, then the window, then the
    // source's block.
    tracker.set_limit(0);
    tracker.deallocate_bytes(&mut Some(copy_block));
    assert_eq!(
        inflate_end(&mut state),
        ReturnCode::OK,
        "inflateEnd on the source"
    );
    tracker.deallocate_bytes(&mut Some(state_block));

    assert_eq!(tracker.rogue(), 0, "a release was not recognised");
    assert_eq!(tracker.not_lifo(), 0, "a release was not LIFO");
    tracker.assert_clean();
}

/// A ceiling that admits the state block but nothing after it still tears down
/// safely.
///
/// The literal reading of "permits the state allocation but fails a later one":
/// the ceiling is armed *before* anything is allocated and set to exactly one
/// state object, so the facade's `ZALLOC(strm, 1, sizeof(inflate_state))` is
/// granted and the lazily allocated window is refused. The stream then reports
/// `Z_MEM_ERROR`, and the two things that must follow are checked: teardown does
/// not panic, and nothing is left outstanding.
#[test]
fn a_ceiling_that_admits_only_the_state_block_fails_the_lazy_window() {
    let footprint = inflate_state_footprint();
    let tracker = TrackingAllocator::new();
    tracker.set_limit(footprint);

    let state_block = tracker
        .allocate_bytes(1, footprint)
        .expect("the state block fits the ceiling exactly");
    let mut state = inflate_init2(InflateConfig::new(RAW_WINDOW_BITS), &tracker)
        .expect("inflate_init2 allocates nothing, so the ceiling cannot stop it");

    // The window is the next request, and there is no room for it.
    let code = inflate_set_dictionary(&mut state, corpus::DICTIONARY);
    assert_eq!(
        code,
        ReturnCode::MEM_ERROR,
        "the refused window must surface as Z_MEM_ERROR"
    );
    assert_eq!(
        tracker.total(),
        footprint,
        "the refused window recorded an allocation"
    );

    // Teardown after the failure: no panic, no rogue release, nothing left.
    assert_eq!(
        inflate_end(&mut state),
        ReturnCode::OK,
        "inflateEnd after a refused window"
    );
    tracker.deallocate_bytes(&mut Some(state_block));

    assert_eq!(tracker.total(), 0, "teardown after Z_MEM_ERROR leaked");
    assert_eq!(tracker.rogue(), 0, "teardown after Z_MEM_ERROR was rogue");
    tracker.assert_clean();
}

/// Lifting the ceiling restores normal service.
///
/// `mem_limit(&strm, 0)` means *no limit*, not *no allocation*
/// (`test/infcover.c` L176-L181), and the C harness relies on being able to arm
/// and disarm the ceiling around a single call -- L326-L332 does exactly that:
/// `mem_limit(&strm, 1)`, a call that must report `Z_MEM_ERROR`,
/// `mem_limit(&strm, 0)`, and then the same call again, which must report `Z_OK`.
///
/// So a refusal must leave no lasting mark on the allocator. After disarming, a
/// full round trip must work, and the high-water mark must show one compressor's
/// footprint rather than two -- the refused attempt took nothing, so it cannot
/// have raised the peak.
#[test]
fn lifting_the_ceiling_restores_normal_service() {
    let tracker = TrackingAllocator::new();

    tracker.set_limit(1);
    let refused = deflate_init2(compact_config(), &tracker)
        .map(|_| ())
        .expect_err("the armed ceiling did not refuse");
    assert_eq!(refused, ReturnCode::MEM_ERROR, "the refusal's status code");

    // `mem_limit(&strm, 0)`.
    tracker.set_limit(0);

    let (packed, plain) = round_trip_through(corpus::HELLO, compact_config(), &tracker);
    assert_eq!(plain, corpus::HELLO, "the round trip after disarming");
    assert!(packed.len() > 6, "the compressed stream is malformed");

    assert_eq!(
        tracker.high_water(),
        COMPACT_FOOTPRINT,
        "the peak should reflect one live compressor at a time"
    );
    tracker.assert_clean();
}

// ---------------------------------------------------------------------------
//  §3.5  The anomaly detectors themselves
// ---------------------------------------------------------------------------
//
// Every assertion in §3.1 through §3.4 is an assertion about a counter. A tracker
// that stopped incrementing its counters would turn all of them green and prove
// nothing whatsoever -- a silent, total loss of coverage with no visible symptom.
// So the four detectors are exercised here directly, without the library in the
// way, and each is shown to fire on the situation it exists for and to stay quiet
// otherwise.
//
// These four cases are the reason this file is worth running under Miri at all,
// and none of them is ever gated: each allocates a handful of small blocks and
// terminates immediately.
//
// `mem_free` (`test/infcover.c` L112-L154) is the oracle for all four. It walks
// its list from the most recent allocation backwards, and the three outcomes are:
// the block is the most recent, which is a clean last-in-first-out release and
// touches no counter (L127-L128); the block is present but not most recent, which
// increments `notlifo` (L136); or the block is absent, which increments `rogue`
// (L149-L150). The fourth detector is `mem_done`'s leftover scan (L210-L222).

/// Releasing two blocks newest-first is clean.
///
/// The negative control for the ordering detector, and the situation the library
/// is required to produce: `deflateEnd` releases "in reverse order of
/// allocations" (`deflate.c` L1300-L1306). If this case reported an anomaly, the
/// detector would be inverted and every §3.1 assertion would be meaningless.
#[test]
fn releasing_two_blocks_in_reverse_order_reports_no_anomaly() {
    let tracker = TrackingAllocator::new();

    let first = tracker.allocate_bytes(1, 16).expect("first allocation");
    let second = tracker.allocate_bytes(1, 32).expect("second allocation");
    assert_eq!(tracker.total(), 48, "both blocks are outstanding");

    // Newest first: `second` is the head of the list `mem_free` walks.
    tracker.deallocate_bytes(&mut Some(second));
    tracker.deallocate_bytes(&mut Some(first));

    assert_eq!(tracker.not_lifo(), 0, "a LIFO release was called non-LIFO");
    assert_eq!(tracker.rogue(), 0, "a known block was called unrecognised");
    assert_eq!(tracker.total(), 0, "both blocks were released");
    tracker.assert_clean();
}

/// Releasing two blocks oldest-first is reported as one non-LIFO release.
///
/// The positive control. The count is asserted to be exactly one, not merely
/// non-zero: the *older* block is released while a newer one is still live, which
/// is the single out-of-order event; the release that follows is then of the only
/// remaining block and is in order. A detector that reported two would be counting
/// releases rather than out-of-order ones.
///
/// This is the mistake a Rust port is most likely to make, and it is invisible
/// without this counter. Rust drops the fields of a struct in *declaration* order,
/// so a state type that listed its buffers in allocation order would release them
/// in exactly the order this test flags.
#[test]
fn releasing_two_blocks_in_allocation_order_is_reported_as_not_lifo() {
    let tracker = TrackingAllocator::new();

    let first = tracker.allocate_bytes(1, 16).expect("first allocation");
    let second = tracker.allocate_bytes(1, 32).expect("second allocation");

    // Oldest first: `first` is behind `second` in the list, which is C's L136 path.
    tracker.deallocate_bytes(&mut Some(first));
    assert_eq!(
        tracker.not_lifo(),
        1,
        "releasing the older block first was not flagged"
    );

    // The second release is of the only remaining block, so it is in order.
    tracker.deallocate_bytes(&mut Some(second));
    assert_eq!(
        tracker.not_lifo(),
        1,
        "the in-order release that followed was also flagged"
    );

    // Both blocks really were released -- the anomaly is about order, not loss.
    assert_eq!(tracker.total(), 0, "an out-of-order release lost a block");
    assert_eq!(
        tracker.rogue(),
        0,
        "an out-of-order release was called rogue"
    );

    let report = tracker.finish();
    assert_eq!(report.not_lifo, 1, "the report disagrees with the counter");
    assert!(
        !report.is_clean(),
        "a non-LIFO release must not read as clean"
    );
}

/// Releasing a block the tracker never handed out is reported as rogue.
///
/// `mem_free` L149-L150: a block absent from the list increments `rogue`. In C a
/// rogue free is a heap-corruption symptom -- it means `free` was called on an
/// address the zone did not own. Here it can only ever be an accounting
/// observation, because a block is released through its own owner and safe Rust
/// cannot free memory twice; but the accounting is exactly what proves the port
/// never crosses allocators, which is what §3.1 asserts.
///
/// The foreign block comes from the crate's own default allocator, so it is
/// genuinely an address this tracker never produced.
#[test]
fn releasing_a_block_the_tracker_never_produced_is_reported_as_rogue() {
    let tracker = TrackingAllocator::new();
    assert_eq!(tracker.rogue(), 0, "a fresh tracker has no history");

    // A block from somewhere else entirely.
    let foreign = GlobalAllocator
        .allocate_bytes(1, 24)
        .expect("the default allocator must satisfy a 24-byte request");

    tracker.deallocate_bytes(&mut Some(foreign));

    assert_eq!(
        tracker.rogue(),
        1,
        "a release of a block the tracker never produced was not flagged"
    );
    assert_eq!(
        tracker.total(),
        0,
        "a rogue release must not perturb the outstanding total"
    );
    assert_eq!(
        tracker.not_lifo(),
        0,
        "a rogue release was also called non-LIFO"
    );

    let report = tracker.finish();
    assert_eq!(report.rogue, 1, "the report disagrees with the counter");
    assert!(!report.is_clean(), "a rogue release must not read as clean");
}

/// A block that is never released is reported as outstanding.
///
/// `mem_done`'s leftover scan (`test/infcover.c` L210-L222), which prints
/// "%zu bytes in %d blocks not freed". The block is dropped rather than released,
/// which returns its memory to Rust but leaves the tracker's books showing it
/// outstanding -- and that accounting discrepancy is exactly the leak the C
/// harness reports.
///
/// Both the byte count and the block count are asserted, because those are the two
/// numbers `mem_done` prints, and the report is checked to be un-clean so that the
/// `assert_clean` used throughout §3.1-§3.4 is known to be capable of failing.
#[test]
fn a_block_that_is_never_released_is_reported_as_outstanding() {
    let tracker = TrackingAllocator::new();

    // Two blocks taken, one returned.
    let kept = tracker.allocate_bytes(1, 40).expect("first allocation");
    let returned = tracker.allocate_bytes(1, 8).expect("second allocation");
    tracker.deallocate_bytes(&mut Some(returned));

    assert_eq!(tracker.total(), 40, "the kept block is not outstanding");

    // Dropped, not released: the memory goes back to Rust, the accounting does not.
    drop(kept);
    assert_eq!(
        tracker.total(),
        40,
        "dropping a block without releasing it cleared the accounting"
    );

    let report = tracker.finish();
    assert_eq!(report.leaked_bytes, 40, "the reported byte count");
    assert_eq!(report.leaked_blocks, 1, "the reported block count");
    assert_eq!(report.not_lifo, 0, "a leak was also called non-LIFO");
    assert_eq!(report.rogue, 0, "a leak was also called rogue");
    assert!(!report.is_clean(), "a leak must not read as clean");

    // `finish` ends tracking, so a second call reports no further leak -- which is
    // what makes it safe to call `assert_clean` once at the end of a test.
    let after = tracker.finish();
    assert_eq!(after.leaked_bytes, 0, "tracking did not end");
    assert_eq!(after.leaked_blocks, 0, "tracking did not end");
    assert_eq!(
        after.high_water, report.high_water,
        "the peak must survive the end of tracking"
    );
}
