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
//! 2. **The callbacks are panic-free `extern "C"`.** They are invoked from library
//!    frames that are not prepared to receive an unwind, so a panic crossing one is
//!    undefined behaviour. Both are `extern "C"` and never `extern "C-unwind"`, so a
//!    panic would abort at the boundary rather than unwind into the caller -- but
//!    they are additionally written so that no panic is reachable at all: no
//!    `unwrap`, no `expect`, no `assert`, no indexing, no allocation, no formatting,
//!    and no arithmetic that can overflow. Everything they need is computed *before*
//!    `inflateBack` is entered and parked in [`BackDesc`]; anything they observe is
//!    recorded there and asserted afterwards, from the `fuzz_target!` body, where
//!    `assert!` is the intended crash-reporting mechanism.
//!
//! # The polarities are inverted, and that is the point
//!
//! `infback.c` L182-L183: "in() should return zero on failure. out() should return
//! non-zero on failure." Writing both callbacks to one convention is the single most
//! likely bug in a harness like this one, so [`pull_cb`] and [`push_cb`] state their
//! own polarity at the top of each body.
//!
//! # Why this terminates
//!
//! A callback that can be asked for input forever is a guaranteed libFuzzer hang, so
//! termination here is structural rather than hoped for. Every successful `in()`
//! advances the payload cursor by at least one byte and the payload is truncated to
//! [`MAX_PAYLOAD_LEN`], so `in()` returns zero -- which `PULL()` turns into
//! `Z_BUF_ERROR` -- after a bounded number of calls. [`MAX_IN_CALLS`],
//! [`MAX_OUT_CALLS`] and [`MAX_TOTAL_OUT`] bound it again from the other side, and
//! bound the work a compression bomb can extract from a few hundred input bytes.
//!
//! # What this target deliberately does not do
//!
//! * It never reaches into the decoder's state to force an otherwise unreachable
//!   mode. `infcover.c`'s `pull` does exactly that (its L457-L459), and the
//!   unmodified `test/infcover.c` relinked against this library still covers it;
//!   replicating it here would require this file to know the internal state's layout.
//! * It never mutates the payload or the window from inside a callback. `zlib.h`
//!   L1174-L1175 forbids both, and [`push_cb`] only ever *reads* the region it is
//!   handed.
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
//! is on by default, and this is the target where it earns the most: every raw
//! pointer in the harness crosses into the library here.

#![no_main]

use core::ffi::{c_int, c_void};
use core::ptr;
use std::alloc::{alloc, dealloc, Layout};

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;

// `libz_rs_sys`, never `z`. The facade sets `[lib] name = "z"` so that the build
// emits a drop-in `libz.so`, and Cargo derives an `--extern` name from the lib
// target rather than the package, so `fuzz/Cargo.toml` applies a `package =
// "libz-rs-sys"` rename to restore this path. Guessing `z` does not compile.
//
// `Z_NULL` is deliberately absent from this list even though it is re-exported.
// It is C's single spelling for a null of any kind, and the facade's own
// convention -- recorded in `crates/libz-rs-sys/src/types.rs` -- maps it to
// `ptr::null()`/`ptr::null_mut()` in a data-pointer slot and to `None` in a
// function-pointer slot. Importing the integer as well would add an unused name
// and nothing else. Every other constant below is consumed from the library
// rather than redeclared, so this file cannot drift from `zlib.h`.
use libz_rs_sys::{
    alloc_func, free_func, in_func, inflateBack, inflateBackEnd, inflateBackInit_, out_func, uInt,
    voidpf, z_stream, z_streamp, Bytef, ZLIB_VERSION, Z_BUF_ERROR, Z_DATA_ERROR, Z_MEM_ERROR, Z_OK,
    Z_STREAM_END, Z_STREAM_ERROR, Z_VERSION_ERROR,
};

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
/// of it is read by [`push_cb`].
const MAX_TOTAL_OUT: usize = 262_144;

/// Sentinel for "this call index never fails", stored in
/// [`BackDesc::fail_in_at`]/[`BackDesc::fail_out_at`].
///
/// Call indices are one-based and capped far below `u32::MAX`, so this value can
/// never compare equal to a real one.
const NEVER: u32 = u32::MAX;

/// Slots in [`BackDesc::blocks`], the tracked allocator's ledger.
///
/// `inflateBackInit_` allocates one block -- the decoder state -- so eight slots is
/// generous. Exhausting them is recorded as [`V_BLOCK_TABLE_FULL`] and answered with
/// a null, which the library reports as `Z_MEM_ERROR`.
const MAX_LIVE_BLOCKS: usize = 8;

/// Alignment the tracked allocator hands back, matching what `malloc` guarantees on
/// the LP64 target `zcalloc` was measured on.
///
/// Comfortably covers every alignment the library asks of a block -- the strictest is
/// `u16` -- so the "hand the block straight back" path a misaligned hook would trip is
/// never taken from here.
const HOOK_ALIGN: usize = 16;

/// Byte the tracked allocator fills a fresh block with.
///
/// `test/infcover.c` L87 fills with `0xa5` rather than zero for a specific reason: any
/// code path that quietly assumes zero-initialised memory then misbehaves
/// deterministically instead of surviving on whatever the allocator happened to reuse.
/// `malloc` -- which is what `zcalloc` is -- hands back uninitialised bytes, so filling
/// here is faithful to the C allocator's *contract* while being far more revealing than
/// its typical behaviour.
const HOOK_FILL: u8 = 0xa5;

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

/// `in()` was called with a null output parameter, which it cannot satisfy.
const V_IN_BUF_NULL: u32 = 1 << 3;

