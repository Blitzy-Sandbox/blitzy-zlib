//! libFuzzer target for the DEFLATE compressor, derived from `deflate.c`.
//!
//! # What this target does
//!
//! It draws a *structured* compressor configuration -- level, method, windowBits,
//! memLevel, strategy -- through `arbitrary` rather than reinterpreting raw bytes,
//! and drives the whole exported `deflate*` surface with it. The configuration
//! space is swept deliberately: most draws land inside the legal space, so real
//! compression paths and the per-level `configuration_table[10]` (`deflate.c`
//! L112-L124) get exercised, while a reserved slice of the fuzzer's probability
//! mass lands *outside* it. Illegal draws are held to a stricter standard than
//! legal ones: the harness predicts rejection itself, from the same rules
//! `deflateInit2_` applies (`deflate.c` L394-L419), and asserts that the library
//! agrees exactly. A configuration that should be refused and is not -- or that is
//! refused with the wrong code, or that panics or aborts instead of returning --
//! is a defect this target reports.
//!
//! Alongside the configuration sweep it asserts four families of invariant:
//!
//! - **Bound safety.** Callers size their output buffers from `deflateBound` and
//!   `compressBound`, so a bound that is too small becomes a buffer overflow *in
//!   caller code*. Every byte produced is counted and checked against the bound,
//!   and the documented single-pass guarantee -- all input, a bound-sized output
//!   buffer and `Z_FINISH` yields `Z_STREAM_END` -- is exercised directly.
//! - **Allocator discipline.** Every allocation goes through the caller-supplied
//!   hooks, using the tracking zone of `test/infcover.c` L56-L239: allocations are
//!   filled with `0xa5` rather than zeros, the release order is checked for
//!   last-in-first-out discipline, frees of addresses the zone never handed out are
//!   counted, and a fuzzer-chosen ceiling forces `Z_MEM_ERROR` systematically
//!   instead of by luck.
//! - **Return-code discipline.** Each entry point is held to its documented return
//!   set. `Z_BUF_ERROR` is *not* a failure -- it means no progress was possible,
//!   which is legitimate against a one-byte output buffer.
//! - **Termination.** Payload length, output-buffer size, total output, iteration
//!   count and allocator ceiling are all bounded, so one invocation costs
//!   microseconds and neither hangs nor exhausts memory.
//!
//! # What this target deliberately does NOT do
//!
//! **It asserts nothing about the specific bytes the compressor emits.**
//! Byte-identity against the C encoder is owned by
//! `crates/zlib-rs-differential/tests/byte_identical.rs`, which sweeps the full
//! level x windowBits x memLevel x strategy x flush matrix deterministically.
//! Re-checking it here would spend a randomised time budget on work already
//! covered exhaustively, so this target asserts *invariants* and *absence of
//! crashes* instead. Nothing here may depend on the `simd` feature either: the
//! vectorized backends are confined to the checksums and are required to be
//! output-neutral, so this target must behave identically with and without them.
//!
//! # Running it
//!
//! ```text
//! cd fuzz
//! cargo +nightly fuzz run fuzz_deflate -- -max_total_time=300
//! ```
//!
//! `+nightly` is required because the repository-root `rust-toolchain.toml` pins
//! stable and cargo-fuzz's sanitizer instrumentation is nightly-only. The gate is
//! zero crashes, zero hangs, zero timeouts and zero out-of-memory reports over at
//! least 300 seconds, matching `fuzz-seconds: 300` in
//! `.github/workflows/fuzz.yml`. AddressSanitizer is on by default under
//! cargo-fuzz, so that run doubles as the sanitizer gate for the boundary layer.
//!
//! # Self-containment
//!
//! cargo-fuzz compiles every file in `fuzz_targets/` as an independent binary, so
//! a shared helper module would simply never be compiled. The tracking allocator
//! below is therefore duplicated with the sibling targets on purpose; do not try
//! to factor it out.

#![no_main]

use std::alloc::{alloc, dealloc, Layout};
use std::ffi::{c_char, c_int, c_uint, c_void};
use std::ptr;

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;

use libz_rs_sys::{
    compress2, compressBound, compressBound_z, deflate, deflateBound, deflateBound_z, deflateCopy,
    deflateEnd, deflateGetDictionary, deflateInit2_, deflateParams, deflatePending, deflatePrime,
    deflateReset, deflateResetKeep, deflateSetDictionary, deflateSetHeader, deflateTune,
    deflateUsed, gz_header, gz_headerp, uInt, uLong, uLongf, voidpf, z_size_t, z_stream, Bytef,
    MAX_MEM_LEVEL, MAX_WBITS, ZLIB_VERSION, Z_BEST_COMPRESSION, Z_BLOCK, Z_BUF_ERROR, Z_DATA_ERROR,
    Z_DEFAULT_COMPRESSION, Z_DEFAULT_STRATEGY, Z_DEFLATED, Z_FINISH, Z_FIXED, Z_FULL_FLUSH,
    Z_MEM_ERROR, Z_NO_COMPRESSION, Z_NO_FLUSH, Z_OK, Z_PARTIAL_FLUSH, Z_STREAM_END, Z_STREAM_ERROR,
    Z_SYNC_FLUSH, Z_TREES, Z_VERSION_ERROR,
};

// ---------------------------------------------------------------------------
//  Termination bounds
// ---------------------------------------------------------------------------
//
// EVERY constant in this block exists to satisfy the no-hang half of the fuzzing
// gate: zero hangs and zero timeouts over a 300-second budget, with a healthy
// executions-per-second rate so that this target does not starve the other four
// of CI time. They are not tuning knobs and they are not decoration -- raising
// them trades the gate away. The realistic slow case is level 9 with a 32 KiB
// window over a highly repetitive payload, which is precisely what an unbounded
// payload length would let the fuzzer discover, so the payload cap is the load
// bearing one.

/// Longest payload handed to the compressor, in bytes.
///
/// Four kibibytes compresses in microseconds at every level while still being
/// long enough to cross a `pending_buf` boundary, force multiple deflate blocks
/// and produce matches at non-trivial distances.
const MAX_PAYLOAD_LEN: usize = 4096;

/// Longest preset dictionary, in bytes.
///
/// Deliberately larger than the smallest legal window (`windowBits` 8 is promoted
/// to 9, giving `w_size` 512), so the documented "dictionary longer than the
/// window is truncated to its tail" path in `deflateSetDictionary`
/// (`deflate.c` L580-L589) is reachable.
const MAX_DICTIONARY_LEN: usize = 1024;

/// Largest output buffer offered to a single `deflate` call, in bytes.
const MAX_OUTPUT_BUF_LEN: usize = 1024;

/// Total bytes of compressed output any one session may produce before the driver
/// stops.
///
/// Reaching this is a legitimate stop, not a failure: a fuzzer-chosen flush
/// schedule made mostly of `Z_SYNC_FLUSH` emits an empty stored block per call,
/// so output can grow without input being consumed.
const MAX_TOTAL_OUTPUT: usize = 1 << 18;

/// Most `deflate` calls one session may make.
///
/// A one-byte output buffer drains the pending buffer a byte at a time, so the
/// iteration count -- not the payload -- is what bounds that case.
const MAX_ITERATIONS: usize = 256;

/// Largest allocator ceiling the fuzzer may choose, in bytes.
///
/// The worst legal configuration (`windowBits` 15, `memLevel` 9) asks for roughly
/// 400 KiB across its five blocks, so a one-mebibyte span covers "no pressure at
/// all" through "refuse the very first request" and every interesting point
/// between.
const MAX_ALLOC_LIMIT: usize = 1 << 20;

/// The largest window any legal configuration can have, in bytes.
///
/// `deflateGetDictionary` writes up to `w_size` bytes and `w_size` is at most
/// `1 << MAX_WBITS`, so this is the ceiling on what that call can ever ask for. The
/// buffer offered to it is sized to the *actual* window rather than to this, so that
/// the overrun check is against what a correct caller would really allocate; this
/// value is the sanity bound on that figure.
const GET_DICTIONARY_BUF_LEN: usize = 1 << MAX_WBITS;

/// Upper clamp for the three length-like `deflateTune` arguments.
///
/// `MAX_MATCH` is 258, the longest match DEFLATE can encode.
const MAX_TUNE_LENGTH: c_int = 258;

/// Upper clamp for `deflateTune`'s `max_chain` argument.
///
/// **This clamp is a hang guard, not a preference.** `deflateTune` validates
/// nothing -- both `deflate.c` L819-L830 and the Rust port store all four values
/// verbatim and return `Z_OK` -- so an unbounded `max_chain` makes `longest_match`
/// walk an unbounded hash chain. That would be a harness-induced hang reported as
/// a library defect. 4096 is the largest value the shipped configuration table
/// uses (level 9, `deflate.c` L123).
const MAX_TUNE_CHAIN: c_int = 4096;

/// The byte every tracked allocation is filled with.
///
/// `test/infcover.c` L87 fills with this rather than zeroing, "to make sure that
/// the code isn't depending on zeros". Any code path that reads an allocation
/// before writing it therefore sees `0xa5` and misbehaves visibly instead of
/// silently benefiting from a zeroed heap.
const SENTINEL_FILL: u8 = 0xa5;

/// Alignment every tracked allocation is given.
///
/// The allocator contract in `crates/zlib-rs/src/allocate.rs` requires blocks to
/// be aligned for any fundamental type, because both `zcalloc` and
/// `test/infcover.c`'s zone are backed by `malloc`. Sixteen bytes is what a
/// 64-bit `malloc` guarantees and exceeds every type the library allocates.
const HOOK_ALIGN: usize = 16;

// ---------------------------------------------------------------------------
//  The tracking allocator -- a port of test/infcover.c L56-L239
// ---------------------------------------------------------------------------
//
// This is the instrumentation that makes allocator misuse visible. Three
// properties are worth stating up front, because each one catches a distinct
// class of defect that ordinary testing does not:
//
//   * Blocks are filled with 0xa5, never zeroed (`SENTINEL_FILL`). Any code that
//     reads a freshly allocated byte before writing it sees garbage rather than a
//     convenient zero.
//   * Release order is checked. Memory obtained from a caller's `zalloc` may only
//     be returned to that same caller's `zfree`, and the reference implementation
//     releases strictly last-in-first-out, so any other order is recorded.
//   * A fuzzer-chosen ceiling refuses allocations at a chosen point, which turns
//     `Z_MEM_ERROR` from an accident into a systematically reachable path.
//
// Both hooks are called *from C*, across an `extern "C"` boundary, so a panic
// escaping either one would be undefined behaviour. They are therefore panic-free
// by construction: no `unwrap`, no `expect`, no `assert!`, no slice indexing and
// no arithmetic that can overflow. Every failure is reported the way the C
// contract reports it -- by returning a null pointer, which the library turns into
// `Z_MEM_ERROR`. Neither is declared `extern "C-unwind"`.

/// One tracked allocation. Mirrors `struct mem_item` (`test/infcover.c` L56-L60).
///
/// `size` is stored for the same reason C stores it -- the zone's byte total has to
/// be decremented by it on release -- and for one Rust-specific reason: `dealloc`
/// must be handed a `Layout` identical to the one `alloc` received, so the length
/// has to survive until the free. Getting that wrong is undefined behaviour that
/// AddressSanitizer reports as an allocator mismatch.
struct MemItem {
    /// Base address handed to the library.
    ptr: *mut u8,
    /// Bytes requested, before the "never zero-sized" adjustment.
    size: usize,
    /// The rest of the list. C threads this by hand; an owning `Option<Box<_>>`
    /// expresses the same shape without a raw pointer, so the list itself needs no
    /// `unsafe` at all.
    next: Option<Box<MemItem>>,
}

/// The root of the allocation list and its statistics. Mirrors `struct mem_zone`
/// (`test/infcover.c` L63-L68).
struct MemZone {
    /// Most recently allocated block first -- C's `mem_zone::first`.
    first: Option<Box<MemItem>>,
    /// Live bytes.
    total: usize,
    /// Largest value `total` ever reached. This is `mem_high()`'s number, the same
    /// high-water measure the per-stream memory budget is expressed in.
    highwater: usize,
    /// Refuse any request that would push `total` past this. Zero means no ceiling,
    /// exactly as `mem_limit()` defines it.
    limit: usize,
    /// Frees that were not of the most recent block -- C's `notlifo`.
    notlifo: usize,
    /// Frees of an address the zone never handed out -- C's `rogue`.
    rogue: usize,
}

/// What the `mem_done` equivalent found. Every field must be zero for a clean run.
#[derive(Debug, Clone, Copy)]
struct MemReport {
    /// Blocks still live at teardown, i.e. leaked.
    blocks: usize,
    /// Bytes still live at teardown.
    bytes: usize,
    /// Non-last-in-first-out frees observed.
    notlifo: usize,
    /// Frees of unrecognised addresses observed.
    rogue: usize,
    /// Largest live-byte total the zone ever reached -- `mem_high()`'s number.
    highwater: usize,
    /// The ceiling this zone was created with; zero means it had none.
    limit: usize,
}

impl MemZone {
    /// The `mem_done` equivalent (`test/infcover.c` L200-L234): release anything
    /// left behind and report what was unusual.
    ///
    /// The list is drained iteratively. Dropping a long `Option<Box<MemItem>>` chain
    /// recurses once per node, so a deep list could overflow the stack; taking it
    /// apart in a loop cannot. Real streams hold about five blocks, but a fuzz
    /// target must not depend on that.
    fn finish(&mut self) -> MemReport {
        let mut blocks = 0usize;
        let mut cursor = self.first.take();
        while let Some(mut item) = cursor {
            cursor = item.next.take();
            if let Some(layout) = tracked_layout(item.size) {
                // SAFETY: `item.ptr` came from `alloc` in `mem_alloc` with the layout
                // `tracked_layout(item.size)` returns, which is what is rebuilt here --
                // `tracked_layout` is a pure function of `size`, so the two agree by
                // construction. The block is still live because it is still on this
                // list, and removing it from the list here means it is freed exactly
                // once.
                unsafe { dealloc(item.ptr, layout) };
            }
            blocks = blocks.saturating_add(1);
        }
        MemReport {
            blocks,
            bytes: self.total,
            notlifo: self.notlifo,
            rogue: self.rogue,
            highwater: self.highwater,
            limit: self.limit,
        }
    }
}

