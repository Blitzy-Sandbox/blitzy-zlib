//! `fuzz_inflate_back` -- libFuzzer coverage of `inflateBack`, the callback-driven
//! decompression entry point.
//!
//! # Derivation
//!
//! Ported from `../../infback.c` -- `inflateBackInit_` at its L25-L64, the `PULL()`
//! and `ROOM()` macros at its L99-L162, `inflateBack` at its L191-L570 and
//! `inflateBackEnd` at its L572-L579 -- and shaped after `cover_back()` at
//! `../../test/infcover.c` L471-L505, whose `pull` (its L447) and `push` (its L463)
//! are the model for the two callbacks here: descriptor-driven, deterministic,
//! allocation-free, and failing on demand.
//!
//! The published contract this target holds the implementation to is `zlib.h`
//! L1138-L1204.
//!
//! # The two non-negotiables
//!
//! 1. **The window is exactly `1 << windowBits` bytes.** `inflateBackInit_`
//!    *borrows* the caller's buffer -- it stores the pointer and sets
//!    `wsize = 1U << windowBits` without allocating or copying (`infback.c`
//!    L58-L59). It is the only entry point in the library with that property, and it
//!    makes window sizing a memory-safety obligation of the **caller**. Both the
//!    allocation extent and the `windowBits` argument are therefore derived from one
//!    variable, in [`window_extent`] and its single call site, so they cannot drift
//!    apart. A window one byte short is a heap buffer overflow, not a test failure.
//! 2. **The callbacks are panic-free.** The `extern "C"` shims the library actually
//!    invokes live in `zlib_rs_differential::port`, which is where the unwinding
//!    obligation is discharged: both are `extern "C"` and never `extern "C-unwind"`,
//!    so a panic would abort at the boundary rather than unwind into a frame with no
//!    unwind tables. This file supplies only the safe bodies behind them --
//!    [`BackSource`] for `in()` and [`BackSink`] for `out()` -- and they are written
//!    so that no panic is reachable at all: no `unwrap`, no `expect`, no `assert`, no
//!    fallible indexing, no allocation, no formatting, and no arithmetic that can
//!    overflow. Everything they need is computed *before* `inflateBack` is entered and
//!    parked in [`InSide`] and [`OutSide`]; anything they observe is recorded there
//!    and asserted afterwards, from the `fuzz_target!` body, where `assert!` is the
//!    intended crash-reporting mechanism.
//!
//! # The polarities are inverted, and that is the point
//!
//! `infback.c` L182-L183: "in() should return zero on failure. out() should return
//! non-zero on failure." Writing both callbacks to one convention is the single most
//! likely bug in a harness like this one, which is why the boundary's traits normalise
//! it once: [`BackSource::next_chunk`] answers [`None`] to refuse and [`BackSink`]
//! answers `false`, and the two `extern "C"` shims behind them are the only place the
//! inverted C conventions appear. Each trait documents the polarity it replaces.
//!
//! # Why this terminates
//!
//! A callback that can be asked for input forever is a guaranteed libFuzzer hang, so
//! termination here is structural rather than hoped for. Every successful `in()`
//! advances the payload cursor by at least one byte and the payload is truncated to
//! [`MAX_PAYLOAD_LEN`], so `in()` refuses -- which `PULL()` turns into `Z_BUF_ERROR`
//! -- after a bounded number of calls. [`MAX_IN_CALLS`], [`MAX_OUT_CALLS`] and
//! [`MAX_TOTAL_OUT`] bound it again from the other side, and bound the work a
//! compression bomb can extract from a few hundred input bytes.
//!
//! # What this target deliberately does not do
//!
//! * It never reaches into the decoder's state to force an otherwise unreachable
//!   mode. `infcover.c`'s `pull` does exactly that (its L457-L459), and the
//!   unmodified `test/infcover.c` relinked against this library still covers it;
//!   replicating it here would require this file to know the internal state's layout.
//! * It never mutates the payload or the window from inside a callback. `zlib.h`
//!   L1174-L1175 forbids both, and [`OutSide::write_out`] receives a shared slice, so
//!   the language forbids it too.
//! * It does not expect zlib- or gzip-wrapped data to decode. `inflateBack` is raw
//!   DEFLATE only (`zlib.h` L1155-L1162); wrapped input is a legitimate
//!   `Z_DATA_ERROR`.
//!
//! # Running it
//!
//! ```text
//! cd fuzz
//! cargo +nightly fuzz run fuzz_inflate_back -- -max_total_time=300
//! ```
//!
//! `+nightly` is required because `../rust-toolchain.toml` pins the stable channel
//! and cargo-fuzz's sanitizer instrumentation is nightly-only. The 300-second budget
//! matches `fuzz-seconds: 300` in `../.github/workflows/fuzz.yml`. AddressSanitizer
//! is on by default, and this is the target where it earns the most: the boundary's
//! `inflateBack` bridge is the densest concentration of raw pointers anywhere in the
//! harness, and this is what exercises it.

#![no_main]
#![forbid(unsafe_code)]

use core::ffi::c_int;
use core::ptr;

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;

// `libz_rs_sys`, never `z`, and only for its types and constants -- every entry
// point is reached through `zlib_rs_differential::port`. The facade sets
// `[lib] name = "z"` so that the build emits a drop-in `libz.so`, and Cargo derives
// an `--extern` name from the lib target rather than the package, so
// `fuzz/Cargo.toml` applies a `package = "libz-rs-sys"` rename to restore this path.
// Guessing `z` does not compile.
//
// `Z_NULL` is deliberately absent from this list even though it is re-exported. It
// is C's single spelling for a null of any kind, and every place this target needs
// one -- a null stream, a null window, a null version -- has a gate of its own
// precisely because a safe signature cannot express it. Every constant below is
// consumed from the library rather than redeclared, so this file cannot drift from
// `zlib.h`.
use libz_rs_sys::{
    uInt, Bytef, Z_BUF_ERROR, Z_DATA_ERROR, Z_MEM_ERROR, Z_OK, Z_STREAM_END, Z_STREAM_ERROR,
    Z_VERSION_ERROR,
};
use zlib_rs_differential::port::{self, BackSink, BackSource, TrackingAllocator, TrackingReport};
use zlib_rs_fuzz::reached;