/// `zfree` was handed an address the matching `zalloc` never returned.
///
/// The rogue-free half of what `test/infcover.c`'s `mem_done` detects.
const V_ROGUE_FREE: u32 = 1 << 4;

/// `zalloc` was asked for more concurrent blocks than the ledger can track.
const V_BLOCK_TABLE_FULL: u32 = 1 << 5;

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
//  The descriptor threaded through `void *desc`
// ---------------------------------------------------------------------------

/// One tracked allocation, so that [`tracked_free`] can rebuild the exact [`Layout`]
/// [`tracked_alloc`] used.
///
/// A ledger rather than an in-band size header: a header would have to be read back
/// through a pointer cast whose alignment the compiler cannot see, and this keeps the
/// returned block a plain, unadorned allocation.
#[derive(Clone, Copy)]
struct Block {
    /// Address handed to the library, or zero for a free slot.
    addr: usize,
    /// Size requested, in bytes. Zero marks the slot free.
    size: usize,
}

/// A free ledger slot.
const EMPTY_BLOCK: Block = Block { addr: 0, size: 0 };

/// Everything the callbacks need, and everything they observe.
///
/// Reached only through a `*mut c_void` for the whole `inflateBackInit_` ->
/// `inflateBack` -> `inflateBackEnd` sequence, exactly as C's `in_desc`, `out_desc`
/// and `opaque` are. Every field is filled in before that sequence begins, which is
/// what makes the callbacks' panic-freedom structural: they compute nothing that
/// could fail.
struct BackDesc {
    /// Base of the payload `in()` hands out. Borrowed, never freed here.
    payload: *const Bytef,
    /// Payload length in bytes, at most [`MAX_PAYLOAD_LEN`].
    payload_len: usize,
    /// Bytes staged directly in `z_stream::next_in` before each call -- the
    /// convenience path of `zlib.h` L1181-L1188. Zero leaves `next_in` null, which
    /// makes `in()` be called immediately.
    ///
    /// Also where [`BackDesc::cursor`] resumes from, so the payload is consumed
    /// exactly once whichever route it arrives by.
    staged: usize,
    /// Offset of the next byte `in()` will hand out.
    cursor: usize,
    /// Bytes `in()` offers per call, at least one. Small values exercise the bit
    /// reader's chunk boundaries; large ones the single-shot path.
    chunk: usize,
    /// One-based `in()` call index at which to refuse, or [`NEVER`].
    fail_in_at: u32,
    /// `in()` invocations so far, capped at [`MAX_IN_CALLS`].
    in_calls: u32,
    /// Whether `in()` has returned zero, which is C's `have == 0` failure.
    in_refused: bool,

    /// Base of the window handed to `inflateBackInit_`, for containment checks.
    window: *const Bytef,
    /// Window length in bytes: exactly `1 << windowBits`.
    window_len: usize,
    /// One-based `out()` call index at which to refuse, or [`NEVER`].
    fail_out_at: u32,
    /// `out()` invocations so far, capped at [`MAX_OUT_CALLS`].
    out_calls: u32,
    /// Whether `out()` has returned non-zero.
    out_refused: bool,
    /// Bytes accepted from `out()`, capped at [`MAX_TOTAL_OUT`].
    total_out: usize,
    /// Highest window offset any `out()` call has reported as written.
    ///
    /// The decoder writes only what it then reports, so the window beyond this is
    /// still untouched and must still hold [`sentinel`].
    max_out_end: usize,
    /// Bytes [`push_cb`] has actually read, including on refused calls.
    folded: usize,
    /// Rolling fold over those bytes, so the reads cannot be optimised away.
    fold: u64,

    /// Bitwise OR of the `V_*` flags, asserted zero once the session ends.
    violations: u32,

    /// The tracked allocator's ledger.
    blocks: [Block; MAX_LIVE_BLOCKS],
    /// Occupied ledger slots. Must be zero once `inflateBackEnd` has returned.
    live_blocks: u32,
    /// `zalloc` invocations.
    alloc_calls: u32,
    /// `zfree` invocations.
    free_calls: u32,
}

impl BackDesc {
    /// Clears everything that is per-`inflateBack`-call, leaving the sticky
    /// observations -- violations, the write high-water mark and the allocator
    /// ledger -- alone.
    ///
    /// `cursor` is the payload offset the next call should resume from, which is the
    /// number of bytes staged directly in `next_in`.
    fn rewind(&mut self, cursor: usize) {
        self.cursor = cursor;
        self.in_calls = 0;
        self.in_refused = false;
        self.out_calls = 0;
        self.out_refused = false;
        self.total_out = 0;
        self.folded = 0;
        self.fold = 0;
    }
}

// ---------------------------------------------------------------------------
//  The callbacks -- the critical code in this file
// ---------------------------------------------------------------------------
//
// ★ Both are `extern "C"` and NEITHER may become `extern "C-unwind"`. The library
// invokes them from frames compiled without unwind tables, so letting a panic unwind
// across one is undefined behaviour; with the `"C"` ABI a panic aborts at the
// boundary instead, which is the conservative and correct choice at an FFI edge.
// Aborting is the backstop, not the plan: neither body contains a reachable panic,
// because every fallible decision was already taken before `inflateBack` was called
// and its outcome parked in `BackDesc`.
//
// The buffers handed out live in allocations that outlive the whole
// `inflateBackInit_`/`inflateBack`/`inflateBackEnd` sequence and are not touched by
// anything else while it runs, which is what `infback.c` L172-L175 requires: "The
// application must not change the provided input until in() is called again or
// inflateBack() returns. The application must not change the window/output buffer
// until inflateBack() returns."