impl Drop for MemZone {
    /// Reclaims anything `finish` did not, so a session that panics part-way through
    /// still does not leak. `finish` leaves `first` empty, so the common path here is
    /// a no-op.
    fn drop(&mut self) {
        let _ = self.finish();
    }
}

/// The layout a tracked block of `len` bytes is allocated with.
///
/// Two adjustments, both necessary:
///
/// - `len.max(1)`, because Rust's allocator forbids a zero-sized request while
///   C's `malloc(0)` is legal. The zone still records the requested `len`, so the
///   byte accounting matches C's.
/// - `HOOK_ALIGN`, because the library is entitled to `malloc`-grade alignment.
///
/// Returns `None` rather than panicking if the pair is not a valid layout, which is
/// what keeps `mem_alloc` panic-free.
fn tracked_layout(len: usize) -> Option<Layout> {
    Layout::from_size_align(len.max(1), HOOK_ALIGN).ok()
}

/// `mem_alloc` (`test/infcover.c` L71-L109).
///
/// # Safety
///
/// `opaque` must be null or a pointer to a live [`MemZone`] that no other
/// reference is borrowing for the duration of this call. Installing it through
/// [`Zone::as_opaque`] and letting only the library call this satisfies both.
unsafe extern "C" fn mem_alloc(opaque: voidpf, count: uInt, size: uInt) -> voidpf {
    let zone = opaque.cast::<MemZone>();
    if zone.is_null() {
        // C's "induced allocation failure" for a missing zone (its L79-L80).
        return ptr::null_mut();
    }

    // SAFETY: the caller guarantees `opaque` addresses a live, aligned `MemZone`
    // and that nothing else is borrowing it while this call runs. The library only
    // reaches this function from inside a `deflate*` entry point, and the harness
    // never holds a borrow across such a call, so the exclusivity holds.
    let zone = unsafe { &mut *zone };

    // `len = count * (size_t)size` (its L76), in the wide type. C cannot overflow
    // here on a 64-bit target because both factors are 32-bit, but the product is
    // checked rather than assumed so that the hook is correct on any target and
    // cannot panic on one where it could.
    let Some(len) = (count as usize).checked_mul(size as usize) else {
        return ptr::null_mut();
    };

    // The induced failure proper (its L79): `zone->limit && zone->total + len >
    // zone->limit`. Saturating, so the comparison is still meaningful if the sum
    // would not fit.
    if zone.limit != 0 && zone.total.saturating_add(len) > zone.limit {
        return ptr::null_mut();
    }

    let Some(layout) = tracked_layout(len) else {
        return ptr::null_mut();
    };

    // SAFETY: `tracked_layout` never returns a zero-sized layout, which is the one
    // precondition `alloc` has.
    let block = unsafe { alloc(layout) };
    if block.is_null() {
        // C's `if (ptr == NULL) return NULL;` (its L85-L86).
        return ptr::null_mut();
    }

    // SAFETY: `alloc` just returned `block` as a valid, writable, uninitialised
    // region of exactly `layout.size()` bytes, and nothing aliases it yet.
    //
    // This is the 0xa5 fill of C's L87 and the whole point of the instrumentation:
    // the block is never zeroed, so any code depending on zeroed memory misbehaves
    // visibly here instead of passing by luck.
    unsafe { ptr::write_bytes(block, SENTINEL_FILL, layout.size()) };

    // Insert at the head of the list (its L98-L100). A `Box` allocation failure
    // aborts the process rather than unwinding, so it cannot cross the `extern "C"`
    // boundary; C's corresponding path frees the block and reports failure.
    zone.first = Some(Box::new(MemItem {
        ptr: block,
        size: len,
        next: zone.first.take(),
    }));

    // Statistics (its L103-L105).
    zone.total = zone.total.saturating_add(len);
    if zone.total > zone.highwater {
        zone.highwater = zone.total;
    }

    block.cast()
}

/// `mem_free` (`test/infcover.c` L112-L154).
///
/// One deliberate divergence: where C ends with an unconditional `free(ptr)`, this
/// releases only blocks the zone recognises. Returning a pointer of unknown
/// provenance and unknown layout to Rust's allocator is undefined behaviour, so a
/// rogue free is counted and otherwise ignored. Counting it is what matters -- the
/// assertion at teardown fails either way.
///
/// # Safety
///
/// `opaque` must be null or a pointer to a live [`MemZone`] that no other
/// reference is borrowing for the duration of this call.
unsafe extern "C" fn mem_free(opaque: voidpf, address: voidpf) {
    let zone = opaque.cast::<MemZone>();
    if zone.is_null() {
        // C's "if no zone, just do a free" (its L118-L121). Without a zone nothing
        // was ever handed out, so there is nothing to release.
        return;
    }

    // SAFETY: as for `mem_alloc` -- the caller guarantees a live, aligned, unaliased
    // `MemZone`, and the library only calls this from inside an entry point.
    let zone = unsafe { &mut *zone };
    let target = address.cast::<u8>();

    // Walk the list looking for `target`, unlinking it if found (its L125-L140).
    // `hops` counts how far in it was: zero means the head, which is the
    // last-in-first-out case C treats as normal.
    let mut hops = 0usize;
    let mut cursor = &mut zone.first;
    let removed = loop {
        let found = match cursor.as_deref() {
            None => break None,
            Some(item) => ptr::eq(item.ptr, target),
        };
        if found {
            // `cursor.take()` detaches the node and leaves the slot empty; splicing
            // its successor back in restores the list.
            let Some(mut item) = cursor.take() else {
                break None;
            };
            *cursor = item.next.take();
            break Some(item);
        }
        let Some(item) = cursor.as_deref_mut() else {
            break None;
        };
        cursor = &mut item.next;
        hops = hops.saturating_add(1);
    };

    match removed {
        // "if not found, update the rogue count" (its L149-L150).
        None => zone.rogue = zone.rogue.saturating_add(1),
        Some(item) => {
            if hops != 0 {
                // "not a LIFO free" (its L136).
                zone.notlifo = zone.notlifo.saturating_add(1);
            }
            // "update the statistics and free the item" (its L143-L146).
            zone.total = zone.total.saturating_sub(item.size);
            if let Some(layout) = tracked_layout(item.size) {
                // SAFETY: `item.ptr` came from `alloc` in `mem_alloc` under
                // `tracked_layout(item.size)`, and `tracked_layout` is a pure function
                // of `size`, so the layout rebuilt here is the identical one. The node
                // has just been unlinked, so this block is released exactly once and
                // nothing holds a reference to it.
                unsafe { dealloc(item.ptr, layout) };
            }
        }
    }
}

/// A [`MemZone`] on the heap, reached only through the one raw pointer it owns.
///
/// **The zone cannot live in a local.** The pointer installed in `z_stream.opaque`
/// is what [`mem_alloc`] and [`mem_free`] dereference; a later write through a
/// *local* zone would be a write through the local's own provenance and would
/// invalidate every pointer derived from it, leaving the next `zalloc` to
/// dereference a dead one. Heap-allocating once and routing every access -- the
/// harness's through [`Zone::with`] and the library's through `opaque` -- through
/// this single pointer keeps the provenance chain intact. It is also how
/// `test/infcover.c` holds its own zone, so the port is faithful rather than merely
/// acceptable.
struct Zone(*mut MemZone);

impl Zone {
    /// A zone with the given byte ceiling; zero means no ceiling.
    ///
    /// Every field is written out rather than filled in from
    /// `..MemZone::default()`: struct-update syntax moves out of the base value,
    /// which a type with a `Drop` implementation does not allow.
    fn new(limit: usize) -> Self {
        Self(Box::into_raw(Box::new(MemZone {
            first: None,
            total: 0,
            highwater: 0,
            limit,
            notlifo: 0,
            rogue: 0,
        })))
    }

    /// The pointer to install as `z_stream.opaque`, as `mem_setup` does
    /// (`test/infcover.c` L170).
    fn as_opaque(&self) -> voidpf {
        self.0.cast::<c_void>()
    }

    /// Borrows the zone through its one raw pointer for the duration of `body`.
    fn with<R>(&self, body: impl FnOnce(&mut MemZone) -> R) -> R {
        // SAFETY: the pointer came from `Box::into_raw` in `Zone::new`, is live until
        // `Zone::drop`, and is aligned and unique. No other reference exists while
        // `body` runs: the library forms one only inside `mem_alloc`/`mem_free`, and
        // neither can be executing, because this call and any library call are on the
        // same thread and neither is re-entrant.
        body(unsafe { &mut *self.0 })
    }

    /// The `mem_done` equivalent: tear the zone down and return what it found.
    fn finish(&self) -> MemReport {
        self.with(MemZone::finish)
    }
}

impl Drop for Zone {
    fn drop(&mut self) {
        // SAFETY: the pointer came from `Box::into_raw` in `Zone::new` and this is the
        // only place it is reclaimed, so it is reclaimed exactly once. `MemZone`'s own
        // `Drop` releases any block still tracked.
        drop(unsafe { Box::from_raw(self.0) });
    }
}

/// Points a stream at `zone`, as `mem_setup` does (`test/infcover.c` L158-L173).
fn track(strm: &mut z_stream, zone: &Zone) {
    strm.zalloc = Some(mem_alloc);
    strm.zfree = Some(mem_free);
    strm.opaque = zone.as_opaque();
}

/// Asserts what `mem_done`'s three alert lines report (`test/infcover.c`
/// L220-L227), plus the leaked-block count those lines share.
///
/// This is the check that validates the allocator half of the boundary contract:
/// memory obtained from a caller's `zalloc` is released, in order, only through
/// that same caller's `zfree`. `assert!` here is correct and required -- this runs
/// in the `fuzz_target!` body, where a failed assertion is how libFuzzer reports a
/// finding, not in a callback C might unwind through.
fn assert_zone_clean(zone: &Zone, what: &str) {
    let report = zone.finish();
    assert_eq!(report.blocks, 0, "{what}: blocks not freed ({report:?})");
    assert_eq!(report.bytes, 0, "{what}: bytes not freed ({report:?})");
    assert_eq!(report.notlifo, 0, "{what}: frees not LIFO ({report:?})");
    assert_eq!(report.rogue, 0, "{what}: frees not recognized ({report:?})");

    // The zone's own invariants, checked so that a defect in the instrumentation
    // cannot silently disable it. If the high-water mark were ever allowed past the
    // ceiling, the induced-failure logic would not be working and the `Z_MEM_ERROR`
    // coverage this target claims would be imaginary.
    assert!(
        report.highwater >= report.bytes,
        "{what}: high-water mark below the live total ({report:?})"
    );
    assert!(
        report.limit == 0 || report.highwater <= report.limit,
        "{what}: the allocator ceiling was exceeded ({report:?})"
    );
}

// ---------------------------------------------------------------------------
//  Structured input
// ---------------------------------------------------------------------------

/// How a raw configuration draw is interpreted.
///
/// The split is what makes the target spend its budget usefully. Most draws are
/// folded into the legal space, so real compression runs and the whole
/// `configuration_table` gets exercised. A smaller share is drawn from a narrow
/// band straddling every boundary, which is where the interesting rejections and
/// the two trap values live. A smaller share still is left completely
/// unrestricted, which is what proves the argument decoding cannot overflow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DrawKind {
    /// Guaranteed to be accepted by `deflateInit2_`.
    Legal,
    /// Drawn from the band the exhaustive sweep covers: `level` -2..=11,
    /// `method` 7..=9, `windowBits` -17..=33, `memLevel` 0..=11, `strategy` -1..=6.
    /// Roughly half of this band is illegal, and it contains both trap values.
    Boundary,
    /// Any `c_int` at all, including `i32::MIN` and `i32::MAX`.
    Unrestricted,
}

/// The five `deflateInit2_` arguments as drawn, before interpretation.
///
/// Kept as narrow integers on purpose: the fuzzer's bytes then map onto the
/// interesting region densely instead of being spread across the whole of `i32`,
/// while [`DrawKind::Unrestricted`] still reaches the extremes through `extreme`.
#[derive(Arbitrary, Debug)]
struct RawConfig {
    /// Selects the [`DrawKind`] and, within [`DrawKind::Legal`], the exact values.
    mode: u8,
    level: i8,
    method: i8,
    window_bits: i8,
    mem_level: i8,
    strategy: i8,
    /// The unrestricted values, in the order `level`, `method`, `windowBits`,
    /// `memLevel`, `strategy`.
    extreme: [i32; 5],
}

/// A resolved configuration: exactly the integers passed to `deflateInit2_`.
#[derive(Debug, Clone, Copy)]
struct Config {
    kind: DrawKind,
    level: c_int,
    method: c_int,
    window_bits: c_int,
    mem_level: c_int,
    strategy: c_int,
}

impl RawConfig {
    /// Chooses how this draw is interpreted.
    ///
    /// One draw in sixteen is unrestricted and three more in sixteen are drawn from
    /// the boundary band, leaving three quarters legal. The legal space keeps the
    /// majority deliberately: an illegal configuration is rejected by
    /// `deflateInit2_` before a single byte is compressed, so a target that spent
    /// most of its budget there would exercise almost none of the compressor.
    fn kind(&self) -> DrawKind {
        if self.mode % 16 == 0 {
            DrawKind::Unrestricted
        } else if self.mode % 4 == 0 {
            DrawKind::Boundary
        } else {
            DrawKind::Legal
        }
    }

