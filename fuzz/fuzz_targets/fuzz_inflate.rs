#![no_main]
//! Arbitrary, attacker-controlled bytes into `inflate` -- the library's primary
//! untrusted-input attack surface.
//!
//! Ported from **`test/infcover.c`**, whose `inf()` driver (L284-L347) is the template
//! this target follows: install a tracking allocator, `inflateInit2` with a chosen
//! `windowBits`, collect a gzip header when the request is an auto-detect one, then feed
//! the input in fixed-size increments and call `inflate` until the input is consumed,
//! cloning the stream on each pass. The one thing that changes is where the parameters
//! come from: `inf()` is called with fixed arguments from a table of hand-written
//! streams, and here every one of them -- the payload, `windowBits`, the output-buffer
//! size, the chunk size, the flush mode, the allocation limit and the dictionary -- is
//! drawn from the fuzzer.
//!
//! # What this target asserts
//!
//! Three properties, and deliberately nothing about *contents*: arbitrary bytes are
//! almost never a valid DEFLATE stream, so "what came back out" is not a thing this
//! harness can predict. What it can hold the library to is:
//!
//! 1. **It never panics.** libFuzzer reports a panic as a crash, and so does the
//!    `abort` that `extern "C"` gives a panic trying to leave the facade.
//! 2. **It never hangs.** A malicious stream is exactly the input that can loop
//!    forever, so the driver carries hard iteration and total-output caps. See
//!    [`MAX_ITERATIONS`] and [`MAX_TOTAL_OUTPUT`] -- those bounds are a gate
//!    requirement, not a convenience.
//! 3. **It never over-reads or over-writes.** Two independent checks: the stream's own
//!    `total_in`/`total_out` accounting is reconciled against the bytes this harness
//!    actually offered and actually received, and the three `gz_header` buffers are
//!    surrounded by sentinel guard regions that must come back untouched.
//!
//! # Why the tracking allocator is ported and not simplified away
//!
//! `test/infcover.c`'s zone is worth more than a `malloc` shim, for three reasons that
//! all apply here:
//!
//! * It fills every block with `0xa5` (`test/infcover.c` L87) rather than zeros, so any
//!   path that quietly assumes zero-initialised memory misbehaves visibly.
//! * Its `mem_done` (L200-L234) detects leaks, non-LIFO frees and rogue frees. Those
//!   three are the observable face of the allocator contract: memory obtained from a
//!   caller's `zalloc` may only be returned to that same caller's `zfree`. Here they are
//!   assertions rather than messages on stderr, so a violation is a crash report.
//! * Its `limit` induces allocation failure at a controlled point, which turns
//!   `Z_MEM_ERROR` from a path fuzzing reaches by luck into one it reaches on purpose.
//!
//! # Unsafe
//!
//! This target drives the C ABI, so raw pointers are inherent to it and `unsafe` is
//! permitted -- `#![forbid(unsafe_code)]` binds the safe core crate, not this harness.
//! Every block below carries a `// SAFETY:` comment naming its invariant, the manifest
//! denies `unsafe_op_in_unsafe_fn` so each operation is scoped individually, and
//! `extern "C-unwind"` appears nowhere: the two allocator hooks are called *from* the
//! library and a panic crossing that edge would be undefined behaviour, so they are
//! panic-free by construction and answer every failure with a null pointer, exactly as
//! `mem_alloc` answers with `NULL` and the caller turns that into `Z_MEM_ERROR`.
//!
//! `assert!` inside the fuzz target body is a different matter and is the intended
//! reporting mechanism: it runs on this harness's own stack, never inside a callback.
//!
//! # Running it
//!
//! ```text
//! cd fuzz
//! cargo +nightly fuzz run fuzz_inflate -- -max_total_time=300
//! ```
//!
//! `+nightly` because `../rust-toolchain.toml` pins stable and cargo-fuzz's sanitizer
//! instrumentation is nightly-only. AddressSanitizer is on by default, so that command
//! is simultaneously the fuzzing gate and the ASan gate for the boundary layer.

use std::alloc::{alloc, dealloc, Layout};
use std::ffi::{c_int, c_long, c_ulong, c_void};
use std::mem::size_of;
use std::ptr::{self, addr_of_mut};

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;

// The facade is imported as `libz_rs_sys` and never as `z`. `crates/libz-rs-sys` sets
// `[lib] name = "z"` so that cargo emits the drop-in `libz.so`/`libz.a`, and cargo
// derives a dependency's default `--extern` name from the lib target rather than the
// package; `fuzz/Cargo.toml` therefore renames the key back to `libz_rs_sys`. Writing
// `use z::...` here would not compile.
use libz_rs_sys::{
    gz_header, inflate, inflateCodesUsed, inflateCopy, inflateEnd, inflateGetDictionary,
    inflateGetHeader, inflateInit2_, inflateMark, inflatePrime, inflateReset, inflateReset2,
    inflateSetDictionary, inflateSync, inflateSyncPoint, inflateUndermine, inflateValidate, uInt,
    uLong, voidpf, z_stream, Bytef, MAX_WBITS, ZLIB_VERSION, Z_BLOCK, Z_BUF_ERROR, Z_DATA_ERROR,
    Z_FINISH, Z_MEM_ERROR, Z_NEED_DICT, Z_NO_FLUSH, Z_OK, Z_STREAM_END, Z_STREAM_ERROR,
    Z_SYNC_FLUSH, Z_TREES,
};

// ---------------------------------------------------------------------------
//  Bounds -- the anti-hang mechanism
// ---------------------------------------------------------------------------
//
// EVERY CONSTANT IN THIS BLOCK IS A GATE REQUIREMENT. The fuzzing gate is zero crashes
// AND ZERO HANGS over at least 300 seconds per target, and `inflate` is the one entry
// point where a hang is a realistic input-dependent outcome rather than a bug in the
// harness: a few bytes of DEFLATE can legitimately expand without bound, and a stream
// that makes no progress makes none however many times it is asked. Nothing below is
// tuning for its own sake, and none of it may be raised or removed to "let the fuzzer
// explore more" -- the exploration a fuzzer needs comes from many cheap executions, not
// from a few expensive ones, and libFuzzer treats a slow unit as a failure.
//
// TERMINATION AUDIT. This file contains four loops and every one of them is bounded:
//
//   * `drive`'s decode loop -- bounded twice over, by `MAX_ITERATIONS` on passes and by
//     `MAX_TOTAL_OUTPUT` on cumulative output. Its natural exit is `avail_in == 0`, which
//     an input that consumes nothing while answering `Z_BUF_ERROR` would never reach; the
//     two caps are what make it terminate for *every* input rather than for well-behaved
//     ones. Reaching either is a successful stop and never an assertion failure.
//   * `mem_free`'s and `mem_done`'s list walks -- bounded by the length of the allocation
//     list, which is finite and acyclic by construction: a node is created once, inserted
//     at the head once, and unlinked before it is released, so no node can be reached
//     twice and none can point back into the chain.
//   * `GuardedBuf::assert_intact`'s scan -- a `for` over `0..GUARD_LEN`.
//
// A fifth thing that is *not* a loop but bounds the work all the same: no operation in
// `extras` iterates, so the whole post-loop stage is a fixed number of calls.

/// Largest payload actually offered to `inflate`, in bytes.
///
/// libFuzzer's own default `-max_len` is 4096; matching it means the clamp is normally
/// a no-op and only bites when a corpus entry or `-max_len` override is larger.
const MAX_PAYLOAD_LEN: usize = 4096;

/// Largest output buffer handed to one `inflate` call, in bytes.
///
/// The output buffer is refilled on every pass, so this is a per-call bound and
/// [`MAX_TOTAL_OUTPUT`] is the cumulative one.
const MAX_OUT_BUF_LEN: usize = 4096;

/// Total decompressed bytes one execution may accumulate before the driver stops.
///
/// This is the bound that actually contains a decompression bomb. Reaching it is a
/// legitimate, successful stop and never an assertion failure: the library did nothing
/// wrong, the harness simply declines to spend more of the time budget on one input.
const MAX_TOTAL_OUTPUT: usize = 1 << 20;

/// Hard cap on passes through the `inflate` loop.
///
/// The loop's natural exit is `avail_in == 0`, and a stream that consumes no input
/// while returning `Z_BUF_ERROR` -- which `inflate` is entitled to do, and which
/// `inf()` explicitly continues on -- would never reach it. This counter is what makes
/// the loop terminate for *every* input rather than for well-behaved ones.
const MAX_ITERATIONS: usize = 1024;

/// Cap on `inflateCopy` calls per execution.
///
/// `inf()` clones the stream on every pass, which is affordable for a handful of
/// hand-written streams and is not affordable here: a clone allocates a fresh state
/// block plus a fresh window and copies the live part of the window, so a thousand of
/// them would dominate the execution and starve the other four targets of CI time. The
/// first few passes are where the interesting states are anyway.
const MAX_COPIES: usize = 8;

/// Largest induced allocation limit, in bytes.
///
/// A complete inflate stream needs roughly a 7 KiB state block plus a 32 KiB window, so
/// a limit drawn from `1..=MAX_ALLOC_LIMIT` straddles the interesting boundary: some
/// draws refuse the state outright, some allow the state and refuse the window, and some
/// allow everything.
const MAX_ALLOC_LIMIT: usize = 1 << 17;

/// Largest dictionary offered to `inflateSetDictionary`.
const MAX_DICT_LEN: usize = 4096;

/// Buffer size `inflateGetDictionary` requires, `1 << MAX_WBITS` == 32768 bytes.
///
/// `zlib.h` L936-L942 obliges the caller to provide at least a whole window's worth and
/// gives the function no way to learn the buffer's real size, so this is a hard
/// requirement rather than a guess.
const DICT_OUT_LEN: usize = 1 << MAX_WBITS;

/// Largest `extra`, `name` or `comment` buffer advertised through `gz_header`.
///
/// Small on purpose. The point of these buffers is to be *overrun* if the library ever
/// fails to clamp, and a small capacity with a large advertised field length is the
/// shape that provokes it.
const MAX_HEADER_FIELD_LEN: usize = 256;

/// Bytes of sentinel placed either side of each `gz_header` buffer.
const GUARD_LEN: usize = 16;

/// The sentinel byte itself.
///
/// Deliberately not `0xa5`: that is the allocator's fill byte, so a distinct value keeps
/// "the library wrote past the buffer" and "the allocator initialised the block"
/// separable in a crash report.
const GUARD_BYTE: u8 = 0x5a;

/// Fill byte for every block the tracking allocator hands out -- `test/infcover.c` L87.
///
/// The C comment states the purpose exactly: fill with a non-zero value "to make sure
/// that the code isn't depending on zeros".
///
/// ★ MEASURED: substituting `0x00` here changes nothing. A 60-second session with a zero
/// fill completed 1,148,357 executions with the same coverage and no failure, so this port
/// has no dependency on zero-initialised memory to expose. That is a real result rather
/// than an absence of evidence, and it has a mechanical explanation: the facade fills every
/// *buffer* allocation with its own `0xA5` before handing it to the core, and the one
/// allocation it deliberately does not pre-fill -- the state block -- has every byte written
/// by the move that occupies it before anything reads it. So the port cannot observe this
/// byte at all. The fill is kept regardless: it is what makes that argument *checkable*
/// rather than merely stated, and it would catch a future change that started handing a
/// buffer out unfilled.
const ALLOC_FILL_BYTE: u8 = 0xa5;

/// Alignment every block from the tracking allocator is given.
///
/// NOT cosmetic, and not a free choice. `zlib`'s allocator contract is `malloc`'s, whose
/// result is suitably aligned for any fundamental type -- `zutil.c`'s `zcalloc` and
/// `test/infcover.c` L84 are both plain `malloc`. The facade holds an allocator to that:
/// its `reserve::<T>` hands a misaligned block straight back and answers `Z_MEM_ERROR`.
/// An under-aligned hook would therefore not crash, it would make *every* stream fail to
/// initialise and this target would silently fuzz nothing at all -- the quiet failure
/// mode worth guarding hardest against. Sixteen is `malloc`'s guarantee on every target
/// this port is built for and exceeds the alignment of every type the library allocates.
const BLOCK_ALIGN: usize = 16;