// ---------------------------------------------------------------------------
//  Bounds
// ---------------------------------------------------------------------------

/// Longest payload fed to the decoder, in bytes.
///
/// Caps the work one input can demand and, together with the one-byte-minimum
/// progress `in()` makes on every successful call, is half of the termination
/// argument in the module documentation.
const MAX_PAYLOAD_LEN: usize = 4_096;

/// Largest window `inflateBackInit_` accepts: `1 << 15`, the `windowBits == 15`
/// case, and the size `cover_back`'s `unsigned char win[32768]` uses.
///
/// Used only for the out-of-range `windowBits` probe, where no valid extent exists
/// to derive one from.
const MAX_WINDOW_LEN: usize = 32_768;

/// Hard ceiling on `in()` invocations per `inflateBack` call.
///
/// Deliberately smaller than `MAX_PAYLOAD_LEN + 1` so that the cap is genuinely
/// reachable -- a maximum-length payload handed out one byte at a time hits it --
/// rather than being unreachable decoration.
const MAX_IN_CALLS: u32 = 1_024;

/// Hard ceiling on `out()` invocations per `inflateBack` call.
const MAX_OUT_CALLS: u32 = 256;

/// Hard ceiling on bytes accepted from `out()` per `inflateBack` call.
///
/// Bounds what a compression bomb can extract: a highly repetitive payload of a
/// few hundred bytes can otherwise expand by orders of magnitude, and every byte
/// of it is read by [`OutSide::write_out`].
const MAX_TOTAL_OUT: usize = 262_144;

/// Sentinel for "this call index never fails", stored in
/// [`InSide::fail_in_at`]/[`OutSide::fail_out_at`].
///
/// Call indices are one-based and capped far below `u32::MAX`, so this value can
/// never compare equal to a real one.
const NEVER: u32 = u32::MAX;

// ---------------------------------------------------------------------------
//  Contract violations the callbacks record for the caller to assert
// ---------------------------------------------------------------------------

/// `out()` was called with a zero length.
///
/// `infback.c` calls it only from `ROOM()` with the whole window (its L157) and from
/// `inf_leave` with `wsize - left` under `left < wsize` (its L562-L563), so the
/// length is always positive.
const V_OUT_EMPTY: u32 = 1;

/// `out()` was called with a length exceeding the window.
///
/// `zlib.h` L1177-L1178: "The length written by out() will be at most the window
/// size."
const V_OUT_TOO_LONG: u32 = 1 << 1;

/// `out()` was handed a region that is not inside the window the caller supplied.
///
/// `zlib.h` L1175-L1177 makes the window "also the buffer that out() uses to write
/// from", so the reported region must lie within it.
const V_OUT_OUTSIDE_WINDOW: u32 = 1 << 2;

/// `in()` was called with an unusable output parameter, which it cannot satisfy.
///
/// The allocator's own faults -- a leak, a free out of order, a free of an address the
/// zone never handed out -- are not flags here: [`TrackingReport`] counts each of them
/// separately, and [`assert_session_invariants`] asserts on those counts directly.
const V_IN_BUF_NULL: u32 = 1 << 3;

/// The byte the window is pre-filled with at `index`, so that a stray write is
/// visible afterwards.
///
/// Position-dependent on purpose: a uniform fill would be reproduced by any
/// window-internal copy of an equal byte, whereas this pattern is not. `0xa5` is the
/// value `test/infcover.c`'s instrumented allocator fills with, for the same reason.
fn sentinel(index: usize) -> u8 {
    (index as u8) ^ 0xa5
}

// ---------------------------------------------------------------------------
//  The two callback bodies, as the boundary's traits
// ---------------------------------------------------------------------------
//
// C threads a single `void *desc` to each callback and `test/infcover.c` uses one
// object for both. The boundary's bridge takes the two halves separately -- a
// `BackSource` for `in()` and a `BackSink` for `out()` -- so they are two structs
// here, which turns out to be the better shape: neither half can read the other's
// ledger, so a cross-check written by mistake against the wrong side does not compile,
// and each carries only the violations its own callback can record.
//
// Everything either one needs is computed before `inflateBack` is entered. That is
// what makes their panic-freedom structural rather than a matter of inspection: they
// compute nothing that could fail, allocate nothing, format nothing, index nothing
// fallibly, and use saturating arithmetic throughout. The `extern "C"` shims that call
// into them are `port::pull` and `port::push`, which never unwind.