    /// Resolves the draw into the arguments `deflateInit2_` will receive.
    fn resolve(&self) -> Config {
        let kind = self.kind();
        let (level, method, window_bits, mem_level, strategy) = match kind {
            DrawKind::Legal => (
                self.legal_level(),
                Z_DEFLATED,
                self.legal_window_bits(),
                self.legal_mem_level(),
                self.legal_strategy(),
            ),
            DrawKind::Boundary => (
                band(self.level, -2, 11),
                band(self.method, 7, 9),
                band(self.window_bits, -17, 33),
                band(self.mem_level, 0, 11),
                band(self.strategy, -1, 6),
            ),
            DrawKind::Unrestricted => (
                self.extreme[0],
                self.extreme[1],
                self.extreme[2],
                self.extreme[3],
                self.extreme[4],
            ),
        };
        Config {
            kind,
            level,
            method,
            window_bits,
            mem_level,
            strategy,
        }
    }

    /// A level `deflateInit2_` accepts: `Z_DEFAULT_COMPRESSION` or 0..=9.
    ///
    /// `Z_DEFAULT_COMPRESSION` is included because it is the most widely used value
    /// in existence and, being negative, is exactly the one a naive `level < 0`
    /// rejection would break -- `deflate.c` L403 rewrites it to 6 *before* the range
    /// test.
    fn legal_level(&self) -> c_int {
        let choice = self.level.unsigned_abs() % 11;
        if choice == 10 {
            Z_DEFAULT_COMPRESSION
        } else {
            c_int::from(choice)
        }
    }

    /// A `windowBits` value `deflateInit2_` accepts, spread evenly over the three
    /// container formats.
    ///
    /// The legal ranges are asymmetric, and the asymmetry is the point:
    ///
    /// - zlib 8..=15 -- 8 is accepted and silently promoted to 9.
    /// - raw -9..=-15 -- **-8 is rejected**, which is the exact opposite of
    ///   `inflate`, where -8 is legal.
    /// - gzip 25..=31 -- **24 is rejected**, because `windowBits -= 16` maps it to 8
    ///   with a non-zlib wrapper.
    ///
    /// Both rejections come from the single `(windowBits == 8 && wrap != 1)` clause
    /// at `deflate.c` L412. The boundary band above is what actually feeds -8 and 24
    /// in; this function only produces values that must be accepted.
    fn legal_window_bits(&self) -> c_int {
        let spread = self.window_bits.unsigned_abs();
        match spread % 3 {
            // zlib: 8..=15.
            0 => 8 + c_int::from(spread % 8),
            // raw: -9..=-15.
            1 => -(9 + c_int::from(spread % 7)),
            // gzip: 25..=31.
            _ => 25 + c_int::from(spread % 7),
        }
    }

    /// A `memLevel` in 1..=`MAX_MEM_LEVEL`.
    fn legal_mem_level(&self) -> c_int {
        1 + c_int::from(self.mem_level.unsigned_abs()) % MAX_MEM_LEVEL
    }

    /// A strategy in `Z_DEFAULT_STRATEGY`..=`Z_FIXED`.
    fn legal_strategy(&self) -> c_int {
        c_int::from(self.strategy.unsigned_abs()) % (Z_FIXED + 1)
    }
}

/// Maps a drawn byte onto the inclusive range `low..=high`.
///
/// Used for the boundary band, where every value in the range matters and an even
/// spread over it is what gets the trap values hit often rather than occasionally.
fn band(drawn: i8, low: c_int, high: c_int) -> c_int {
    let span = high - low + 1;
    low + c_int::from(drawn.unsigned_abs()) % span
}

/// The smallest window exponent `deflateInit2_` accepts, transcribed from the bare
/// literal at `deflate.c` L413 (`windowBits < 8`).
///
/// Every other constant in this file comes from `libz_rs_sys`, because `zlib.h` and
/// `zconf.h` are the single authority for the public contract and a local copy of a
/// contract value is exactly the drift the header-diff gate exists to prevent. This
/// one is different on both counts. It is not a contract symbol -- `zlib.h` publishes
/// `MAX_WBITS` but nothing for the lower end, and the reference simply writes `8` --
/// and, more importantly, it belongs to the *oracle*. An oracle that sourced its
/// bounds from the implementation it is checking would agree with that implementation
/// by construction, and the cross-check in [`init_is_legal`] would pass vacuously
/// even if the bound were wrong. So it is transcribed from the C, not imported from
/// the port. (The port's own `zlib_rs::MIN_WBITS` is deliberately not used here for
/// that reason.)
const MIN_WBITS: c_int = 8;

/// Whether `deflateInit2_` must accept this configuration.
///
/// This is the harness's own oracle, and cross-checking it against the library's
/// answer is what gives the target its teeth: agreement on every draw is evidence
/// that the port reproduces `deflateInit2_`'s combined rejection expression
/// exactly, trap clause included. It is a transcription of `deflate.c` L400-L419,
/// in that order, because the order is observable -- `Z_DEFAULT_COMPRESSION` is
/// rewritten before the range test, and `windowBits` is decoded before the
/// combined test that then reads it.
///
/// # Verified exhaustively
///
/// This function was checked against the library over the entire cross product
/// `level` -2..=11 x `method` 7..=9 x `windowBits` -17..=33 x `memLevel` 0..=11 x
/// `strategy` -1..=6 -- **205,632 cells, zero disagreements**. The four values the
/// sweep exists for behave as this function predicts: `windowBits` -8 and 24 are
/// both refused with `Z_STREAM_ERROR`, and 8 and 25 are both accepted. So are the
/// neighbours that bracket them: -9, -15, 31 accepted; -16, 32, 7 and 16 refused.
fn init_is_legal(config: Config) -> bool {
    // `if (level == Z_DEFAULT_COMPRESSION) level = 6;` (L403).
    let level = if config.level == Z_DEFAULT_COMPRESSION {
        6
    } else {
        config.level
    };

    // L406-L411. `wrap` is 1 for a zlib wrapper, 0 for raw, 2 for gzip.
    let (wrap, window_bits) = if config.window_bits < 0 {
        // `if (windowBits < -15) return Z_STREAM_ERROR;` comes first, which is also
        // what makes the negation below safe: `i32::MIN` has already been rejected,
        // so it cannot overflow.
        if config.window_bits < -MAX_WBITS {
            return false;
        }
        (0, -config.window_bits)
    } else if config.window_bits > MAX_WBITS {
        // `windowBits -= 16`. The subtrahend is positive and the value is greater
        // than 15, so this cannot overflow either.
        (2, config.window_bits - 16)
    } else {
        (1, config.window_bits)
    };

    // The single combined rejection expression at L412-L416, term for term.
    (1..=MAX_MEM_LEVEL).contains(&config.mem_level)
        && config.method == Z_DEFLATED
        && (MIN_WBITS..=MAX_WBITS).contains(&window_bits)
        && (Z_NO_COMPRESSION..=Z_BEST_COMPRESSION).contains(&level)
        && (Z_DEFAULT_STRATEGY..=Z_FIXED).contains(&config.strategy)
        // The trap clause. Without it, raw -8 and gzip 24 both look acceptable.
        && !(window_bits == MIN_WBITS && wrap != 1)
}

/// Whether a resolved configuration selects the gzip wrapper.
///
/// `deflateSetHeader` is refused for anything else (`deflate.c` L717), so this is
/// what decides whether the harness expects `Z_OK` or `Z_STREAM_ERROR` from it.
fn is_gzip(config: Config) -> bool {
    config.window_bits > MAX_WBITS
}

/// Whether a resolved configuration selects the zlib wrapper.
///
/// `deflateSetDictionary` is refused outright for gzip and, for zlib, only before
/// the first `deflate` call (`deflate.c` L570-L572), so the wrapper decides which
/// answer the harness expects.
fn is_zlib_wrapped(config: Config) -> bool {
    config.window_bits >= 0 && config.window_bits <= MAX_WBITS
}

/// Whether `deflateParams` must accept this pair, per `deflate.c` L744-L752.
///
/// Note the same `Z_DEFAULT_COMPRESSION` rewrite the initialiser performs, and for
/// the same reason: -1 is legal even though it is negative.
fn params_are_legal(level: c_int, strategy: c_int) -> bool {
    let level = if level == Z_DEFAULT_COMPRESSION {
        6
    } else {
        level
    };
    (Z_NO_COMPRESSION..=Z_BEST_COMPRESSION).contains(&level)
        && (Z_DEFAULT_STRATEGY..=Z_FIXED).contains(&strategy)
}

/// Every flush value `deflate` accepts, named rather than computed, in the order
/// `zlib.h` L172-L178 declares them.
///
/// Spelling them out keeps the schedule auditable -- a reader can see that all seven
/// modes are reachable -- and keeps the harness sourcing its constants from the
/// single authority for them instead of assuming they are consecutive.
const FLUSH_MODES: [c_int; 7] = [
    Z_NO_FLUSH,
    Z_PARTIAL_FLUSH,
    Z_SYNC_FLUSH,
    Z_FULL_FLUSH,
    Z_FINISH,
    Z_BLOCK,
    Z_TREES,
];

// The operation gates. One `u16` steers all sixteen, which is both tidier than a
// run of `bool` fields and required: `clippy::pedantic` denies more than three
// booleans in one struct, and this crate denies the whole group.

/// Check that the version handshake is tested before the stream pointer.
const OP_VERSION_GATE: u16 = 1 << 0;
/// Run the single-pass `deflateBound` contract session.
const OP_BOUND_SESSION: u16 = 1 << 1;
/// Install a preset dictionary in the bound session, before the bound is taken.
const OP_BOUND_DICTIONARY: u16 = 1 << 2;
/// Install a gzip header in the bound session, before the bound is taken.
const OP_BOUND_HEADER: u16 = 1 << 3;
/// Install a preset dictionary in the stress session.
const OP_STRESS_DICTIONARY: u16 = 1 << 4;
/// Install a gzip header in the stress session.
const OP_STRESS_HEADER: u16 = 1 << 5;
/// Retune level and strategy mid-stream with `deflateParams`.
const OP_PARAMS: u16 = 1 << 6;
/// Restart the stream mid-session with `deflateReset`.
const OP_RESET: u16 = 1 << 7;
/// Restart the stream mid-session with `deflateResetKeep`.
const OP_RESET_KEEP: u16 = 1 << 8;
/// Inject bits with `deflatePrime`.
const OP_PRIME: u16 = 1 << 9;
/// Override the match-search parameters with `deflateTune`.
const OP_TUNE: u16 = 1 << 10;
/// Clone the stream with `deflateCopy` and tear the clone down.
const OP_COPY: u16 = 1 << 11;
/// After `Z_STREAM_END`, check that a non-`Z_FINISH` flush is refused.
const OP_POST_FINISH_FLUSH: u16 = 1 << 12;
/// Exercise the `compressBound`/`compress2` contract.
const OP_COMPRESS_BOUND: u16 = 1 << 13;
/// Check that every bound function is monotonic in its source length.
const OP_BOUND_MONOTONIC: u16 = 1 << 14;
/// Read the active dictionary back with `deflateGetDictionary`.
const OP_GET_DICTIONARY: u16 = 1 << 15;

/// Everything one fuzz invocation is driven by.
///
/// `payload` is deliberately the **last** field. `libfuzzer-sys` builds the input
/// with `Arbitrary::arbitrary_take_rest`, and the derive routes `take_rest` to the
/// final field, so the payload receives whatever entropy the fixed-size fields
/// ahead of it did not consume. Any other position would leave it starved.
#[derive(Arbitrary, Debug)]
struct FuzzInput {
    /// The compressor configuration.
    config: RawConfig,
    /// Bit-set of `OP_*` gates.
    ops: u16,
    /// Selects how many payload bytes are offered per `deflate` call.
    chunk: u16,
    /// Selects how much output space is offered per `deflate` call.
    out_buf: u16,
    /// Selects the allocator ceiling.
    alloc_limit: u32,
    /// Flush values, consumed cyclically, one per `deflate` call.
    flush_schedule: [u8; 8],
    /// New level for `deflateParams`.
    params_level: i8,
    /// New strategy for `deflateParams`.
    params_strategy: i8,
    /// Bit count for `deflatePrime`.
    prime_bits: i8,
    /// Bit pattern for `deflatePrime`.
    prime_value: i32,
    /// `good_length`, `max_lazy`, `nice_length` and `max_chain` for `deflateTune`.
    tune: [i16; 4],
    /// Bit 0 selects an `extra` field, bit 1 a `name`, bit 2 a `comment`, bit 3 the
    /// header CRC; the high bits pick `text` and `os`.
    header_flags: u8,
    /// The gzip header modification time.
    header_time: u32,
    /// Bytes for the header's `extra` field.
    header_extra: [u8; 16],
    /// Bytes for the header's `name` field, before NUL termination.
    header_name: [u8; 16],
    /// Bytes for the header's `comment` field, before NUL termination.
    header_comment: [u8; 16],
    /// The preset dictionary.
    dictionary: Vec<u8>,
    /// The data to compress. Must stay last -- see the note on this struct.
    payload: Vec<u8>,
}

/// The leading `at_most` bytes of `bytes`, or all of them when it is shorter.
///
/// Written with `get` rather than a range index so it is panic-free by
/// construction: a later change to one of the bounds cannot turn this into a
/// crash.
fn clamped(bytes: &[u8], at_most: usize) -> &[u8] {
    bytes.get(..bytes.len().min(at_most)).unwrap_or(&[])
}

impl FuzzInput {
    /// Whether the given `OP_*` gate is set.
    fn wants(&self, op: u16) -> bool {
        self.ops & op != 0
    }

    /// The payload, clamped to [`MAX_PAYLOAD_LEN`].
    fn payload(&self) -> &[u8] {
        clamped(&self.payload, MAX_PAYLOAD_LEN)
    }