// ---------------------------------------------------------------------------
//  The tracking allocator -- `test/infcover.c` L55-L234
// ---------------------------------------------------------------------------
//
// Self-contained on purpose. cargo-fuzz compiles each file under `fuzz_targets/` as an
// independent `[[bin]]`, so a sibling helper module would simply not be part of any
// crate; `fuzz_deflate.rs` carries its own copy of this allocator rather than sharing
// one, and that duplication is the correct arrangement here rather than a smell.

/// One live allocation, strung into a singly linked list -- `struct mem_item`,
/// `test/infcover.c` L56-L60.
struct MemItem {
    /// Base address handed to the library.
    ptr: *mut u8,
    /// Bytes the library asked for. Kept so the free path can rebuild the exact
    /// [`Layout`] the block was allocated with; see [`block_layout`].
    size: usize,
    /// Next item, or null.
    next: *mut MemItem,
}

/// The root of the list and the statistics -- `struct mem_zone`, `test/infcover.c`
/// L62-L68.
struct MemZone {
    /// Head of the list of live allocations, or null.
    first: *mut MemItem,
    /// Bytes currently outstanding.
    total: usize,
    /// Largest value `total` ever reached.
    highwater: usize,
    /// Allocation ceiling in bytes, or zero for "no limit" -- `mem_limit`'s argument,
    /// `test/infcover.c` L176-L181.
    limit: usize,
    /// Count of frees that were not of the most recent allocation.
    notlifo: u32,
    /// Count of frees of addresses this zone never handed out.
    rogue: u32,
}

impl MemZone {
    /// An empty zone carrying `limit` as its ceiling -- `mem_setup` (L158-L173) with
    /// `mem_limit` (L176-L181) folded in.
    ///
    /// The limit is fixed at construction rather than adjusted later, and that is
    /// deliberate: the zone is reached through exactly one raw pointer for the whole of
    /// its life (see [`run`]), and giving it nothing to change mid-run keeps that single
    /// chain of provenance trivially intact.
    const fn new(limit: usize) -> Self {
        Self {
            first: ptr::null_mut(),
            total: 0,
            highwater: 0,
            limit,
            notlifo: 0,
            rogue: 0,
        }
    }
}

/// What the zone looked like when tracking stopped -- the numbers `mem_done`
/// (`test/infcover.c` L200-L234) prints, gathered so the caller can assert on them.
#[derive(Debug)]
struct MemReport {
    /// Blocks still outstanding when tracking stopped. `mem_done` reports these as
    /// `** ...: N bytes in M blocks not freed`.
    leaked_blocks: usize,
    /// Bytes still outstanding, the other half of the same message.
    leaked_bytes: usize,
    /// `mem_done`'s `N frees not LIFO`.
    notlifo: u32,
    /// `mem_done`'s `N frees not recognized`.
    rogue: u32,
    /// `mem_high`'s high water mark. Zero proves the hooks were never called, which is
    /// how this target detects that it has silently fuzzed nothing.
    highwater: usize,
}

/// The [`Layout`] a block of `len` requested bytes is allocated and released with.
///
/// One function, used by both the allocation and the free path, because that is what
/// makes the two agree by construction: releasing a block under a layout that differs in
/// size or alignment from the one it was allocated with is undefined behaviour, and it is
/// the sort AddressSanitizer reports as a mismatched free. Deriving the layout from the
/// recorded size through a single pure function removes the possibility rather than
/// testing for it.
///
/// `len.max(1)` handles a zero-byte request: [`alloc`] may not be called with a
/// zero-sized layout, while C's `malloc(0)` returns either null or a unique freeable
/// pointer. Rounding up to one byte reproduces the useful half of that and stays
/// deterministic, so the free path computes the same layout from the same recorded size.
/// The library never asks for zero bytes -- every `ZALLOC` names a positive item count
/// and a positive item size -- so this arm exists for completeness rather than for use.
///
/// Returns [`None`] rather than panicking when the request cannot be a layout at all,
/// which is what a hostile `items * size` product produces.
fn block_layout(len: usize) -> Option<Layout> {
    Layout::from_size_align(len.max(1), BLOCK_ALIGN).ok()
}

/// `zalloc` -- `mem_alloc`, `test/infcover.c` L71-L109.
///
/// # Panic-freedom
///
/// This function is called **from the library**, across an `extern "C"` boundary, so a
/// panic escaping it would be undefined behaviour. It is therefore panic-free by
/// construction and not merely by inspection: no `unwrap`, no `expect`, no indexing, no
/// slicing, no `assert`, no arithmetic that can overflow -- every product and sum is
/// checked or saturating -- and no allocation that aborts, because [`alloc`] reports
/// failure by returning null rather than by calling the out-of-memory handler. Every
/// failure path answers with a null pointer, which is precisely the C contract:
/// `zlib.h` L149 lets `zalloc` answer `Z_NULL` and the caller turns that into
/// `Z_MEM_ERROR`.
///
/// # Safety
///
/// `mem` must be either null or a pointer to a live [`MemZone`] that stays live and
/// unmoved for as long as the stream using it does, and that is reached through no other
/// path while this call runs. Both hold here: the zone is a local in [`run`] which
/// outlives the stream, and it is touched only through the one raw pointer installed in
/// `z_stream::opaque`.
unsafe extern "C" fn mem_alloc(mem: voidpf, count: uInt, size: uInt) -> voidpf {
    let zone = mem.cast::<MemZone>();

    // L79's `zone == NULL` half. Nothing below may run without a zone, and there is
    // nothing sensible to allocate for one either -- which is also what makes
    // `mem_free`'s corresponding branch unreachable: no block can ever originate from a
    // null zone.
    if zone.is_null() {
        return ptr::null_mut();
    }

    // L76: `size_t len = count * (size_t)size`. Computed in the wide type exactly as C
    // does, but checked: `count` and `size` are both 32-bit and their product need not
    // fit a `usize` on a 32-bit target, and even on a 64-bit one it need not fit the
    // `isize` bound `Layout` imposes. An overflow is a refused request.
    let Some(len) = usize::try_from(count)
        .ok()
        .and_then(|count| count.checked_mul(usize::try_from(size).unwrap_or(usize::MAX)))
    else {
        return ptr::null_mut();
    };

    // SAFETY: `zone` is non-null by the test above and, by this function's contract,
    // addresses a live `MemZone` reached through no other path for the duration of this
    // call. Two scalar fields are read out by value, so no borrow of the zone outlives
    // this statement.
    let (limit, total) = unsafe { ((*zone).limit, (*zone).total) };

    // L79's induced-failure half: `zone->limit && zone->total + len > zone->limit`. The
    // sum is checked rather than wrapped, so a request large enough to carry
    // `total + len` past zero cannot masquerade as fitting under the ceiling.
    if limit != 0 {
        match total.checked_add(len) {
            Some(after) if after <= limit => {}
            _ => return ptr::null_mut(),
        }
    }

    let Some(layout) = block_layout(len) else {
        return ptr::null_mut();
    };

    // L84: `ptr = malloc(len)`. `alloc` answers null on failure and never aborts, which
    // is what keeps this hook panic-free.
    // SAFETY: `layout` came from `block_layout`, which never produces a zero size -- the
    // one precondition `alloc` has.
    let block = unsafe { alloc(layout) };
    if block.is_null() {
        return ptr::null_mut();
    }

    // L87: `memset(ptr, 0xa5, len)`, and the highest-value line in the whole allocator.
    // The C comment gives the reason: a non-zero fill makes sure "that the code isn't
    // depending on zeros". Anywhere the port assumed zero-initialised state, this is
    // what would expose it.
    // SAFETY: `block` is non-null and `alloc` returned a region of `layout.size()`
    // bytes, which is `len.max(1)` and therefore at least `len`. The region is freshly
    // allocated, so nothing else references it and the write cannot overlap anything.
    unsafe { ptr::write_bytes(block, ALLOC_FILL_BYTE, len) };

    // L90-L94: the bookkeeping node, under the same null-on-failure discipline. If it
    // cannot be had, the block goes back before returning, so a failed allocation leaks
    // nothing.
    // `cast_ptr_alignment` is allowed for this one statement and for a reason clippy
    // cannot see: the lint compares the alignment of `alloc`'s `*mut u8` return type
    // against `MemItem`'s, whereas the alignment that actually governs is the one carried
    // by the `Layout` -- `Layout::new::<MemItem>()` requests `align_of::<MemItem>()`, so
    // the returned block is aligned for it by the allocator's contract. This is the idiom
    // `std::alloc::alloc`'s own documentation uses. The allow is written here, at the
    // statement, rather than on the function or the crate, so it cannot silently cover a
    // second cast added later.
    #[allow(clippy::cast_ptr_alignment)]
    // SAFETY: `Layout::new::<MemItem>()` is non-zero-sized -- `MemItem` holds two
    // pointers and a `usize` -- so `alloc`'s precondition holds, and the block it returns
    // carries that layout's alignment, which is what makes the cast below sound.
    let item = unsafe { alloc(Layout::new::<MemItem>()) }.cast::<MemItem>();
    if item.is_null() {
        // SAFETY: `block` was allocated moments ago with exactly `layout` and nothing
        // else has taken ownership of it, so this is the matching deallocation.
        unsafe { dealloc(block, layout) };
        return ptr::null_mut();
    }

    // L95-L100: fill the node in and insert it at the **head** of the list. Head
    // insertion is not an implementation detail -- it is the mechanism that makes a
    // non-LIFO free detectable at all, because it puts the most recent allocation where
    // `mem_free` looks first.
    // SAFETY: `item` is non-null and addresses `size_of::<MemItem>()` writable,
    // uninitialised bytes from the allocation above; `write` initialises them without
    // dropping what was there, which is correct for uninitialised storage. `zone` is
    // live per this function's contract and is the only path to the list.
    unsafe {
        item.write(MemItem {
            ptr: block,
            size: len,
            next: (*zone).first,
        });
        (*zone).first = item;

        // L103-L105: the statistics. Saturating rather than checked, because the
        // allocation has already succeeded and there is no failure left to report --
        // and because a wrapping `+` would panic in a debug build, which this hook may
        // never do. `total` cannot in practice approach `usize::MAX`; saturating is the
        // shape that makes that unnecessary to argue.
        let after = total.saturating_add(len);
        (*zone).total = after;
        if after > (*zone).highwater {
            (*zone).highwater = after;
        }
    }

    // L108: the allocated memory.
    block.cast::<c_void>()
}