/// C's `in()` -- `zlib.h` L1134-L1135, modelled on `test/infcover.c`'s `pull`.
///
/// ★ **Polarity: zero means FAILURE.** A non-zero return is the number of bytes made
/// available at `*buf`; zero is "no input", which `PULL()` (`infback.c` L101-L108)
/// turns into `next = Z_NULL; ret = Z_BUF_ERROR`.
///
/// Returns zero for five reasons, all of them that same signal: a null descriptor
/// (`infcover.c`'s "no input, already provided at next_in", its L453-L456), the
/// [`MAX_IN_CALLS`] cap, the fuzzer's chosen refusal point, an unusable output
/// parameter, and an exhausted payload.
unsafe extern "C" fn pull_cb(desc: *mut c_void, buf: *mut *const Bytef) -> uInt {
    // `infcover.c` L453-L456: a null descriptor is not an error, it is the caller
    // saying it has already staged everything in `next_in`.
    //
    // The alignment test is applied to the *narrowed* pointer, never to the incoming
    // `*mut c_void`: `c_void`'s alignment is one, so testing it there would always
    // succeed and prove nothing.
    let handle = desc.cast::<BackDesc>();
    if handle.is_null() || !handle.is_aligned() {
        return 0;
    }

    // SAFETY: `handle` is the `Box<BackDesc>` pointer this harness passed as
    // `inflateBack`'s `in_desc`, checked non-null and aligned above. It is live for
    // the whole call -- the box is reclaimed only after `inflateBackEnd` -- and the
    // reborrow is unique: the driver touches the descriptor only through this same
    // raw pointer, `in()` and `out()` are never re-entered from one another, and the
    // reference does not outlive this body.
    let state = unsafe { &mut *handle };

    // Anti-hang, checked before the counter moves so the counter stays inside its
    // documented bound. A callback that can be asked for input forever is a
    // guaranteed libFuzzer timeout.
    if state.in_calls >= MAX_IN_CALLS {
        state.in_refused = true;
        return 0;
    }
    state.in_calls = state.in_calls.saturating_add(1);

    // The fuzzer-selected refusal, decided before this call and merely looked up here.
    if state.in_calls == state.fail_in_at {
        state.in_refused = true;
        return 0;
    }

    // `zlib.h` L1170-L1172 lets a failing `in()` leave `buf` untouched, so refusing is
    // always available and no path here writes through an unusable pointer.
    if buf.is_null() || !buf.is_aligned() {
        state.violations |= V_IN_BUF_NULL;
        state.in_refused = true;
        return 0;
    }

    // `saturating_sub` cannot panic and is exact: `cursor` never passes `payload_len`.
    let remaining = state.payload_len.saturating_sub(state.cursor);
    let take = state.chunk.min(remaining);
    // `zlib.h` L1178-L1179: "Any non-zero amount of input may be provided by in()",
    // so a zero count is only ever the failure signal. Written as a fallible
    // conversion rather than a cast so the bound is enforced by code.
    let Ok(count) = uInt::try_from(take) else {
        state.in_refused = true;
        return 0;
    };
    if count == 0 {
        state.in_refused = true;
        return 0;
    }

    // SAFETY: `payload` is the base of a live `Box<[u8]>` of `payload_len` bytes and
    // `cursor + take <= payload_len`, so the offset is in bounds -- one past the end
    // is the worst case and is permitted. Nothing writes to that allocation while the
    // library holds it, so the bytes stay valid until `in()` is next called, which is
    // exactly the promise `infback.c` L172-L174 requires.
    let at = unsafe { state.payload.add(state.cursor) };
    // SAFETY: `buf` is non-null and aligned by the guard above and, by this
    // callback's contract, addresses a live `*const Bytef` the library owns for the
    // duration of the call.
    unsafe { buf.write(at) };
    state.cursor = state.cursor.saturating_add(take);
    count
}