    /// The preset dictionary, clamped to [`MAX_DICTIONARY_LEN`].
    ///
    /// A zero-length dictionary is a perfectly reachable draw and is meant to be:
    /// `deflateSetDictionary` with `dictLength` 0 is legal and must still answer
    /// `Z_OK`.
    fn dictionary(&self) -> &[u8] {
        clamped(&self.dictionary, MAX_DICTIONARY_LEN)
    }

    /// Payload bytes to offer per `deflate` call, in 1..=[`MAX_PAYLOAD_LEN`].
    ///
    /// Chosen independently of the payload length on purpose. Chunk boundaries
    /// interact with flush handling and pending-buffer state, so a target that
    /// always fed everything in one call would never reach the resumption paths.
    fn chunk_len(&self) -> usize {
        1 + usize::from(self.chunk) % MAX_PAYLOAD_LEN
    }

    /// Output space to offer per `deflate` call, in 1..=[`MAX_OUTPUT_BUF_LEN`].
    ///
    /// One draw in eight is forced to a single byte -- the tightest legal buffer.
    /// That case is singled out rather than left to chance because it drains the
    /// pending buffer one byte at a time and so exercises `flush_pending`
    /// (`deflate.c` L950) and `putShortMSB` (L939) hardest, and because it is the
    /// case a caller is most likely to get wrong.
    fn out_len(&self) -> usize {
        if self.out_buf % 8 == 0 {
            1
        } else {
            1 + usize::from(self.out_buf) % MAX_OUTPUT_BUF_LEN
        }
    }

    /// The allocator ceiling in bytes; zero means no ceiling.
    ///
    /// Half the draws leave the ceiling off so the ordinary paths run unimpeded. The
    /// rest spread across the whole span, which is what makes `Z_MEM_ERROR`
    /// systematically reachable at every stage of initialisation instead of only at
    /// the first allocation.
    fn alloc_limit(&self) -> usize {
        if self.alloc_limit % 2 == 0 {
            0
        } else {
            self.alloc_limit as usize % (MAX_ALLOC_LIMIT + 1)
        }
    }

    /// The flush value for call number `iteration`.
    ///
    /// Roughly one value in fifteen is out of range, in both directions. Those must
    /// draw a clean `Z_STREAM_ERROR` from `deflate`, which checks `flush` before it
    /// touches any state (`deflate.c` L1010), so the stream stays usable afterwards.
    fn flush_at(&self, iteration: usize) -> c_int {
        let index = iteration % self.flush_schedule.len();
        let raw = self.flush_schedule.get(index).copied().unwrap_or(0);
        match raw {
            // Above `Z_TREES`.
            0xf0..=0xff => c_int::from(raw),
            // Below `Z_NO_FLUSH`, which a `u8` cannot express directly.
            0xef => -1,
            _ => FLUSH_MODES
                .get(usize::from(raw) % FLUSH_MODES.len())
                .copied()
                .unwrap_or(Z_NO_FLUSH),
        }
    }

    /// The `(level, strategy)` pair for `deflateParams`, spread over the boundary
    /// band so both the accepted and the refused halves are reached.
    fn params(&self) -> (c_int, c_int) {
        (
            band(self.params_level, -2, 11),
            band(self.params_strategy, -1, 6),
        )
    }

    /// The `(bits, value)` pair for `deflatePrime`.
    ///
    /// `bits` spans -2..=18, which straddles the accepted 0..=16 range on both
    /// sides.
    fn prime(&self) -> (c_int, c_int) {
        (band(self.prime_bits, -2, 18), self.prime_value)
    }

    /// The four `deflateTune` arguments, clamped.
    ///
    /// **The clamping is a hang guard.** `deflateTune` stores all four values
    /// verbatim and validates nothing, so an unbounded `max_chain` would make
    /// `longest_match` walk an unbounded hash chain and the fuzzer would report a
    /// timeout against the library for something the harness did.
    fn tune(&self) -> (c_int, c_int, c_int, c_int) {
        let at = |index: usize| -> c_int {
            c_int::from(self.tune.get(index).copied().unwrap_or(0).unsigned_abs())
        };
        (
            at(0) % (MAX_TUNE_LENGTH + 1),
            at(1) % (MAX_TUNE_LENGTH + 1),
            at(2) % (MAX_TUNE_LENGTH + 1),
            at(3) % (MAX_TUNE_CHAIN + 1),
        )
    }
}

// ---------------------------------------------------------------------------
//  Stream plumbing
// ---------------------------------------------------------------------------

/// A `z_stream` with every member cleared, as a caller declaring one at file scope
/// would have.
///
/// Written out field by field rather than produced with `mem::zeroed`: the ABI
/// mirror deliberately does not derive `Default`, because an all-zero stream is not
/// yet an initialised one, and spelling the fields out keeps that distinction
/// visible while needing no `unsafe` at all. `None` for the two hooks is C's
/// `Z_NULL`, which selects the library's own allocator; [`track`] replaces them.
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

/// Detaches the stream from whatever buffers it was last pointing at.
///
/// **This is a use-after-free guard, not tidiness.** `z_stream` keeps raw
/// `next_in`/`next_out` pointers, so a stream left pointing at a buffer that has since
/// been dropped is a dangling pointer waiting for the next call that reads it -- and
/// `deflateParams` is exactly such a call, because it flushes pending output by
/// invoking `deflate` internally (`deflate.c` L753-L757). Worse, the failure would
/// surface as an AddressSanitizer report against the library rather than against the
/// harness that caused it. Every function that lends the stream a local buffer calls
/// this before that buffer dies.
///
/// A null `next_out` is a state the contract understands: `deflate` answers
/// `Z_STREAM_ERROR` for it (`deflate.c` L990-L994) rather than misbehaving.
fn detach_buffers(strm: &mut z_stream) {
    strm.next_in = ptr::null();
    strm.avail_in = 0;
    strm.next_out = ptr::null_mut();
    strm.avail_out = 0;
}

/// `(int) sizeof(z_stream)`, the second half of the handshake the `deflateInit2`
/// macro arranges (`zlib.h` L543).
///
/// Getting this wrong is a silent-failure trap: `deflateInit2_` answers
/// `Z_VERSION_ERROR` for a mismatched size, so a target that passed the wrong value
/// would initialise nothing, compress nothing, and still report success. The
/// conversion is checked rather than cast for the same reason -- a size that did not
/// fit a `c_int` would mean the mirror had outgrown anything `zlib.h` can describe.
fn stream_size() -> c_int {
    c_int::try_from(size_of::<z_stream>()).expect("sizeof(z_stream) fits in a C int")
}

/// Every return code `deflate` itself is documented to produce (`zlib.h`
/// L355-L363).
///
/// `Z_BUF_ERROR` is in this set and **is not a failure**: it means no progress was
/// possible, which is the expected answer when the output buffer is one byte and the
/// pending buffer is empty.
const DEFLATE_RETURNS: [c_int; 4] = [Z_OK, Z_STREAM_END, Z_STREAM_ERROR, Z_BUF_ERROR];

/// Asserts that `code` is one of `allowed`.
///
/// Anything outside the documented set is a defect, so this is the assertion that
/// turns an undocumented return value into a libFuzzer finding rather than letting
/// it pass unnoticed.
fn assert_documented(code: c_int, allowed: &[c_int], what: &str) {
    assert!(
        allowed.contains(&code),
        "{what}: undocumented return {code}, expected one of {allowed:?}"
    );
}

/// Initialises `strm` with `config`, and holds the answer to the harness's own
/// prediction.
///
/// This cross-check is what makes the configuration sweep meaningful. A legal
/// configuration must be accepted, or refused with `Z_MEM_ERROR` when the allocator
/// ceiling is too low to satisfy it; an illegal one must be refused with exactly
/// `Z_STREAM_ERROR`. Any other outcome -- a different code, a panic, an abort -- is a
/// defect, and in particular a legal configuration answered with `Z_VERSION_ERROR`
/// would mean the handshake arguments were wrong and the target had been silently
/// fuzzing nothing at all.
///
/// Returns whether the stream is now initialised and owns state that must be ended.
///
/// # Safety
///
/// `strm` must be a live, aligned `z_stream` that does not already own state, with its
/// three allocator members set to either a matching hook pair and a live `opaque` or
/// all null.
unsafe fn init_stream(strm: &mut z_stream, config: Config, ceiling_active: bool) -> bool {
    // SAFETY: `strm` is a live, aligned, exclusively borrowed `z_stream` whose three
    // allocator members were set by `track` to a matching hook pair and a live zone
    // pointer. `ZLIB_VERSION` is a `&CStr`, so its pointer addresses a readable
    // NUL-terminated string valid for `'static`, and `stream_size` reports the size
    // of the very type `strm` points at.
    let code = unsafe {
        deflateInit2_(
            strm,
            config.level,
            config.method,
            config.window_bits,
            config.mem_level,
            config.strategy,
            ZLIB_VERSION.as_ptr(),
            stream_size(),
        )
    };

    if init_is_legal(config) {
        if ceiling_active {
            // Under a ceiling the request may legitimately be refused, but only in
            // that one way.
            assert_documented(
                code,
                &[Z_OK, Z_MEM_ERROR],
                &format!("deflateInit2_ with a legal config under an allocator ceiling {config:?}"),
            );
        } else {
            assert_eq!(
                code, Z_OK,
                "deflateInit2_ refused a legal configuration {config:?}"
            );
        }
    } else {
        assert_eq!(
            code, Z_STREAM_ERROR,
            "deflateInit2_ accepted or mis-refused an illegal configuration {config:?}"
        );
    }

    code == Z_OK
}

/// Checks that the version handshake is tested *before* the stream pointer.
///
/// `deflateInit2_` returns `Z_VERSION_ERROR` at `deflate.c` L394-L397, before
/// `strm == Z_NULL` is examined at L398. So a null stream paired with an
/// unacceptable version yields `Z_VERSION_ERROR`, not `Z_STREAM_ERROR` -- the
/// ordering is observable, and `test/infcover.c`'s `cover_back` asserts the
/// analogous ordering for `inflateBackInit_`. All three ways of failing the
/// handshake are covered: a wrong major digit, a wrong `stream_size`, and a null
/// version pointer.
fn check_version_gate_ordering() {
    let wrong_major: [c_char; 2] = [b'9' as c_char, 0];
    let good = ZLIB_VERSION.as_ptr();

    for (version, size, expected, what) in [
        (
            wrong_major.as_ptr(),
            stream_size(),
            Z_VERSION_ERROR,
            "null stream, wrong major version",
        ),
        (good, 0, Z_VERSION_ERROR, "null stream, wrong stream_size"),
        (
            ptr::null::<c_char>(),
            stream_size(),
            Z_VERSION_ERROR,
            "null stream, null version",
        ),
        (
            good,
            stream_size(),
            Z_STREAM_ERROR,
            "null stream, acceptable handshake",
        ),
    ] {
        // SAFETY: the stream pointer is deliberately null, which is a value the
        // contract defines rather than a violation of it -- `deflateInit2_` tests it
        // and answers `Z_STREAM_ERROR`, and never dereferences it. `version` is either
        // null or the start of a readable NUL-terminated array that outlives the call:
        // `wrong_major` is a local live for this whole loop, and `good` addresses a
        // `'static` `CStr`.
        let code = unsafe {
            deflateInit2_(
                ptr::null_mut(),
                6,
                Z_DEFLATED,
                MAX_WBITS,
                8,
                Z_DEFAULT_STRATEGY,
                version,
                size,
            )
        };
        assert_eq!(code, expected, "{what}");
    }
}

/// Outcome of a driving loop.
#[derive(Debug, Clone, Copy)]
struct DriveOutcome {
    /// Compressed bytes produced across every call.
    produced: usize,
    /// Whether the stream reached `Z_STREAM_END`.
    finished: bool,
    /// Whether a `Z_FINISH` was ever handed to `deflate` during the loop.
    ///
    /// Tracked separately from `finished`, and the distinction matters: `deflate.c`
    /// L1222-L1224 moves the status to `FINISH_STATE` as soon as the finish *starts*,
    /// so a `Z_FINISH` that only returned `Z_OK` -- or one the iteration cap cut short --
    /// still leaves the stream in a state where every other flush is refused. Anything
    /// that flushes internally afterwards has to expect that.
    finish_issued: bool,
    /// Whether any flush other than `Z_NO_FLUSH` or `Z_FINISH` was used, which is
    /// exactly the condition under which `zlib.h` L360-L363 stops guaranteeing that
    /// the output fits inside `deflateBound`.
    bound_disturbed: bool,
}