/// `zfree` -- `mem_free`, `test/infcover.c` L112-L154.
///
/// The classification is the whole point and it is exactly C's: if the head of the list
/// matches, the free is LIFO and unremarkable; if a *later* item matches, the free is out
/// of order and `notlifo` records it; if nothing matches, the address was never handed
/// out by this zone and `rogue` records it. Those three outcomes are the observable face
/// of the allocator contract -- memory obtained from a caller's `zalloc` may only be
/// returned to that same caller's `zfree` -- and [`assert_zone_clean`] turns them into
/// crash reports.
///
/// # Panic-freedom
///
/// As [`mem_alloc`], and for the same reason: this is called from the library. The
/// subtraction is saturating and the list walk uses raw pointers with explicit null
/// tests rather than any construct that can panic.
///
/// # Safety
///
/// `mem` must satisfy [`mem_alloc`]'s contract. `ptr_arg` must be either null or a base
/// address this same zone returned from [`mem_alloc`] and has not yet been given back.
unsafe extern "C" fn mem_free(mem: voidpf, ptr_arg: voidpf) {
    let zone = mem.cast::<MemZone>();
    let block = ptr_arg.cast::<u8>();

    // L118-L121: C's `if (zone == NULL) { free(ptr); return; }`.
    //
    // DIVERGENCE, and it is forced rather than chosen. Rust cannot release a block
    // without the layout it was allocated with, and there is no layout to be had for a
    // pointer this harness did not allocate. Nor can the case arise: `mem_alloc` answers
    // a null zone with null, so no block ever originates from one, and this hook is only
    // ever installed alongside that one. Doing nothing is therefore the only sound
    // response -- guessing a layout would be undefined behaviour, and AddressSanitizer
    // would rightly report it.
    if zone.is_null() {
        return;
    }

    // L125-L140: find the item whose `ptr` matches, remove it from the list, and record
    // whether the match was at the head.
    let mut found: *mut MemItem = ptr::null_mut();

    // SAFETY: `zone` addresses a live `MemZone` per this function's contract, and every
    // item reachable from `first` was produced by `mem_alloc` and has not been released,
    // so each is a live, initialised `MemItem`. Every dereference below is guarded by an
    // explicit null test, and the whole walk happens with no other access to the list.
    unsafe {
        let head = (*zone).first;
        if !head.is_null() {
            if (*head).ptr == block {
                // L127-L128: the first one is it. A LIFO free, which needs no counter.
                (*zone).first = (*head).next;
                found = head;
            } else {
                // L130-L133: walk the rest of the list.
                let mut prev = head;
                let mut next = (*head).next;
                while !next.is_null() && (*next).ptr != block {
                    prev = next;
                    next = (*next).next;
                }
                if !next.is_null() {
                    // L134-L137: found, but not at the head.
                    (*prev).next = (*next).next;
                    (*zone).notlifo = (*zone).notlifo.saturating_add(1);
                    found = next;
                }
            }
        }
    }

    // L149-L150: not found at all.
    if found.is_null() {
        // SAFETY: `zone` is live per this function's contract.
        unsafe { (*zone).rogue = (*zone).rogue.saturating_add(1) };
        // C's L153 frees `ptr` regardless. This cannot, for the reason given above: the
        // address is by definition not one this zone allocated, so no layout for it
        // exists here. The `rogue` count is the report, and `assert_zone_clean` fires on
        // it.
        return;
    }

    // L143-L146 and L153: update the statistics, release the node, release the block.
    // SAFETY: `found` was removed from the list above, so nothing references it; it was
    // allocated by `mem_alloc` with `Layout::new::<MemItem>()` and initialised there, so
    // reading `size` reads an initialised field and the deallocation layout matches.
    // `block` equals the recorded `ptr` -- that is what made this item match -- and
    // `block_layout` recomputes, from the same recorded size, the identical layout the
    // block was allocated with. It is released exactly once, because the item is gone
    // from the list and no second free can find it.
    unsafe {
        let size = (*found).size;
        (*zone).total = (*zone).total.saturating_sub(size);
        dealloc(found.cast::<u8>(), Layout::new::<MemItem>());
        if let Some(layout) = block_layout(size) {
            dealloc(block, layout);
        }
    }
}

/// `mem_done` -- `test/infcover.c` L200-L234, minus the printing.
///
/// Releases whatever the library left behind, exactly as C does at L209-L217, and returns
/// the numbers C would have printed so the caller can assert on them. Freeing first and
/// reporting second is C's order and is the right one here too: an assertion that fires
/// should report a leak, not add one, and LeakSanitizer -- which AddressSanitizer enables
/// by default under cargo-fuzz -- would otherwise report the same blocks a second time.
///
/// The zone is not reset the way C's L229-L233 clears the stream's hooks, because both
/// are about to go out of scope together.
///
/// # Safety
///
/// `zone` must address a live [`MemZone`] that no stream is still using: every stream
/// that named it must already have been ended, or the blocks freed here are blocks the
/// library still believes it owns.
unsafe fn mem_done(zone: *mut MemZone) -> MemReport {
    let mut leaked_blocks = 0_usize;

    // SAFETY: `zone` addresses a live `MemZone` per this function's contract, and every
    // item reachable from `first` is a live, initialised `MemItem` whose `ptr` names a
    // block allocated with `block_layout(size)`. Each is visited exactly once and the
    // chain is read out of a node before that node is released, so nothing is touched
    // after being freed. No stream references any of it, per the contract.
    let (leaked_bytes, notlifo, rogue, highwater) = unsafe {
        let mut item = (*zone).first;
        while !item.is_null() {
            let next = (*item).next;
            let size = (*item).size;
            let block = (*item).ptr;
            if let Some(layout) = block_layout(size) {
                dealloc(block, layout);
            }
            dealloc(item.cast::<u8>(), Layout::new::<MemItem>());
            item = next;
            leaked_blocks = leaked_blocks.saturating_add(1);
        }
        (*zone).first = ptr::null_mut();
        (
            (*zone).total,
            (*zone).notlifo,
            (*zone).rogue,
            (*zone).highwater,
        )
    };

    MemReport {
        leaked_blocks,
        leaked_bytes,
        notlifo,
        rogue,
        highwater,
    }
}

/// Asserts that a finished zone is clean, and that it was used at all.
///
/// The first four assertions are `mem_done`'s three complaints turned into crash reports:
/// nothing leaked, no free out of order, no free of an address the zone never handed out.
/// Together they validate the allocator half of the C ABI contract -- that memory
/// obtained from a caller's `zalloc` is released only through that same caller's `zfree`,
/// in a discipline the caller can actually audit.
///
/// The fifth is a harness self-check rather than a check on the library, and it earns its
/// place: if `inflateInit2_` reported success then the state block must have come through
/// these hooks, so a zero high water mark means they were never really installed and this
/// execution tested nothing. That is the one failure mode a fuzz target can have while
/// looking perfectly healthy, so it is asserted rather than assumed.
///
/// ★ MEASURED: all three library-facing counters were shown to be reachable, because a
/// check that cannot fail is worth nothing. With `mem_free`'s list removal disabled the
/// first assertion fired at once (`leaked_blocks: 1, leaked_bytes: 7384`); with its
/// comparison made never to match, the rogue branch was taken and reported
/// (`rogue: 2`); and with `mem_alloc` appending at the tail instead of the head, the
/// non-LIFO assertion fired in isolation (`notlifo: 1, leaked_blocks: 0, rogue: 0`). That
/// last one is also an independent confirmation of the allocation order the port documents:
/// head insertion plus state-block-before-window is exactly what makes the real run LIFO.
fn assert_zone_clean(report: &MemReport, expect_allocation: bool) {
    assert_eq!(
        report.leaked_blocks, 0,
        "blocks not freed -- inflate leaked state: {report:?}"
    );
    assert_eq!(
        report.leaked_bytes, 0,
        "bytes not freed -- inflate leaked state: {report:?}"
    );
    assert_eq!(
        report.notlifo, 0,
        "frees not LIFO -- allocation order violated: {report:?}"
    );
    assert_eq!(
        report.rogue, 0,
        "frees not recognized -- a block was returned to the wrong allocator: {report:?}"
    );
    assert!(
        !expect_allocation || report.highwater > 0,
        "inflateInit2_ succeeded without allocating through the caller's hooks, \
         so this execution fuzzed nothing: {report:?}"
    );
}

// ---------------------------------------------------------------------------
//  Structured input
// ---------------------------------------------------------------------------

/// Everything one execution needs, drawn from the fuzzer.
///
/// `payload` is **last** on purpose: `libfuzzer_sys` builds a typed input through
/// `Arbitrary::arbitrary_take_rest`, whose derived form gives the final field whatever
/// bytes remain. Putting the untrusted stream there means the selectors cost a fixed,
/// small prefix and every other byte the fuzzer produces goes straight into `inflate`.
#[derive(Arbitrary, Debug)]
struct InflateInput {
    /// Selects a `windowBits` request. See [`window_bits_for`].
    window_bits: u8,
    /// Selects the `windowBits` for the post-loop `inflateReset2`, over the same space.
    reset_window_bits: u8,
    /// Selects the output-buffer size, clamped into `1..=MAX_OUT_BUF_LEN`.
    out_len: u16,
    /// Selects the chunk size the payload is fed in. Zero means "all at once", which is
    /// what `test/infcover.c` L312-L313 does with `step == 0`.
    step: u16,
    /// Selects the induced allocation ceiling. See [`alloc_limit_for`].
    alloc_limit: u16,
    /// Selects the `flush` argument. See [`flush_for`].
    flush: u8,
    /// `bits` for `inflatePrime`, spanning the legal `-32..=16` window and beyond it.
    prime_bits: i8,
    /// `value` for `inflatePrime`, unconstrained -- the function masks it itself.
    prime_value: i32,
    /// Which of the optional operations to perform. See [`ops`].
    ops: u16,
    /// Selects the advertised capacity of the `extra`, `name` and `comment` buffers, one
    /// selector each. Kept separate so the three capacities differ, which is what makes
    /// an overrun attributable to a particular field.
    header_caps: [u8; 3],
    /// Dictionary offered when the stream asks for one, and to `inflateSetDictionary`
    /// directly. Clamped to [`MAX_DICT_LEN`].
    dictionary: Vec<u8>,
    /// The untrusted stream. Clamped to [`MAX_PAYLOAD_LEN`].
    payload: Vec<u8>,
}

/// Bit positions within [`InflateInput::ops`].
///
/// A bitfield rather than a run of `bool` fields, for two reasons. The manifest denies
/// `clippy::pedantic`, which includes `struct_excessive_bools` at a threshold of three,
/// and this target has eight independent switches; and eight named bits cost one `u16` of
/// fuzzer input where eight `bool`s would cost eight bytes, which is eight bytes not
/// spent on the stream.
mod ops {
    /// Clone the stream with `inflateCopy` on each pass, as `test/infcover.c` L335-L336
    /// does.
    pub const COPY: u16 = 1 << 0;
    /// Install a `gz_header` when the request is an auto-detect one, and check its guard
    /// regions afterwards.
    pub const GET_HEADER: u16 = 1 << 1;
    /// Read the window back out with `inflateGetDictionary`.
    pub const GET_DICTIONARY: u16 = 1 << 2;
    /// Offer the dictionary directly with `inflateSetDictionary`, outside the
    /// `Z_NEED_DICT` path.
    pub const SET_DICTIONARY: u16 = 1 << 3;
    /// Exercise `inflateSync` and `inflateSyncPoint` on the state the loop left behind.
    pub const SYNC: u16 = 1 << 4;
    /// Exercise `inflatePrime`.
    pub const PRIME: u16 = 1 << 5;
    /// Exercise `inflateUndermine` and `inflateValidate`.
    pub const FLAGS: u16 = 1 << 6;
    /// Exercise `inflateReset` and `inflateReset2`.
    pub const RESET: u16 = 1 << 7;
}

/// `windowBits` requests that `inflate` must refuse cleanly.
///
/// Every one of these has to come back `Z_STREAM_ERROR` rather than panic, wrap, or
/// allocate. They are not arbitrary:
///
/// * `-16` is one past the raw floor -- `inflateReset2`'s `windowBits < -15` arm.
/// * `-1` passes that test and then fails the second one, because negating it gives an
///   exponent of 1 and `1 < 8`. It is the case a "check the sign, then check the range"
///   implementation gets wrong.
/// * `48` is the first request for which the `windowBits &= 15` masking is *skipped*, so
///   the exponent stays 48 and is rejected. `47` is accepted for exactly that reason,
///   which makes the pair the boundary worth pinning.
/// * `i32::MIN` cannot be negated at all. C reaches its `< -15` test first and never
///   tries; a port that negated first would overflow, which in a Rust debug build is a
///   panic and in a release build is a wrong answer.
const OUT_OF_RANGE_WINDOW_BITS: [c_int; 4] = [-16, -1, 48, c_int::MIN];