/// C's `out()` -- `zlib.h` L1136, modelled on `test/infcover.c`'s `push`.
///
/// ★ **Polarity: non-zero means FAILURE, the opposite of `in()`.** `infcover.c`'s
/// `push` is `return desc != Z_NULL` (its L467), written so that a non-null
/// descriptor forces an output error on demand; this one keeps the null-descriptor
/// success case and selects failure from the descriptor instead.
///
/// Reads the whole reported region and folds it into [`BackDesc::fold`]. That read is
/// the point: it proves the extent the library reported is genuinely readable, and
/// under AddressSanitizer a region that runs past the window is caught here rather
/// than corrupting something later. It never *writes* -- `zlib.h` L1175-L1177 forbids
/// a callback from changing the window.
unsafe extern "C" fn push_cb(desc: *mut c_void, buf: *mut Bytef, len: uInt) -> c_int {
    // Mirrors `pull_cb`'s null-descriptor branch on the success side: `infcover.c`'s
    // `push` treats a null descriptor as the non-failing case. The alignment test is
    // again applied after narrowing, for the reason `pull_cb` records.
    let handle = desc.cast::<BackDesc>();
    if handle.is_null() || !handle.is_aligned() {
        return 0;
    }

    // SAFETY: as `pull_cb` -- `handle` is this harness's live `Box<BackDesc>` pointer,
    // checked non-null and aligned, and the reborrow is unique and confined to this
    // body. `in()` and `out()` are never active at the same time.
    let state = unsafe { &mut *handle };

    if state.out_calls >= MAX_OUT_CALLS {
        state.out_refused = true;
        return 1;
    }
    state.out_calls = state.out_calls.saturating_add(1);

    let Ok(count) = usize::try_from(len) else {
        state.violations |= V_OUT_TOO_LONG;
        state.out_refused = true;
        return 1;
    };

    // Record, never assert: an assertion here would panic across the FFI edge. The
    // flags are read from the `fuzz_target!` body once the session has ended.
    if count == 0 {
        state.violations |= V_OUT_EMPTY;
    }
    if count > state.window_len {
        state.violations |= V_OUT_TOO_LONG;
    }
    match window_end(state.window, state.window_len, buf.cast_const(), count) {
        Some(end) => state.max_out_end = state.max_out_end.max(end),
        None => state.violations |= V_OUT_OUTSIDE_WINDOW,
    }

    // Read before deciding, so that even a refused call validates the extent it was
    // handed. Bounded: the caps below refuse on the first call that would exceed
    // `MAX_TOTAL_OUT`, so at most that plus one window is ever read.
    if count != 0 {
        if buf.is_null() {
            state.violations |= V_OUT_OUTSIDE_WINDOW;
            state.out_refused = true;
            return 1;
        }
        // SAFETY: unsafe-site category 2 -- slice reconstruction, once, over the
        // region this callback was handed. `buf` is non-null by the guard above, and
        // by `out()`'s contract `buf[0..len-1]` is live and readable for the duration
        // of the call: it is a prefix of the window the decoder has just written and
        // still owns. `u8` needs no alignment beyond one. The slice is read-only and
        // does not outlive this body.
        let data = unsafe { core::slice::from_raw_parts(buf.cast_const(), count) };
        let mut fold = state.fold;
        for byte in data {
            fold = fold.rotate_left(7) ^ u64::from(*byte);
        }
        state.fold = fold;
        state.folded = state.folded.saturating_add(count);
    }

    if state.total_out.saturating_add(count) > MAX_TOTAL_OUT || state.out_calls == state.fail_out_at
    {
        state.out_refused = true;
        return 1;
    }
    state.total_out = state.total_out.saturating_add(count);
    0
}

/// End offset of `[buf, buf + count)` within `[window, window + window_len)`, or
/// [`None`] when it does not fit.
///
/// Compares addresses rather than pointers so that a region from an unrelated
/// allocation is rejected instead of being subtracted, and so that nothing here can
/// overflow or panic.
fn window_end(
    window: *const Bytef,
    window_len: usize,
    buf: *const Bytef,
    count: usize,
) -> Option<usize> {
    let base = window as usize;
    let at = buf as usize;
    if at < base {
        return None;
    }
    let end = (at - base).checked_add(count)?;
    if end > window_len {
        return None;
    }
    Some(end)
}

/// The caller-supplied `zalloc` -- `zlib.h` L85, the Rust counterpart of `zcalloc`
/// (`zutil.c` L215).
///
/// Routes the decoder state through this harness's ledger so that a leak, a rogue
/// free or an unexpected number of blocks becomes an assertion after the session --
/// the same thing `test/infcover.c`'s instrumented allocator buys, and the reason the
/// caller-hook path (unsafe-site category 4) is worth fuzzing at all.
///
/// Panic-free like the two stream callbacks, and for the same reason: `zalloc` is
/// reached from library frames. Refusal is a null return, which the library reports
/// as `Z_MEM_ERROR`.
unsafe extern "C" fn tracked_alloc(opaque: voidpf, items: uInt, size: uInt) -> voidpf {
    let handle = opaque.cast::<BackDesc>();
    if handle.is_null() || !handle.is_aligned() {
        return ptr::null_mut();
    }
    // SAFETY: `handle` is the `Box<BackDesc>` pointer this harness published in
    // `z_stream::opaque` before `inflateBackInit_`, checked non-null and aligned. It
    // outlives every call the library makes through these hooks, and the reborrow is
    // unique and confined to this body.
    let state = unsafe { &mut *handle };
    state.alloc_calls = state.alloc_calls.saturating_add(1);

    let (Ok(items), Ok(size)) = (usize::try_from(items), usize::try_from(size)) else {
        return ptr::null_mut();
    };
    let Some(bytes) = items.checked_mul(size) else {
        return ptr::null_mut();
    };
    // A zero-byte request has no valid answer here -- `alloc` requires a non-zero
    // layout -- and the library never makes one.
    if bytes == 0 {
        return ptr::null_mut();
    }
    let Ok(layout) = Layout::from_size_align(bytes, HOOK_ALIGN) else {
        return ptr::null_mut();
    };
    let Some(slot) = state.blocks.iter_mut().find(|block| block.size == 0) else {
        state.violations |= V_BLOCK_TABLE_FULL;
        return ptr::null_mut();
    };

    // SAFETY: `layout` has a non-zero size, which is `alloc`'s one requirement. A
    // null return is handled below rather than dereferenced.
    let block = unsafe { alloc(layout) };
    if block.is_null() {
        return ptr::null_mut();
    }
    // SAFETY: `block` is non-null by the guard above and addresses `bytes` writable
    // bytes that `alloc` has just handed over exclusively -- nothing else references
    // them yet -- so the fill is in bounds and cannot race.
    unsafe { ptr::write_bytes(block, HOOK_FILL, bytes) };
    *slot = Block {
        addr: block as usize,
        size: bytes,
    };
    state.live_blocks = state.live_blocks.saturating_add(1);
    block.cast::<c_void>()
}