/// The `in()` half: hands out the payload in `chunk`-sized runs.
///
/// ★ **Polarity.** C's `in()` returns a *count*, so zero is failure. The trait states it
/// as [`None`], and `PULL()` (`infback.c` L101-L108) turns that into
/// `next = Z_NULL; ret = Z_BUF_ERROR`.
///
/// Refuses for four reasons, all of them that same signal: the [`MAX_IN_CALLS`] cap, the
/// fuzzer's chosen refusal point, an exhausted payload, and [`InSide::starved`].
struct InSide<'i> {
    /// The payload `in()` hands out. Borrowed from the fuzzer for the whole session, which
    /// is what `'i` records -- `infback.c` L172-L174 requires the run handed over to stay
    /// valid until `in()` is next called, and a borrow that outlives the whole call is a
    /// stronger promise than that.
    payload: &'i [u8],

    /// Bytes staged directly in `z_stream::next_in` before each call -- the convenience
    /// path of `zlib.h` L1181-L1188. Zero leaves `next_in` null, which makes `in()` be
    /// called immediately.
    ///
    /// Also where [`InSide::cursor`] resumes from, so the payload is consumed exactly once
    /// whichever route it arrives by.
    staged: usize,

    /// Offset of the next byte `in()` will hand out.
    cursor: usize,

    /// Bytes `in()` offers per call, at least one. Small values exercise the bit reader's
    /// chunk boundaries; large ones the single-shot path.
    chunk: usize,

    /// One-based `in()` call index at which to refuse, or [`NEVER`].
    fail_in_at: u32,

    /// `in()` invocations so far, capped at [`MAX_IN_CALLS`].
    in_calls: u32,

    /// Whether `in()` has refused, which is C's `have == 0` failure.
    in_refused: bool,

    /// Refuse every call, so the decoder has only the staged bytes.
    ///
    /// This is `cover_back` L489's `inflateBack(&strm, pull, Z_NULL, push, Z_NULL)`, in the
    /// only form the safe bridge admits. A null `in_desc` cannot be spelled through it --
    /// the bridge installs its own descriptor -- and no library coverage is lost by that,
    /// because `in_desc` is opaque to the library and only ever handed back to the
    /// callback. What is gained is that the input ledger exists even in this case, so the
    /// cross-checks in [`check_outcome`] apply to it too, where the raw form had to skip
    /// them.
    starved: bool,

    /// Bitwise OR of the `V_*` flags this side can record.
    violations: u32,
}

impl InSide<'_> {
    /// Clears everything that is per-`inflateBack`-call, leaving the sticky observations
    /// -- the violations -- alone.
    ///
    /// The cursor resumes at [`InSide::staged`], which is the number of bytes the caller
    /// staged directly in `next_in`.
    fn rewind(&mut self) {
        self.cursor = self.staged;
        self.in_calls = 0;
        self.in_refused = false;
    }
}

impl<'i> BackSource<'i> for InSide<'i> {
    fn next_chunk(&mut self) -> Option<&'i [u8]> {
        // Anti-hang, checked before the counter moves so the counter stays inside its
        // documented bound. A callback that can be asked for input forever is a
        // guaranteed libFuzzer timeout.
        if self.in_calls >= MAX_IN_CALLS {
            self.in_refused = true;
            return None;
        }
        self.in_calls = self.in_calls.saturating_add(1);

        if self.starved || self.in_calls == self.fail_in_at {
            self.in_refused = true;
            return None;
        }

        // `payload` is copied out of `self` first so the run handed back carries the
        // payload's own `'i` rather than the lifetime of this borrow of `self`.
        let payload: &'i [u8] = self.payload;
        // `saturating_sub` cannot panic and is exact: `cursor` never passes the length.
        let remaining = payload.len().saturating_sub(self.cursor);
        let take = self.chunk.min(remaining);
        // `zlib.h` L1178-L1179: "Any non-zero amount of input may be provided by in()", so
        // a zero count is only ever the failure signal.
        if take == 0 {
            self.in_refused = true;
            return None;
        }
        let at = self.cursor;
        self.cursor = self.cursor.saturating_add(take);
        // `get` rather than an index: the bound holds by the arithmetic above, and a
        // fallible form is what keeps this body provably panic-free. The `None` arm is
        // unreachable for that reason, and records a refusal rather than assuming so.
        let chunk = payload.get(at..at + take);
        if chunk.is_none() {
            self.in_refused = true;
        }
        chunk
    }

    fn note_unusable_out_parameter(&mut self) {
        self.violations |= V_IN_BUF_NULL;
        self.in_refused = true;
    }
}

/// The `out()` half: reads every reported region and folds it.
///
/// ★ **Polarity, and it is inverted.** C's `out()` returns a *status*, so non-zero is
/// failure -- the opposite of `in()`. The trait states it as `false` to refuse.
///
/// Reading the whole reported region is the point: it proves the extent the library
/// reported is genuinely readable, and under AddressSanitizer a region that runs past the
/// window is caught here rather than corrupting something later. It never *writes* --
/// `zlib.h` L1175-L1177 forbids a callback from changing the window, and a shared slice
/// makes that a compile-time fact.
struct OutSide {
    /// Window length in bytes: exactly `1 << windowBits`.
    window_len: usize,

    /// One-based `out()` call index at which to refuse, or [`NEVER`].
    fail_out_at: u32,

    /// `out()` invocations so far, capped at [`MAX_OUT_CALLS`].
    out_calls: u32,

    /// Whether `out()` has refused.
    out_refused: bool,

    /// Bytes accepted from `out()`, capped at [`MAX_TOTAL_OUT`].
    total_out: usize,

    /// Highest window offset any `out()` call has reported as written.
    ///
    /// The decoder writes only what it then reports, so the window beyond this is still
    /// untouched and must still hold [`sentinel`].
    max_out_end: usize,

    /// Bytes this side has actually read, including on refused calls.
    folded: usize,

    /// Rolling fold over those bytes, so the reads cannot be optimised away.
    fold: u64,

    /// Bitwise OR of the `V_*` flags this side can record.
    violations: u32,
}

impl OutSide {
    /// Clears everything that is per-`inflateBack`-call, leaving the sticky observations
    /// -- the violations and the write high-water mark -- alone.
    fn rewind(&mut self) {
        self.out_calls = 0;
        self.out_refused = false;
        self.total_out = 0;
        self.folded = 0;
        self.fold = 0;
    }
}