/// Maps a selector byte onto the `windowBits` space `inflate` accepts, plus a slice of
/// values it must refuse.
///
/// The space is the one `inflateReset2` (`inflate.c` L1129-L1155) defines, and it is
/// **wider than deflate's** in two ways that a target written by analogy to the deflate
/// one would miss: `-8` is legal here, and so is `0`.
///
/// | Bucket | Request | `wrap` | Meaning |
/// |---|---|---|---|
/// | 0..=11 | `47` | 7 | zlib-or-gzip auto-detect; the only request that enables header collection |
/// | 12..=19 | `8..=15` | 5 | zlib wrapper |
/// | 20..=27 | `-8..=-15` | 0 | raw DEFLATE; note `-8`, which `deflateInit2_` rejects |
/// | 28..=35 | `24..=31` | 6 | gzip wrapper |
/// | 36..=42 | `40..=46` | 7 | the rest of the auto-detect range |
/// | 43 | `0` | 5 | take the window size from the header |
/// | 44..=47 | [`OUT_OF_RANGE_WINDOW_BITS`] | -- | must be refused with `Z_STREAM_ERROR` |
///
/// A quarter of the mass sits on `47`, and deliberately so: `47 >> 4` is 2 so `wrap`
/// becomes 7, and `47 < 48` so the exponent masks down to 15. That combination is what
/// makes `inflateGetHeader` legal, and the header path is where the
/// `extra_max`/`name_max`/`comm_max` clamping that this target guards lives.
fn window_bits_for(selector: u8) -> c_int {
    let bucket = c_int::from(selector % 48);
    match bucket {
        0..=11 => 47,
        12..=19 => 8 + (bucket - 12),
        20..=27 => -(8 + (bucket - 20)),
        28..=35 => 24 + (bucket - 28),
        36..=42 => 40 + (bucket - 36),
        43 => 0,
        // 44..=47. Written as four arms indexing the table at constant positions rather
        // than as arithmetic on `bucket`, so the mapping is total and cannot panic: the
        // indices are checked when this file compiles, and the final arm absorbs
        // anything the modulus above could ever be changed to produce.
        44 => OUT_OF_RANGE_WINDOW_BITS[0],
        45 => OUT_OF_RANGE_WINDOW_BITS[1],
        46 => OUT_OF_RANGE_WINDOW_BITS[2],
        _ => OUT_OF_RANGE_WINDOW_BITS[3],
    }
}

/// Whether a `windowBits` request enables gzip header collection.
///
/// C's test is `(state->wrap & 2) == 0` (`inflate.c` L1225-L1226), and `wrap` is
/// `(windowBits >> 4) + 5` for a non-negative request. Bit 1 of that is set for
/// `24..=31` and `40..=47` and clear everywhere else, which is why `test/infcover.c`
/// installs a header only for `win == 47`.
fn collects_header(window_bits: c_int) -> bool {
    window_bits >= 0 && ((window_bits >> 4) + 5) & 2 != 0
}

/// `flush` values worth passing to `inflate`, plus one that must be refused.
///
/// `Z_PARTIAL_FLUSH` and `Z_FULL_FLUSH` are omitted because `inflate` treats them
/// identically to `Z_NO_FLUSH`, so they would cost input bytes and buy no coverage.
/// `Z_BLOCK` and `Z_TREES` are the two least-exercised inflate flush modes in the
/// existing suite and are the reason this table exists at all.
///
/// The final entry is deliberately **outside** the `0..=6` domain, and it is here to check
/// that it is *served* rather than refused. `inflate` validates `flush` nowhere -- see
/// [`assert_inflate_return`] -- so `7` must behave exactly as `Z_NO_FLUSH` behaves. It is
/// the one entry whose expected answer is a property of the implementation rather than of
/// the format.
const FLUSH_MODES: [c_int; 6] = [
    Z_NO_FLUSH,
    Z_SYNC_FLUSH,
    Z_FINISH,
    Z_BLOCK,
    Z_TREES,
    Z_TREES + 1,
];

/// Maps a selector byte onto [`FLUSH_MODES`].
fn flush_for(selector: u8) -> c_int {
    FLUSH_MODES[usize::from(selector) % FLUSH_MODES.len()]
}

/// Maps a selector onto an allocation ceiling in bytes, or zero for "no limit".
///
/// Weighted heavily towards no limit. A ceiling is valuable -- it is what turns
/// `Z_MEM_ERROR` from a path fuzzing reaches by luck into one it reaches on purpose --
/// but a *tight* ceiling makes `inflateInit2_` fail, and an execution that never gets a
/// stream is an execution that never reaches the decoder. Roughly one in eight draws
/// imposes one, which is often enough to keep the failure paths covered and rare enough
/// to leave the decoder the bulk of the budget.
fn alloc_limit_for(selector: u16) -> usize {
    if selector % 8 == 0 {
        // Inside `1..MAX_ALLOC_LIMIT`, spread across the whole range so that draws land
        // on all three of the interesting outcomes: too tight for the state block, tight
        // enough for the state but not the window, and roomy enough for both. Never
        // zero, because zero means "no limit" and this arm exists precisely to impose
        // one.
        1 + (usize::from(selector) * 2) % MAX_ALLOC_LIMIT
    } else {
        0
    }
}

/// The clamped, decoded parameters one execution runs with.
///
/// Every length and every selector is narrowed **here**, in one place, before any FFI
/// call happens. That is deliberate: a clamp applied at the point of use is a clamp that
/// can be forgotten at one of several points of use, whereas the driver below cannot see
/// an unclamped value at all because it is only ever handed this.
struct Params<'a> {
    /// The untrusted stream, already truncated to [`MAX_PAYLOAD_LEN`].
    payload: &'a [u8],
    /// The dictionary, already truncated to [`MAX_DICT_LEN`].
    dictionary: &'a [u8],
    /// Output-buffer capacity, in `1..=MAX_OUT_BUF_LEN`.
    ///
    /// Never zero. `test/infcover.c` never passes zero either, and there is a reason
    /// beyond fidelity: with `avail_out == 0` every `inflate` call answers `Z_BUF_ERROR`
    /// without consuming input, so the driver's `avail_in != 0` exit condition would
    /// never be met and every execution would run to [`MAX_ITERATIONS`] having tested
    /// nothing.
    out_len: usize,
    /// Bytes offered per pass, or zero for "all at once" -- `test/infcover.c` L312-L313.
    step: usize,
    /// `windowBits` for `inflateInit2_`.
    window_bits: c_int,
    /// `windowBits` for the optional post-loop `inflateReset2`.
    reset_window_bits: c_int,
    /// `flush` for every `inflate` call.
    flush: c_int,
    /// `bits` for the optional `inflatePrime`.
    prime_bits: c_int,
    /// `value` for the optional `inflatePrime`.
    prime_value: c_int,
    /// Induced allocation ceiling in bytes, or zero for no limit.
    alloc_limit: usize,
    /// Advertised capacities of the `extra`, `name` and `comment` buffers.
    header_caps: [usize; 3],
    /// The optional-operation selector; read through [`Params::enabled`].
    ops: u16,
}

impl<'a> Params<'a> {
    /// Narrows a fuzzer input into the parameter space the driver may be handed.
    fn from_input(input: &'a InflateInput) -> Self {
        let payload_len = input.payload.len().min(MAX_PAYLOAD_LEN);
        let dictionary_len = input.dictionary.len().min(MAX_DICT_LEN);
        Self {
            payload: &input.payload[..payload_len],
            dictionary: &input.dictionary[..dictionary_len],
            out_len: 1 + usize::from(input.out_len) % MAX_OUT_BUF_LEN,
            step: usize::from(input.step) % (MAX_PAYLOAD_LEN + 1),
            window_bits: window_bits_for(input.window_bits),
            reset_window_bits: window_bits_for(input.reset_window_bits),
            flush: flush_for(input.flush),
            prime_bits: c_int::from(input.prime_bits),
            prime_value: input.prime_value,
            alloc_limit: alloc_limit_for(input.alloc_limit),
            header_caps: [
                usize::from(input.header_caps[0]) % (MAX_HEADER_FIELD_LEN + 1),
                usize::from(input.header_caps[1]) % (MAX_HEADER_FIELD_LEN + 1),
                usize::from(input.header_caps[2]) % (MAX_HEADER_FIELD_LEN + 1),
            ],
            ops: input.ops,
        }
    }

    /// Whether the operation named by `mask` -- one of the [`ops`] constants -- is
    /// selected for this execution.
    fn enabled(&self, mask: u16) -> bool {
        self.ops & mask != 0
    }
}

// ---------------------------------------------------------------------------
//  Guarded buffers
// ---------------------------------------------------------------------------

/// A byte buffer with a sentinel region on either side of the usable capacity.
///
/// This is `test/infcover.c` L301's `out = malloc(len); assert(out != NULL);` with two
/// differences, both of which add checking rather than change behaviour.
///
/// First, the sentinel regions. The library is told about the middle only, so every byte
/// of either guard that comes back changed is a write past a boundary the caller
/// declared -- an over-write, reported as a crash. That is what "must never over-read or
/// over-write" means for a harness that cannot see inside the library.
///
/// Second, it is reached only through the pointer [`alloc`] returned, and never through a
/// borrow of a container. That matters for a buffer handed across an FFI boundary and
/// retained there: the pointer's provenance is the allocation itself, so the library
/// writing through it and this harness reading through it cannot invalidate one another
/// however the owning value is borrowed in between. A `Vec` would give the same bytes and
/// not that property.
struct GuardedBuf {
    /// Base of the whole allocation, guard region included.
    base: *mut u8,
    /// Usable bytes between the two guards.
    capacity: usize,
}

impl GuardedBuf {
    /// Allocates a buffer of `capacity` usable bytes, sentinel-filled end to end.
    ///
    /// The usable region starts filled with [`GUARD_BYTE`] as well, which is harmless --
    /// the library is free to write anything there -- and means a stray write just inside
    /// a boundary is as visible as one just outside it.
    fn new(capacity: usize) -> Self {
        // Two harness self-checks, not error handling. Every capacity this is called with
        // is bounded by a Phase A constant -- `MAX_OUT_BUF_LEN`, `MAX_HEADER_FIELD_LEN` or
        // `DICT_OUT_LEN` -- so neither can fire, and a failure would be a bug in this file
        // rather than in the library. Reporting a bug in this file as a crash is precisely
        // what a fuzz target's body is for: the panic-free discipline binds the two
        // `extern "C"` allocator hooks, which the library calls, and not harness setup,
        // which it does not.
        assert!(
            capacity <= usize::MAX - 2 * GUARD_LEN,
            "guarded buffer capacity {capacity} cannot carry its guard regions"
        );
        // Never zero: the two guards alone are `2 * GUARD_LEN` bytes, so the layout is
        // always valid even for a zero-capacity buffer. A zero *capacity* is a real case
        // and must stay reachable -- `zlib.h` L118-L133 lets a caller advertise a buffer
        // it has no room in, and the port documents that a non-null pointer with a zero
        // maximum has to behave as such.
        let total = capacity + 2 * GUARD_LEN;
        let layout = Layout::from_size_align(total, BLOCK_ALIGN)
            .expect("BLOCK_ALIGN is a power of two and the bounds above keep total small");
        // SAFETY: `layout` has a non-zero size -- it is at least `2 * GUARD_LEN` -- which
        // is `alloc`'s only precondition.
        let base = unsafe { alloc(layout) };
        assert!(!base.is_null(), "out of memory allocating a guarded buffer");
        // SAFETY: `base` is non-null and `alloc` returned `total` writable bytes, which is
        // exactly what is written here. The region is freshly allocated, so nothing else
        // references it.
        unsafe { ptr::write_bytes(base, GUARD_BYTE, total) };
        Self { base, capacity }
    }

    /// The usable region's base address -- what the library is given.
    ///
    /// Typed as `*mut Bytef` rather than `*mut u8` because that is what it becomes: the
    /// `next_out` of a `z_stream` and the `extra`, `name` and `comment` of a `gz_header`
    /// are all `Bytef *`, and naming the ABI's own alias at the boundary keeps the two
    /// spellings from drifting apart if the alias is ever anything but `u8`.
    fn data(&self) -> *mut Bytef {
        // SAFETY: the allocation is `capacity + 2 * GUARD_LEN` bytes, so `GUARD_LEN` is
        // an offset within it and the result is at worst a one-past-the-end pointer for a
        // zero-capacity buffer, which is a valid pointer to form.
        unsafe { self.base.add(GUARD_LEN) }
    }

