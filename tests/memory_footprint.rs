//! Runtime memory-footprint gate (AAP §0.7.3 / §0.8.3): the live per-stream
//! heap allocation of the Rust deflate and inflate engines must be **<= C
//! zlib**.
//!
//! Two complementary checks enforce this requirement:
//!
//! * The `size_of`-based unit gates in `src/deflate/state.rs` and
//!   `src/inflate/state.rs` (`memory_footprint_within_c_zlib_gate`) prove the
//!   *structural* footprint exactly — the four deflate buffers and the inflate
//!   `codes` table are sized with formulas byte-identical to C, so only the
//!   state-record width can differ, and the unit gate pins that.
//!
//! * This integration test closes the loop *end-to-end*: it installs a
//!   process-wide counting allocator and measures the **actual** bytes the
//!   public API (`Deflate::with_options` / `Inflate::new`) retains on the heap,
//!   then asserts each is at or below the measured C zlib reference for the
//!   default `level = 6` / `windowBits = 15` / `memLevel = 8` configuration.
//!
//! Reference measurement — gcc + system libz 1.3.1, a counting `zalloc` hook
//! summing every block requested at init:
//!
//! ```text
//! c_deflate_engine_bytes after_init = 268096
//! c_inflate_engine_bytes after_init =   7160
//! ```
//!
//! This file is a dedicated integration-test binary containing a single
//! `#[test]`, so libtest runs it on one thread and the measured windows contain
//! no concurrent allocation from other tests.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

/// Net heap bytes currently outstanding (bytes allocated minus bytes freed)
/// through the global allocator.
static OUTSTANDING: AtomicUsize = AtomicUsize::new(0);

/// A pass-through allocator that delegates to the system allocator while
/// tracking the net number of outstanding bytes. Transient allocations that are
/// freed before a measurement is read cancel out, so a measured delta reflects
/// only the memory *retained* by the value under test.
struct CountingAllocator;

// SAFETY: every method forwards directly to `System`, which is a sound
// `GlobalAlloc`, and only adds atomic bookkeeping around the returned pointer.
// The pointer/layout contracts are upheld by `System`; we never fabricate,
// alias, or mutate the returned pointer.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarded verbatim to the system allocator with the same
        // layout; the caller's `GlobalAlloc::alloc` contract is upheld by it.
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            OUTSTANDING.fetch_add(layout.size(), Ordering::Relaxed);
        }
        p
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // `vec![0u8; n]` / `vec![0u16; n]` take this path; tracking it ensures
        // the zero-initialised window/prev/head/pending buffers are counted.
        // SAFETY: forwarded verbatim to the system allocator with the same
        // layout.
        let p = unsafe { System.alloc_zeroed(layout) };
        if !p.is_null() {
            OUTSTANDING.fetch_add(layout.size(), Ordering::Relaxed);
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr`/`layout` originate from this allocator's own
        // `alloc`/`alloc_zeroed`/`realloc`, which all return system-allocated
        // blocks, so they are valid arguments to `System.dealloc`.
        unsafe { System.dealloc(ptr, layout) };
        OUTSTANDING.fetch_sub(layout.size(), Ordering::Relaxed);
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: `ptr`/`layout` were produced by this allocator (hence by
        // `System`), and `new_size` is forwarded unchanged, satisfying
        // `System.realloc`'s contract.
        let p = unsafe { System.realloc(ptr, layout, new_size) };
        if !p.is_null() {
            // The old block of `layout.size()` is released and `new_size` is
            // acquired. The old block was outstanding, so subtracting after
            // adding never underflows the counter.
            OUTSTANDING.fetch_add(new_size, Ordering::Relaxed);
            OUTSTANDING.fetch_sub(layout.size(), Ordering::Relaxed);
        }
        p
    }
}

#[global_allocator]
static GLOBAL: CountingAllocator = CountingAllocator;

fn outstanding() -> usize {
    OUTSTANDING.load(Ordering::Relaxed)
}

/// Measure the net heap bytes retained by `build`'s return value. The value is
/// kept alive across the second counter read (via `black_box`) so its buffers
/// are still allocated when the delta is computed, then dropped.
fn measure_retained<T>(build: impl FnOnce() -> T) -> usize {
    let before = outstanding();
    let engine = build();
    let after = outstanding();
    // Keep `engine` (and therefore its heap buffers) alive until *after* the
    // post-read, and prevent the optimiser from eliding the construction.
    std::hint::black_box(&engine);
    let retained = after.saturating_sub(before);
    drop(engine);
    retained
}

#[test]
fn engine_allocation_within_c_zlib() {
    use zlib_rs::{Deflate, Inflate, Strategy};

    // Reference engine bytes from C zlib (system libz 1.3.1) at the default
    // level=6 / windowBits=15 / memLevel=8 configuration.
    const C_ZLIB_DEFLATE_ENGINE_BYTES: usize = 268_096;
    const C_ZLIB_INFLATE_ENGINE_BYTES: usize = 7_160;

    // Warm up the allocator and any one-time lazy initialisation so the first
    // measured construction is not charged for unrelated startup allocations.
    {
        let warm_d = Deflate::with_options(6, 15, 8, Strategy::Default).unwrap();
        let warm_i = Inflate::new().unwrap();
        std::hint::black_box((&warm_d, &warm_i));
    }

    // Deflate engine: Box<DeflateState> + window + prev + head + pending_buf,
    // all allocated up front by `deflateInit2_`.
    let deflate_bytes =
        measure_retained(|| Deflate::with_options(6, 15, 8, Strategy::Default).unwrap());

    // Inflate engine: Box<InflateState> + the eager `codes[ENOUGH]` table. The
    // sliding window is allocated lazily on first use (exactly like C), so it is
    // not part of the init-time footprint.
    let inflate_bytes = measure_retained(|| Inflate::new().unwrap());

    eprintln!("rust_deflate_engine_bytes after_init = {deflate_bytes}");
    eprintln!("rust_inflate_engine_bytes after_init = {inflate_bytes}");

    assert!(
        deflate_bytes <= C_ZLIB_DEFLATE_ENGINE_BYTES,
        "deflate engine retained {deflate_bytes} bytes at default init, exceeding the C zlib \
         reference {C_ZLIB_DEFLATE_ENGINE_BYTES} (AAP memory gate). Narrow a state field or \
         formally change the gate.",
    );
    assert!(
        inflate_bytes <= C_ZLIB_INFLATE_ENGINE_BYTES,
        "inflate engine retained {inflate_bytes} bytes at default init, exceeding the C zlib \
         reference {C_ZLIB_INFLATE_ENGINE_BYTES} (AAP memory gate). Narrow a state field or \
         formally change the gate.",
    );

    // Lower sanity bounds: the engines must actually allocate their buffers, so
    // the measured footprint cannot be trivially small. A tiny value would mean
    // the counting allocator missed the engine allocation and the gate is
    // vacuous.
    assert!(
        deflate_bytes >= 200_000,
        "deflate engine retained only {deflate_bytes} bytes — the measurement likely missed the \
         engine allocation",
    );
    assert!(
        inflate_bytes >= 6_000,
        "inflate engine retained only {inflate_bytes} bytes — the measurement likely missed the \
         engine allocation",
    );
}