impl BackSink for OutSide {
    fn write_out(&mut self, data: &[u8], offset: Option<usize>) -> bool {
        if self.out_calls >= MAX_OUT_CALLS {
            self.out_refused = true;
            return false;
        }
        self.out_calls = self.out_calls.saturating_add(1);

        // Record, never assert: an assertion here would panic across the FFI edge. The
        // flags are read from the `fuzz_target!` body once the session has ended.
        let count = data.len();
        if count == 0 {
            self.violations |= V_OUT_EMPTY;
        }
        if count > self.window_len {
            self.violations |= V_OUT_TOO_LONG;
        }
        match offset {
            Some(start) => {
                let end = start.saturating_add(count);
                self.max_out_end = self.max_out_end.max(end);
            }
            None => self.violations |= V_OUT_OUTSIDE_WINDOW,
        }

        // Read before deciding, so that even a refused call validates the extent it was
        // handed. Bounded: the caps below refuse on the first call that would exceed
        // `MAX_TOTAL_OUT`, so at most that plus one window is ever read.
        let mut fold = self.fold;
        for byte in data {
            fold = fold.rotate_left(7) ^ u64::from(*byte);
        }
        self.fold = fold;
        self.folded = self.folded.saturating_add(count);

        if self.total_out.saturating_add(count) > MAX_TOTAL_OUT
            || self.out_calls == self.fail_out_at
        {
            self.out_refused = true;
            return false;
        }
        self.total_out = self.total_out.saturating_add(count);
        true
    }
}

// ---------------------------------------------------------------------------
//  Structured input
// ---------------------------------------------------------------------------

/// Which callback the fuzzer wants to fail, and on which call.
///
/// Recorded as a call index rather than a flag so that failures land in the middle of
/// a stream as well as at its start: the interesting cases are an `out()` refusal
/// after several window flushes, and an `in()` refusal part-way through a Huffman
/// table.
#[derive(Arbitrary, Debug)]
enum FailureMode {
    /// Both callbacks always succeed, so only the payload decides the outcome.
    Never,
    /// Refuse the `n + 1`-th `in()` call.
    RefuseInput(u8),
    /// Refuse the `n + 1`-th `out()` call.
    RefuseOutput(u8),
}

/// One fuzzer-chosen configuration of the `inflateBack` session.
///
/// `payload` is last so that the derived `arbitrary_take_rest` -- which is what
/// `fuzz_target!` calls for a typed input -- gives it every remaining byte, keeping
/// the corpus mutation-friendly instead of spending a length prefix on it.
#[derive(Arbitrary, Debug)]
struct BackInput<'a> {
    /// Selector narrowed by [`select_window_bits`].
    window_bits: u8,
    /// Selector narrowed by [`select_chunk`] into the bytes `in()` offers per call.
    chunk: u8,
    /// Zero stages nothing; otherwise `n - 1` payload bytes are staged directly in
    /// `next_in`, which is the convenience path `zlib.h` L1181-L1188 describes.
    ///
    /// A zero-length stage collapses to "nothing staged", and that loses no coverage:
    /// `infback.c` L219 is `have = next != Z_NULL ? strm->avail_in : 0`, so a non-null
    /// `next_in` with `avail_in == 0` and a null `next_in` reach the decoder as exactly
    /// the same state -- nothing available, so `in()` is called immediately.
    staged: u8,
    /// Which callback refuses, and when.
    failure: FailureMode,
    /// Make `in()` always answer "no input", so the decoder has only the staged bytes --
    /// `cover_back` L489's `inflateBack(&strm, pull, Z_NULL, push, Z_NULL)`. See
    /// [`InSide::starved`] for why that is the shape it takes here.
    starve_input: bool,
    /// Install a [`TrackingAllocator`]'s hooks instead of leaving them null and letting
    /// the library substitute its own (`infback.c` L37-L50).
    tracked_allocator: bool,
    /// Run `inflateBack` twice on one state, which `zlib.h` L1150-L1152 permits, and
    /// require the second answer to match the first.
    reuse_state: bool,
    /// Raw DEFLATE input, borrowed from the fuzzer's own buffer and truncated to
    /// [`MAX_PAYLOAD_LEN`] in [`drive`].
    ///
    /// ★ **A borrowed slice, not a `Vec<u8>`, and the difference is not cosmetic.**
    /// Two properties of `arbitrary` 1.4.2 make the owned form actively harmful here,
    /// and both were measured on this target rather than reasoned about:
    ///
    /// * `Vec<u8>` is drawn through `ArbitraryTakeRestIter`, which spends a
    ///   *continuation byte per element*, so the length is geometric with p = 1/2 --
    ///   twenty-four bytes of input produced an **empty** payload, and a DEFLATE
    ///   stream long enough to get past a block header was effectively unreachable.
    /// * Sidestepping that with `#[arbitrary(with = ...)]` fixes the draw but wrecks
    ///   `size_hint`: the derive reports `size_of::<Vec<u8>>()`, i.e. **24**, as the
    ///   field's lower bound, and `fuzz_target!` rejects any input shorter than the
    ///   struct's lower bound outright -- so a 12-byte input was never executed at
    ///   all.
    ///
    /// `&[u8]` has neither problem: its `arbitrary_take_rest` is the verbatim tail and
    /// its `size_hint` lower bound is zero, which puts the struct's at ten. Verified:
    /// `size_hint(0) == (10, None)`, and a 12-byte input yields a two-byte payload.
    payload: &'a [u8],
}

/// `windowBits` values outside the 8..=15 `inflateBackInit_` accepts.
///
/// `c_int::MIN` is included because the naive implementation of the bound is a shift,
/// and a shift by a negative or huge exponent is exactly what a bounds check has to
/// happen before.
const OUT_OF_RANGE_WINDOW_BITS: [c_int; 6] = [-1, 0, 7, 16, 100, c_int::MIN];

/// Narrows a selector to a `windowBits` argument.
///
/// Roughly 94% of the space lands in the accepted 8..=15, because that is where the
/// decoder actually runs; the remaining slice probes the rejection ladder, which is
/// observable behaviour in its own right (`infback.c` L33-L35).
fn select_window_bits(selector: u8) -> c_int {
    if selector < 240 {
        8 + c_int::from(selector % 8)
    } else {
        let choice = usize::from(selector.wrapping_sub(240)) % OUT_OF_RANGE_WINDOW_BITS.len();
        OUT_OF_RANGE_WINDOW_BITS[choice]
    }
}