/// The caller-supplied `zfree` -- `zlib.h` L86, the Rust counterpart of `zcfree`
/// (`zutil.c` L240).
///
/// Releases only addresses [`tracked_alloc`] handed out, with the exact [`Layout`] it
/// used; anything else is recorded as [`V_ROGUE_FREE`] and left alone rather than
/// passed to the global allocator. Panic-free, and silent on every path, because
/// `zfree` returns nothing and is called from library frames.
unsafe extern "C" fn tracked_free(opaque: voidpf, address: voidpf) {
    let handle = opaque.cast::<BackDesc>();
    if handle.is_null() || !handle.is_aligned() || address.is_null() {
        return;
    }
    // SAFETY: as `tracked_alloc` -- the descriptor this harness published in
    // `z_stream::opaque`, non-null and aligned, live for the whole session.
    let state = unsafe { &mut *handle };
    state.free_calls = state.free_calls.saturating_add(1);

    let at = address as usize;
    let Some(slot) = state
        .blocks
        .iter_mut()
        .find(|block| block.size != 0 && block.addr == at)
    else {
        state.violations |= V_ROGUE_FREE;
        return;
    };
    let Ok(layout) = Layout::from_size_align(slot.size, HOOK_ALIGN) else {
        return;
    };
    *slot = EMPTY_BLOCK;

    // SAFETY: `address` is exactly the pointer `tracked_alloc` returned for this
    // ledger slot -- found by address match above -- so it came from `alloc` with
    // this very `layout`, has not been released before (the slot was still occupied),
    // and the round trip through `*mut c_void` preserved its provenance.
    unsafe { dealloc(address.cast::<u8>(), layout) };
    state.live_blocks = state.live_blocks.saturating_sub(1);
}

/// `in()` as the library's own nullable function-pointer alias sees it.
///
/// Declaring the constant rather than casting at the call site is what makes a
/// signature drift a compile error: `in_func` is generated from `zlib.h`
/// L1134-L1135, so a wrong parameter or return type fails here instead of becoming a
/// silent ABI break.
const PULL: in_func = Some(pull_cb);

/// `out()` as `out_func` -- `zlib.h` L1136 -- for the same reason as [`PULL`].
const PUSH: out_func = Some(push_cb);

/// `zalloc` as `alloc_func` -- `zlib.h` L85.
const TRACKED_ALLOC: alloc_func = Some(tracked_alloc);

/// `zfree` as `free_func` -- `zlib.h` L86.
const TRACKED_FREE: free_func = Some(tracked_free);

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
    /// Pass a null `in_desc`, so `in()` always answers "no input" and the decoder has
    /// only the staged bytes -- `cover_back` L489's `inflateBack(&strm, pull, Z_NULL,
    /// push, Z_NULL)`.
    starve_input: bool,
    /// Install [`TRACKED_ALLOC`]/[`TRACKED_FREE`] instead of leaving the hooks null
    /// and letting the library substitute its own (`infback.c` L37-L50).
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

/// A `z_stream` with every member zeroed, which is what a C caller declares before an
/// init call.
///
/// Spelled out field by field because the facade deliberately does not derive
/// `Default`: a zeroed stream is not a usable one until an init function has run.
fn blank_stream() -> z_stream {
    z_stream {
        next_in: ptr::null(),
        avail_in: 0,
        total_in: 0,
        next_out: ptr::null_mut(),
        avail_out: 0,
        total_out: 0,
        msg: ptr::null(),
        state: ptr::null_mut(),
        zalloc: None,
        zfree: None,
        opaque: ptr::null_mut(),
        data_type: 0,
        adler: 0,
        reserved: 0,
    }
}