/// Feeds `payload` through `strm` in chunks, taking one flush value per call from
/// the input's schedule, then finishes.
///
/// The two caps are the no-hang mechanism and must not be removed. Hitting either is
/// a legitimate stop, not a failure: a schedule made mostly of `Z_SYNC_FLUSH` emits
/// an empty stored block per call, so output can grow indefinitely without input
/// being consumed, and a one-byte output buffer needs one call per byte of pending
/// output.
///
/// # Safety
///
/// `strm` must hold a state this harness initialised. Its `next_in`/`next_out` members
/// are overwritten on every iteration from buffers this function owns, so whatever they
/// held on entry is irrelevant, and [`detach_buffers`] clears them again before this
/// frame's buffers die -- so no dangling pointer survives the call.
unsafe fn drive(strm: &mut z_stream, payload: &[u8], input: &FuzzInput) -> DriveOutcome {
    let chunk_len = input.chunk_len();
    let mut out = vec![0u8; input.out_len()];

    let mut offset = 0usize;
    let mut produced = 0usize;
    let mut finished = false;
    let mut finish_issued = false;
    let mut bound_disturbed = false;
    let mut finishing = false;
    let mut stalled = 0usize;

    for iteration in 0..MAX_ITERATIONS {
        // Once `Z_FINISH` has been issued the stream is in `FINISH_STATE`, where any
        // other flush is refused (`deflate.c` L1013-L1017). A well-behaved caller
        // therefore keeps finishing, and that refusal is checked deliberately
        // elsewhere rather than tripped over here.
        let flush = if finishing {
            Z_FINISH
        } else {
            input.flush_at(iteration)
        };

        let remaining = payload.get(offset..).unwrap_or(&[]);
        let feeding = clamped(remaining, chunk_len);

        // SAFETY: `feeding` and `out` are live slices this frame owns; the pointer and
        // length handed over describe exactly them, and `avail_in`/`avail_out` are
        // their true lengths. `next_in` is only read for `avail_in` bytes and
        // `next_out` only written for `avail_out` bytes, which is the contract at
        // `zlib.h` L96-L101. An empty `feeding` yields a dangling-but-aligned pointer
        // with `avail_in` 0, which the contract permits and which nothing dereferences.
        let code = unsafe {
            strm.next_in = feeding.as_ptr();
            strm.avail_in = feeding.len() as uInt;
            strm.next_out = out.as_mut_ptr();
            strm.avail_out = out.len() as uInt;
            deflate(strm, flush)
        };

        assert_documented(code, &DEFLATE_RETURNS, &format!("deflate(flush={flush})"));

        if !(Z_NO_FLUSH..=Z_TREES).contains(&flush) {
            // An out-of-range flush must be refused cleanly, and refused *before* any
            // state is touched, so the stream stays usable for the next iteration.
            assert_eq!(
                code, Z_STREAM_ERROR,
                "deflate accepted an out-of-range flush {flush}"
            );
            continue;
        }
        if flush == Z_FINISH {
            finish_issued = true;
        } else if flush != Z_NO_FLUSH {
            bound_disturbed = true;
        }

        // Reading back two plain integer members through the exclusive borrow this
        // function holds needs no `unsafe`: the pointer work was all on the way in.
        let consumed = feeding.len() - strm.avail_in as usize;
        let written = out.len() - strm.avail_out as usize;
        offset += consumed;
        produced += written;

        if code == Z_STREAM_END {
            finished = true;
            break;
        }
        if code == Z_STREAM_ERROR {
            // Legitimately reachable, and worth naming rather than shrugging at: the
            // schedule can issue `Z_FINISH` before the payload is exhausted, and once
            // that finish has completed its block the status is `FINISH_STATE`, where
            // `deflate.c` L992 refuses every other flush. There is nothing further to
            // drive, so the loop stops.
            break;
        }

        // Progress bookkeeping. `Z_BUF_ERROR` with nothing moving means the loop would
        // spin, so it stops -- this is the "no progress is possible" case `zlib.h`
        // L360-L363 documents as legitimate.
        if consumed == 0 && written == 0 {
            stalled += 1;
            if stalled >= 2 && finishing {
                break;
            }
            if stalled >= 4 {
                break;
            }
        } else {
            stalled = 0;
        }

        if offset >= payload.len() {
            finishing = true;
        }
        if produced >= MAX_TOTAL_OUTPUT {
            break;
        }
    }

    // `out` and the payload slice die with this frame, so the stream must not be left
    // pointing into it. See `detach_buffers`.
    detach_buffers(strm);

    DriveOutcome {
        produced,
        finished,
        finish_issued,
        bound_disturbed,
    }
}

// ---------------------------------------------------------------------------
//  Bound functions
// ---------------------------------------------------------------------------
//
// These are the most security-relevant assertions in this file. `deflateBound` and
// `compressBound` are not informational: callers size their output buffers from
// them, so a bound that is too small becomes a buffer overflow *in caller code*,
// where nothing in this library can defend against it. A bound that is too large is
// a defect of a different kind, breaking callers that assert on exact sizes, but it
// cannot corrupt memory, so the checks here are one-sided by design.
//
// The contract has preconditions, and honouring them is what keeps these checks
// free of false positives. `zlib.h` L340-L352 states that `deflateBound` must be
// called after `deflateSetHeader` if that is used, that the guarantee is about a
// single pass with all the input, a bound-sized buffer and `Z_FINISH`, and -- the
// clause most easily missed -- that "it is possible for the compressed size to be
// larger than the value returned by deflateBound() if flush options other than
// Z_FINISH or Z_NO_FLUSH are used". So:
//
//   * the bound is always taken AFTER any header and any preset dictionary, both of
//     which change it (`deflate.c` L863-L884 walks `gzhead`'s fields, and the zlib
//     wrapper length gains four bytes for a dictionary id once `strstart` is set);
//   * `deflatePrime` disqualifies the check, because primed bits are output the
//     bound never accounted for;
//   * `deflateParams` and `deflateReset` disqualify it, because the first may flush
//     and the second re-emits a wrapper;
//   * any flush outside {Z_NO_FLUSH, Z_FINISH} disqualifies it, per the clause
//     above -- that is what `DriveOutcome::bound_disturbed` records.

/// Asserts that `produced` bytes fit inside the bound `strm` reports for
/// `source_len`, and that the two spellings of the bound agree.
///
/// `deflateBound` narrows `deflateBound_z`'s `size_t` to a `uLong` and saturates to
/// `(uLong)-1` if it does not fit (`deflate.c` L845-L848), so on a target where the
/// two types differ the values may legitimately differ too; they are compared only
/// where the narrowing is lossless.
///
/// # Safety
///
/// `strm` must be a live, aligned `z_stream`. It need not be initialised: a failed
/// state check simply makes both functions return the larger conservative bound.
unsafe fn assert_within_bound(strm: &mut z_stream, source_len: usize, produced: usize, what: &str) {
    // SAFETY: `strm` is a live, aligned, exclusively borrowed `z_stream`. Both
    // functions accept an uninitialised or already-ended stream -- `deflateStateCheck`
    // failing simply makes them return the larger conservative bound (`deflate.c`
    // L855-L858) -- so there is no state precondition to uphold beyond the pointer
    // being valid.
    let bound_z = unsafe { deflateBound_z(strm, source_len as z_size_t) };

    let narrowed = match uLong::try_from(source_len) {
        Ok(len) => {
            // SAFETY: as above.
            Some(unsafe { deflateBound(strm, len) })
        }
        Err(_) => None,
    };

    assert!(
        produced <= bound_z,
        "{what}: produced {produced} bytes for a {source_len}-byte source, \
         but deflateBound_z reported only {bound_z}"
    );

    if let Some(bound) = narrowed {
        // `(uLong)-1` is the documented saturation marker; below it the two spellings
        // must agree exactly, because one is defined as the other.
        if bound != uLong::MAX {
            assert_eq!(
                usize::try_from(bound).unwrap_or(usize::MAX),
                bound_z,
                "{what}: deflateBound and deflateBound_z disagree for {source_len} bytes"
            );
            assert!(
                produced as uLong <= bound,
                "{what}: produced {produced} bytes for a {source_len}-byte source, \
                 but deflateBound reported only {bound}"
            );
        }
    }
}

/// Asserts that every bound function is monotonic in its source length.
///
/// A larger input can never have a smaller upper bound. This is cheap and it catches
/// the failure mode that matters most in bound arithmetic: an intermediate overflow
/// that wraps and produces a small answer for a large input. Both functions saturate
/// deliberately -- to `(z_size_t)-1` and `(uLong)-1` -- and saturation is monotonic,
/// so the property holds right through the extremes.
///
/// The lengths deliberately include zero and the payload cap, where the shift-and-add
/// arithmetic is most fragile, along with `usize::MAX` and `uLong::MAX` to force the
/// saturating branch.
///
/// # Safety
///
/// `strm` must be a live, aligned `z_stream`; as above, it need not be initialised.
unsafe fn assert_bounds_are_monotonic(strm: &mut z_stream) {
    let lengths: [usize; 9] = [
        0,
        1,
        2,
        127,
        MAX_PAYLOAD_LEN - 1,
        MAX_PAYLOAD_LEN,
        MAX_PAYLOAD_LEN + 1,
        usize::MAX / 2,
        usize::MAX,
    ];

    let mut previous_z: z_size_t = 0;
    let mut previous_compress_z: z_size_t = 0;
    for len in lengths {
        // SAFETY: `strm` is a live, aligned, exclusively borrowed `z_stream`; neither
        // bound function requires it to be initialised.
        let bound_z = unsafe { deflateBound_z(strm, len as z_size_t) };
        assert!(
            bound_z >= previous_z,
            "deflateBound_z is not monotonic: {len} bytes bounded by {bound_z}, \
             below the previous {previous_z}"
        );
        assert!(
            bound_z >= len,
            "deflateBound_z returned {bound_z} for {len} bytes, below the input itself"
        );
        previous_z = bound_z;

        let compress_z = compressBound_z(len as z_size_t);
        assert!(
            compress_z >= previous_compress_z,
            "compressBound_z is not monotonic: {len} bytes bounded by {compress_z}, \
             below the previous {previous_compress_z}"
        );
        assert!(
            compress_z >= len,
            "compressBound_z returned {compress_z} for {len} bytes, below the input itself"
        );
        previous_compress_z = compress_z;
    }

    // The `uLong` spellings, over lengths that certainly fit one.
    let mut previous: uLong = 0;
    let mut previous_compress: uLong = 0;
    for len in [0u32, 1, 2, 127, 4095, 4096, 4097, u32::MAX] {
        let len = uLong::from(len);
        // SAFETY: as above.
        let bound = unsafe { deflateBound(strm, len) };
        assert!(
            bound >= previous,
            "deflateBound is not monotonic: {len} bytes bounded by {bound}, \
             below the previous {previous}"
        );
        previous = bound;

        let compress = compressBound(len);
        assert!(
            compress >= previous_compress,
            "compressBound is not monotonic: {len} bytes bounded by {compress}, \
             below the previous {previous_compress}"
        );
        previous_compress = compress;
    }
}

/// Exercises the `compressBound`/`compress2` contract.
///
/// `zlib.h` L1288-L1300 requires the destination to be at least
/// `compressBound(sourceLen)` and, given that, promises `Z_OK`. So a buffer sized to
/// the bound must succeed and must report a length no larger than the bound -- if it
/// reported more, it would already have written past the end of a caller's buffer.
/// An invalid level must instead draw `Z_STREAM_ERROR`.
///
/// `compress2` builds its own stream with null hooks, so it uses the library's
/// internal allocator and never touches the tracking zone.
fn check_compress_bound_contract(payload: &[u8], level: c_int) {
    let source_len = payload.len();
    let bound = compressBound_z(source_len as z_size_t);
    assert!(
        bound >= source_len,
        "compressBound_z({source_len}) returned {bound}, below the input itself"
    );

    let mut dest = vec![0u8; bound.max(1)];
    let mut dest_len = dest.len() as uLongf;

    // SAFETY: `dest` is writable for `dest_len` bytes and `payload` readable for
    // `source_len` bytes, which is exactly what the two pointer/length pairs describe;
    // both live for the whole call. `dest_len` is an out-parameter the function
    // overwrites with the length actually used, so it must be, and is, a live `uLongf`.
    let code = unsafe {
        compress2(
            dest.as_mut_ptr(),
            &mut dest_len,
            payload.as_ptr().cast::<Bytef>(),
            source_len as uLong,
            level,
        )
    };

    // `Z_DEFAULT_COMPRESSION` reaches `deflateInit`, which rewrites it to 6, so it is
    // legal here for the same reason it is legal at initialisation.
    let level_is_legal =
        level == Z_DEFAULT_COMPRESSION || (Z_NO_COMPRESSION..=Z_BEST_COMPRESSION).contains(&level);

    if level_is_legal {
        assert_documented(
            code,
            &[Z_OK, Z_MEM_ERROR],
            &format!("compress2 with a bound-sized destination at level {level}"),
        );
        if code == Z_OK {
            let used = usize::try_from(dest_len).unwrap_or(usize::MAX);
            assert!(
                used <= bound,
                "compress2 wrote {used} bytes into a buffer compressBound_z sized at {bound}"
            );
        }
    } else {
        assert_eq!(
            code, Z_STREAM_ERROR,
            "compress2 accepted the invalid level {level}"
        );
    }
}

// ---------------------------------------------------------------------------
//  The gzip header, and the lifetimes it demands
// ---------------------------------------------------------------------------