/// Narrows a selector to the number of bytes `in()` offers per call, at least one.
///
/// Tiered rather than uniform: a quarter of the space is single-byte chunks, which is
/// what walks the bit reader across every `NEEDBITS`/`PULLBYTE` boundary
/// (`infback.c` L111-L128), and the top tier reaches the whole payload for the
/// single-shot case.
fn select_chunk(selector: u8) -> usize {
    let coarse = usize::from(selector / 4);
    match selector % 4 {
        0 => 1,
        1 => 1 + coarse % 8,
        2 => 1 + coarse % 64,
        _ => 1 + coarse % MAX_PAYLOAD_LEN,
    }
}

/// The one-based call indices at which `in()` and `out()` must refuse.
fn failure_points(mode: &FailureMode) -> (u32, u32) {
    match *mode {
        FailureMode::Never => (NEVER, NEVER),
        FailureMode::RefuseInput(at) => (u32::from(at) + 1, NEVER),
        FailureMode::RefuseOutput(at) => (NEVER, u32::from(at) + 1),
    }
}

/// `1 << bits` when `bits` is a `windowBits` `inflateBackInit_` accepts, else
/// [`None`].
///
/// ★ The single most important function in this file. `inflateBackInit_` sets
/// `wsize = 1U << windowBits` and stores the caller's pointer unchanged
/// (`infback.c` L58-L59), so the extent returned here and the argument passed
/// alongside it must come from the same value. They do: there is one call site, and
/// it uses `bits` for both.
fn window_extent(bits: c_int) -> Option<usize> {
    if (8..=15).contains(&bits) {
        // `bits` is now known to lie in 8..=15, so the shift is in range for `usize`
        // on every target this port supports.
        Some(1_usize << bits)
    } else {
        None
    }
}

/// A `usize` narrowed to `uInt` for `z_stream::avail_in`.
///
/// Exact for every value this harness produces -- staged lengths never exceed
/// [`MAX_PAYLOAD_LEN`] -- and saturating rather than panicking regardless.
fn narrow(len: usize) -> uInt {
    uInt::try_from(len).unwrap_or(uInt::MAX)
}

// ---------------------------------------------------------------------------
//  The `cover_back()` parameter-validation ladder
// ---------------------------------------------------------------------------

/// `test/infcover.c` L477-L483, plus the null-window case its `win` array cannot
/// express.
///
/// ★ **The ordering is the assertion.** The first call passes a null stream *and* a
/// null version *and* a zero `stream_size`, and the answer must be
/// `Z_VERSION_ERROR`: `infback.c` L30-L35 checks the version-and-layout handshake
/// *before* it looks at the stream pointer. An implementation that tested the
/// pointer first would answer `Z_STREAM_ERROR`, pass a naive "rejects bad input"
/// test, and still be wrong -- so the first two calls are written adjacently and
/// differ only in the version arguments.
///
/// Run on every invocation. All five calls return before touching the window or
/// allocating anything, so the cost is five branches.
fn parameter_ladder(window: &mut [u8]) {
    // Each of the five calls is a gate of its own, because a safe signature cannot express
    // the null it exists to have rejected: obligation (a) makes a `&mut z_stream` non-null
    // by construction, and the version pointer has no safe spelling either. The first two
    // differ only in their last two arguments, which is what makes the ordering testable.
    let version_first = port::inflate_back_init_null_stream_null_version(0, window);
    assert_eq!(
        version_first, Z_VERSION_ERROR,
        "infback.c L30-L32: the version handshake must be checked before the null stream"
    );

    // With the handshake satisfied, the null stream is what fails. This is what C's
    // `inflateBackInit(Z_NULL, 0, win)` macro expands to.
    let null_stream = port::inflate_back_init_null_stream(0, window);
    assert_eq!(
        null_stream, Z_STREAM_ERROR,
        "infback.c L33-L35: a null stream is Z_STREAM_ERROR"
    );

    // A real stream this time, and an empty window slice -- which the initialisation gate
    // presents as the null pointer this call exists to have rejected. Nothing is installed,
    // so nothing leaks.
    //
    // The empty slice is declared before the session for the reason `port::Session` exists: a
    // window handed to `inflateBackInit_` is retained until `inflateBackEnd`, so the session holds
    // it borrowed for its own whole life, and the compiler enforces the order. This one is refused
    // and therefore retains nothing, but the signature cannot know that in advance.
    let mut empty: [u8; 0] = [];
    let mut session = port::Session::new(port::zeroed_stream());
    let null_window = port::inflate_back_init(&mut session, 15, &mut empty);
    assert_eq!(
        null_window, Z_STREAM_ERROR,
        "infback.c L33-L35: a null window is Z_STREAM_ERROR -- it is borrowed, so there is \
         no fallback"
    );
    drop(session);

    // `inflateBack(Z_NULL, Z_NULL, Z_NULL, Z_NULL, Z_NULL)`. The stream is tested before
    // use and no callback is invoked on a stream that failed that test, so no null function
    // pointer is ever called.
    let no_stream = port::inflate_back_null_stream();
    assert_eq!(
        no_stream, Z_STREAM_ERROR,
        "infback.c L209-L210: inflateBack refuses a null stream"
    );

    let no_end = port::inflate_back_end_null_stream();
    assert_eq!(
        no_end, Z_STREAM_ERROR,
        "infback.c L573: inflateBackEnd refuses a null stream"
    );
}

/// The out-of-range `windowBits` probe: `inflateBackInit_` must refuse, and must
/// install nothing when it does.
///
/// Uses a full 32 KiB window so that the *only* invalid argument is `bits`; there is
/// no valid extent to derive from an invalid exponent, which is precisely why the
/// bound has to be checked before the shift.
fn reject_bad_window_bits(bits: c_int) {
    // Window first, session second: see `parameter_ladder`.
    let mut window: Box<[u8]> = (0..MAX_WINDOW_LEN).map(sentinel).collect();
    let mut session = port::Session::new(port::zeroed_stream());

    // The window is a live 32 KiB allocation that outlives the call and the gate satisfies
    // the version handshake, so `bits` is the one thing left to reject.
    let refused = port::inflate_back_init(&mut session, bits, &mut window);
    assert_eq!(
        refused, Z_STREAM_ERROR,
        "infback.c L33-L35: windowBits {bits} is outside 8..=15 and must be Z_STREAM_ERROR"
    );

    assert!(
        session.peek().state.is_null(),
        "a refused inflateBackInit_ must leave z_stream::state null"
    );
}