/// `(int) sizeof(z_stream)`, the second half of the handshake the C
/// `inflateBackInit` macro arranges (`zlib.h` L1113).
fn stream_size() -> c_int {
    c_int::try_from(core::mem::size_of::<z_stream>()).unwrap_or(c_int::MAX)
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
fn parameter_ladder(window: *mut Bytef) {
    let mut strm = blank_stream();
    let strm_ptr: z_streamp = ptr::addr_of_mut!(strm);

    // SAFETY: every argument below is one the entry point is contracted to test
    // rather than dereference. A null `z_streamp` and a null version pointer are both
    // documented inputs, `window` is this session's live buffer, and the callbacks are
    // `None`, which is C's `Z_NULL` in a function-pointer slot.
    let version_first = unsafe { inflateBackInit_(ptr::null_mut(), 0, window, ptr::null(), 0) };
    assert_eq!(
        version_first, Z_VERSION_ERROR,
        "infback.c L30-L32: the version handshake must be checked before the null stream"
    );

    // SAFETY: as above, with the handshake satisfied so that the null stream is what
    // fails. This is what C's `inflateBackInit(Z_NULL, 0, win)` macro expands to.
    let null_stream = unsafe {
        inflateBackInit_(
            ptr::null_mut(),
            0,
            window,
            ZLIB_VERSION.as_ptr(),
            stream_size(),
        )
    };
    assert_eq!(
        null_stream, Z_STREAM_ERROR,
        "infback.c L33-L35: a null stream is Z_STREAM_ERROR"
    );

    // SAFETY: `strm_ptr` addresses a live, zeroed local stream; the window is the null
    // this call exists to have rejected. Nothing is installed, so nothing leaks.
    let null_window = unsafe {
        inflateBackInit_(
            strm_ptr,
            15,
            ptr::null_mut(),
            ZLIB_VERSION.as_ptr(),
            stream_size(),
        )
    };
    assert_eq!(
        null_window, Z_STREAM_ERROR,
        "infback.c L33-L35: a null window is Z_STREAM_ERROR -- it is borrowed, so there is \
         no fallback"
    );

    // SAFETY: `inflateBack(Z_NULL, Z_NULL, Z_NULL, Z_NULL, Z_NULL)`. The stream is
    // tested before use and no callback is invoked on a stream that failed that test,
    // so no null function pointer is ever called.
    let no_stream = unsafe {
        inflateBack(
            ptr::null_mut(),
            None,
            ptr::null_mut(),
            None,
            ptr::null_mut(),
        )
    };
    assert_eq!(
        no_stream, Z_STREAM_ERROR,
        "infback.c L209-L210: inflateBack refuses a null stream"
    );

    // SAFETY: a null `z_streamp`, which this entry point is contracted to reject
    // rather than dereference (`infback.c` L573).
    let no_end = unsafe { inflateBackEnd(ptr::null_mut()) };
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
    let mut window: Box<[u8]> = (0..MAX_WINDOW_LEN).map(sentinel).collect();
    let mut strm = blank_stream();
    let strm_ptr: z_streamp = ptr::addr_of_mut!(strm);
    let window_ptr: *mut Bytef = window.as_mut_ptr();

    // SAFETY: `strm_ptr` addresses a live zeroed local, `window_ptr` a live 32 KiB
    // allocation that outlives the call, and the version handshake is satisfied, so
    // `bits` is the one thing left to reject.
    let refused = unsafe {
        inflateBackInit_(
            strm_ptr,
            bits,
            window_ptr,
            ZLIB_VERSION.as_ptr(),
            stream_size(),
        )
    };
    assert_eq!(
        refused, Z_STREAM_ERROR,
        "infback.c L33-L35: windowBits {bits} is outside 8..=15 and must be Z_STREAM_ERROR"
    );

    // SAFETY: reads one member of the live local stream through a raw place, forming
    // no reference to it.
    let state = unsafe { ptr::addr_of!((*strm_ptr).state).read() };
    assert!(
        state.is_null(),
        "a refused inflateBackInit_ must leave z_stream::state null"
    );
}

// ---------------------------------------------------------------------------
//  The session
// ---------------------------------------------------------------------------

/// One `inflateBackInit_` -> `inflateBack` (once or twice) -> `inflateBackEnd`
/// sequence.
///
/// The stream is reached only through `strm_ptr` from the moment it is handed to
/// `inflateBackInit_`: the facade records that very pointer inside the state it
/// installs and compares later calls against it, and a `&mut z_stream` reborrow in
/// between would invalidate it under Rust's aliasing model even though the memory is
/// untouched. Every read of the stream below therefore goes through a raw place.
fn run_session(input: &BackInput<'_>, bits: c_int, window: *mut Bytef, desc: *mut BackDesc) {
    let mut strm = blank_stream();
    let strm_ptr: z_streamp = ptr::addr_of_mut!(strm);

    if input.tracked_allocator {
        // `infback.c` L37-L50 substitutes the library's own routines when these are
        // null, so leaving them alone covers the default path and setting them covers
        // the caller-hook path. The fuzzer picks.
        // SAFETY: writes three members of a live local stream through raw places,
        // before any state exists, forming no reference.
        unsafe {
            ptr::addr_of_mut!((*strm_ptr).zalloc).write(TRACKED_ALLOC);
            ptr::addr_of_mut!((*strm_ptr).zfree).write(TRACKED_FREE);
            ptr::addr_of_mut!((*strm_ptr).opaque).write(desc.cast::<c_void>());
        }
    }

    let started = {
        // SAFETY: `strm_ptr` addresses a live local stream whose hooks are either both
        // null or both this harness's, `window` is a live allocation of exactly
        // `1 << bits` bytes that outlives the whole session and is reached only through
        // this pointer until `inflateBackEnd` returns, and the version handshake is
        // satisfied.
        unsafe { inflateBackInit_(strm_ptr, bits, window, ZLIB_VERSION.as_ptr(), stream_size()) }
    };
    if started == Z_MEM_ERROR {
        // The tracked allocator refused. Nothing was installed, so there is nothing to
        // release and nothing further to exercise.
        return;
    }
    assert_eq!(
        started, Z_OK,
        "inflateBackInit_ must accept windowBits {bits} with a window of exactly 1 << {bits} bytes"
    );

    // `cover_back` L489 passes `Z_NULL` as `in_desc` so that `pull` answers "no input,
    // already provided at next_in"; `out_desc` stays non-null so the output side keeps
    // its ledger either way.
    let in_desc: *mut c_void = if input.starve_input {
        ptr::null_mut()
    } else {
        desc.cast::<c_void>()
    };
    let out_desc: *mut c_void = desc.cast::<c_void>();

    let first = one_pass(strm_ptr, desc, in_desc, out_desc, !input.starve_input);
    if input.reuse_state {
        // `zlib.h` L1150-L1152: "inflateBack() may then be used multiple times".
        // `infback.c` L214-L223 resets mode, `last`, `whave` and the bit accumulator on
        // entry and leaves `wsize`, `window` and `dmax` alone, so replaying identical
        // input on the same state must produce an identical answer. A state that is
        // only partly reset shows up here and nowhere else.
        let again = one_pass(strm_ptr, desc, in_desc, out_desc, !input.starve_input);
        assert_eq!(
            again, first,
            "a reset inflateBack state answered {again} where the first pass answered {first}"
        );
    }

    // SAFETY: `strm_ptr` still addresses the same live local stream, and its state was
    // installed by the `inflateBackInit_` above and has not been ended.
    let ended = unsafe { inflateBackEnd(strm_ptr) };
    assert_eq!(
        ended, Z_OK,
        "infback.c L572-L579: inflateBackEnd must release a live inflateBack state"
    );

    // SAFETY: reads one member of the live local stream through a raw place.
    let state = unsafe { ptr::addr_of!((*strm_ptr).state).read() };
    assert!(
        state.is_null(),
        "infback.c L577: inflateBackEnd must clear z_stream::state"
    );
}

/// One `inflateBack` call, from re-staging the input to checking the outcome.
///
/// `tracked_input` says whether `in_desc` is this harness's descriptor; with a null
/// `in_desc` the input side keeps no ledger, so the cross-checks that rely on one are
/// skipped while every contract assertion that does not still applies.
fn one_pass(
    strm_ptr: z_streamp,
    desc: *mut BackDesc,
    in_desc: *mut c_void,
    out_desc: *mut c_void,
    tracked_input: bool,
) -> c_int {
    // SAFETY: `desc` is this harness's live `Box<BackDesc>` pointer. The reborrow ends
    // before `inflateBack` is entered, so it cannot alias the reborrows the callbacks
    // take from the same raw pointer.
    let (payload, staged) = unsafe {
        let state = &mut *desc;
        let staged = state.staged;
        state.rewind(staged);
        (state.payload, staged)
    };

    // `zlib.h` L1181-L1188: input may be staged in `next_in`/`avail_in` for the first
    // call, and a null `next_in` makes `in()` be called immediately. Both are covered:
    // `staged == 0` with nothing staged leaves the pointer null.
    let (next_in, avail_in) = if staged == 0 {
        (ptr::null(), 0)
    } else {
        (payload, staged)
    };
    // SAFETY: writes two members of the caller's live stream through raw places. The
    // pointer, when non-null, is the base of the live payload allocation and `staged`
    // bytes are readable there.
    unsafe {
        ptr::addr_of_mut!((*strm_ptr).next_in).write(next_in);
        ptr::addr_of_mut!((*strm_ptr).avail_in).write(narrow(avail_in));
    }

    // SAFETY: `strm_ptr` addresses the stream `inflateBackInit_` initialised, both
    // callbacks are non-null and match the `zlib.h` L1134-L1136 prototypes through the
    // `in_func`/`out_func` aliases, and both descriptors are either null or this
    // harness's live descriptor. Neither callback frees or reallocates the stream, its
    // state or its window, and neither can unwind.
    let ret = unsafe { inflateBack(strm_ptr, PULL, in_desc, PUSH, out_desc) };

    // SAFETY: reads the two members `inf_leave` publishes (`infback.c` L567-L568)
    // through raw places, forming no reference to the stream.
    let (out_next_in, out_avail_in) = unsafe {
        (
            ptr::addr_of!((*strm_ptr).next_in).read(),
            ptr::addr_of!((*strm_ptr).avail_in).read(),
        )
    };
    check_outcome(ret, out_next_in, out_avail_in, desc, tracked_input);
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
    desc: *const BackDesc,
    tracked_input: bool,
) {
    // SAFETY: `desc` is this harness's live `Box<BackDesc>` pointer and the library has
    // returned, so no callback holds a reborrow of it. This one is shared and does not
    // outlive the body.
    let state = unsafe { &*desc };

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
        if tracked_input {
            assert!(
                state.in_refused,
                "next_in came back null but in() never returned zero"
            );
        }
    } else {
        if tracked_input {
            assert!(
                !state.in_refused,
                "in() returned zero but next_in did not come back null"
            );
        }
        if ret == Z_BUF_ERROR {
            // The documented discriminator, in the direction that is genuinely
            // under-tested: `zlib.h` L1201-L1203 -- "If strm->next_in is not Z_NULL,
            // then the Z_BUF_ERROR was due to out() returning non-zero."
            assert!(
                state.out_refused,
                "a Z_BUF_ERROR with a non-null next_in must come from out(), but out() never \
                 declined"
            );
        }
        assert!(
            state.payload_len != 0,
            "next_in is non-null although no input was ever available"
        );
        assert_input_within(next_in, avail_in, state);
    }

    // `infback.c` L563-L565 downgrades only success, so a declined out() cannot leave
    // `Z_STREAM_END` standing -- while an earlier data error is reported unchanged,
    // which is why this is an inequality rather than an equality.
    if state.out_refused {
        assert_ne!(
            ret, Z_STREAM_END,
            "out() declined yet the stream still reported success"
        );
    }

    assert!(
        state.in_calls <= MAX_IN_CALLS,
        "in() ran {} times, past its own cap",
        state.in_calls
    );
    assert!(
        state.out_calls <= MAX_OUT_CALLS,
        "out() ran {} times, past its own cap",
        state.out_calls
    );
    assert!(
        state.total_out <= MAX_TOTAL_OUT,
        "out() delivered {} bytes, past its own cap",
        state.total_out
    );
    assert!(
        state.cursor <= state.payload_len,
        "the input cursor reached {} in a {}-byte payload",
        state.cursor,
        state.payload_len
    );
}