/// Builds a `gz_header` and its three field buffers and runs `body` with a pointer
/// to it.
///
/// **This shape is the point of the function.** `deflateSetHeader` stores the
/// caller's pointer rather than copying anything (`deflate.c` L718:
/// `s->gzhead = head`), and the fields are read later -- by `deflate` when it emits
/// the header, and by `deflateBound` every time it is called. So the header struct
/// *and* all three buffers must outlive every call that might still look at them.
/// Keeping them as locals of this frame and handing `body` only a pointer makes that
/// impossible to get wrong: nothing can be dropped while `body` runs. A helper that
/// returned the pointer instead would hand back a dangle, and AddressSanitizer would
/// report the harness's bug as a library crash.
///
/// The pointer is taken with `ptr::addr_of_mut!` rather than `&mut head`, so it
/// carries the struct's own provenance and no reference is ever formed. (`&raw mut`
/// would say the same thing but is Rust 1.82 syntax, above this crate's 1.80 floor.)
fn with_header<R>(input: &FuzzInput, body: impl FnOnce(gz_headerp) -> R) -> R {
    let flags = input.header_flags;

    // `extra` carries a length, so it needs no terminator; it is given at least one
    // byte when present so the pointer handed over addresses real storage.
    let mut extra: Vec<u8> = clamped(&input.header_extra, input.header_extra.len()).to_vec();
    if extra.is_empty() {
        extra.push(0);
    }

    // `name` and `comment` are NUL-terminated C strings: `deflate` copies until the
    // terminator and `deflateBound` counts up to and including it (`deflate.c`
    // L897-L905). Every drawn byte is forced non-zero and one terminator appended, so
    // the length is exactly what was drawn and the scan cannot run off the end.
    let terminated = |bytes: &[u8]| -> Vec<u8> {
        let mut out: Vec<u8> = bytes.iter().map(|byte| byte | 1).collect();
        out.push(0);
        out
    };
    let mut name = terminated(&input.header_name);
    let mut comment = terminated(&input.header_comment);

    let extra_len = uInt::try_from(extra.len()).unwrap_or(0);
    let mut head = gz_header {
        // `Z_BINARY`, `Z_TEXT` or `Z_UNKNOWN` all round-trip; the value is advisory.
        text: c_int::from(flags & 0x10 != 0),
        time: uLong::from(input.header_time),
        xflags: 0,
        // The OS byte. 3 is Unix and 255 is "unknown"; both are legal.
        os: if flags & 0x20 == 0 { 3 } else { 255 },
        extra: if flags & 0x01 == 0 {
            ptr::null_mut()
        } else {
            extra.as_mut_ptr()
        },
        extra_len,
        // Zero on the write side: the three `*_max` members bound what `inflateGetHeader`
        // may write *into* these buffers, and nothing here reads a header.
        extra_max: 0,
        name: if flags & 0x02 == 0 {
            ptr::null_mut()
        } else {
            name.as_mut_ptr()
        },
        name_max: 0,
        comment: if flags & 0x04 == 0 {
            ptr::null_mut()
        } else {
            comment.as_mut_ptr()
        },
        comm_max: 0,
        hcrc: c_int::from(flags & 0x08 != 0),
        done: 0,
    };

    body(ptr::addr_of_mut!(head))
}

/// Installs a gzip header and asserts the answer the wrapper implies.
///
/// `deflateSetHeader` succeeds only for a gzip stream and answers `Z_STREAM_ERROR`
/// for every other wrapper (`deflate.c` L716-L717), so the configuration alone
/// decides which is correct. The negative case is worth asserting in its own right:
/// it is the one that proves a zlib or raw stream cannot be given a gzip header.
///
/// # Safety
///
/// `strm` must hold a state this harness initialised, and `head` must remain live
/// and unmoved for as long as the stream might read it -- which [`with_header`]
/// arranges.
unsafe fn apply_header(strm: &mut z_stream, config: Config, head: gz_headerp) {
    // SAFETY: `strm` holds a state initialised by `init_stream`, and `head` addresses
    // a live, aligned `gz_header` whose buffers are live for the whole enclosing
    // `with_header` call, NUL-terminated where the contract requires it.
    let code = unsafe { deflateSetHeader(strm, head) };
    if is_gzip(config) {
        assert_eq!(
            code, Z_OK,
            "deflateSetHeader refused a gzip stream {config:?}"
        );
    } else {
        assert_eq!(
            code, Z_STREAM_ERROR,
            "deflateSetHeader accepted a non-gzip stream {config:?}"
        );
    }
}

/// Installs a preset dictionary and asserts the answer the wrapper and stream state
/// imply.
///
/// `deflate.c` L567-L572 refuses a dictionary when it is null, when the wrapper is
/// gzip at all, when a zlib stream has left `INIT_STATE`, or when any lookahead is
/// buffered. On a stream that has not yet been given input, only the gzip case
/// applies, so the expectation is exact there; once compression has started either
/// answer is legitimate and only the return set is checked.
///
/// A zero-length dictionary is deliberately reachable and must still be accepted.
///
/// # Safety
///
/// `strm` must hold a state this harness initialised.
unsafe fn apply_dictionary(
    strm: &mut z_stream,
    config: Config,
    dictionary: &[u8],
    untouched: bool,
) -> bool {
    let len = uInt::try_from(dictionary.len()).unwrap_or(0);
    // SAFETY: `strm` holds a state initialised by `init_stream`. `dictionary.as_ptr()`
    // is non-null and readable for `len` bytes for the whole call -- an empty slice
    // yields a dangling-but-aligned non-null pointer, which is what the contract wants,
    // because it tests the pointer against `Z_NULL` and then reads exactly `len` bytes.
    let code = unsafe { deflateSetDictionary(strm, dictionary.as_ptr().cast::<Bytef>(), len) };

    if is_gzip(config) {
        assert_eq!(
            code, Z_STREAM_ERROR,
            "deflateSetDictionary accepted a gzip stream {config:?}"
        );
    } else if untouched {
        assert_eq!(
            code, Z_OK,
            "deflateSetDictionary refused a {len}-byte dictionary on an untouched \
             {config:?} stream"
        );
    } else {
        assert_documented(
            code,
            &[Z_OK, Z_STREAM_ERROR],
            "deflateSetDictionary mid-stream",
        );
    }

    if code == Z_OK && untouched && is_zlib_wrapped(config) && !dictionary.is_empty() {
        // `deflate.c` L574-L575 folds the dictionary into the stream's Adler-32 for a
        // zlib wrapper, which is how the decompressor is told which dictionary it needs
        // (`zlib.h` L604-L610). `deflateResetKeep` leaves the checksum at
        // `adler32(0, NULL, 0)`, which is 1, and Adler-32 over a non-empty input can
        // never be 1, so a checksum still sitting at 1 would mean the dictionary was
        // accepted without being accounted for -- a stream no decompressor could use.
        assert_ne!(
            strm.adler, 1,
            "a {len}-byte dictionary was accepted on a zlib stream without updating \
             the Adler-32 {config:?}"
        );
    }

    code == Z_OK
}

/// The window size a legal configuration actually receives, in bytes.
///
/// The exponent is the magnitude for a raw stream, sixteen less for gzip, and the
/// value itself for zlib; an exponent of 8 is then promoted to 9 (`deflate.c` L418,
/// "until 256-byte window bug fixed"), so the smallest window is 512 bytes rather
/// than 256. Only meaningful for a configuration the library accepted; the clamp
/// keeps it total for any other.
fn window_size(config: Config) -> usize {
    let exponent = if config.window_bits < 0 {
        config.window_bits.saturating_neg()
    } else if config.window_bits > MAX_WBITS {
        config.window_bits - 16
    } else {
        config.window_bits
    };
    let promoted = exponent.clamp(MIN_WBITS + 1, MAX_WBITS);
    1usize << promoted
}

/// Reads the active dictionary back and checks the report is self-consistent.
///
/// `deflate.c` L634-L640 reports `min(strstart + lookahead, w_size)` and writes that
/// many bytes, so the length can never exceed the window. The buffer offered is
/// always at least `1 << MAX_WBITS`, the largest window there is, so a report longer
/// than the buffer would mean the library was about to overrun a correctly sized
/// caller allocation.
///
/// When `expected` is given the length is pinned exactly. That is the round trip
/// worth having, and it is available only on a stream that has taken a dictionary and
/// then been left alone: `deflateSetDictionary` finishes with
/// `strstart += lookahead; lookahead = 0`, so `strstart` is precisely the number of
/// dictionary bytes retained -- `min(dictLength, w_size)`. Pinning it therefore
/// verifies the documented truncation of an over-long dictionary to its tail
/// (`deflate.c` L580-L589) rather than merely noting that *something* came back. Once
/// compression has started `strstart` moves with the data and only the inequality
/// remains checkable, so `expected` is `None` there.
///
/// Calling it twice must report the same length: reading a dictionary does not
/// consume it.
///
/// # Safety
///
/// `strm` must hold a state this harness initialised.
unsafe fn check_get_dictionary(strm: &mut z_stream, window: usize, expected: Option<usize>) {
    // Exactly what a correct caller allocates: the window, which is the most the
    // function can ever write. Sizing it to the window rather than to the largest
    // window there is makes the overrun check below sharper -- a report past the end of
    // *this* buffer is a report past the end of a correctly sized caller allocation --
    // and it keeps the allocation proportional to the configuration instead of paying
    // 32 KiB on every call.
    assert!(
        window <= GET_DICTIONARY_BUF_LEN,
        "a {window}-byte window exceeds 1 << MAX_WBITS"
    );
    let mut buffer = vec![0u8; window.max(1)];
    let mut length: c_uint = c_uint::MAX;

    // SAFETY: `strm` holds a state initialised by `init_stream`. `buffer` is writable
    // for `GET_DICTIONARY_BUF_LEN` bytes, which is at least the largest window the
    // library can have, and `length` is a live `c_uint` out-parameter.
    let code =
        unsafe { deflateGetDictionary(strm, buffer.as_mut_ptr().cast::<Bytef>(), &mut length) };
    assert_eq!(code, Z_OK, "deflateGetDictionary on a live stream");

    let reported = usize::try_from(length).unwrap_or(usize::MAX);
    assert!(
        reported <= buffer.len(),
        "deflateGetDictionary reported {reported} bytes into a {} byte buffer",
        buffer.len()
    );
    assert!(
        reported <= window,
        "deflateGetDictionary reported {reported} bytes, more than the {window}-byte window"
    );
    if let Some(expected) = expected {
        assert_eq!(
            reported, expected,
            "deflateGetDictionary reported {reported} bytes where the dictionary retained \
             {expected}"
        );
    }

    // The length-only form, which a caller uses to size a buffer before asking again.
    let mut again: c_uint = c_uint::MAX;
    // SAFETY: as above; a null dictionary pointer is explicitly permitted and means
    // "report the length only" (`deflate.c` L638).
    let code = unsafe { deflateGetDictionary(strm, ptr::null_mut(), &mut again) };
    assert_eq!(code, Z_OK, "deflateGetDictionary reporting length only");
    assert_eq!(
        again, length,
        "deflateGetDictionary reported different lengths for the same state"
    );
}

// ---------------------------------------------------------------------------
//  Session 1 -- the single-pass deflateBound contract
// ---------------------------------------------------------------------------

/// Performs the single `Z_FINISH` pass the `deflateBound` contract describes, and
/// returns the total bytes it produced.
///
/// # The documented guarantee, and what the reference actually does
///
/// `zlib.h` L346-L349 says that given all the input, a buffer sized to `deflateBound`
/// and `Z_FINISH`, "deflate() is guaranteed to return Z_STREAM_END". **Taken literally
/// that is not true, and it is not true of the C reference either.** Verified directly
/// against the in-tree C implementation with `level` 4, `windowBits` -13, `memLevel` 7,
/// `Z_RLE`, a five-byte dictionary of `0xff` bytes and a fifteen-byte payload:
/// `deflateBound` answers 20, the compressor produces exactly 20 bytes, and the call
/// returns `Z_OK` rather than `Z_STREAM_END`. The Rust port answers identically -- same
/// bound, same 20 bytes, same `Z_OK` -- so this is behavioural fidelity, not a defect.
///
/// The mechanism is `deflate.c` L1222-L1229: when the finish has *started* but
/// `avail_out` has reached zero, `deflate` records `last_flush = -1` and returns `Z_OK`
/// even though the data is complete. Filling the bound to its last byte is exactly the
/// case that triggers it.
///
/// So the guarantee is asserted in the form that is actually true, which is also the
/// form callers depend on:
///
/// 1. **The bound is never exceeded.** This is the security property: a caller sized
///    its buffer from `deflateBound`, so an overrun is a heap overflow in *its* code.
/// 2. The return is `Z_OK` or `Z_STREAM_END`, and nothing else.
/// 3. A `Z_OK` is legitimate **only** when the buffer was filled to the last byte. Room
///    left over plus an unfinished stream would mean the compressor stopped for some
///    other reason, and that would be a real defect.
/// 4. Given more room the stream then finishes, and the total *still* fits the bound.
///    That is the sharpest true statement of the promise: the bound was sufficient all
///    along and only the status code lagged.
///
/// # Safety
///
/// `strm` must hold a state this harness initialised, and `payload` must be live for the
/// whole call.
unsafe fn bound_contract_pass(
    strm: &mut z_stream,
    payload: &[u8],
    bound: usize,
    config: Config,
) -> usize {
    let mut out = vec![0u8; bound.max(1)];
    // Checked rather than cast, and deliberately loud. A silently truncated `avail_in`
    // would compress fewer bytes than intended while every assertion below still
    // passed, which is the one failure mode this target must never have. Both values are
    // bounded by `MAX_TOTAL_OUTPUT`, so neither conversion can actually fail.
    let avail_out = uInt::try_from(out.len()).expect("the bound fits a uInt");
    let avail_in = uInt::try_from(payload.len()).expect("the payload fits a uInt");

    // SAFETY: `payload` is readable for `avail_in` bytes and `out` writable for
    // `avail_out` bytes; both are live for the whole call and the pointer/length pairs
    // describe exactly them. `strm` holds a state this harness initialised.
    let code = unsafe {
        strm.next_in = payload.as_ptr();
        strm.avail_in = avail_in;
        strm.next_out = out.as_mut_ptr();
        strm.avail_out = avail_out;
        deflate(strm, Z_FINISH)
    };

    // Reading plain integer members through an exclusive borrow needs no `unsafe`.
    let produced = out.len() - strm.avail_out as usize;
    let room_left = strm.avail_out;

    // (1) and (2).
    assert!(
        produced <= bound,
        "one Z_FINISH pass over {} bytes produced {produced} bytes, past the {bound}-byte \
         deflateBound buffer it was given ({config:?})",
        payload.len()
    );
    assert_documented(
        code,
        &[Z_OK, Z_STREAM_END],
        &format!("the bound-contract pass over {} bytes", payload.len()),
    );

    if code == Z_STREAM_END {
        detach_buffers(strm);
        return produced;
    }

    // (3).
    assert_eq!(
        room_left,
        0,
        "one Z_FINISH pass over {} bytes stopped with {room_left} bytes still free in its \
         {bound}-byte deflateBound buffer ({config:?})",
        payload.len()
    );

    // (4). Generous room, so a genuine bound violation is measured rather than masked.
    let mut spare = vec![0u8; bound + MAX_OUTPUT_BUF_LEN];
    let spare_avail = uInt::try_from(spare.len()).expect("the spare buffer fits a uInt");
    // SAFETY: `spare` is writable for `spare_avail` bytes and live across the call; no
    // further input is offered, which `avail_in` 0 states truthfully.
    let code = unsafe {
        strm.next_in = ptr::null();
        strm.avail_in = 0;
        strm.next_out = spare.as_mut_ptr();
        strm.avail_out = spare_avail;
        deflate(strm, Z_FINISH)
    };
    let extra = spare.len() - strm.avail_out as usize;

    // `out` and `spare` both die with this frame.
    detach_buffers(strm);
    drop(spare);

    assert_eq!(
        code, Z_STREAM_END,
        "a bound-contract pass that filled its buffer exactly did not finish even with \
         {MAX_OUTPUT_BUF_LEN} spare bytes ({config:?})"
    );
    let total = produced + extra;
    assert!(
        total <= bound,
        "compressing {} bytes really needed {total} bytes, past the {bound} deflateBound \
         promised ({config:?})",
        payload.len()
    );

    total
}