    /// Usable bytes, excluding the guards.
    fn capacity(&self) -> usize {
        self.capacity
    }

    /// Asserts that neither guard region has been touched.
    ///
    /// `what` names the buffer so a crash report says which boundary was crossed.
    fn assert_intact(&self, what: &str) {
        for offset in 0..GUARD_LEN {
            // SAFETY: `offset` is below `GUARD_LEN` and `GUARD_LEN + capacity + offset` is
            // below `capacity + 2 * GUARD_LEN`, so both addresses lie inside the
            // allocation. Both bytes were initialised by `new` and can only have been
            // overwritten, never deinitialised.
            let (leading, trailing) = unsafe {
                (
                    self.base.add(offset).read(),
                    self.base.add(GUARD_LEN + self.capacity + offset).read(),
                )
            };
            assert_eq!(
                leading, GUARD_BYTE,
                "{what}: byte {offset} of the leading guard was overwritten -- \
                 a write before the start of a caller buffer"
            );
            assert_eq!(
                trailing, GUARD_BYTE,
                "{what}: byte {offset} of the trailing guard was overwritten -- \
                 a write past the end of a caller buffer of {} bytes",
                self.capacity
            );
        }
    }
}

impl Drop for GuardedBuf {
    fn drop(&mut self) {
        let Some(total) = self.capacity.checked_add(2 * GUARD_LEN) else {
            return;
        };
        let Ok(layout) = Layout::from_size_align(total, BLOCK_ALIGN) else {
            return;
        };
        // SAFETY: `base` came from `alloc` with exactly this layout in `new`, which is the
        // only place a `GuardedBuf` is built, and `Drop` runs once. Recomputing the layout
        // from the same recorded capacity guarantees it matches the allocation's.
        unsafe { dealloc(self.base, layout) };
    }
}

// ---------------------------------------------------------------------------
//  The driver -- `test/infcover.c` L284-L347
// ---------------------------------------------------------------------------

/// Narrows a length to the `uInt` the C API takes.
///
/// Total by construction: every length reaching this is bounded by a Phase A constant far
/// below `uInt::MAX`, so the saturating arm is unreachable and exists only so that this
/// cannot be the thing that panics.
fn narrow(len: usize) -> uInt {
    uInt::try_from(len).unwrap_or(uInt::MAX)
}

/// Widens a `uInt` the C API reported back to a `usize`.
fn widen(value: uInt) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}

/// `sizeof(z_stream)` as `inflateInit2_`'s `stream_size` argument.
///
/// Getting this wrong is not a compile error and is not loud: `inflateInit2_` compares it
/// against its own `size_of::<z_stream>()` and answers `Z_VERSION_ERROR`, so a wrong value
/// would leave this target initialising nothing at all while appearing to run. It is
/// derived rather than written down for exactly that reason, and the driver additionally
/// asserts that `Z_VERSION_ERROR` never comes back.
fn stream_size() -> c_int {
    c_int::try_from(size_of::<z_stream>()).unwrap_or(0)
}

/// A `z_stream` with every field explicitly set and the tracking allocator installed.
///
/// `test/infcover.c`'s `mem_setup` (L158-L173) assigns only `opaque`, `zalloc` and
/// `zfree` and leaves the rest of a stack `z_stream` uninitialised, which C permits
/// because `inflateInit2_` writes what it needs. Rust does not permit a partly
/// initialised struct, and writing all fourteen fields out is the better shape anyway:
/// the three the C code really chooses are visible as choices, and `Z_NULL` -- which the
/// facade exports as a `c_int` and which therefore cannot stand in for a pointer in Rust
/// -- is spelled `ptr::null()` where a pointer is what is meant.
fn new_stream(zone: *mut MemZone) -> z_stream {
    z_stream {
        next_in: ptr::null(),
        avail_in: 0,
        total_in: 0,
        next_out: ptr::null_mut(),
        avail_out: 0,
        total_out: 0,
        msg: ptr::null(),
        state: ptr::null_mut(),
        zalloc: Some(mem_alloc),
        zfree: Some(mem_free),
        opaque: zone.cast::<c_void>(),
        data_type: 0,
        adler: 0,
        reserved: 0,
    }
}

/// What the driver loop observed, gathered for the assertions that follow it.
struct LoopOutcome {
    /// Bytes this harness watched `inflate` consume, summed over every call.
    consumed: usize,
    /// Bytes this harness watched `inflate` produce, summed over every call.
    produced: usize,
    /// The subset of [`LoopOutcome::consumed`] contributed by calls that reached
    /// `inflate`'s epilogue, which is the only place the stream's `total_in` is updated.
    /// See [`reaches_epilogue`].
    accounted_in: usize,
    /// The `total_out` counterpart of [`LoopOutcome::accounted_in`].
    accounted_out: usize,
    /// Passes actually made.
    iterations: usize,
}

/// Whether a return value means `inflate` reached its epilogue.
///
/// This is what decides whether a call's consumption reaches `total_in` and `total_out`,
/// and it is a genuine and slightly surprising part of the contract rather than an
/// implementation detail. `inflate.c` updates the two totals in its `inf_leave` epilogue,
/// which three returns bypass:
///
/// * **`Z_NEED_DICT`** -- `case DICT` does `RESTORE(); return Z_NEED_DICT;`, so the caller
///   sees the header and dictionary identifier consumed in `avail_in` while `total_in`
///   never learns of them. This one matters most, because the driver *continues* after it,
///   so the shortfall can accumulate over several calls.
/// * **`Z_MEM_ERROR`** -- two ways. `case MEM` returns without `RESTORE()`, so no
///   consumption is visible either and the omission is invisible; and `inf_leave` calls
///   `updatewindow` *after* `RESTORE()` and returns on its failure three lines before
///   `strm->total_in += in`, so that one is visible.
/// * **`Z_STREAM_ERROR`** -- returns before `LOAD()` or from `case SYNC`, in both cases
///   without `RESTORE()`, so nothing is visible. Listed for completeness; the driver
///   asserts this value never comes back at all.
///
/// Modelling the rule exactly is what keeps [`assert_accounting`] an equality rather than
/// an inequality, and an equality is what makes it able to catch over-reporting in either
/// direction.
fn reaches_epilogue(ret: c_int) -> bool {
    !matches!(ret, Z_NEED_DICT | Z_MEM_ERROR | Z_STREAM_ERROR)
}

/// Asserts that `inflate` answered with a value `zlib.h` L405-L470 documents.
///
/// The full set is `Z_OK`, `Z_STREAM_END`, `Z_NEED_DICT`, `Z_DATA_ERROR`,
/// `Z_STREAM_ERROR`, `Z_MEM_ERROR` and `Z_BUF_ERROR`; anything else -- `Z_ERRNO`,
/// `Z_VERSION_ERROR`, or a value that is not a status code at all -- is a defect.
///
/// `Z_BUF_ERROR` is emphatically **not** a failure here. `inflate` returns it whenever no
/// progress was possible, which for a truncated or malformed stream is the common case,
/// and `test/infcover.c` L321 explicitly continues on it. Treating it as an error is the
/// classic way to write an inflate fuzz target that reports floods of false positives.
///
/// # ★ Why `Z_STREAM_ERROR` is ruled out entirely
///
/// The second assertion is the stronger and the more interesting one. `inflate` has exactly
/// four ways to answer `Z_STREAM_ERROR`, and this harness closes all four:
///
/// 1. The state check -- the stream here was initialised by `inflateInit2_` and has not
///    been ended, so it passes.
/// 2. The pointer guard (`inflate.c` L494-L496) -- `next_out` is always the live output
///    window, and `next_in` is always the live payload, so neither trips.
/// 3. A failed input snapshot, which the facade takes only when the input and output
///    regions overlap -- they are two separate allocations here, and an empty region is
///    disjoint from everything, so no snapshot is ever attempted.
/// 4. Being asked to decode from `SYNC` mode, which only `inflateSync` can enter. Every
///    `inflate` call this harness makes happens *before* [`try_sync`], so that mode is
///    unreachable during one. **If a future change moves the sync operations into the
///    decode loop, this assertion has to be revisited** -- it would then be reachable and
///    legitimate.
///
/// What is left is the case worth pinning: an out-of-domain `flush` must be **served, not
/// refused**. C's `inflate` validates `flush` nowhere -- it only ever compares the value
/// against `Z_FINISH`, `Z_BLOCK` and `Z_TREES` -- so any other integer behaves exactly as
/// `Z_NO_FLUSH` does, and a wrapper that forwards a flush it computed must not be told
/// `Z_STREAM_ERROR` where C would decompress. That is what [`FLUSH_MODES`]' final entry
/// exists to check, and it caught this very assertion asserting the opposite on its first
/// run.
fn assert_inflate_return(ret: c_int, flush: c_int) {
    assert!(
        matches!(
            ret,
            Z_OK | Z_STREAM_END
                | Z_NEED_DICT
                | Z_DATA_ERROR
                | Z_STREAM_ERROR
                | Z_MEM_ERROR
                | Z_BUF_ERROR
        ),
        "inflate returned {ret}, which is outside the documented return set"
    );
    assert_ne!(
        ret, Z_STREAM_ERROR,
        "inflate refused a well-formed call with flush {flush}; \
         an out-of-domain flush must behave as Z_NO_FLUSH, not be rejected"
    );
}

/// The `Z_NEED_DICT` response, standing in for `test/infcover.c` L323-L334.
///
/// The C driver's ladder there is white-box: it asserts a precise sequence of
/// `Z_DATA_ERROR`, `Z_MEM_ERROR` and `Z_OK` answers, and reaches the last one by writing
/// `mode = DICT` directly into the state through the `#[repr(C)]` prefix. That poke is
/// properly covered by `test/infcover.c` itself being relinked against this library
/// unmodified; reproducing it from here would mean this target depended on the state
/// layout, which is not what a black-box input fuzzer should know. What it does instead is
/// the useful half: offer the dictionary the fuzzer supplied -- possibly empty -- and
/// check that the answer is one the contract allows.
///
/// Returns whether the stream accepted it. A refusal ends the loop, because a stream still
/// waiting for a dictionary consumes no input, so continuing would spin to
/// [`MAX_ITERATIONS`] making no progress.
fn offer_dictionary(strm: *mut z_stream, params: &Params<'_>) -> bool {
    // SAFETY: `strm` addresses a live `z_stream` this function's caller initialised with
    // `inflateInit2_`, and `dictionary` is a slice this harness owns, so its pointer is
    // readable for its length. `inflateSetDictionary` only reads it and does not retain
    // it, so the borrow ends with the call. A zero length with the non-null,
    // possibly-dangling pointer an empty slice yields is the legal `(Z_NULL, 0)` shape the
    // port documents.
    let ret = unsafe {
        inflateSetDictionary(
            strm,
            params.dictionary.as_ptr(),
            narrow(params.dictionary.len()),
        )
    };
    assert!(
        matches!(ret, Z_OK | Z_DATA_ERROR | Z_MEM_ERROR | Z_STREAM_ERROR),
        "inflateSetDictionary returned {ret}, which is outside its documented return set"
    );
    ret == Z_OK
}