// ---------------------------------------------------------------------------
//  The session
// ---------------------------------------------------------------------------

/// One `inflateBackInit_` -> `inflateBack` (once or twice) -> `inflateBackEnd`
/// sequence.
///
/// The window is handed to `inflateBackInit_` and the library holds it until
/// `inflateBackEnd` returns (`zlib.h` L1175-L1177), so it stays borrowed for the whole
/// of this function and is read back only by the caller, afterwards.
fn run_session(
    input: &BackInput<'_>,
    bits: c_int,
    window: &mut [u8],
    in_side: &mut InSide<'_>,
    out_side: &mut OutSide,
) -> Option<TrackingReport> {
    // ★ THE SESSION IS WHAT MAKES THIS FUNCTION SOUND, and it is why the allocator is declared
    // before it. Two of the three pointers the library keeps past a call are installed here -- the
    // ledger behind `opaque`, reached by every allocation and free including the ones inside
    // `inflateBackEnd`, and the window, retained from `inflateBackInit_` until that same call. A
    // `&mut` that ends with the call cannot say either, and this function used to be written with
    // exactly those signatures and the obligations in prose; `port::Session` holds both borrows for
    // as long as the stream is reachable, and its drop-check obligation is what forces the
    // declaration order below. Reversing the two `let`s is E0597, not a review comment.
    let allocator = if input.tracked_allocator {
        // `infback.c` L37-L50 substitutes the library's own routines when these are null, so
        // leaving them alone covers the default path and installing them covers the
        // caller-hook path. The fuzzer picks.
        Some(TrackingAllocator::new())
    } else {
        None
    };
    let mut session = port::Session::new(port::zeroed_stream());
    if let Some(allocator) = allocator.as_ref() {
        allocator.install(&mut session);
    }

    let started = port::inflate_back_init(&mut session, bits, window);
    if started == Z_MEM_ERROR {
        // The tracked allocator refused. Nothing was installed, so there is nothing to
        // release and nothing further to exercise.
        drop(session);
        return allocator.as_ref().map(TrackingAllocator::finish);
    }
    assert_eq!(
        started, Z_OK,
        "inflateBackInit_ must accept windowBits {bits} with a window of exactly 1 << {bits} bytes"
    );

    let first = one_pass(&mut session, in_side, out_side);
    if input.reuse_state {
        // `zlib.h` L1150-L1152: "inflateBack() may then be used multiple times".
        // `infback.c` L214-L223 resets mode, `last`, `whave` and the bit accumulator on
        // entry and leaves `wsize`, `window` and `dmax` alone, so replaying identical
        // input on the same state must produce an identical answer. A state that is only
        // partly reset shows up here and nowhere else.
        let again = one_pass(&mut session, in_side, out_side);
        assert_eq!(
            again, first,
            "a reset inflateBack state answered {again} where the first pass answered {first}"
        );
    }

    let ended = port::inflate_back_end(session.stream());
    assert_eq!(
        ended, Z_OK,
        "infback.c L572-L579: inflateBackEnd must release a live inflateBack state"
    );
    assert!(
        session.peek().state.is_null(),
        "infback.c L577: inflateBackEnd must clear z_stream::state"
    );

    // The library has let go of both the window and the ledger, so the session may end. The window
    // is the caller's again from here, which is what lets `attempt` read the sentinels back.
    drop(session);

    // Off unless `ZLIB_RS_FUZZ_REPORT` is set. `stream_end` is the value a committed seed
    // has to reach: it means the decoder consumed a complete raw DEFLATE stream and handed
    // the bytes back through the output callback. `no_output` is the shape a seed built from
    // an uncompressed fixture produces -- the decoder refused the first byte and the
    // callback never ran -- which is the whole reason `fuzz/seeds/` exists. See
    // `zlib_rs_fuzz`.
    reached(
        "fuzz_inflate_back",
        back_reached_name(first, out_side),
        |fields| {
            fields
                .with("windowBits", bits)
                .with("payload", in_side.payload.len())
                .with("status", first)
                .with("outCalls", out_side.out_calls)
                .with("totalOut", out_side.total_out)
        },
    );

    allocator.as_ref().map(TrackingAllocator::finish)
}

/// The seed-quality vocabulary for one `inflateBack` pass.
///
/// A closed set, because `.github/workflows/rust.yml` matches on it. Two of the three names
/// distinguish cases that share a status: `Z_BUF_ERROR` after the output callback has run is
/// a truncated stream that still decoded something, and `Z_BUF_ERROR` before it has run is an
/// input that was rejected outright.
fn back_reached_name(status: c_int, out_side: &OutSide) -> &'static str {
    if status == Z_STREAM_END {
        "stream_end"
    } else if out_side.out_calls == 0 {
        "no_output"
    } else {
        "partial_output"
    }
}