/// The operational form of "never over-read": whatever the decoder gives back as
/// unused input has to lie inside the payload this harness owns.
///
/// `infback.c` L567-L568 publishes `next` and `have` verbatim, and `next` is either
/// the pointer a successful `in()` handed over or the staged pointer, advanced by what
/// was consumed. Either way the pair must describe a sub-range of the payload, one
/// past the end included.
fn assert_input_within(next_in: *const Bytef, avail_in: uInt, state: &BackDesc) {
    let base = state.payload as usize;
    let limit = base.saturating_add(state.payload_len);
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
fn assert_window_untouched(window: &[u8], state: &BackDesc) {
    let from = state.max_out_end.min(window.len());
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
/// a leak and a released one it never handed out is a rogue free.
fn assert_session_invariants(state: &BackDesc, tracked_allocator: bool) {
    let violations = state.violations;
    assert_eq!(
        violations, 0,
        "the callbacks recorded contract violations: {violations:#010x}"
    );
    assert_eq!(
        state.live_blocks, 0,
        "{} block(s) were still allocated after inflateBackEnd",
        state.live_blocks
    );
    assert!(
        state.free_calls <= state.alloc_calls,
        "zfree ran {} times against {} zalloc calls",
        state.free_calls,
        state.alloc_calls
    );
    if tracked_allocator {
        assert!(
            state.alloc_calls != 0,
            "infback.c L51-L53: the decoder state must be obtained through the caller's zalloc"
        );
    }
    if state.folded == 0 {
        assert_eq!(
            state.fold, 0,
            "out() delivered no bytes yet the read fold moved"
        );
    }
    assert!(
        state.folded <= MAX_TOTAL_OUT.saturating_add(state.window_len),
        "push_cb read {} bytes, more than its caps allow",
        state.folded
    );
}

// ---------------------------------------------------------------------------
//  Driver
// ---------------------------------------------------------------------------

/// Turns one fuzzer configuration into one complete `inflateBack` session.
///
/// The window and the payload are each held as a `Box<[u8]>` converted to a raw
/// pointer for the whole session and reclaimed afterwards. That is not ceremony: the
/// facade documents that the window must be reached *through the pointer given to
/// `inflateBackInit_`* for as long as the state lives, so deriving that pointer from a
/// `&mut [u8]` and then touching the slice again in between would invalidate it under
/// Rust's aliasing model even though the bytes are untouched. Handing ownership to a
/// raw pointer makes the discipline structural. It also guarantees the payload and the
/// window are distinct allocations, which keeps the staged input provably disjoint from
/// the window and so keeps the library's overlap-copy path out of the picture.
fn drive(input: &BackInput<'_>) {
    let bits = select_window_bits(input.window_bits);
    let Some(window_len) = window_extent(bits) else {
        reject_bad_window_bits(bits);
        return;
    };

    // ★ Both the extent and the `windowBits` argument come from `bits`. One byte short
    // of `1 << bits` is a heap buffer overflow inside the library, because
    // `inflateBackInit_` borrows this buffer and trusts its size (`infback.c` L58-L59).
    let window: Box<[u8]> = (0..window_len).map(sentinel).collect();
    let window_raw: *mut [u8] = Box::into_raw(window);
    let window_ptr: *mut Bytef = window_raw.cast::<Bytef>();

    let payload_len = input.payload.len().min(MAX_PAYLOAD_LEN);
    let payload_raw: *mut [u8] = Box::into_raw(Box::<[u8]>::from(&input.payload[..payload_len]));
    let payload_ptr: *const Bytef = payload_raw.cast::<Bytef>();

    // Five immediate-return calls, run every invocation because they are that cheap and
    // because they hold the argument-validation ordering that nothing else observes.
    parameter_ladder(window_ptr);

    let (fail_in_at, fail_out_at) = failure_points(&input.failure);
    let staged = if payload_len == 0 || input.staged == 0 {
        0
    } else {
        usize::from(input.staged - 1).min(payload_len)
    };
    let desc_raw = Box::into_raw(Box::new(BackDesc {
        payload: payload_ptr,
        payload_len,
        staged,
        cursor: staged,
        chunk: select_chunk(input.chunk),
        fail_in_at,
        in_calls: 0,
        in_refused: false,
        window: window_ptr.cast_const(),
        window_len,
        fail_out_at,
        out_calls: 0,
        out_refused: false,
        total_out: 0,
        max_out_end: 0,
        folded: 0,
        fold: 0,
        violations: 0,
        blocks: [EMPTY_BLOCK; MAX_LIVE_BLOCKS],
        live_blocks: 0,
        alloc_calls: 0,
        free_calls: 0,
    }));

    run_session(input, bits, window_ptr, desc_raw);

    // The library has returned from `inflateBackEnd`, so the window is no longer
    // borrowed and the descriptor is no longer reachable from any callback. Ownership
    // comes back before anything reads the bytes as a slice again.
    //
    // SAFETY: `desc_raw` came from `Box::into_raw` in this very function, has not been
    // reclaimed already, and every reborrow the callbacks took from it has ended --
    // the library returned from `inflateBackEnd` above, and it is the only thing that
    // could still have been calling them.
    let state = unsafe { Box::from_raw(desc_raw) };
    // SAFETY: `window_raw` came from `Box::into_raw` in this very function and has not
    // been reclaimed already. `inflateBackEnd` released the borrow the state held over
    // it, so nothing else addresses this allocation.
    let window = unsafe { Box::from_raw(window_raw) };
    // SAFETY: `payload_raw` came from `Box::into_raw` in this very function and has not
    // been reclaimed already. The last thing to hold a pointer into it was the decoder,
    // through `in()`, and it is documented to stop at the `inflateBack` return.
    let payload = unsafe { Box::from_raw(payload_raw) };

    assert_window_untouched(&window, &state);
    assert_session_invariants(&state, input.tracked_allocator);
    // `inflateBack` only ever *reads* its input -- `zlib.h` L91 declares `next_in`
    // `z_const` -- so the payload must come back byte for byte as it went in. A write
    // through an input pointer is the mirror image of the window overflow the sentinel
    // check looks for, and neither is visible without an explicit comparison.
    assert_eq!(
        &payload[..],
        &input.payload[..state.payload_len],
        "the decoder modified the caller's input buffer"
    );
}

fuzz_target!(|input: BackInput<'_>| {
    drive(&input);
});