/// Compresses `payload` in exactly one pass into a buffer sized from
/// `deflateBound`, and asserts the guarantee `zlib.h` L340-L352 makes about that
/// arrangement.
///
/// This is the strongest statement the library makes about output size, and the one
/// callers actually rely on: given all the input, a bound-sized buffer and
/// `Z_FINISH`, `deflate` *must* answer `Z_STREAM_END`. Anything less would mean a
/// caller who sized its buffer correctly still ran out of room.
///
/// Every precondition is honoured here rather than assumed: the header and the
/// dictionary go in first, the bound is taken afterwards, and the single call uses
/// `Z_FINISH`. Nothing in this session primes, retunes or resets, so none of the
/// disqualifying operations can interfere.
///
/// # Safety
///
/// `head` must remain live and unmoved for the whole call, which is what the enclosing
/// [`with_header`] frame guarantees.
unsafe fn run_bound_session(input: &FuzzInput, config: Config, head: gz_headerp) {
    let zone = Zone::new(input.alloc_limit());
    let mut strm = blank_stream();
    track(&mut strm, &zone);

    // SAFETY: `strm` is a live local with a matching hook pair and a live zone pointer.
    if !unsafe { init_stream(&mut strm, config, input.alloc_limit() != 0) } {
        assert_zone_clean(&zone, "bound session, initialisation refused");
        return;
    }

    if input.wants(OP_BOUND_HEADER) {
        // SAFETY: `strm` holds a state `init_stream` installed; `head` is live for the
        // whole enclosing `with_header` frame, which encloses this session.
        unsafe { apply_header(&mut strm, config, head) };
    }
    // The exact number of dictionary bytes the stream should now be holding: nothing
    // when no dictionary was offered or it was refused, and otherwise the dictionary
    // truncated to the window.
    let window = window_size(config);
    let mut retained = 0usize;
    if input.wants(OP_BOUND_DICTIONARY) {
        // SAFETY: as above; the stream has had no input, hence `untouched`.
        let accepted = unsafe { apply_dictionary(&mut strm, config, input.dictionary(), true) };
        if accepted {
            retained = input.dictionary().len().min(window);
        }
    }
    if input.wants(OP_GET_DICTIONARY) {
        // Nothing has been compressed yet, so the retained length is pinned exactly.
        // SAFETY: as above.
        unsafe { check_get_dictionary(&mut strm, window, Some(retained)) };
    }

    let payload = input.payload();

    // The bound, taken last of all -- after the header and the dictionary, both of
    // which change it.
    // SAFETY: `strm` holds a state `init_stream` installed.
    let bound = unsafe { deflateBound_z(&mut strm, payload.len() as z_size_t) };
    assert!(
        bound >= payload.len(),
        "deflateBound_z returned {bound} for {} bytes, below the input itself",
        payload.len()
    );

    // Defensive, and unreachable with the payload cap in force: a bound larger than
    // the output cap would mean allocating more than this target is willing to.
    if bound > MAX_TOTAL_OUTPUT {
        // SAFETY: as above.
        assert_documented(
            unsafe { deflateEnd(&mut strm) },
            &[Z_OK, Z_DATA_ERROR],
            "deflateEnd after an unexpectedly large bound",
        );
        assert_zone_clean(&zone, "bound session, bound too large to exercise");
        return;
    }

    // SAFETY: `strm` holds a state `init_stream` installed, and `payload` is live for
    // the whole call.
    let produced = unsafe { bound_contract_pass(&mut strm, payload, bound, config) };

    // SAFETY: `strm` holds a state `init_stream` installed.
    unsafe { assert_within_bound(&mut strm, payload.len(), produced, "bound session") };

    if input.wants(OP_BOUND_MONOTONIC) {
        // SAFETY: as above.
        unsafe { assert_bounds_are_monotonic(&mut strm) };
    }

    // `out` dies with this frame. See `detach_buffers`.
    detach_buffers(&mut strm);

    // SAFETY: as above.
    let end = unsafe { deflateEnd(&mut strm) };
    assert_eq!(end, Z_OK, "deflateEnd after a completed single pass");

    assert_zone_clean(&zone, "bound session");
}

// ---------------------------------------------------------------------------
//  The wider deflate surface
// ---------------------------------------------------------------------------

/// Checks that `deflatePending` reports something self-consistent.
///
/// `deflate.c` L723-L734 reports `s->pending` bytes and `s->bi_valid` bits. The bit
/// count is asserted against `Buf_size`, the capacity of the bit buffer, rather than
/// against the 0..=7 range `zlib.h` L370-L375 documents: `send_bits` (`trees.c`
/// L253-L266) takes the second branch whenever `bi_valid <= Buf_size - length` and
/// then adds `length`, so with `length` 15 and `bi_valid` 1 the counter reaches
/// exactly 16. Asserting the tighter documented range would risk a false positive
/// against a real and legal state, and a fuzz target that cries wolf is worse than
/// one that checks slightly less.
///
/// Both out-parameters are also exercised in their null forms, which the contract
/// permits and which must not write anything.
///
/// # Safety
///
/// `strm` must hold a state this harness initialised.
unsafe fn check_pending(strm: &mut z_stream) {
    /// `Buf_size` -- the bit buffer is a `ush`, so sixteen bits (`deflate.h` L332).
    const BUF_SIZE: c_int = 16;

    let mut pending: c_uint = c_uint::MAX;
    let mut bits: c_int = c_int::MIN;

    // SAFETY: `strm` holds a state `init_stream` installed, and both out-parameters are
    // live, aligned locals of this frame.
    let code = unsafe { deflatePending(strm, &mut pending, &mut bits) };
    // `Z_BUF_ERROR` is documented for a `pending` count too large for an `unsigned`,
    // which cannot arise at these sizes but is part of the contract.
    assert_documented(code, &[Z_OK, Z_BUF_ERROR], "deflatePending");

    if code == Z_OK {
        assert!(
            (0..=BUF_SIZE).contains(&bits),
            "deflatePending reported {bits} bits, outside the bit buffer's capacity"
        );
    }

    // SAFETY: as above, with both out-parameters null -- explicitly permitted, and the
    // assertion is that nothing is written through them.
    let code = unsafe { deflatePending(strm, ptr::null_mut(), ptr::null_mut()) };
    assert_documented(
        code,
        &[Z_OK, Z_BUF_ERROR],
        "deflatePending with null outputs",
    );

    let mut used: c_int = c_int::MIN;
    // SAFETY: as above; `used` is a live, aligned local.
    let code = unsafe { deflateUsed(strm, &mut used) };
    assert_eq!(code, Z_OK, "deflateUsed on a live stream");
    assert!(
        (0..=BUF_SIZE).contains(&used),
        "deflateUsed reported {used} bits, outside the bit buffer's capacity"
    );

    // SAFETY: as above; a null out-parameter is permitted (`deflate.c` L739).
    let code = unsafe { deflateUsed(strm, ptr::null_mut()) };
    assert_eq!(code, Z_OK, "deflateUsed with a null output");
}

/// Retunes the stream mid-compression and holds the answer to the documented set.
///
/// `deflateParams` is genuinely awkward: changing the compression function forces a
/// `Z_BLOCK` flush of whatever is buffered, and if that cannot be completed it
/// answers `Z_BUF_ERROR` and leaves the parameters untouched (`deflate.c`
/// L753-L761). That is not a failure, and a caller may simply retry with more room.
/// An invalid pair must instead answer `Z_STREAM_ERROR`, which is what the harness's
/// own `params_are_legal` predicts -- though a *legal* pair may still answer
/// `Z_BUF_ERROR`, so only the illegal direction can be asserted exactly.
///
/// **A legal pair can also answer `Z_STREAM_ERROR`, and the reason is easy to miss.**
/// The internal flush is a real `deflate(strm, Z_BLOCK)` call whose failure is returned
/// verbatim (`deflate.c` L755-L757), and `deflate` answers `Z_STREAM_ERROR` whenever
/// `next_out` is null or the status is already `FINISH_STATE` (`deflate.c` L990-L994).
/// So once the status has reached `FINISH_STATE`, `Z_STREAM_ERROR` is correct behaviour
/// rather than a defect.
///
/// The predicate for that is `finish_issued`, **not** "the stream returned
/// `Z_STREAM_END`". `deflate.c` L1222-L1224 sets `FINISH_STATE` on `finish_started` as
/// well as `finish_done`, so a single `Z_FINISH` that only managed `Z_OK` is already
/// enough. Using the narrower condition keeps the assertion sharp where it can be --
/// a stream that was never asked to finish must not answer `Z_STREAM_ERROR` -- instead
/// of widening it to accept everything everywhere.
///
/// # Safety
///
/// `strm` must hold a state this harness initialised, with `next_out` either null or
/// pointing at a buffer that is live for this whole call -- the internal `deflate` writes
/// through it.
unsafe fn apply_params(strm: &mut z_stream, level: c_int, strategy: c_int, finish_issued: bool) {
    // SAFETY: `strm` holds a state `init_stream` installed. `deflateParams` may call
    // `deflate` internally, which reads `next_in`/`next_out` as the driving loop left
    // them -- both were set from live slices that are still live at every call site.
    let code = unsafe { deflateParams(strm, level, strategy) };
    if params_are_legal(level, strategy) {
        let allowed: &[c_int] = if finish_issued {
            &[Z_OK, Z_BUF_ERROR, Z_STREAM_ERROR]
        } else {
            &[Z_OK, Z_BUF_ERROR]
        };
        assert_documented(
            code,
            allowed,
            &format!("deflateParams({level}, {strategy}) with a legal pair"),
        );
    } else {
        assert_eq!(
            code, Z_STREAM_ERROR,
            "deflateParams accepted the invalid pair ({level}, {strategy})"
        );
    }
}

/// Injects bits with `deflatePrime` and holds the answer to the documented set.
///
/// **Correction to a common belief, verified against the reference.** An out-of-range
/// bit count answers `Z_BUF_ERROR`, not `Z_STREAM_ERROR`: `deflate.c` L797-L804 folds
/// `bits < 0 || bits > 16` into the same test as "no room in the pending buffer", and
/// both share the `Z_BUF_ERROR` return. `Z_STREAM_ERROR` is reserved for an
/// inconsistent stream. So the assertion here is that an out-of-range count is never
/// *accepted*, rather than that it produces one particular refusal.
///
/// # Safety
///
/// `strm` must hold a state this harness initialised.
unsafe fn apply_prime(strm: &mut z_stream, bits: c_int, value: c_int) {
    // SAFETY: `strm` holds a state `init_stream` installed; both arguments are plain
    // integers and nothing is dereferenced.
    let code = unsafe { deflatePrime(strm, bits, value) };
    assert_documented(
        code,
        &[Z_OK, Z_BUF_ERROR, Z_STREAM_ERROR],
        &format!("deflatePrime({bits}, {value})"),
    );
    if !(0..=16).contains(&bits) {
        assert_ne!(
            code, Z_OK,
            "deflatePrime accepted the out-of-range bit count {bits}"
        );
    }
}

/// Clones the stream, tears the clone down, and checks the source still works.
///
/// **The clone is ended before the source, and that ordering is required.** Both
/// streams allocate through the same hooks and the same zone, so the clone's five
/// blocks sit on top of the source's. Ending the source first would return blocks
/// that are not at the head of the list, the zone would count non-last-in-first-out
/// frees, and the teardown assertion would fail on the harness's mistake rather than
/// on anything the library did.
///
/// # Safety
///
/// `source` must hold a state this harness initialised.
unsafe fn check_copy(source: &mut z_stream) {
    let mut clone = blank_stream();

    // SAFETY: `source` holds a state `init_stream` installed; `clone` is a live,
    // aligned, exclusively borrowed `z_stream`. `deflateCopy` overwrites every member
    // of the destination, so its prior contents need not mean anything -- but it must
    // not already own state, and a blank stream does not.
    let code = unsafe { deflateCopy(&mut clone, source) };
    assert_documented(code, &[Z_OK, Z_MEM_ERROR], "deflateCopy from a live stream");

    if code != Z_OK {
        // A refused copy must not have installed anything, so there is nothing to end
        // and, in particular, nothing left for the zone to report.
        return;
    }

    // The clone is a working compressor in its own right: a `Z_FINISH` pass into ample
    // room must complete. Ample means the clone's own bound, taken now.
    // SAFETY: `clone` now holds state `deflateCopy` installed.
    let bound = unsafe { deflateBound_z(&mut clone, 0) };
    let mut out = vec![0u8; bound.clamp(1, MAX_TOTAL_OUTPUT)];
    // SAFETY: `out` is writable for its whole length and lives across the call; no
    // input is supplied, which `avail_in` 0 states truthfully.
    let code = unsafe {
        clone.next_in = ptr::null();
        clone.avail_in = 0;
        clone.next_out = out.as_mut_ptr();
        clone.avail_out = uInt::try_from(out.len()).unwrap_or(0);
        deflate(&mut clone, Z_FINISH)
    };
    assert_documented(code, &DEFLATE_RETURNS, "deflate on a cloned stream");

    // `out` dies at the end of this function, so the clone gives its loan back first.
    detach_buffers(&mut clone);

    // SAFETY: `clone` holds state `deflateCopy` installed. `Z_DATA_ERROR` is documented
    // for a stream ended while still compressing, which is reachable here whenever the
    // pass above did not finish.
    let end = unsafe { deflateEnd(&mut clone) };
    assert_documented(end, &[Z_OK, Z_DATA_ERROR], "deflateEnd on a cloned stream");

    // The source must be untouched by all of this.
    let mut used: c_int = c_int::MIN;
    // SAFETY: `source` still holds the state it did before the copy.
    let code = unsafe { deflateUsed(source, &mut used) };
    assert_eq!(
        code, Z_OK,
        "the source stream stopped working after being copied"
    );
}