/// One `inflateBack` call, from re-staging the input to checking the outcome.
fn one_pass(
    session: &mut port::Session<'_>,
    in_side: &mut InSide<'_>,
    out_side: &mut OutSide,
) -> c_int {
    in_side.rewind();
    out_side.rewind();

    // `zlib.h` L1181-L1188: input may be staged in `next_in`/`avail_in` for the first
    // call, and a null `next_in` makes `in()` be called immediately. Both are covered:
    // `staged == 0` leaves the pointer null.
    let staged = in_side.staged;
    let (next_in, avail_in) = if staged == 0 {
        (ptr::null(), 0)
    } else {
        (in_side.payload.as_ptr(), staged)
    };
    // Two plain field writes on a stream this frame borrows mutably; no pointer is
    // dereferenced. The pointer, when non-null, is the base of the payload the fuzzer
    // lent for the whole session, and `staged` bytes are readable there.
    session.stream().next_in = next_in;
    session.stream().avail_in = narrow(avail_in);

    // No window argument: the session recorded the extent when `inflateBackInit_` was given the
    // window, so the offsets `OutSide` is handed are computed against the same allocation the
    // library was initialised with by construction rather than by the caller passing it twice.
    let ret = port::inflate_back(session, in_side, out_side);

    // The two members `inf_leave` publishes (`infback.c` L567-L568), read back by value.
    let (out_next_in, out_avail_in) = (session.peek().next_in, session.peek().avail_in);
    check_outcome(ret, out_next_in, out_avail_in, in_side, out_side);
    ret
}

// ---------------------------------------------------------------------------
//  Post-call assertions
// ---------------------------------------------------------------------------

/// Everything `zlib.h` L1197-L1204 promises about one `inflateBack` return, checked.
///
/// This runs from the `fuzz_target!` body's call chain, outside every callback, which
/// is the only place `assert!` belongs in this file -- it is libFuzzer's crash
/// reporting mechanism here, whereas the same macro inside a callback would panic
/// across the FFI edge.
fn check_outcome(
    ret: c_int,
    next_in: *const Bytef,
    avail_in: uInt,
    in_side: &InSide<'_>,
    out_side: &OutSide,
) {
    // `zlib.h` L1204: "Note that inflateBack() cannot return Z_OK." Free to check and
    // it catches a whole class of control-flow mistake, because every `Z_OK` in
    // `infback.c` belongs to an init or an end.
    assert_ne!(ret, Z_OK, "zlib.h L1204: inflateBack() cannot return Z_OK");
    assert!(
        matches!(
            ret,
            Z_STREAM_END | Z_BUF_ERROR | Z_DATA_ERROR | Z_STREAM_ERROR | Z_MEM_ERROR
        ),
        "inflateBack returned {ret}, which is not one of the five codes zlib.h L1197-L1201 \
         documents"
    );
    // The stream *was* properly initialised, both callbacks are non-null, and the
    // payload never overlaps the window, so the three situations that produce
    // `Z_STREAM_ERROR` here are all excluded by construction.
    assert_ne!(
        ret, Z_STREAM_ERROR,
        "inflateBack rejected a stream that inflateBackInit_ had just accepted"
    );

    if next_in.is_null() {
        // `zlib.h` L1201-L1202 and `infback.c` L104-L106: a null `next_in` on return
        // means one thing only -- `in()` declined -- and `PULL()` runs only when `have`
        // is already zero, so nothing is left over.
        assert_eq!(
            ret, Z_BUF_ERROR,
            "next_in came back null, which is only ever a refused in(), yet the status is {ret}"
        );
        assert_eq!(
            avail_in, 0,
            "infback.c L104: a refused in() leaves avail_in at zero, not {avail_in}"
        );
        assert!(
            in_side.in_refused,
            "next_in came back null but in() never refused"
        );
    } else {
        assert!(
            !in_side.in_refused,
            "in() refused but next_in did not come back null"
        );
        if ret == Z_BUF_ERROR {
            // The documented discriminator, in the direction that is genuinely
            // under-tested: `zlib.h` L1201-L1203 -- "If strm->next_in is not Z_NULL,
            // then the Z_BUF_ERROR was due to out() returning non-zero."
            assert!(
                out_side.out_refused,
                "a Z_BUF_ERROR with a non-null next_in must come from out(), but out() never \
                 declined"
            );
        }
        assert!(
            !in_side.payload.is_empty(),
            "next_in is non-null although no input was ever available"
        );
        assert_input_within(next_in, avail_in, in_side);
    }

    // `infback.c` L563-L565 downgrades only success, so a declined out() cannot leave
    // `Z_STREAM_END` standing -- while an earlier data error is reported unchanged,
    // which is why this is an inequality rather than an equality.
    if out_side.out_refused {
        assert_ne!(
            ret, Z_STREAM_END,
            "out() declined yet the stream still reported success"
        );
    }

    assert!(
        in_side.in_calls <= MAX_IN_CALLS,
        "in() ran {} times, past its own cap",
        in_side.in_calls
    );
    assert!(
        out_side.out_calls <= MAX_OUT_CALLS,
        "out() ran {} times, past its own cap",
        out_side.out_calls
    );
    assert!(
        out_side.total_out <= MAX_TOTAL_OUT,
        "out() delivered {} bytes, past its own cap",
        out_side.total_out
    );
    assert!(
        in_side.cursor <= in_side.payload.len(),
        "the input cursor reached {} in a {}-byte payload",
        in_side.cursor,
        in_side.payload.len()
    );
}

/// The operational form of "never over-read": whatever the decoder gives back as
/// unused input has to lie inside the payload this harness owns.
///
/// `infback.c` L567-L568 publishes `next` and `have` verbatim, and `next` is either
/// the pointer a successful `in()` handed over or the staged pointer, advanced by what
/// was consumed. Either way the pair must describe a sub-range of the payload, one
/// past the end included.
fn assert_input_within(next_in: *const Bytef, avail_in: uInt, in_side: &InSide<'_>) {
    let base = in_side.payload.as_ptr() as usize;
    let limit = base.saturating_add(in_side.payload.len());
    let at = next_in as usize;
    assert!(
        at >= base && at <= limit,
        "next_in escaped the payload buffer"
    );
    let remaining = usize::try_from(avail_in).unwrap_or(usize::MAX);
    assert!(
        at.saturating_add(remaining) <= limit,
        "avail_in claims {remaining} bytes past the end of the payload -- an over-read"
    );
}