/// `inflateCopy` followed immediately by `inflateEnd` on the clone -- `test/infcover.c`
/// L335-L336.
///
/// Cheap, and it is the only thing in the tree that fuzzes the state-cloning path.
///
/// Ending the clone **immediately** is not incidental. `inflateCopy` allocates the state
/// block before the window precisely so that `inflateEnd`, which releases the window
/// before the block, is last-in-first-out against the tracking allocator's zone. That
/// holds only while nothing else allocates in between: hold a clone open across an
/// allocation in the original and the eventual release genuinely is out of order, and
/// `assert_zone_clean`'s `notlifo` assertion would fire on a harness bug rather than a
/// library one.
fn clone_and_end(strm: *mut z_stream) {
    // The destination is overwritten wholesale by `inflateCopy`, hooks included, so what
    // it starts as does not matter -- only that it is a real, distinct `z_stream`.
    let mut copy = new_stream(ptr::null_mut());
    // SAFETY: `strm` addresses a live initialised `z_stream`; `&mut copy` addresses a
    // distinct, fully initialised one, which is what `inflateCopy`'s disjointness check
    // requires. Nothing else references either for the duration of the call.
    let ret = unsafe { inflateCopy(addr_of_mut!(copy), strm) };
    assert!(
        matches!(ret, Z_OK | Z_MEM_ERROR | Z_STREAM_ERROR),
        "inflateCopy returned {ret}, which is outside its documented return set"
    );
    if ret != Z_OK {
        // `Z_MEM_ERROR` is entirely legitimate under an induced allocation ceiling, and
        // the contract is that `dest` was left untouched, so there is nothing to end.
        return;
    }
    // SAFETY: `copy` now holds a live state `inflateCopy` installed, reached through no
    // other pointer.
    let end = unsafe { inflateEnd(addr_of_mut!(copy)) };
    assert_eq!(end, Z_OK, "inflateEnd on a successful clone returned {end}");
}

/// Feeds the payload through `inflate` -- `test/infcover.c` L311-L341.
///
/// The C loop is reproduced step for step: chunk the input, refill `avail_out`/`next_out`
/// on every pass, stop on any answer other than `Z_OK`, `Z_BUF_ERROR` or `Z_NEED_DICT`,
/// clone the stream, then hand back whatever `inflate` did not consume and re-split it.
/// The two additions are the caps from Phase A, and they are what make the loop terminate
/// for *every* input rather than for well-behaved ones -- see [`MAX_ITERATIONS`] and
/// [`MAX_TOTAL_OUTPUT`].
fn drive(strm: *mut z_stream, params: &Params<'_>, out: &GuardedBuf) -> LoopOutcome {
    let mut outcome = LoopOutcome {
        consumed: 0,
        produced: 0,
        accounted_in: 0,
        accounted_out: 0,
        iterations: 0,
    };

    // L312-L316: `if (step == 0 || step > have) step = have;` then offer the first chunk.
    let mut have = params.payload.len();
    let step = if params.step == 0 || params.step > have {
        have
    } else {
        params.step
    };
    have -= step;

    // SAFETY: `strm` addresses a live `z_stream` the caller initialised with
    // `inflateInit2_` and which nothing else references. `payload` is a slice this harness
    // owns and keeps alive across the whole loop, so its pointer is readable for its
    // length; `step` is at most that length, so the `(next_in, avail_in)` pair it forms
    // names a readable region. An empty payload yields the non-null dangling pointer an
    // empty slice has together with `avail_in == 0`, which is the legal shape
    // `test/infcover.c` L294-L295 relies on.
    unsafe {
        (*strm).next_in = params.payload.as_ptr();
        (*strm).avail_in = narrow(step);
    }

    let mut copies = 0_usize;
    loop {
        // The anti-hang caps. Reaching either is a successful stop, not a failure: the
        // library did nothing wrong, this harness simply declines to spend more of the
        // time budget on one input.
        if outcome.iterations >= MAX_ITERATIONS || outcome.produced >= MAX_TOTAL_OUTPUT {
            break;
        }
        outcome.iterations += 1;

        // L318-L320: refill the output window and decode.
        // SAFETY: as above for `strm`. `out` is a live `GuardedBuf` this harness owns for
        // the whole loop, so its usable region is writable for `capacity` bytes and does
        // not overlap the payload, which is a separate allocation.
        let (before_in, ret) = unsafe {
            let before_in = (*strm).avail_in;
            (*strm).next_out = out.data();
            (*strm).avail_out = narrow(out.capacity());
            (before_in, inflate(strm, params.flush))
        };

        // SAFETY: as above. Two scalar fields read out by value.
        let (after_in, after_out) = unsafe { ((*strm).avail_in, (*strm).avail_out) };
        let call_in = widen(before_in.saturating_sub(after_in));
        let call_out = out.capacity().saturating_sub(widen(after_out));
        outcome.consumed += call_in;
        outcome.produced += call_out;
        if reaches_epilogue(ret) {
            outcome.accounted_in += call_in;
            outcome.accounted_out += call_out;
        }

        assert_inflate_return(ret, params.flush);

        // L321-L322: any other answer ends the run.
        if ret != Z_OK && ret != Z_BUF_ERROR && ret != Z_NEED_DICT {
            break;
        }

        // L323-L334, in the reduced form `offer_dictionary` documents.
        if ret == Z_NEED_DICT && !offer_dictionary(strm, params) {
            break;
        }

        // L335-L336, capped for throughput. See `MAX_COPIES`.
        if params.enabled(ops::COPY) && copies < MAX_COPIES {
            copies += 1;
            clone_and_end(strm);
        }

        // L338-L341: give back what was not consumed, re-split, and stop when there is
        // nothing left to offer.
        have += widen(after_in);
        let next = step.min(have);
        have -= next;
        // SAFETY: as above. `next_in` is left exactly where `inflate` advanced it to, so
        // it still addresses the first byte this harness has offered and not yet had
        // consumed, and `next` counts no more bytes than remain after it.
        unsafe { (*strm).avail_in = narrow(next) };
        if next == 0 {
            break;
        }
    }

    outcome
}

/// Reconciles the stream's own accounting against what this harness watched happen.
///
/// This is the over-read and over-write detector, and it is the strongest one available to
/// a harness that cannot see inside the library. `total_in` and `total_out` are the
/// library's numbers, accumulated inside `inflate`; `consumed` and `produced` are this
/// harness's, accumulated from the outside by differencing `avail_in` and `avail_out`
/// across each call. They must agree exactly, and both must stay within what was actually
/// offered: a library that reported consuming more bytes than were made available would
/// have read past `next_in`, and one that reported producing more than the output window
/// could hold would have written past `next_out`.
///
/// It must run **before** any reset. `inflateReset` and `inflateReset2` zero `total_in`
/// and `total_out`, and `inflateSync` deliberately preserves and restores them across the
/// reset it performs internally, so every one of those would make the comparison
/// meaningless rather than false.
///
/// # ★ What the totals are compared against, and why it is not the raw sum
///
/// `total_in` is **not** the sum of everything `avail_in` gave up, and expecting it to be
/// is the trap this assertion fell into twice before it was right. Three of `inflate`'s
/// return values bypass the epilogue that maintains the totals, so the comparison is
/// against the sum over the calls that reached it -- see [`reaches_epilogue`], which
/// carries the enumeration and the `inflate.c` line-by-line argument. That rule is the
/// reference's, not the port's; the induced allocation ceiling and the `FDICT` bit make
/// both visible paths routine rather than exotic.
///
/// Modelling it exactly rather than relaxing the comparison to an inequality is the whole
/// point: an equality catches a library that *over*-reports as readily as one that
/// under-reports, which an inequality would not.
fn assert_accounting(strm: *const z_stream, outcome: &LoopOutcome, params: &Params<'_>) {
    // SAFETY: `strm` addresses a live `z_stream` the caller initialised, and two scalar
    // fields are read out by value.
    let (total_in, total_out) = unsafe { ((*strm).total_in, (*strm).total_out) };
    let reported_in = usize::try_from(total_in).unwrap_or(usize::MAX);
    let reported_out = usize::try_from(total_out).unwrap_or(usize::MAX);

    assert_eq!(
        reported_in, outcome.accounted_in,
        "total_in disagrees with the bytes the epilogue-reaching calls gave up"
    );
    assert_eq!(
        reported_out, outcome.accounted_out,
        "total_out disagrees with the bytes the epilogue-reaching calls took"
    );
    assert!(
        outcome.accounted_in <= outcome.consumed && outcome.accounted_out <= outcome.produced,
        "the accounted subset exceeds the observed total, which cannot happen"
    );

    assert!(
        outcome.consumed <= params.payload.len(),
        "inflate consumed {} bytes from a {}-byte payload -- an over-read",
        outcome.consumed,
        params.payload.len()
    );
    assert!(
        outcome.produced <= params.out_len.saturating_mul(outcome.iterations),
        "inflate produced {} bytes into {} windows of {} bytes -- an over-write",
        outcome.produced,
        outcome.iterations,
        params.out_len
    );
}

/// `test/infcover.c` L344-L345: reset the stream, then end it.
///
/// The `-8` is the interesting part and is not a typo. `inflateReset2` has no
/// `windowBits == 8` rejection clause, so `-8` is a legal raw request for *inflate* --
/// unlike `deflateInit2_`, which refuses it outright with its
/// `(windowBits == 8 && wrap != 1)` test. A target written by analogy to the deflate one
/// would assert the wrong answer here.
fn teardown(strm: *mut z_stream) {
    // SAFETY: `strm` addresses a live `z_stream` the caller initialised with
    // `inflateInit2_` and has not yet ended, reached through no other pointer.
    let reset = unsafe { inflateReset2(strm, -8) };
    assert_eq!(
        reset, Z_OK,
        "inflateReset2(strm, -8) returned {reset}; -8 is a legal raw request for inflate"
    );
    // SAFETY: as above; after this the state is gone and `strm` must not be reused.
    let end = unsafe { inflateEnd(strm) };
    assert_eq!(end, Z_OK, "inflateEnd on a live stream returned {end}");
}

// ---------------------------------------------------------------------------
//  The rest of the inflate surface
// ---------------------------------------------------------------------------
//
// Everything in this section runs *after* the decode loop and after
// `assert_accounting`, and the ordering is a requirement rather than a convenience.
// `inflateReset`, `inflateReset2` and the reset inside `inflateSync` all disturb the
// stream's `total_in`/`total_out` accounting, so reconciling those numbers has to happen
// while they still mean what the loop made them mean.
//
// Running afterwards also puts these calls on a genuinely mid-stream state whenever the
// payload was truncated or malformed, which is the state worth exercising: a stream that
// has parsed part of a header, or stopped mid-block, is where `inflateSync`'s recovery
// path and `inflateMark`'s composite answer actually have something to say.
//
// Each operation is gated on a bit of `ops` so that the time budget is not dominated by
// them; the decoder is what this target exists to fuzz.

/// Reads the sliding window back out -- `inflateGetDictionary`, `zlib.h` L936.
///
/// The buffer is [`DICT_OUT_LEN`] == 32768 bytes and that is not a guess: `zlib.h`
/// L936-L942 obliges the caller to provide at least a whole window's worth, and the
/// function is given no way to learn the buffer's real size, so anything smaller is a
/// buffer overflow waiting for a stream with enough history. The guard regions confirm
/// the library respects the window bound it does know.
///
/// `dictLength` is an out-parameter that `zlib.h` documents no obligation on beforehand,
/// so it is passed uninitialised-in-spirit -- set to a sentinel here so that the
/// assertion below has something to detect a missing write with.
fn read_dictionary(strm: *mut z_stream) {
    let buffer = GuardedBuf::new(DICT_OUT_LEN);
    let mut length: uInt = uInt::MAX;
    // SAFETY: `strm` addresses a live `z_stream` initialised by `inflateInit2_` and not
    // yet ended. `buffer`'s usable region is writable for `DICT_OUT_LEN` bytes, which is
    // the capacity `zlib.h` L936-L942 requires, and `length` is a live, aligned, writable
    // `uInt` this frame owns. Neither aliases the other or the stream.
    let ret = unsafe { inflateGetDictionary(strm, buffer.data(), addr_of_mut!(length)) };
    assert_eq!(
        ret, Z_OK,
        "inflateGetDictionary on a live stream returned {ret}"
    );
    assert!(
        widen(length) <= DICT_OUT_LEN,
        "inflateGetDictionary reported {length} bytes of history, more than a window holds"
    );
    buffer.assert_intact("inflateGetDictionary destination");
}