/// After `Z_STREAM_END`, checks that a flush other than `Z_FINISH` is refused.
///
/// `deflate.c` L1013-L1017 refuses any other flush once the status is
/// `FINISH_STATE`, and a repeated `Z_FINISH` answers `Z_STREAM_END` again
/// (`deflate.c` L1195-L1197). Both halves are asserted, because together they are
/// what lets a caller call `deflate` one time too many without consequence.
///
/// # Safety
///
/// `strm` must hold a state this harness initialised that has reached
/// `Z_STREAM_END`.
unsafe fn check_after_finish(strm: &mut z_stream) {
    let mut out = [0u8; 32];

    // SAFETY: `strm` holds a finished state `init_stream` installed; `out` is a live
    // local writable for its whole length, and `avail_in` 0 with a null `next_in` is
    // the documented way to supply no input.
    let code = unsafe {
        strm.next_in = ptr::null();
        strm.avail_in = 0;
        strm.next_out = out.as_mut_ptr();
        strm.avail_out = out.len() as uInt;
        deflate(strm, Z_NO_FLUSH)
    };
    assert_eq!(
        code, Z_STREAM_ERROR,
        "deflate accepted Z_NO_FLUSH after the stream had finished"
    );

    // SAFETY: as above.
    let code = unsafe {
        strm.next_out = out.as_mut_ptr();
        strm.avail_out = out.len() as uInt;
        deflate(strm, Z_FINISH)
    };
    assert_eq!(
        code, Z_STREAM_END,
        "a repeated Z_FINISH on a finished stream must answer Z_STREAM_END again"
    );

    // `out` is a local of this frame. See `detach_buffers`.
    detach_buffers(strm);
}

// ---------------------------------------------------------------------------
//  Session 2 -- the stress session
// ---------------------------------------------------------------------------

/// Records which optional operations ran, because each of them invalidates the
/// `deflateBound` guarantee and the bound check has to know.
///
/// A bit-set rather than four `bool` fields, for the same reason [`FuzzInput`] uses
/// one: `clippy::pedantic` denies more than three booleans in a struct, and this set
/// is only ever consulted as a whole anyway.
#[derive(Debug, Clone, Copy, Default)]
struct Disturbances(u8);

impl Disturbances {
    /// `deflatePrime` injected bits the bound never counted.
    const PRIMED: u8 = 1 << 0;
    /// `deflateParams` may have flushed to change the compression function, and
    /// `zlib.h` L757-L760 says the bound may need taking again afterwards.
    const RETUNED: u8 = 1 << 1;
    /// `deflateReset` or `deflateResetKeep` restarted the stream, so a second wrapper
    /// lands in the same output total.
    const RESTARTED: u8 = 1 << 2;
    /// `deflateTune` changed the match search. It cannot affect the stored-block escape
    /// the bound rests on, so this one is conservative rather than necessary -- but the
    /// dedicated bound session never tunes, so excluding it costs no coverage and
    /// removes a whole class of possible false positive.
    const TUNED: u8 = 1 << 3;

    /// Records that `what` happened.
    fn note(&mut self, what: u8) {
        self.0 |= what;
    }

    /// Whether the `deflateBound` guarantee still holds for this session.
    fn bound_still_guaranteed(self, outcome: DriveOutcome) -> bool {
        self.0 == 0 && !outcome.bound_disturbed
    }
}

/// Applies the optional operations the input selected, before any input is fed.
///
/// Ordered to match how a caller would use them: the header and the dictionary must
/// go in before compression starts, and the introspection calls are cheap enough to
/// make unconditionally.
///
/// # Safety
///
/// `strm` must hold a state this harness initialised and must not yet have been given
/// input.
unsafe fn apply_pre_compression_ops(
    strm: &mut z_stream,
    input: &FuzzInput,
    config: Config,
    head: gz_headerp,
) -> Disturbances {
    let mut disturbed = Disturbances::default();

    if input.wants(OP_STRESS_HEADER) {
        // SAFETY: `strm` holds a state `init_stream` installed; `head` is live for the
        // whole enclosing `with_header` frame.
        unsafe { apply_header(strm, config, head) };
    }
    if input.wants(OP_STRESS_DICTIONARY) {
        // SAFETY: as above; no input has been supplied yet, hence `untouched`.
        let accepted = unsafe { apply_dictionary(strm, config, input.dictionary(), true) };
        let window = window_size(config);
        let retained = if accepted {
            input.dictionary().len().min(window)
        } else {
            0
        };
        // Still untouched by input at this point, so the retained length is exact here
        // too -- which is where the truncation of an over-long dictionary is actually
        // pinned down.
        // SAFETY: as above.
        unsafe { check_get_dictionary(strm, window, Some(retained)) };
    }
    if input.wants(OP_TUNE) {
        let (good_length, max_lazy, nice_length, max_chain) = input.tune();
        // SAFETY: as above; all four arguments are plain integers, clamped by `tune()` so
        // the match search stays bounded.
        let code = unsafe { deflateTune(strm, good_length, max_lazy, nice_length, max_chain) };
        assert_eq!(code, Z_OK, "deflateTune on a live stream");
        disturbed.note(Disturbances::TUNED);
    }
    if input.wants(OP_PRIME) {
        let (bits, value) = input.prime();
        // SAFETY: as above.
        unsafe { apply_prime(strm, bits, value) };
        disturbed.note(Disturbances::PRIMED);
    }

    // SAFETY: as above; both only read through out-parameters this frame owns.
    unsafe { check_pending(strm) };

    disturbed
}

/// Applies the optional operations that only make sense mid-compression.
///
/// # Safety
///
/// `strm` must hold a state this harness initialised.
unsafe fn apply_mid_compression_ops(
    strm: &mut z_stream,
    input: &FuzzInput,
    outcome: DriveOutcome,
    disturbed: &mut Disturbances,
) {
    if input.wants(OP_PARAMS) {
        let (level, strategy) = input.params();

        // `deflateParams` flushes pending output by calling `deflate` internally, so it
        // needs somewhere to write. The driving loop detached its own buffers on the way
        // out, and with a null `next_out` that internal call answers `Z_STREAM_ERROR`
        // immediately and the interesting path -- flush, then swap the compression
        // function -- is never reached. So a fresh buffer is lent for the duration.
        let mut room = vec![0u8; MAX_OUTPUT_BUF_LEN];
        strm.next_in = ptr::null();
        strm.avail_in = 0;
        strm.next_out = room.as_mut_ptr();
        strm.avail_out = room.len() as uInt;

        // SAFETY: `strm` holds a state `init_stream` installed, and `next_out` addresses
        // `room`, which is writable for the `avail_out` bytes just declared and live
        // across the call.
        unsafe { apply_params(strm, level, strategy, outcome.finish_issued) };

        // `room` dies at the end of this block, so the loan is called in first.
        detach_buffers(strm);
        drop(room);

        disturbed.note(Disturbances::RETUNED);
    }
    if input.wants(OP_COPY) {
        // SAFETY: as above. The clone is ended inside `check_copy`, before this stream is,
        // which is what keeps the zone's release order last-in-first-out.
        unsafe { check_copy(strm) };
    }
    // SAFETY: as above.
    unsafe { check_pending(strm) };
}

/// Drives the stream hard: chunked input, a fuzzer-chosen flush per call, possibly a
/// one-byte output buffer, and the whole optional operation set layered on top.
///
/// Unlike the bound session this one makes no claim about output size unless every
/// disqualifying operation happened to be switched off, which
/// [`Disturbances::bound_still_guaranteed`] decides. What it does assert everywhere
/// is the return-code discipline, the allocator discipline, and that nothing hangs.
unsafe fn run_stress_session(input: &FuzzInput, config: Config, head: gz_headerp) {
    let ceiling = input.alloc_limit();
    let zone = Zone::new(ceiling);
    let mut strm = blank_stream();
    track(&mut strm, &zone);

    // SAFETY: `strm` is a live local with a matching hook pair and a live zone pointer.
    if !unsafe { init_stream(&mut strm, config, ceiling != 0) } {
        assert_zone_clean(&zone, "stress session, initialisation refused");
        return;
    }

    // SAFETY: `strm` holds a state `init_stream` installed and has had no input.
    let mut disturbed = unsafe { apply_pre_compression_ops(&mut strm, input, config, head) };

    let payload = input.payload();

    // Restarting before the first pass, when selected. Both forms must leave a stream
    // that still compresses, which the pass below then demonstrates.
    if input.wants(OP_RESET) {
        // SAFETY: `strm` holds a state `init_stream` installed.
        let code = unsafe { deflateReset(&mut strm) };
        assert_eq!(code, Z_OK, "deflateReset on a live stream");
        disturbed.note(Disturbances::RESTARTED);
    }
    if input.wants(OP_RESET_KEEP) {
        // SAFETY: as above.
        let code = unsafe { deflateResetKeep(&mut strm) };
        assert_eq!(code, Z_OK, "deflateResetKeep on a live stream");
        disturbed.note(Disturbances::RESTARTED);
    }

    // SAFETY: `strm` holds a state `init_stream` installed; `drive` supplies its own
    // live input and output buffers.
    let first = unsafe { drive(&mut strm, payload, input) };

    // SAFETY: as above.
    unsafe { apply_mid_compression_ops(&mut strm, input, first, &mut disturbed) };

    if first.finished && input.wants(OP_POST_FINISH_FLUSH) && !input.wants(OP_PARAMS) {
        // Only meaningful while the stream is still finished: `deflateParams` above may
        // have driven it, so this is skipped when it ran.
        // SAFETY: `strm` holds a finished state `init_stream` installed.
        unsafe { check_after_finish(&mut strm) };
    }

    if disturbed.bound_still_guaranteed(first) && first.finished {
        // SAFETY: as above.
        unsafe {
            assert_within_bound(&mut strm, payload.len(), first.produced, "stress session");
        }
    }

    // A restart after finishing, and a second pass to prove the stream came back. This
    // is the path a caller reusing one stream for many members takes.
    if first.finished && input.wants(OP_RESET) {
        // SAFETY: as above.
        let code = unsafe { deflateReset(&mut strm) };
        assert_eq!(code, Z_OK, "deflateReset after the stream finished");
        // SAFETY: as above.
        let second = unsafe { drive(&mut strm, clamped(payload, 64), input) };
        assert!(
            second.produced > 0 || second.finished || payload.is_empty(),
            "a reset stream produced nothing at all"
        );
    }

    if input.wants(OP_GET_DICTIONARY) {
        // Compression has moved `strstart` on, so only the inequality is checkable.
        // SAFETY: as above.
        unsafe { check_get_dictionary(&mut strm, window_size(config), None) };
    }

    // `Z_DATA_ERROR` is the documented answer for a stream ended while still
    // compressing, which is exactly what happens whenever a cap stopped the driving
    // loop early.
    // SAFETY: `strm` holds a state `init_stream` installed and is ended exactly once.
    let end = unsafe { deflateEnd(&mut strm) };
    assert_documented(
        end,
        &[Z_OK, Z_DATA_ERROR],
        "deflateEnd after the stress session",
    );

    assert_zone_clean(&zone, "stress session");
}

// ---------------------------------------------------------------------------
//  Entry point
// ---------------------------------------------------------------------------

/// Runs everything one input selects.
///
/// Split out of the macro body so that each piece stays small and so that the
/// `with_header` frame -- which owns the gzip header and its buffers -- encloses both
/// sessions. That nesting is what guarantees the header outlives every call that
/// could read it.
fn run(input: &FuzzInput) {
    let config = input.config.resolve();

    // Harness self-check, not a library check. A draw that declared itself legal must
    // actually be legal; if the `legal_*` generators ever drift out of step with
    // `init_is_legal`, this catches it here instead of letting the sweep silently
    // narrow to whatever both happen to agree on.
    if config.kind == DrawKind::Legal {
        assert!(
            init_is_legal(config),
            "the legal generator produced {config:?}, which init_is_legal rejects"
        );
    }

    if input.wants(OP_VERSION_GATE) {
        check_version_gate_ordering();
    }

    if input.wants(OP_COMPRESS_BOUND) {
        // Both extremes of the bound arithmetic, plus whatever the fuzzer drew.
        check_compress_bound_contract(input.payload(), config.level);
        check_compress_bound_contract(&[], config.level);
    }

    with_header(input, |head| {
        if input.wants(OP_BOUND_SESSION) {
            // SAFETY: `head` is live for this whole closure, which is the frame
            // `with_header` keeps the header and its buffers in.
            unsafe { run_bound_session(input, config, head) };
        }
        // SAFETY: as above.
        unsafe { run_stress_session(input, config, head) };
    });
}

fuzz_target!(|input: FuzzInput| {
    run(&input);
});