/// The window beyond what `out()` reported must still hold [`sentinel`].
///
/// The decoder writes into the window only at its own cursor and then reports every
/// byte it wrote -- `ROOM()` hands over the whole window (`infback.c` L156-L157) and
/// `inf_leave` hands over the `wsize - left` bytes of the final partial cycle (its
/// L562-L563), both starting at the window base. So the highest reported end *is* the
/// write high-water mark, and anything modified above it was modified by something
/// that had no business doing so. The callbacks are not candidates: neither writes at
/// all, which `zlib.h` L1175-L1177 requires of them.
fn assert_window_untouched(window: &[u8], out_side: &OutSide) {
    let from = out_side.max_out_end.min(window.len());
    let modified = window[from..]
        .iter()
        .enumerate()
        .find(|&(offset, byte)| *byte != sentinel(from + offset))
        .map(|(offset, _)| from + offset);
    assert!(
        modified.is_none(),
        "out() reported {from} window bytes, yet byte {modified:?} of {} was modified",
        window.len()
    );
}

/// The invariants that outlive the individual calls: the callbacks saw no contract
/// violation, and the allocator ledger balances.
///
/// The leak check is the same thing `test/infcover.c`'s `mem_done` performs through
/// its own injected hooks, and it is why installing them is worth doing at all:
/// `inflateBackEnd` releases the state and nothing else, so a live block afterwards is
/// a leak and a released one it never handed out is a rogue free. The ledger is
/// [`TrackingAllocator`]'s, so those three faults are separate counts rather than one
/// flag, and each is asserted by name.
fn assert_session_invariants(
    in_side: &InSide<'_>,
    out_side: &OutSide,
    report: Option<&TrackingReport>,
) {
    let violations = in_side.violations | out_side.violations;
    assert_eq!(
        violations, 0,
        "the callbacks recorded contract violations: {violations:#010x}"
    );

    if let Some(report) = report {
        assert_eq!(
            report.live_blocks, 0,
            "{} block(s) were still allocated after inflateBackEnd: {report:?}",
            report.live_blocks
        );
        assert_eq!(
            report.live_bytes, 0,
            "{} byte(s) were still allocated after inflateBackEnd: {report:?}",
            report.live_bytes
        );
        assert_eq!(
            report.rogue, 0,
            "zfree was handed an address zalloc never returned: {report:?}"
        );
        assert_eq!(
            report.notlifo, 0,
            "a block was freed out of order: {report:?}"
        );
        assert!(
            report.frees <= report.allocations,
            "zfree ran {} times against {} zalloc calls",
            report.frees,
            report.allocations
        );
        assert!(
            report.allocations != 0,
            "infback.c L51-L53: the decoder state must be obtained through the caller's zalloc"
        );
    }

    if out_side.folded == 0 {
        assert_eq!(
            out_side.fold, 0,
            "out() delivered no bytes yet the read fold moved"
        );
    }
    assert!(
        out_side.folded <= MAX_TOTAL_OUT.saturating_add(out_side.window_len),
        "out() read {} bytes, more than its caps allow",
        out_side.folded
    );
}

// ---------------------------------------------------------------------------
//  Driver
// ---------------------------------------------------------------------------

/// Turns one fuzzer configuration into one complete `inflateBack` session.
///
/// The window and the payload are two separate `Box<[u8]>` allocations, and the
/// separateness is load-bearing rather than incidental: it keeps the staged input
/// provably disjoint from the window, which keeps the library's overlap-copy path out of
/// the picture. Both outlive the session, because [`run_session`] borrows them for its
/// whole body and the library holds the window until `inflateBackEnd` returns.
fn drive(input: &BackInput<'_>) {
    let bits = select_window_bits(input.window_bits);
    let Some(window_len) = window_extent(bits) else {
        reject_bad_window_bits(bits);
        return;
    };

    // ★ Both the extent and the `windowBits` argument come from `bits`. One byte short
    // of `1 << bits` is a heap buffer overflow inside the library, because
    // `inflateBackInit_` borrows this buffer and trusts its size (`infback.c` L58-L59).
    let mut window: Box<[u8]> = (0..window_len).map(sentinel).collect();

    let payload_len = input.payload.len().min(MAX_PAYLOAD_LEN);
    let payload: Box<[u8]> = Box::<[u8]>::from(&input.payload[..payload_len]);

    // Five immediate-return calls, run every invocation because they are that cheap and
    // because they hold the argument-validation ordering that nothing else observes.
    parameter_ladder(&mut window);

    let (fail_in_at, fail_out_at) = failure_points(&input.failure);
    let staged = if payload_len == 0 || input.staged == 0 {
        0
    } else {
        usize::from(input.staged - 1).min(payload_len)
    };
    let mut in_side = InSide {
        payload: &payload,
        staged,
        cursor: staged,
        chunk: select_chunk(input.chunk),
        fail_in_at,
        in_calls: 0,
        in_refused: false,
        starved: input.starve_input,
        violations: 0,
    };
    let mut out_side = OutSide {
        window_len,
        fail_out_at,
        out_calls: 0,
        out_refused: false,
        total_out: 0,
        max_out_end: 0,
        folded: 0,
        fold: 0,
        violations: 0,
    };

    let report = run_session(input, bits, &mut window, &mut in_side, &mut out_side);

    // The library has returned from `inflateBackEnd`, so the window is no longer borrowed
    // and the two callback bodies are no longer reachable from it. Everything below reads
    // what they recorded.
    assert_window_untouched(&window, &out_side);
    assert_session_invariants(&in_side, &out_side, report.as_ref());
    // `inflateBack` only ever *reads* its input -- `zlib.h` L91 declares `next_in`
    // `z_const` -- so the payload must come back byte for byte as it went in. A write
    // through an input pointer is the mirror image of the window overflow the sentinel
    // check looks for, and neither is visible without an explicit comparison.
    assert_eq!(
        &payload[..],
        &input.payload[..payload_len],
        "the decoder modified the caller's input buffer"
    );
}

fuzz_target!(|input: BackInput<'_>| {
    drive(&input);
});