/// Offers a dictionary outside the `Z_NEED_DICT` path -- `inflateSetDictionary`,
/// `zlib.h` L913.
///
/// A different question from the one [`offer_dictionary`] asks. There the stream had
/// requested a dictionary; here it has not, so the interesting answers are the refusals:
/// C returns `Z_STREAM_ERROR` for any wrapped stream that is not in `DICT` mode
/// (`inflate.c` L1140-L1141), and accepts one for a raw stream, where it seeds the window.
/// Both paths matter, and a zero-length dictionary -- which an empty `Vec` produces --
/// exercises the `(pointer, 0)` shape as well.
fn set_dictionary(strm: *mut z_stream, params: &Params<'_>) {
    // SAFETY: as `offer_dictionary`; the dictionary is read for the duration of the call
    // and not retained.
    let ret = unsafe {
        inflateSetDictionary(
            strm,
            params.dictionary.as_ptr(),
            narrow(params.dictionary.len()),
        )
    };
    assert!(
        matches!(ret, Z_OK | Z_DATA_ERROR | Z_MEM_ERROR | Z_STREAM_ERROR),
        "inflateSetDictionary returned {ret}, which is outside its documented return set"
    );
}

/// Attempts sync recovery on whatever state the loop left behind -- `inflateSync` and
/// `inflateSyncPoint`, `zlib.h` L951 and L2033.
///
/// The sync path is a real-world one -- it is how a reader resumes after a damaged block --
/// and outside this target the only thing in the tree that touches it is
/// `test/example.c`. All three of its answers are legitimate: `Z_BUF_ERROR` when there is
/// nothing left to search (`avail_in == 0` and fewer than eight bits held), `Z_DATA_ERROR`
/// when no `00 00 FF FF` marker is found, and `Z_OK` when one is.
fn try_sync(strm: *mut z_stream) {
    // SAFETY: `strm` addresses a live initialised `z_stream` reached through no other
    // pointer. `inflateSyncPoint` only reads the state.
    let point = unsafe { inflateSyncPoint(strm) };
    assert!(
        matches!(point, 0 | 1),
        "inflateSyncPoint returned {point}, which is neither a boolean nor a live-stream error"
    );
    // SAFETY: as above.
    let ret = unsafe { inflateSync(strm) };
    assert!(
        matches!(ret, Z_OK | Z_DATA_ERROR | Z_BUF_ERROR | Z_STREAM_ERROR),
        "inflateSync returned {ret}, which is outside its documented return set"
    );
}

/// Reads the two introspection values -- `inflateMark` and `inflateCodesUsed`,
/// `zlib.h` L1042 and L2029.
///
/// Called and discarded, because neither has a value this harness can predict. What is
/// being tested is that both are safe to ask in any state the fuzzer can reach; the two
/// assertions below are the only invariants that hold for *every* such state, and
/// tightening them further would manufacture false positives rather than find defects.
///
/// `inflateMark` returns a `long` and a **composite**, not a status code: the high bits are
/// the number of bits of the last consumed byte still unused -- C's `state->back`, which is
/// `-1` when no length or literal code is pending -- and the low sixteen are the progress
/// through a copy. A negative answer is therefore ordinary. Note in particular that
/// `-65536` is *both* the documented state-check failure value and a perfectly legitimate
/// answer, namely `back == -1` with no copy in progress, which is the state every freshly
/// reset stream is in (`inflate.c` L120). Asserting against that sentinel would fire on
/// almost every execution. What does hold is the floor on `back` itself: it is initialised
/// to `-1` and thereafter only ever assigned `0` or incremented by a bit count
/// (`inflate.c` L120, L920-L923, L938-L941, L952, L969), and the progress term is always
/// below 65536, so the arithmetic shift recovers `back` exactly.
///
/// `inflateCodesUsed` has a sentinel that genuinely cannot collide: it counts entries in a
/// table bounded by `ENOUGH`, a few thousand at most, so `ULONG_MAX` can only mean the
/// state check failed -- which for a live stream it must not have.
fn read_marks(strm: *mut z_stream) {
    // SAFETY: `strm` addresses a live initialised `z_stream` reached through no other
    // pointer, and `inflateMark` only reads the state.
    let mark: c_long = unsafe { inflateMark(strm) };
    // SAFETY: as immediately above; `inflateCodesUsed` likewise only reads the state.
    let codes: c_ulong = unsafe { inflateCodesUsed(strm) };
    assert!(
        (mark >> 16) >= -1,
        "inflateMark reported a back-pointer below the -1 sentinel: {mark}"
    );
    assert_ne!(
        codes,
        c_ulong::MAX,
        "inflateCodesUsed reported a failed state check on a live stream"
    );
}

/// Injects bits into the bit accumulator -- `inflatePrime`, `zlib.h` L1011.
///
/// The bounds are exactly C's (`inflate.c` L1073-L1084) and are worth pinning because they
/// are not the obvious ones: `bits == 0` is accepted and does nothing, a *negative* `bits`
/// is accepted and clears the accumulator rather than being rejected, `bits > 16` is
/// refused, and a request that would carry the accumulator past 32 bits is refused even
/// when `bits` itself is in range. The last of those depends on the stream's current
/// contents, which is why this asserts the return is one of the two rather than predicting
/// which.
fn prime(strm: *mut z_stream, params: &Params<'_>) {
    // SAFETY: `strm` addresses a live initialised `z_stream` reached through no other
    // pointer; both arguments are plain integers.
    let ret = unsafe { inflatePrime(strm, params.prime_bits, params.prime_value) };
    assert!(
        matches!(ret, Z_OK | Z_STREAM_ERROR),
        "inflatePrime returned {ret}, which is outside its documented return set"
    );
    if params.prime_bits > 16 {
        assert_eq!(
            ret, Z_STREAM_ERROR,
            "inflatePrime accepted {} bits, above the documented maximum of 16",
            params.prime_bits
        );
    }
}

/// Flips the two decode-behaviour flags -- `inflateUndermine` and `inflateValidate`,
/// `zlib.h` L2025 and L2037.
///
/// Cheap, and they are the two calls that change how the decoder treats a stream rather
/// than what it does with one.
///
/// `inflateUndermine` answering `Z_DATA_ERROR` is the **correct** answer for this build,
/// not a failure: the reference only honours the request under
/// `INFLATE_ALLOW_INVALID_DISTANCE_TOOFAR_ARRR`, and with that undefined it forces
/// `sane = 1` and refuses (`inflate.c` L1370-L1381). The port reproduces that exactly, so
/// asserting on the refusal is asserting on the shipped configuration.
///
/// `inflateValidate` always answers `Z_OK`, including when it *declines* the request:
/// C guards the set with `if (check && state->wrap)`, so asking a raw stream to validate a
/// check value it does not have clears the bit instead of setting it and still succeeds.
fn toggle_flags(strm: *mut z_stream, params: &Params<'_>) {
    // SAFETY: `strm` addresses a live initialised `z_stream` reached through no other
    // pointer; the second argument is a plain integer.
    let undermine = unsafe { inflateUndermine(strm, c_int::from(params.prime_bits != 0)) };
    assert_eq!(
        undermine, Z_DATA_ERROR,
        "inflateUndermine returned {undermine}; the shipped configuration refuses it"
    );
    // SAFETY: as above.
    let validate = unsafe { inflateValidate(strm, c_int::from(params.prime_value >= 0)) };
    assert_eq!(
        validate, Z_OK,
        "inflateValidate returned {validate}; it succeeds even when it declines the request"
    );
}

/// Restarts the stream -- `inflateReset` and `inflateReset2`, `zlib.h` L991 and L1001.
///
/// `inflateReset` always succeeds on a live stream. `inflateReset2` succeeds or fails
/// purely on its `windowBits` argument, and the fuzzer draws that from the same space the
/// initial request came from -- legal values *and* the four that must be refused -- so both
/// outcomes are asserted against [`window_bits_for`]'s own notion of which is which rather
/// than against a duplicated copy of the rule.
///
/// This runs last of the extras because it is destructive: it zeroes the accounting and
/// uninstalls any `gz_header`, so nothing after it can assert on either.
fn reset(strm: *mut z_stream, params: &Params<'_>) {
    // SAFETY: `strm` addresses a live initialised `z_stream` reached through no other
    // pointer.
    let plain = unsafe { inflateReset(strm) };
    assert_eq!(
        plain, Z_OK,
        "inflateReset on a live stream returned {plain}"
    );

    let requested = params.reset_window_bits;
    // SAFETY: as above; `requested` is a plain integer and every value of it is a request
    // `inflateReset2` is required to answer rather than trust.
    let ret = unsafe { inflateReset2(strm, requested) };
    if OUT_OF_RANGE_WINDOW_BITS.contains(&requested) {
        assert_eq!(
            ret, Z_STREAM_ERROR,
            "inflateReset2 accepted windowBits {requested}, which is outside the legal space"
        );
    } else {
        assert_eq!(
            ret, Z_OK,
            "inflateReset2 refused windowBits {requested}, which is inside the legal space"
        );
    }
}

/// Runs whichever of the optional operations `ops` selected.
///
/// Ordered so that the destructive one is last: [`reset`] zeroes the accounting and
/// uninstalls the header, so everything that wants a mid-stream state has already had it.
fn extras(strm: *mut z_stream, params: &Params<'_>) {
    if params.enabled(ops::GET_DICTIONARY) {
        read_dictionary(strm);
    }
    if params.enabled(ops::SET_DICTIONARY) {
        set_dictionary(strm, params);
    }
    if params.enabled(ops::SYNC) {
        try_sync(strm);
    }
    // Unconditional, and cheap enough to be: two reads with no allocation and no state
    // change, on a stream whose state the fuzzer chose. Gating them would only reduce how
    // often the states they inspect get inspected.
    read_marks(strm);
    if params.enabled(ops::PRIME) {
        prime(strm, params);
    }
    if params.enabled(ops::FLAGS) {
        toggle_flags(strm, params);
    }
    if params.enabled(ops::RESET) {
        reset(strm, params);
    }
}

// ---------------------------------------------------------------------------
//  The gzip header probe -- the highest-value security check in this file
// ---------------------------------------------------------------------------
//
// `inflateGetHeader` is the one entry point that hands the library three
// caller-owned buffers and lets an *attacker-controlled* stream decide how many bytes go
// into each. The reference clamps all three -- `extra` only while
// `extra_len - length < extra_max` and then to `extra_max - len` (`inflate.c`
// L614-L621), `name` only while `length < name_max` (L639-L642), `comment` only while
// `length < comm_max` (L661-L664) -- and those three bounds are historically where gzip
// header overflow defects have lived. This probe is what turns "the port clamps" from a
// claim into a check.
//
// It departs from `test/infcover.c` L302-L310 in one respect, deliberately. The C driver
// aims all three fields at the *same* `out` buffer, which is legal and which the port
// explicitly supports; but an overrun then tells you only that *something* wrote too far.
// Three separate buffers, of three different sizes, each between its own pair of guard
// regions, say which field did it and by how much.

/// A `gz_header` and the three guarded buffers it points into.
///
/// The whole probe must outlive the stream. `inflateGetHeader` records the pointer and the
/// library holds it until the header completes, the stream is reset or the stream is
/// ended, so freeing any part of this early would be a use-after-free in the library --
/// exactly as it would be under C.
struct HeaderProbe {
    /// The structure whose address is handed to `inflateGetHeader`.
    ///
    /// Reached through one raw pointer from [`HeaderProbe::install`] onwards, and not
    /// touched again until [`HeaderProbe::verify`] runs after the stream has been ended.
    header: gz_header,
    /// Destination for the `FEXTRA` field.
    extra: GuardedBuf,
    /// Destination for the `FNAME` field.
    name: GuardedBuf,
    /// Destination for the `FCOMMENT` field.
    comment: GuardedBuf,
}

impl HeaderProbe {
    /// Builds the three buffers and a `gz_header` whose output members carry sentinels.
    ///
    /// Every one of the seven members the *implementation* fills starts at a value the
    /// implementation cannot legitimately produce -- `-1` for the four `int`s, `0` for
    /// `time` and `extra_len` -- so [`HeaderProbe::verify`] can tell "the stream said so"
    /// from "nobody wrote this". `zlib.h` L118-L133 makes those seven outputs, and
    /// `inflateGetHeader` neither reads nor writes any of them bar `done`, so a C caller is
    /// entitled to leave them uninitialised; Rust is not, and sentinels are the more useful
    /// choice than zeros anyway.
    fn new(params: &Params<'_>) -> Self {
        Self {
            header: gz_header {
                text: -1,
                time: 0,
                xflags: -1,
                os: -1,
                // Filled in by `install`, once the probe has reached its final address.
                extra: ptr::null_mut(),
                extra_len: 0,
                extra_max: 0,
                name: ptr::null_mut(),
                name_max: 0,
                comment: ptr::null_mut(),
                comm_max: 0,
                hcrc: -1,
                done: -1,
            },
            extra: GuardedBuf::new(params.header_caps[0]),
            name: GuardedBuf::new(params.header_caps[1]),
            comment: GuardedBuf::new(params.header_caps[2]),
        }
    }

    /// Points the header at the three buffers and returns its address.
    ///
    /// Called once the probe is at its final address, because the returned pointer must
    /// stay valid for the life of the stream. The buffer pointers themselves are
    /// address-stable regardless: each names a separate heap allocation, so moving the
    /// probe would move only the `gz_header`.
    ///
    /// ★ Nothing may touch the probe between this call and [`HeaderProbe::verify`]. The
    /// library holds the pointer this returns and writes through it during `inflate`;
    /// interposing a borrow of `self` would put a second, conflicting access path on the
    /// same memory. `verify` is safe because by then the stream has been ended and the
    /// library has let go.
    fn install(&mut self) -> *mut gz_header {
        let header = addr_of_mut!(self.header);
        // SAFETY: `header` addresses this probe's own `gz_header`, which is live, aligned
        // and initialised by `new`. The three data pointers name three distinct
        // allocations that this probe owns and keeps alive, each writable for the capacity
        // written beside it. Field-wise writes through `addr_of_mut!` avoid forming a
        // reference to the structure, so this establishes the single access path the
        // library is about to share.
        unsafe {
            addr_of_mut!((*header).extra).write(self.extra.data());
            addr_of_mut!((*header).extra_max).write(narrow(self.extra.capacity()));
            addr_of_mut!((*header).name).write(self.name.data());
            addr_of_mut!((*header).name_max).write(narrow(self.name.capacity()));
            addr_of_mut!((*header).comment).write(self.comment.data());
            addr_of_mut!((*header).comm_max).write(narrow(self.comment.capacity()));
        }
        header
    }

    /// Checks the guards and the reported fields, after the stream has been ended.
    ///
    /// ★ MEASURED, both directions. Over-advertising each maximum by [`GUARD_LEN`] and
    /// feeding a gzip member whose `FNAME` is 32 bytes against a zero-capacity buffer made
    /// the guard fire immediately and attributably -- "gz_header.name: byte 0 of the
    /// trailing guard was overwritten -- a write past the end of a caller buffer of 0
    /// bytes", with `0x41` where `0x5a` belonged. So the detector works, and the library
    /// writes exactly as far as it is told it may. Restoring the true maxima and repeating
    /// the same crafted member against an 8-byte buffer produced no failure over 5,000
    /// executions: the 32-byte name was truncated to 8 and both guards came back clean. The
    /// clamp at `inflate.c` L639-L642 is therefore honoured to the byte, which is what this
    /// probe exists to establish.
    fn verify(&self) {
        self.extra.assert_intact("gz_header.extra");
        self.name.assert_intact("gz_header.name");
        self.comment.assert_intact("gz_header.comment");
        self.assert_fields_consistent();
    }

    /// Asserts what the reported header members must satisfy, and nothing more.
    ///
    /// The restraint matters as much as the checks: a gzip member may legitimately carry
    /// any modification time, any OS byte and any extra-field length, so this asserts
    /// structure rather than content.
    ///
    /// * The three **capacities** must come back exactly as they went in. The library reads
    ///   them and has no business writing them, and a library that "helpfully" reduced one
    ///   would make every later bound check meaningless.
    /// * The three **pointers** must be either untouched or set to null. Nulling is
    ///   specified behaviour -- `inflate.c` L606, L651 and L673 store `Z_NULL` into the
    ///   caller's structure when the stream carries no such field, and the port reproduces
    ///   it -- but redirecting one anywhere else is not.
    /// * `done` must be `-1`, `0` or `1`. Those are the only three values written:
    ///   `inflateGetHeader` writes `0` (L1229), a completed header writes `1` (L688), and a
    ///   stream that turned out to be *zlib* under an auto-detect request writes `-1`
    ///   (L523).
    /// * `done == -1` implies **nothing else was written**, because that assignment happens
    ///   on the very first header byte, before any of the gzip states can run. This is the
    ///   sharpest check here: it says the library did not scribble on a caller's structure
    ///   for a stream that never had a gzip header at all.
    /// * A completed header (`done == 1`) implies the four byte-derived members hold
    ///   byte-derived values: `text` and `hcrc` are single bits, `xflags` and `os` come
    ///   from one byte each, and `time` from four.
    fn assert_fields_consistent(&self) {
        let header = &self.header;

        assert_eq!(
            header.extra_max,
            narrow(self.extra.capacity()),
            "the library modified extra_max"
        );
        assert_eq!(
            header.name_max,
            narrow(self.name.capacity()),
            "the library modified name_max"
        );
        assert_eq!(
            header.comm_max,
            narrow(self.comment.capacity()),
            "the library modified comm_max"
        );

        assert!(
            header.extra.is_null() || ptr::eq(header.extra, self.extra.data()),
            "the library redirected gz_header.extra somewhere other than null"
        );
        assert!(
            header.name.is_null() || ptr::eq(header.name, self.name.data()),
            "the library redirected gz_header.name somewhere other than null"
        );
        assert!(
            header.comment.is_null() || ptr::eq(header.comment, self.comment.data()),
            "the library redirected gz_header.comment somewhere other than null"
        );

        assert!(
            matches!(header.done, -1..=1),
            "gz_header.done is {}, which is none of the three values the header states write",
            header.done
        );

        if header.done == -1 {
            assert_eq!(
                (header.text, header.xflags, header.os, header.hcrc),
                (-1, -1, -1, -1),
                "a zlib stream under auto-detect wrote header members other than done"
            );
            assert_eq!(
                (header.time, header.extra_len),
                (0, 0),
                "a zlib stream under auto-detect wrote header members other than done"
            );
        }

        if header.done == 1 {
            assert!(
                matches!(header.text, 0 | 1),
                "gz_header.text is {}, but it is bit 0 of the flags byte",
                header.text
            );
            assert!(
                matches!(header.hcrc, 0 | 1),
                "gz_header.hcrc is {}, but it is bit 9 of the flags word",
                header.hcrc
            );
            assert!(
                (0..=0xff).contains(&header.xflags),
                "gz_header.xflags is {}, but it comes from a single byte",
                header.xflags
            );
            assert!(
                (0..=0xff).contains(&header.os),
                "gz_header.os is {}, but it comes from a single byte",
                header.os
            );
            assert!(
                header.time <= uLong::from(u32::MAX),
                "gz_header.time is {}, but it comes from four bytes",
                header.time
            );
        }
    }
}

// ---------------------------------------------------------------------------
//  Assembly
// ---------------------------------------------------------------------------

fuzz_target!(|input: InflateInput| {
    run(&input);
});

/// One execution: set up, initialise, decode, check, tear down, check again.
///
/// The declaration order is load-bearing and is the reason this reads slightly
/// back-to-front. `out` and `probe` are declared **before** the stream because Rust drops
/// locals in reverse declaration order, so declaring them first guarantees they are still
/// alive when [`teardown`] ends the stream. `inflateGetHeader` hands the library a pointer
/// it keeps until the header completes or the stream is reset or ended, so a buffer that
/// went out of scope first would be a use-after-free inside the library.
///
/// The zone and the stream are each reached through exactly **one** raw pointer, taken with
/// `addr_of_mut!` and used for every subsequent access. Mixing that with direct use of the
/// bindings would put two access paths on memory the library also holds a pointer to; one
/// path is both sufficient and the only arrangement worth reasoning about.
fn run(input: &InflateInput) {
    let params = Params::from_input(input);

    let out = GuardedBuf::new(params.out_len);
    let mut probe = HeaderProbe::new(&params);

    let mut zone_storage = MemZone::new(params.alloc_limit);
    let zone = addr_of_mut!(zone_storage);

    let mut stream_storage = new_stream(zone);
    let strm = addr_of_mut!(stream_storage);

    // `test/infcover.c` L296: `inflateInit2(&strm, win)`, which is a macro over this.
    // `inflateInit2` is not a symbol in either implementation, so a Rust caller performs
    // the version-and-layout handshake the macro arranges by hand.
    // SAFETY: `strm` addresses a fully initialised `z_stream` whose allocator hooks are
    // both non-null and whose `opaque` is the live zone above; `ZLIB_VERSION` is a
    // `'static` NUL-terminated string, which is what the `*const c_char` parameter needs.
    let init = unsafe {
        inflateInit2_(
            strm,
            params.window_bits,
            ZLIB_VERSION.as_ptr(),
            stream_size(),
        )
    };

    if init != Z_OK {
        // `test/infcover.c` L297-L300: nothing was initialised, so there is nothing to end.
        // Only two failures are reachable. `Z_STREAM_ERROR` is a `windowBits` outside the
        // legal space, and `Z_MEM_ERROR` is the induced allocation ceiling biting. A
        // `Z_VERSION_ERROR` would mean `stream_size` or `ZLIB_VERSION` is wrong and that
        // this target has been fuzzing nothing, which is why it is called out by name.
        assert!(
            matches!(init, Z_STREAM_ERROR | Z_MEM_ERROR),
            "inflateInit2_ with windowBits {} returned {init}; a Z_VERSION_ERROR here \
             would mean the version handshake is wrong and nothing is being fuzzed",
            params.window_bits
        );
        // SAFETY: no stream holds the zone -- initialisation failed, and a failed
        // `inflateInit2_` releases whatever it had reserved before returning.
        let report = unsafe { mem_done(zone) };
        assert_zone_clean(&report, false);
        return;
    }

    // `test/infcover.c` L302-L310, but only when the request actually permits it: C's
    // `(state->wrap & 2) == 0` test refuses a header on a zlib or raw stream, which is why
    // the C driver installs one for `win == 47` alone.
    let probing_header = params.enabled(ops::GET_HEADER) && collects_header(params.window_bits);
    if probing_header {
        let header = probe.install();
        // SAFETY: `strm` addresses the live stream initialised above. `header` addresses
        // `probe`'s own `gz_header`, whose three buffers `install` has just pointed at live
        // allocations `probe` owns; `probe` outlives the stream by declaration order, and
        // nothing touches it again until after `teardown`.
        let got = unsafe { inflateGetHeader(strm, header) };
        assert_eq!(
            got, Z_OK,
            "inflateGetHeader was refused for windowBits {}, which permits gzip decoding",
            params.window_bits
        );
    }

    let outcome = drive(strm, &params, &out);

    // Before anything that resets the stream: see `assert_accounting`.
    assert_accounting(strm, &outcome, &params);

    // The output window is the other buffer an over-write would land in, and the only one
    // `inflate` writes on every pass.
    out.assert_intact("inflate output window");

    extras(strm, &params);

    // `test/infcover.c` L344-L345.
    teardown(strm);

    // Now that the stream is gone the library holds no pointer into the probe, so reading
    // it back is unambiguous.
    if probing_header {
        probe.verify();
    }

    // `test/infcover.c` L346: `mem_done(&strm, what)`. The stream has been ended, so a
    // clean zone is the contract and anything else is a defect.
    // SAFETY: `teardown` ended the stream, so nothing references the zone's blocks.
    let report = unsafe { mem_done(zone) };
    assert_zone_clean(&report, true);
}
