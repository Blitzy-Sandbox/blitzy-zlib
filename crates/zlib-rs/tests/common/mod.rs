//! Shared test support for the `zlib-rs` integration suites.
//!
//! Every integration test binary under `crates/zlib-rs/tests` pulls this module in
//! with `mod common;` and reaches its helpers through it. Four things live here and
//! nothing else does:
//!
//! | Item | Mirrors | Purpose |
//! |---|---|---|
//! | [`check_err`] | `CHECK_ERR`, `test/example.c` L28-L33 | assert a status code is success |
//! | [`h2b`] | `h2b`, `test/infcover.c` L245-L272 | decode the harness's liberal hex vectors |
//! | [`TrackingAllocator`] | `mem_zone` and friends, `test/infcover.c` L56-L234 | instrumented allocation |
//! | [`corpus`] | `test/example.c` fixtures plus the classes of the plan's minimal corpus | deterministic payloads |
//!
//! # Why this is `common/mod.rs` and not `common.rs`
//!
//! Cargo turns **every** `.rs` file placed directly in `tests/` into its own test
//! binary. A sibling `tests/common.rs` would therefore be compiled and linked as a
//! thirteenth test target that contains no `#[test]` function reachable from a
//! suite, reporting `0 passed` on every run and confusing anyone reading the
//! output. Files in a *subdirectory* of `tests/` are exempt from that rule, so the
//! directory form is not a stylistic preference -- it is the only spelling that
//! makes a shared module shared rather than a spurious target.
//!
//! # Why the whole file is `#![allow(dead_code)]`
//!
//! Each test binary that declares `mod common;` compiles this module in its
//! entirety, so any helper a *particular* suite happens not to call is dead code
//! *in that binary*. The workspace lint gate is `-D warnings`, which would turn
//! each of those into a hard error and make it impossible for the suites to share
//! anything they do not all use. The allow is deliberately the only crate-level
//! relaxation in the file: in particular `dead_code` does **not** cover
//! `unused_imports`, so every `use` below is genuinely used here and an unused one
//! would still break every suite at once.
//!
//! # Constraints this module is written to
//!
//! * **No `unsafe`, anywhere.** The crate under test carries
//!   `#![forbid(unsafe_code)]`; its test support holds itself to the same standard.
//!   Where a block's identity is needed for the allocator's live list it is taken
//!   as `slice.as_ptr() as usize` -- a pointer-to-integer cast, which discards the
//!   ability to reach the memory rather than fabricating it, and needs no escape
//!   hatch because nothing is ever read through it.
//! * **No third-party crates.** `crates/zlib-rs/Cargo.toml` has an empty
//!   `[dependencies]` table and declares no `[dev-dependencies]`, and the `[bans]`
//!   section of `deny.toml` names that table as the enforcement point. Only `core`,
//!   `alloc`, `std` and `zlib_rs` itself are available, so the pseudo-random filler
//!   is [`lcg_fill`] rather than a `rand` dependency and the assertions are the
//!   built-in macros rather than an assertion crate.
//! * **`std` is available and `no_std` is not.** The library crate is
//!   `#![cfg_attr(not(feature = "std"), no_std)]`, but an integration test is its
//!   own crate and links `std` unconditionally, so no `#![no_std]` appears here.
//! * **No network, no filesystem, no environment, no clock.** Every payload is
//!   computed from constants, so `cargo test` is hermetic and reproducible
//!   byte-for-byte across runs and platforms. The opt-in Silesia download used for
//!   throughput measurement lives in `crates/zlib-rs-differential/corpus`, never
//!   here.
//! * **No `#[cfg(feature = ...)]`.** This module compiles identically under
//!   `--no-default-features`, `--features std`, `--features simd` and
//!   `--all-features`.
//!
//! # Reaching the crate by module path, not by re-export
//!
//! `zlib-rs` declares `default = []`, and every re-export at the crate root carries
//! `#[cfg(feature = "rust-api")]`. So `zlib_rs::ReturnCode` does not exist in a
//! default build, while `zlib_rs::error::ReturnCode` always does: `pub mod error`
//! and `pub mod allocate` are unconditional. Everything below therefore imports by
//! module path, which is both what makes the no-feature-gating rule above
//! achievable and what the C ABI facade does for the same reason.

#![allow(dead_code)]

use core::cell::RefCell;

use zlib_rs::allocate::{
    block_len, Allocator, AllocatorId, Buffer, ForeignBlock, Opaque, SENTINEL_FILL,
};
use zlib_rs::error::ReturnCode;

/// Asserts that a status code reports success, panicking with `msg` if it does not.
///
/// The Rust form of `CHECK_ERR` (`test/example.c` L28-L33):
///
/// ```c
/// #define CHECK_ERR(err, msg) { \
///     if (err != Z_OK) { \
///         fprintf(stderr, "%s error: %d\n", msg, err); \
///         exit(1); \
///     } \
/// }
/// ```
///
/// A panic is the faithful translation of that `exit(1)`. Inside a test harness it
/// fails exactly the test that tripped it, names the offending step and leaves the
/// remaining tests to run, where terminating the process would abandon the whole
/// binary and report nothing useful.
///
/// The message carries the **numeric** status code, because that is what the C
/// macro prints and therefore what a developer comparing the two implementations
/// will be looking for. The `Debug` form of [`ReturnCode`] is included alongside it
/// as a convenience -- it names the constant, so `-5` reads as `ReturnCode(-5)`
/// without a trip to `zlib.h` -- but it is an addition to the integer, never a
/// substitute for it.
///
/// # Panics
///
/// If `ret` is anything other than `ReturnCode::OK`, which is `Z_OK`.
pub fn check_err(ret: ReturnCode, msg: &str) {
    // Bound before the assertion so the message reads in the order the C macro
    // prints it. `ReturnCode::as_i32` is a `const fn` over a newtype field, so
    // this costs nothing on the success path.
    let code = ret.as_i32();
    assert!(ret == ReturnCode::OK, "{msg} error: {code} ({ret:?})");
}

/// Decodes one of the harness's hexadecimal test vectors into the bytes it denotes.
///
/// A faithful port of `h2b` (`test/infcover.c` L245-L272), whose own comment states
/// the contract: it "decodes liberally". Three rules, and all three matter because
/// the vectors transcribed out of that harness use all of them:
///
/// 1. **Adjacent hex digits pair into a byte.** `"1f8b"` is two bytes.
/// 2. **Any non-hex byte delimits, and is otherwise ignored.** `"1f 8b"`,
///    `"1f,8b"` and `"1F 8B"` all denote the same two bytes as `"1f8b"`.
/// 3. **A single digit followed by a delimiter writes a byte on its own.**
///    `"63 0"` is `0x63, 0x00` -- *not* `0x63, 0x00` by accident of padding but by
///    rule, which is why a strict pair-wise decoder cannot be substituted here.
///    The `inf` and `try` call sites in the C harness mix one- and two-digit
///    tokens freely, and rule 3 is what makes `"3 0"` mean `0x03, 0x00`.
///
/// The C loop is `do { ... } while (*hex++)`, so it runs one final time on the
/// terminating NUL; that pass is what flushes a trailing lone digit under rule 3.
/// It is reproduced by chaining a single synthetic zero byte after the input.
/// Iteration is over **bytes**, not characters, so a multi-byte character simply
/// acts as a run of delimiters, which is what C sees too.
///
/// An empty input yields an empty vector rather than panicking.
#[must_use]
pub fn h2b(hex: &str) -> Vec<u8> {
    // `malloc((strlen(hex) + 1) >> 1)` at L250: two hex digits per byte, rounded
    // up so that a lone trailing digit still has somewhere to go.
    let mut out = Vec::with_capacity(hex.len().div_ceil(2));

    // L254: 1 is the "nothing pending" sentinel. It is distinguishable from a
    // single pending digit because one digit always lands in 16..=31 -- `16 + d`
    // for `d` in 0..=15 -- which is exactly what the `val < 32` test below keys
    // on.
    let mut val: u32 = 1;

    // `take_while` stops before the first zero byte and the chained zero then
    // stands in for C's string terminator, so the sequence of values the loop
    // body sees is byte-for-byte the sequence the C loop sees -- including for the
    // pathological case of an interior NUL, which terminates the C string early.
    for byte in hex
        .bytes()
        .take_while(|&b| b != 0)
        .chain(core::iter::once(0))
    {
        if byte.is_ascii_digit() {
            // L256-L257
            val = (val << 4) + u32::from(byte - b'0');
        } else if (b'A'..=b'F').contains(&byte) {
            // L258-L259
            val = (val << 4) + u32::from(byte - b'A') + 10;
        } else if (b'a'..=b'f').contains(&byte) {
            // L260-L261
            val = (val << 4) + u32::from(byte - b'a') + 10;
        } else if val != 1 && val < 32 {
            // L262-L263: one digit followed by a delimiter. Adding 240 lifts
            // `16 + d` to `256 + d`, which trips the emit below and so makes a
            // single digit look like two.
            val += 240;
        }

        if val > 255 {
            // L264-L267. `val` cannot exceed 511 here -- the largest value
            // reachable is `(31 << 4) + 15` from two digits, and the promotion
            // above tops out at `31 + 240` -- so the low byte is the whole of the
            // decoded value. Taken as a byte rather than by a narrowing cast, so
            // the arithmetic needs no justification and no lint exemption: the
            // last element of a big-endian `u32` is precisely `val & 0xff`.
            let [.., low] = val.to_be_bytes();
            out.push(low);
            val = 1;
        }
    }

    out
}

/// Fills `buf` with a deterministic pseudo-random byte sequence derived from `seed`.
///
/// This exists because `rand` is not available -- see the module documentation on
/// dependency minimalism -- and because a corpus that differs between runs cannot
/// support a byte-for-byte comparison against the reference implementation. The
/// same `seed` always produces the same bytes, on every run and on every platform.
///
/// The generator is a 64-bit linear congruential generator with the multiplier
/// `6364136223846793005` and the increment `1442695040888963407`, the constants
/// Knuth tabulates for `MMIX` and which the PCG family uses for its 64-bit
/// state advance. Both steps are `wrapping`, since congruential arithmetic *is*
/// modular arithmetic and the plain operators would panic on overflow in a debug
/// build -- which is what `cargo test` produces by default.
///
/// Each output byte is taken from the **top** eight bits of the state. That is not
/// arbitrary: the low bit of an LCG with a power-of-two modulus has period 2, the
/// next has period 4, and so on, so bytes taken from the bottom of the state carry
/// visible structure that deflate would happily compress -- which would quietly
/// defeat [`corpus::incompressible`], whose entire job is to be incompressible.
///
/// Any seed works, `0` included: the increment is odd and non-zero, so the sequence
/// advances from every state and can never stick. Filling an empty slice is a
/// harmless no-op.
///
/// Provenance: **no C counterpart.** Nothing in the reference implementation or its
/// test suite generates pseudo-random data -- `test/example.c` and
/// `test/infcover.c` use fixed literal fixtures throughout. This exists solely to
/// supply the "incompressible random" and "window crossing" classes the plan's
/// minimal corpus calls for, without the third-party dependency this crate forbids.
pub fn lcg_fill(seed: u64, buf: &mut [u8]) {
    /// Knuth's `MMIX` multiplier, also the PCG 64-bit state multiplier.
    const MULTIPLIER: u64 = 6_364_136_223_846_793_005;
    /// Knuth's `MMIX` increment. Odd, which is what makes every seed advance.
    const INCREMENT: u64 = 1_442_695_040_888_963_407;

    let mut state = seed;

    // `buf` is already a mutable slice reference, so it iterates directly and
    // yields `&mut u8` without an explicit `iter_mut()`.
    for slot in buf {
        state = state.wrapping_mul(MULTIPLIER).wrapping_add(INCREMENT);
        // The first element of the big-endian representation is bits 56..=63 --
        // the top byte -- obtained without a narrowing cast.
        let [top, ..] = state.to_be_bytes();
        *slot = top;
    }
}

/// One live allocation, as recorded by the tracker.
///
/// The Rust form of `struct mem_item` (`test/infcover.c` L56-L61), which records a
/// pointer, a requested size and a link to the next item. The link is implicit here
/// because the items live in a `Vec`, and a monotonic sequence number is added
/// because it costs nothing and turns "a block leaked" into "the third block
/// allocated leaked".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Block {
    /// Which allocation this was, counting from zero over the tracker's lifetime.
    /// Reported by [`TrackingAllocator::assert_clean`] when a block is never freed.
    sequence: u64,
    /// Base address of the block, used only to recognise it again on release. Never
    /// turned back into a pointer and never read through.
    address: usize,
    /// Size of the block **in bytes**, matching `mem_item.size`, which is the
    /// `count * size` product `mem_alloc` computes at L76 rather than an element
    /// count.
    length: usize,
}

/// The tracker's books: the live-block list plus the five statistics.
///
/// The Rust form of `struct mem_zone` (`test/infcover.c` L63-L69). Held behind a
/// [`RefCell`] because the [`Allocator`] contract hands out memory through `&self`,
/// so the bookkeeping has to mutate through a shared reference. A `RefCell` rather
/// than a `Mutex` because the trait imposes no `Sync` bound and none of the paths
/// below can re-enter the tracker while a borrow is live.
///
/// # List orientation
///
/// `mem_alloc` inserts each new item at the **front** of its linked list
/// (`item->next = zone->first; zone->first = item;`, L98-L100), so C's `zone->first`
/// is the *most recent* allocation and `mem_free` walks from newest to oldest. This
/// `Vec` is pushed at the **back**, so the most recent allocation is
/// `live.last()` and the walk is a reverse search. The two are the same order, and
/// getting this backwards would invert the last-in-first-out verdict, so it is
/// stated here and derived from consistently below.
#[derive(Debug, Default)]
struct Zone {
    /// Blocks handed out and not yet released, oldest first, newest last.
    live: Vec<Block>,
    /// Bytes currently outstanding. `mem_zone.total`.
    total: usize,
    /// Largest value `total` has ever held. `mem_zone.highwater`.
    high_water: usize,
    /// Allocation ceiling in bytes, or zero for no ceiling. `mem_zone.limit`.
    limit: usize,
    /// Releases that were not of the most recent block. `mem_zone.notlifo`.
    not_lifo: usize,
    /// Releases of blocks this tracker never handed out. `mem_zone.rogue`.
    rogue: usize,
    /// Sequence number the next block will be given.
    next_sequence: u64,
}

impl Zone {
    /// Reports whether a request of `length` bytes would breach the ceiling.
    ///
    /// Exactly the induced-failure predicate of `mem_alloc` L79:
    /// `zone->limit && zone->total + len > zone->limit`. A `limit` of zero means no
    /// limit, so it short-circuits first.
    const fn would_exceed_limit(&self, length: usize) -> bool {
        self.limit != 0 && self.total + length > self.limit
    }

    /// Records a newly handed-out block and updates the statistics.
    ///
    /// Mirrors `mem_alloc` L95-L105: the item is created, linked in at the
    /// most-recent end, and then `total` and the high-water mark are advanced.
    fn record(&mut self, address: usize, length: usize) {
        let sequence = self.next_sequence;
        self.next_sequence += 1;
        self.live.push(Block {
            sequence,
            address,
            length,
        });
        self.total += length;
        // `mem_alloc` L104-L105. Monotonic by construction: a release never lowers
        // it, which is what makes it usable as a per-stream memory measurement.
        self.high_water = self.high_water.max(self.total);
    }

    /// Accounts for a block being released, classifying it as `mem_free` does.
    ///
    /// Mirrors `mem_free` L123-L150. The search runs from the most recent block
    /// backwards, which is the direction C walks its list, so:
    ///
    /// * the most recent block is a clean last-in-first-out release and bumps no
    ///   counter beyond `total` (C's L127-L128 path, which reaches neither
    ///   `notlifo` nor `rogue`);
    /// * an earlier block is still released, but increments `not_lifo` (L136);
    /// * a block that is not present at all increments `rogue` and changes nothing
    ///   else (L149-L150).
    ///
    /// A block is recognised by base address **and** length. The length is part of
    /// the key for one specific reason: a zero-length Rust allocation performs no
    /// heap request and reports a dangling-but-aligned address, so two of them would
    /// share an address and collide. Searching from the newest end then treats such
    /// a release as the clean last-in-first-out case rather than inventing a
    /// non-LIFO one. This is defensive only -- every `ZALLOC` call site in the
    /// reference implementation passes at least one item, so the library never asks
    /// for zero bytes -- but the cost is one comparison and the alternative is a
    /// misleading statistic.
    fn release(&mut self, address: usize, length: usize) {
        if let Some(position) = self
            .live
            .iter()
            .rposition(|block| block.address == address && block.length == length)
        {
            // The last element is the most recent allocation; anything else is an
            // out-of-order release.
            if position + 1 != self.live.len() {
                self.not_lifo += 1;
            }
            let block = self.live.remove(position);
            self.total -= block.length;
        } else {
            self.rogue += 1;
        }
    }
}

/// What a tracker observed over its lifetime, as returned by
/// [`TrackingAllocator::finish`].
///
/// `mem_done` (`test/infcover.c` L200-L234) prints this information to stderr and
/// throws it away. Returning it instead is the whole point of the port: a Rust test
/// can assert on the numbers, and the high-water figure becomes a measurement
/// rather than a log line.
///
/// The three fields after `high_water` correspond one-to-one with `mem_done`'s three
/// diagnostics at L220-L227.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MemReport {
    /// Bytes still outstanding when tracking ended. `mem_done` L220-L222.
    pub leaked_bytes: usize,
    /// Blocks still outstanding when tracking ended -- C's `count`, L216.
    pub leaked_blocks: usize,
    /// Releases that were not of the most recent block. `mem_done` L223-L224.
    pub not_lifo: usize,
    /// Releases of blocks the tracker never handed out. `mem_done` L225-L227.
    pub rogue: usize,
    /// The largest number of bytes outstanding at any one moment, which `mem_high`
    /// (L192-L197) prints and `mem_done` repeats at L207.
    pub high_water: usize,
}

impl MemReport {
    /// Reports whether the run was free of every anomaly `mem_done` looks for.
    ///
    /// `high_water` is deliberately not consulted: a non-zero high-water mark means
    /// the allocator was used, not that anything went wrong.
    #[must_use]
    pub const fn is_clean(&self) -> bool {
        self.leaked_bytes == 0 && self.leaked_blocks == 0 && self.not_lifo == 0 && self.rogue == 0
    }
}

/// An [`Allocator`] that fills every block with a non-zero byte and audits its own
/// use, in pure safe Rust.
///
/// The port of the instrumented allocator in `test/infcover.c` (L56-L234), which is
/// the single most valuable piece of the C test suite for a reimplementation. It
/// catches three bug classes that ordinary tests miss:
///
/// 1. **Dependence on zeroed memory.** Every block is filled with
///    [`SENTINEL_FILL`], which is `0xa5` -- the value `mem_alloc` writes at L87 with
///    the comment "fill memory with a non-zero value to make sure that the code
///    isn't depending on zeros". This matters because the reference allocator does
///    not zero anything: the generic `zcalloc` (`zutil.c` L299-L303) reads
///    `sizeof(uInt) > 2 ? malloc(items * size) : calloc(items, size)`, and `uInt` is
///    four bytes wide on every target this port supports, so the branch taken is
///    always `malloc`. Anything that assumes a fresh buffer is zeroed is wrong, and
///    the fill is what makes it fail loudly instead of passing by luck.
///
///    Note that this is a stronger guarantee than the crate's own
///    `GlobalAllocator` offers, which fills with the sentinel in debug builds but
///    with zero in release builds. This tracker passes the sentinel explicitly, so
///    a release-profile test run is exactly as sensitive as a debug one.
/// 2. **Leaks, and releases in the wrong order.** The live-block list detects a
///    block that is never released, one released out of order (`not_lifo`), and one
///    that was never handed out at all (`rogue`). The ordering check is not
///    pedantry: `deflateEnd` frees "in reverse order of allocations", and Rust drops
///    struct fields in *declaration* order, so a state type that declares its
///    buffers in allocation order releases them in precisely the wrong one. That
///    mistake is invisible without this counter.
/// 3. **Unexercised error paths.** [`TrackingAllocator::set_limit`] makes
///    `Z_MEM_ERROR` reachable on demand, the way `mem_limit` (L176-L181) does at
///    `test/infcover.c` L326-L329: `mem_limit(&strm, 1)` forces the failure and
///    `mem_limit(&strm, 0)` restores normal service. Fuzzing reaches those paths
///    only by chance; this reaches them deliberately.
///
/// # Identity
///
/// [`Allocator::id`] returns [`AllocatorId::GLOBAL`], and that is both deliberate
/// and forced. Safe code cannot manufacture the `&'a mut [T]` that
/// `Buffer::from_foreign` requires out of a `&self` method -- the crate's own
/// in-module stand-in declines to allocate for exactly that reason -- so the only
/// route to a real block is `Buffer::try_global`, which stamps every buffer it
/// produces with `AllocatorId::GLOBAL`, and `Buffer::release_to` refuses any block
/// whose stamp does not match the allocator it is offered to. Claiming a foreign
/// identity here would therefore make every release a refusal, which leaks.
///
/// The identity rule this respects is the substantive one: `GLOBAL` must never be
/// claimed for a block that Rust's global deallocator cannot free. Every block below
/// *is* a Rust global allocation, so it can, and this tracker is interchangeable
/// with `GlobalAllocator` in exactly the sense the rule cares about. The obligation
/// to use `AllocatorId::foreign` instead applies to an allocator that hands out
/// blocks it did not obtain from Rust, which this one never does: it calls no
/// caller-supplied hook, holds no function pointer and performs no pointer
/// arithmetic.
///
/// # Sharing
///
/// The bookkeeping sits behind a [`RefCell`], so the tracker needs no `mut` to be
/// used and can be handed to the library as `&tracker` -- a shared reference to an
/// allocator is itself an allocator, through the blanket implementation -- while the
/// test keeps the original to interrogate.
///
/// # ★ What this tracker cannot see, and which instrument to use instead
///
/// Every figure here counts bytes the library requested **through this allocator**,
/// and that is strictly narrower than what a stream costs a process. Four kinds of
/// allocation are invisible to it, and mistaking a figure from here for a per-stream
/// total would understate that total substantially:
///
/// * the facade's `GzBlock` and every `z_stream` state block, which come from the
///   caller's `zalloc` or -- for a `gzFile`, which has no caller hooks at all -- from
///   Rust's global allocator;
/// * a `gzFile`'s path and error message, which are `Box<[u8]>` from the global
///   allocator, exactly as C's are `malloc` (`gzlib.c` L200-L204 and L559-L563);
/// * `EngineBox`, the fallible one-element `Vec` that carries a deflate or inflate
///   state inside a `gzFile`; and
/// * a boxed `GzHandle`, when a handle is injected rather than opened by path.
///
/// So this tracker answers "how many bytes did the algorithm ask *me* for", which is
/// what `test/infcover.c`'s `mem_high` answers and what makes it the right instrument
/// for leak, ordering, exhaustion and zero-dependence checking. It does **not** answer
/// "what does one stream cost". For that figure -- AAP §0.8.4's ≤15% per-stream memory
/// gate -- the instrument is a counting global allocator, and one exists:
/// `crates/libz-rs-sys/tests/gz_memory.rs` installs a pass-through
/// `#[global_allocator]` over `System` and reports idle and active high-water for a
/// `gzFile` separately, against the C figures.
///
/// # ★ And why this tracker must not be used for throughput
///
/// [`SENTINEL_FILL`] is what makes the zero-dependence check above work, and it is
/// also a full pass over every block handed out -- roughly 256 KiB for one default
/// deflate initialisation. C's `malloc` writes none of it. Any timing taken through
/// this allocator therefore measures the fill as well as the library, and a
/// steady-state throughput number taken this way would be wrong in a direction that
/// flatters nothing and explains nothing. Measure throughput through
/// `GlobalAllocator`, or through the caller's own hooks, and keep this tracker for the
/// correctness properties it was built for.
#[derive(Debug, Default)]
pub struct TrackingAllocator {
    /// The books. Private, and reachable only through the accessors below, so no
    /// test can corrupt the statistics it is asserting on.
    zone: RefCell<Zone>,
}

impl TrackingAllocator {
    /// The fill pattern for a `u16` block: both bytes are [`SENTINEL_FILL`].
    ///
    /// Filling a `u16` block byte-for-byte the way a byte block is filled keeps the
    /// two shapes indistinguishable to the code under test, which is what
    /// `memset(ptr, 0xa5, len)` gives C regardless of what the block will hold. The
    /// native byte order is used because both bytes are equal, so it cannot matter.
    const SENTINEL_FILL_U16: u16 = u16::from_ne_bytes([SENTINEL_FILL, SENTINEL_FILL]);

    /// Creates a tracker with no ceiling, no live blocks and every counter at zero.
    ///
    /// The state `mem_setup` (`test/infcover.c` L158-L174) leaves behind: `first`
    /// null, `total`, `highwater`, `limit`, `notlifo` and `rogue` all zero. A
    /// `limit` of zero means *no limit*, not *no allocation*.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the ceiling on outstanding bytes, or removes it when `limit` is zero.
    ///
    /// `mem_limit` (`test/infcover.c` L176-L181). Once armed, any request that would
    /// push the outstanding total above `limit` fails and records nothing, which is
    /// how a test drives the library's `Z_MEM_ERROR` paths without waiting for real
    /// exhaustion. Callable at any point in a stream's life, both to arm and to
    /// disarm, exactly as `test/infcover.c` L326-L329 does around a single call.
    pub fn set_limit(&self, limit: usize) {
        self.zone.borrow_mut().limit = limit;
    }

    /// Returns the number of bytes currently outstanding.
    ///
    /// `mem_used` (`test/infcover.c` L184-L189), which prints this figure; returning
    /// it instead is what lets a test assert on it.
    #[must_use]
    pub fn total(&self) -> usize {
        self.zone.borrow().total
    }

    /// Returns the largest number of bytes ever outstanding at one moment.
    ///
    /// `mem_high` (`test/infcover.c` L192-L197). Monotonically non-decreasing: a
    /// release lowers [`TrackingAllocator::total`] but never this, which is what
    /// makes it a usable measure of a stream's peak working-set size after the
    /// stream has already been torn down.
    ///
    /// It is the peak *this allocator served*, not the peak the process reached: the
    /// state block, a `gzFile`'s path and message, `EngineBox` and a boxed handle are
    /// all outside it. See the type's documentation for the full list and for the
    /// instrument that does measure a per-stream total.
    #[must_use]
    pub fn high_water(&self) -> usize {
        self.zone.borrow().high_water
    }

    /// Returns how many releases were not of the most recent block.
    ///
    /// C's `zone->notlifo`, reported by `mem_done` at L223-L224.
    #[must_use]
    pub fn not_lifo(&self) -> usize {
        self.zone.borrow().not_lifo
    }

    /// Returns how many releases named a block this tracker never handed out.
    ///
    /// C's `zone->rogue`, reported by `mem_done` at L225-L227. In C a rogue free is
    /// a heap-corruption symptom; here it can only ever be an accounting
    /// observation, because a block is released through its own owner and safe Rust
    /// has no way to free memory twice.
    #[must_use]
    pub fn rogue(&self) -> usize {
        self.zone.borrow().rogue
    }

    /// Ends tracking and returns what was observed.
    ///
    /// `mem_done` (`test/infcover.c` L200-L234) with its stderr reporting replaced by
    /// a return value. It ends tracking the way C does -- the live-block list is
    /// discarded and the outstanding total returns to zero -- but the three anomaly
    /// counters and the high-water mark are left standing, so a test can still
    /// interrogate the tracker after the stream it served has been torn down. That is
    /// also why the receiver is `&self`: C frees the zone here and nulls the stream's
    /// hooks, which would make the figures unreachable at exactly the moment a test
    /// wants them.
    ///
    /// Nothing is freed by this call and nothing can leak because of it. C has to
    /// `free(item->ptr)` for each leftover (L212) because the zone owns the raw
    /// blocks; here each block's bytes live inside the `Buffer` that was handed out,
    /// so a `Buffer` dropped without being released has already returned its memory
    /// to Rust. What survives is the *accounting* discrepancy, which is precisely the
    /// leak this reports.
    ///
    /// Normally called once, at the end of a test. Calling it again reports no
    /// further leak, because tracking has already ended.
    #[must_use]
    pub fn finish(&self) -> MemReport {
        let mut zone = self.zone.borrow_mut();
        let report = MemReport {
            leaked_bytes: zone.total,
            leaked_blocks: zone.live.len(),
            not_lifo: zone.not_lifo,
            rogue: zone.rogue,
            high_water: zone.high_water,
        };
        zone.live.clear();
        zone.total = 0;
        report
    }

    /// Ends tracking and panics unless the run was completely clean.
    ///
    /// The assertion form of [`TrackingAllocator::finish`], for the common case where
    /// a test wants `mem_done`'s verdict rather than its numbers. The panic message
    /// carries all three of `mem_done`'s diagnostics (L220-L227) in its own words --
    /// bytes in blocks not freed, releases not last-in-first-out, and releases not
    /// recognised -- plus the high-water mark it prints first at L207, and plus the
    /// sequence numbers of the blocks that leaked, which C cannot report because it
    /// counts leftovers without identifying them.
    ///
    /// # Panics
    ///
    /// If any block was never released, or any release was out of order, or any
    /// release named a block that was never handed out.
    pub fn assert_clean(&self) {
        // Read before `finish` clears the list. An empty iterator does not allocate,
        // so the clean path pays nothing for this.
        let leaked: Vec<u64> = self
            .zone
            .borrow()
            .live
            .iter()
            .map(|block| block.sequence)
            .collect();
        let report = self.finish();
        assert!(
            report.is_clean(),
            "tracking allocator: {} bytes in {} blocks not freed (allocations {:?}), \
             {} releases not LIFO, {} releases not recognised; high water mark {} bytes",
            report.leaked_bytes,
            report.leaked_blocks,
            leaked,
            report.not_lifo,
            report.rogue,
            report.high_water,
        );
    }

    /// Returns a block's base address, for recognising it again on release.
    ///
    /// A pointer-to-integer cast, which is a safe operation: it discards the ability
    /// to reach the memory rather than fabricating it, and only a dereference would
    /// need an escape hatch. Nothing is ever read through the result, and it is never
    /// turned back into a pointer. (`core::ptr::addr` would say this more precisely
    /// but was stabilised after the 1.80 floor this workspace targets.)
    fn address_of<T>(buffer: &Buffer<'_, T>) -> usize {
        buffer.as_slice().as_ptr() as usize
    }
}

impl<'a> Allocator<'a> for TrackingAllocator {
    /// Returns [`AllocatorId::GLOBAL`]. See the discussion of identity on
    /// [`TrackingAllocator`] for why that is both correct and unavoidable here.
    fn id(&self) -> AllocatorId {
        AllocatorId::GLOBAL
    }

    /// Returns [`Opaque::NULL`], since this allocator has no caller hooks to pass an
    /// `opaque` value to.
    ///
    /// `mem_setup` points `strm->opaque` at the zone (L170) because C reaches the
    /// zone through that field. Here the zone is reached through `&self`, so the
    /// `opaque` slot is genuinely unused and reports the null an initialisation
    /// function installs alongside the internal routines.
    fn opaque(&self) -> Opaque {
        Opaque::NULL
    }

    /// Allocates `items * size` bytes filled with [`SENTINEL_FILL`], recording the
    /// block.
    ///
    /// `mem_alloc` (`test/infcover.c` L71-L109), in its order, which matters:
    ///
    /// 1. the byte length is the checked `items * size` product -- C widens before
    ///    multiplying (`count * (size_t)size`, L76) and this rejects a product that
    ///    would not fit rather than wrapping to a short block;
    /// 2. the ceiling is tested **first** (L79), and a refused request records
    ///    nothing at all: no counter moves and no list entry appears, so a test that
    ///    arms the limit sees a pristine tracker afterwards;
    /// 3. the block is filled with the sentinel, never zeroed;
    /// 4. only then is it recorded and the running total advanced.
    fn allocate_bytes(&self, items: usize, size: usize) -> Option<Buffer<'a, u8>> {
        let length = block_len(items, size)?;
        if self.zone.borrow().would_exceed_limit(length) {
            return None;
        }
        let buffer = Buffer::try_global(length, SENTINEL_FILL)?;
        self.zone
            .borrow_mut()
            .record(Self::address_of(&buffer), length);
        Some(buffer)
    }

    /// Allocates `items` 16-bit values, every byte of which is [`SENTINEL_FILL`].
    ///
    /// The hash-chain shape, `ZALLOC(strm, items, sizeof(Pos))`. Identical in every
    /// respect to [`Allocator::allocate_bytes`] except that the accounting is still
    /// done in **bytes**, because that is the unit `mem_alloc` records and the unit
    /// the memory figures are quoted in.
    fn allocate_u16s(&self, items: usize) -> Option<Buffer<'a, u16>> {
        let length = block_len(items, size_of::<u16>())?;
        if self.zone.borrow().would_exceed_limit(length) {
            return None;
        }
        let buffer = Buffer::try_global(items, Self::SENTINEL_FILL_U16)?;
        self.zone
            .borrow_mut()
            .record(Self::address_of(&buffer), length);
        Some(buffer)
    }

    /// Releases a byte block, classifying the release as `mem_free` does.
    ///
    /// `mem_free` (`test/infcover.c` L112-L154). The accounting happens first, while
    /// the block still exists to be identified, and the memory is then surrendered
    /// through `Buffer::release_to` -- the only route by which a block may be
    /// released, and the one that performs the identity check. The outcome is
    /// dropped, which is what actually frees a Rust-owned block, mirroring
    /// `GlobalAllocator` exactly. C's unconditional trailing `free(ptr)` (L153) has
    /// no counterpart because a refused block here is never freed by the wrong
    /// allocator in the first place.
    fn deallocate_bytes(&self, slot: &mut Option<Buffer<'a, u8>>) {
        if let Some(buffer) = slot.as_ref() {
            let address = Self::address_of(buffer);
            let length = buffer.len();
            self.zone.borrow_mut().release(address, length);
        }
        let Some(buffer) = slot.take() else { return };
        drop(buffer.release_to(self));
    }

    /// Releases a `u16` block, classifying the release as `mem_free` does.
    ///
    /// The [`Allocator::deallocate_bytes`] counterpart. `Buffer::len` counts
    /// elements, so it is scaled to bytes to match what was recorded on the way in.
    fn deallocate_u16s(&self, slot: &mut Option<Buffer<'a, u16>>) {
        if let Some(buffer) = slot.as_ref() {
            let address = Self::address_of(buffer);
            let length = buffer.len() * size_of::<u16>();
            self.zone.borrow_mut().release(address, length);
        }
        let Some(buffer) = slot.take() else { return };
        drop(buffer.release_to(self));
    }

    /// Never reached: every block this allocator hands out is Rust-owned, so
    /// `Buffer::release_to` reports it as handled and this method has nothing to do.
    fn release_foreign_bytes(&self, _block: ForeignBlock<'a, u8>) {}

    /// Never reached, for the reason [`TrackingAllocator::release_foreign_bytes`]
    /// gives.
    fn release_foreign_u16s(&self, _block: ForeignBlock<'a, u16>) {}
}

/// The committed deterministic corpus: one fixture per payload class the suites need
/// to cover.
///
/// Two rules govern everything in here. **Determinism**: every payload is computed
/// from constants in this file, so it is byte-for-byte identical on every run and on
/// every platform, which is the precondition for comparing compressed output against
/// the reference implementation. **Hermeticity**: nothing reads the network, the
/// filesystem, the environment or a clock, so `cargo test` and CI need none of them.
/// The large opt-in corpus used for throughput measurement lives in
/// `crates/zlib-rs-differential/corpus` and is deliberately not reachable from here.
///
/// The classes are chosen to reach the decisions a compressor actually makes:
/// the empty and one-byte edges, a payload with matches everywhere, one with no
/// matches at all, one that the text-versus-binary heuristic must call text, one it
/// must call binary, and one long enough to force the sliding window to slide.
/// [`all`] returns them as named pairs so a failing assertion can say which class
/// broke.
///
/// Provenance: [`HELLO`] and [`DICTIONARY`] are transcribed from `test/example.c`
/// L35 and L40. The remaining classes have **no single C counterpart** -- the C
/// suite has no corpus module -- and come from the port's plan, which specifies
/// exactly this set: empty input, a single byte, highly repetitive data,
/// incompressible random data, natural-language text, binary data, and data
/// crossing the 32 KiB window boundary. The text-versus-binary pair is classified
/// per `doc/txtvsbin.txt`.
pub mod corpus {
    /// The byte [`repetitive`] repeats, and the byte [`SINGLE_BYTE`] contains.
    ///
    /// Printable ASCII, so a payload made of it is on the allow list of
    /// `doc/txtvsbin.txt` and is classified as text -- which keeps the two edge-case
    /// classes from accidentally also testing the binary path.
    const FILLER_BYTE: u8 = b'a';

    /// Seed for [`incompressible`]. Arbitrary, and frozen: changing it changes every
    /// byte of that class and therefore every compressed comparison made against it.
    const INCOMPRESSIBLE_SEED: u64 = 0x0DDB_1A5E_5BAD_5EED;

    /// Seed for the distinctive marker block at both ends of [`window_crossing`].
    const WINDOW_MARKER_SEED: u64 = 0x00C0_FFEE_CAFE_F00D;

    /// Seed for the filler between the two marker blocks of [`window_crossing`].
    const WINDOW_FILLER_SEED: u64 = 0x1234_5678_9ABC_DEF0;

    /// Length of each marker block in [`window_crossing`], in bytes.
    const WINDOW_MARKER_LEN: usize = 1024;

    /// Length of the filler between the marker blocks of [`window_crossing`], chosen
    /// so that the total exceeds the 32768-byte window while the distance back to
    /// the first marker stays inside `MAX_DIST`. See [`window_crossing`].
    const WINDOW_FILLER_LEN: usize = 31_000;

    /// How many ascending passes over all 256 byte values [`binary`] emits.
    const BINARY_PASSES: usize = 4;

    /// Length [`all`] uses for [`repetitive`]. A few `KiB`: enough to exercise the
    /// symbol buffer and several blocks without slowing a `Miri` run.
    const REPETITIVE_LEN: usize = 4096;

    /// Length [`all`] uses for [`incompressible`], chosen to match
    /// [`REPETITIVE_LEN`] so the two contrasting classes are the same size.
    const INCOMPRESSIBLE_LEN: usize = 4096;

    /// The prose [`text`] returns. Only allow-list bytes: printable ASCII and the
    /// line feed, no tab, no carriage return and above all no control byte in
    /// `0..=6` or `14..=31`.
    ///
    /// Assembled with `concat!` rather than written as one multi-line literal so that
    /// the source can be indented normally without leaking the indentation into the
    /// payload.
    const PROSE: &str = concat!(
        "Compression is the art of saying the same thing in fewer bits.\n",
        "The deflate algorithm pairs a sliding window matcher with Huffman coding.\n",
        "The matcher replaces a stretch of bytes it has seen before with a length\n",
        "and a distance, and the coder then spends fewer bits on whichever symbols\n",
        "turn out to be common. Neither half is much use on its own. A matcher with\n",
        "no entropy coder still spends eight bits on every literal it cannot pair,\n",
        "and an entropy coder with no matcher never notices that a whole sentence\n",
        "has gone past once already. Together they are the reason this paragraph is\n",
        "smaller on disk than it is in memory, and the reason the two of them have\n",
        "outlasted almost everything else written in the same decade.\n",
    );

    /// The empty payload.
    ///
    /// The edge case every entry point has to survive: a stream with no input still
    /// produces a valid, complete container, and `doc/txtvsbin.txt` classifies an
    /// empty buffer as binary because it holds no allow-list byte.
    pub const EMPTY: &[u8] = &[];

    /// A one-byte payload.
    ///
    /// The smallest non-empty input, which is the shortest path to the "no match is
    /// possible" branch of the matcher. The byte is [`FILLER_BYTE`], `0x61`, which is
    /// on the allow list of `doc/txtvsbin.txt` (32 through 255), so a one-byte payload
    /// is classified as text.
    pub const SINGLE_BYTE: &[u8] = &[FILLER_BYTE];

    /// The payload the C test suite compresses, **including its terminating NUL**.
    ///
    /// `test/example.c` L35 declares `hello[] = "hello, hello!"` with the comment
    /// that `"hello world"` would be more standard but that "the repeated `hello`
    /// stresses the compression code better". Every use feeds it as
    /// `strlen(hello) + 1` (L69, L95, L175, L341, L434), so the NUL is part of the
    /// payload and this constant is **14 bytes**, not 13. Getting that length wrong
    /// would silently invalidate every byte-for-byte comparison against the C suite,
    /// which is why a self-test asserts it.
    pub const HELLO: &[u8] = b"hello, hello!\0";

    /// The preset dictionary the C test suite uses, **including its terminating
    /// NUL**.
    ///
    /// `test/example.c` L40 declares `dictionary[] = "hello"` and passes it as
    /// `(int)sizeof(dictionary)` at L426 and L477 -- `sizeof`, not `strlen`, so the
    /// NUL is included and this constant is **6 bytes**. It pairs with [`HELLO`],
    /// whose first match is exactly this string.
    ///
    /// This is a dictionary, not a payload, so it is deliberately absent from
    /// [`all`].
    pub const DICTIONARY: &[u8] = b"hello\0";

    /// Returns `len` copies of a single byte.
    ///
    /// The maximally compressible class. A run of one byte is the case
    /// `Z_RLE` (`zlib.h` L202) exists for: every match is at distance 1, and each is
    /// capped at `MAX_MATCH` = 258 (`zutil.h` L93), so a long run also exercises the
    /// loop that emits back-to-back maximum-length matches. It is additionally the
    /// shortest route to a block whose Huffman tree has almost no symbols in it,
    /// which is the force-two-codes special case at `trees.c` L655-L661.
    #[must_use]
    pub fn repetitive(len: usize) -> Vec<u8> {
        vec![FILLER_BYTE; len]
    }

    /// Returns `len` pseudo-random bytes from a fixed seed.
    ///
    /// The incompressible class, and the counterpart to [`repetitive`]. With no
    /// matches to find and a near-flat symbol distribution, a compressor's best
    /// option is to store the data verbatim, so this is what drives the stored-block
    /// decision and the "compressed output is larger than the input" accounting that
    /// the bound functions have to allow for.
    ///
    /// The seed is [`INCOMPRESSIBLE_SEED`] and the generator is
    /// [`super::lcg_fill`], so the bytes are identical on every run and every
    /// platform.
    ///
    /// Provenance: no C counterpart; see [`super::lcg_fill`]. The stored-block choice
    /// it drives is made at `trees.c` L1050-L1055, where a block is stored whenever
    /// `stored_len + 4 <= opt_lenb`.
    #[must_use]
    pub fn incompressible(len: usize) -> Vec<u8> {
        let mut out = vec![0_u8; len];
        super::lcg_fill(INCOMPRESSIBLE_SEED, &mut out);
        out
    }

    /// Returns natural-language ASCII that the text-versus-binary heuristic must
    /// classify as **text**.
    ///
    /// `doc/txtvsbin.txt` divides the byte values into an allow list (9, 10, 13 and
    /// 32 through 255), a gray list (7, 8, 11, 12, 26, 27) and a block list (0
    /// through 6, and 14 through 31), and calls a buffer text when it holds at least
    /// one allow-list byte and no block-list byte. This payload is printable ASCII
    /// and line feeds only, so it satisfies both halves, and a self-test checks that
    /// property directly rather than trusting the prose to stay clean through future
    /// edits.
    #[must_use]
    pub fn text() -> Vec<u8> {
        PROSE.as_bytes().to_vec()
    }

    /// Returns data that the text-versus-binary heuristic must classify as
    /// **binary**.
    ///
    /// All 256 byte values in ascending order, repeated [`BINARY_PASSES`] times, for
    /// 1024 bytes. Obviously deterministic, obviously exhaustive over the byte range,
    /// and it necessarily contains NUL and the rest of the block list of
    /// `doc/txtvsbin.txt`, which is what forces the binary verdict. Being a fixed
    /// repeating cycle it is also highly compressible, so it exercises the binary
    /// classification without also being an incompressibility test -- that is
    /// [`incompressible`]'s job.
    #[must_use]
    pub fn binary() -> Vec<u8> {
        let mut out = Vec::with_capacity(BINARY_PASSES * 256);
        for _ in 0..BINARY_PASSES {
            out.extend(0..=u8::MAX);
        }
        out
    }

    /// Returns a payload long enough to force the sliding window to slide, with a
    /// match that spans the crossing.
    ///
    /// The window is 32768 bytes at the default and maximum `windowBits` of 15, so a
    /// payload has to exceed that before the window fills, slides and re-bases the
    /// hash chains -- three code paths that a corpus of small fixtures never reaches
    /// at all.
    ///
    /// Exceeding the length is necessary but not sufficient: the point is to make a
    /// match *survive* the slide. So the payload is a distinctive marker block, then
    /// filler, then the same marker block again:
    ///
    /// | Region | Bytes | Running total |
    /// |---|---|---|
    /// | marker | 1024 | 1024 |
    /// | filler | 31000 | 32024 |
    /// | marker again | 1024 | 33048 |
    ///
    /// The total, 33048, exceeds the 32768-byte window. The distance from the second
    /// marker back to the first is 32024, which is under `MAX_DIST` -- defined as
    /// `(s)->w_size - MIN_LOOKAHEAD` at `deflate.h` L301, where `MIN_LOOKAHEAD` is
    /// `MAX_MATCH + MIN_MATCH + 1` at `deflate.h` L296, giving
    /// `32768 - (258 + 3 + 1) = 32506` -- so the repeat is still findable after the
    /// slide instead of falling off the back of the window. Both regions are
    /// pseudo-random, so the only long match available is the intended one.
    ///
    /// The paths this reaches that no small fixture does are `fill_window`
    /// (`deflate.c` L252) and the `slide_hash` it calls (`deflate.c` L187).
    ///
    /// This is the only class permitted to exceed 32768 bytes, and it exceeds it by
    /// as little as the meaning allows, because every byte here is a byte `Miri`
    /// has to interpret.
    #[must_use]
    pub fn window_crossing() -> Vec<u8> {
        let mut marker = vec![0_u8; WINDOW_MARKER_LEN];
        super::lcg_fill(WINDOW_MARKER_SEED, &mut marker);

        let mut filler = vec![0_u8; WINDOW_FILLER_LEN];
        super::lcg_fill(WINDOW_FILLER_SEED, &mut filler);

        let mut out = Vec::with_capacity(2 * WINDOW_MARKER_LEN + WINDOW_FILLER_LEN);
        out.extend_from_slice(&marker);
        out.extend_from_slice(&filler);
        out.extend_from_slice(&marker);
        out
    }

    /// Returns every payload class, paired with its name.
    ///
    /// The accessor the suites loop over, so that a failure can report *which* class
    /// broke rather than just that something did. Exactly eight entries, one per
    /// payload class:
    ///
    /// `empty`, `single_byte`, `hello`, `repetitive`, `incompressible`, `text`,
    /// `binary`, `window_crossing`.
    ///
    /// [`DICTIONARY`] is not among them: it is a preset dictionary rather than a
    /// payload, and feeding it to a round-trip test as though it were input would
    /// prove nothing that [`HELLO`] does not already prove. It is reached directly, as
    /// `test/example.c` does at L426 and L477.
    ///
    /// Provenance: no C counterpart -- the C suite drives one fixture at a time rather
    /// than iterating a named set. The eight classes are the ones the port's plan
    /// specifies for the committed minimal corpus.
    ///
    /// The two parameterised classes are taken at [`REPETITIVE_LEN`] and
    /// [`INCOMPRESSIBLE_LEN`] bytes; a suite that wants a different size calls
    /// [`repetitive`] or [`incompressible`] directly.
    #[must_use]
    pub fn all() -> Vec<(&'static str, Vec<u8>)> {
        vec![
            ("empty", EMPTY.to_vec()),
            ("single_byte", SINGLE_BYTE.to_vec()),
            ("hello", HELLO.to_vec()),
            ("repetitive", repetitive(REPETITIVE_LEN)),
            ("incompressible", incompressible(INCOMPRESSIBLE_LEN)),
            ("text", text()),
            ("binary", binary()),
            ("window_crossing", window_crossing()),
        ]
    }
}

/// Self-verification for the helpers above.
///
/// An integration test target is compiled with `--test`, which sets `cfg(test)`, so
/// this module is compiled and its tests are collected by **every** suite that
/// declares `mod common;`. That is deliberate: shared test support that is itself
/// untested is a place where a silent bug corrupts every suite at once, and running
/// these in each binary means no suite can be run without them passing first. The
/// price is that they run once per suite, so every one of them is kept to
/// microseconds and none of them allocates more than a few kilobytes.
///
/// The assertions are pinned to the C oracle wherever the oracle has an opinion: the
/// `h2b` vectors are worked examples of `test/infcover.c` L245-L272, and the
/// allocator cases walk `mem_alloc` and `mem_free` (L71-L154) branch by branch.
#[cfg(test)]
mod self_tests {
    // The workspace denies the panic-prone lints, which is right for library code and
    // wrong for a test: a test asserts, an assertion that fails panics, and reading a
    // fixture by index is clearer than defensively matching on it. Scoped to this
    // module, and nothing here ships.
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]

    use super::{check_err, corpus, h2b, lcg_fill, TrackingAllocator};
    use zlib_rs::allocate::{Allocator, Buffer, SENTINEL_FILL};
    use zlib_rs::error::ReturnCode;

    /// The block list of `doc/txtvsbin.txt`: 0 through 6, and 14 through 31.
    fn is_block_listed(byte: u8) -> bool {
        byte <= 6 || (14..=31).contains(&byte)
    }

    /// The allow list of `doc/txtvsbin.txt`: 9, 10, 13, and 32 through 255.
    fn is_allow_listed(byte: u8) -> bool {
        matches!(byte, 9 | 10 | 13) || byte >= 32
    }

    #[test]
    fn h2b_decodes_the_worked_examples_from_the_c_harness() {
        // Empty input must yield an empty vector, not a panic: the C `do`/`while`
        // runs its body once on the terminator with nothing pending.
        assert_eq!(h2b(""), Vec::<u8>::new());

        // A two-digit token then a one-digit token: rule 3 flushes the lone digit on
        // the terminating pass.
        assert_eq!(h2b("63 0"), [0x63, 0x00]);

        // Two one-digit tokens: the first is flushed by the delimiter, the second by
        // the terminator.
        assert_eq!(h2b("3 0"), [0x03, 0x00]);

        // Three adjacent digits: the first two pair, and the third stands alone.
        assert_eq!(h2b("abc"), [0xab, 0x0c]);

        // Delimiting and case are both immaterial.
        assert_eq!(h2b("1f8b"), [0x1f, 0x8b]);
        assert_eq!(h2b("1f 8b"), h2b("1f8b"));
        assert_eq!(h2b("1F 8B"), h2b("1f8b"));

        // A realistic vector of the kind the inflate suites carry: a gzip header with
        // its flag byte set and every one-digit field written as one digit.
        assert_eq!(
            h2b("1f 8b 8 1e 0 0 0 0 0 0 1 0 0 0 0 0 0"),
            [0x1f, 0x8b, 0x08, 0x1e, 0, 0, 0, 0, 0, 0, 0x01, 0, 0, 0, 0, 0, 0]
        );
    }

    #[test]
    fn h2b_treats_every_non_hex_byte_as_a_delimiter() {
        // Punctuation, letters outside a..f and multi-byte characters all delimit and
        // are otherwise ignored, so they cannot change the decoded bytes.
        let expected = [0x1f, 0x8b];
        assert_eq!(h2b("1f,8b"), expected);
        assert_eq!(h2b("1f/8b"), expected);
        assert_eq!(h2b("1fz8b"), expected);
        assert_eq!(h2b("1f\u{2014}8b"), expected);
        // Leading and trailing delimiters leave nothing pending, so they add nothing.
        assert_eq!(h2b("  1f 8b  "), expected);
    }

    #[test]
    fn the_example_c_fixtures_keep_their_terminating_nul() {
        // 13 characters plus the NUL, because example.c feeds strlen(hello) + 1.
        assert_eq!(corpus::HELLO.len(), 14);
        assert_eq!(corpus::HELLO.last(), Some(&0));
        assert_eq!(&corpus::HELLO[..13], b"hello, hello!");

        // 5 characters plus the NUL, because example.c feeds sizeof(dictionary).
        assert_eq!(corpus::DICTIONARY.len(), 6);
        assert_eq!(corpus::DICTIONARY, b"hello\0");

        // The dictionary is a prefix of the payload, which is what makes it useful as
        // a preset dictionary for it.
        assert!(corpus::HELLO.starts_with(b"hello"));
    }

    #[test]
    fn the_corpus_classes_have_their_documented_shapes() {
        assert!(corpus::EMPTY.is_empty());
        assert_eq!(corpus::SINGLE_BYTE.len(), 1);

        // The parameterised classes honour the length asked for, including zero.
        for len in [0_usize, 1, 7, 4096] {
            assert_eq!(corpus::repetitive(len).len(), len);
            assert_eq!(corpus::incompressible(len).len(), len);
        }

        // Repetitive really is one repeated byte, which is what makes every match sit
        // at distance 1.
        let repetitive = corpus::repetitive(300);
        assert!(repetitive.iter().all(|&byte| byte == repetitive[0]));

        assert!(corpus::text().len() > 256);

        // 256 values, four times over.
        assert_eq!(corpus::binary().len(), 1024);
    }

    #[test]
    fn text_classifies_as_text_and_binary_classifies_as_binary() {
        // Text: at least one allow-list byte and no block-list byte.
        let text = corpus::text();
        assert!(text.iter().copied().any(is_allow_listed));
        assert!(
            !text.iter().copied().any(is_block_listed),
            "the prose fixture must contain no control byte in 0..=6 or 14..=31"
        );

        // Binary: at least one block-list byte. NUL is the canonical one, and an
        // ascending pass over every byte value necessarily includes it.
        let binary = corpus::binary();
        assert!(binary.iter().copied().any(is_block_listed));
        assert!(binary.contains(&0));
    }

    #[test]
    fn window_crossing_spans_the_window_and_keeps_its_match_in_reach() {
        let payload = corpus::window_crossing();

        // Longer than the 32768-byte window, so the window must slide.
        assert!(payload.len() > 32768);
        assert_eq!(payload.len(), 33_048);

        // The final 1024 bytes repeat the first 1024, at a distance of 32024 -- under
        // MAX_DIST (32768 - 262 = 32506), so the match survives the slide.
        let marker_len = 1024;
        let distance = payload.len() - marker_len;
        assert_eq!(distance, 32_024);
        assert!(distance < 32_506);
        assert_eq!(&payload[..marker_len], &payload[distance..]);
    }

    #[test]
    fn all_yields_the_eight_payload_classes_under_unique_names() {
        let classes = corpus::all();
        assert_eq!(classes.len(), 8);

        let mut names: Vec<&str> = classes.iter().map(|(name, _)| *name).collect();
        names.sort_unstable();
        assert_eq!(
            names,
            [
                "binary",
                "empty",
                "hello",
                "incompressible",
                "repetitive",
                "single_byte",
                "text",
                "window_crossing",
            ]
        );

        // The dictionary fixture is not a payload and must not appear.
        assert!(!names.contains(&"dictionary"));

        // Each name maps to the payload it claims.
        for (name, payload) in classes {
            match name {
                "empty" => assert!(payload.is_empty()),
                "hello" => assert_eq!(payload, corpus::HELLO),
                _ => assert!(!payload.is_empty(), "class {name} must not be empty"),
            }
        }
    }

    #[test]
    fn lcg_fill_is_deterministic_and_sensitive_to_the_seed() {
        let mut first = [0_u8; 64];
        let mut again = [0_u8; 64];
        lcg_fill(12345, &mut first);
        lcg_fill(12345, &mut again);
        assert_eq!(first, again, "the same seed must produce the same bytes");

        let mut other = [0_u8; 64];
        lcg_fill(12346, &mut other);
        assert_ne!(first, other, "a different seed must produce other bytes");

        // Seed 0 is ordinary: the increment is non-zero, so the sequence advances.
        let mut zero_seeded = [0_u8; 64];
        lcg_fill(0, &mut zero_seeded);
        assert!(zero_seeded.iter().any(|&byte| byte != 0));

        // A prefix of a longer fill matches a shorter fill from the same seed, which
        // is what makes the corpus lengths independent of each other.
        let mut short = [0_u8; 16];
        lcg_fill(12345, &mut short);
        assert_eq!(short, first[..16]);
    }

    #[test]
    fn lcg_fill_of_an_empty_slice_is_a_no_op() {
        let mut empty: [u8; 0] = [];
        lcg_fill(1, &mut empty);
        assert!(empty.is_empty());
        // And through an empty subslice of a real buffer, which must stay untouched.
        let mut buffer = [7_u8; 4];
        lcg_fill(1, &mut buffer[2..2]);
        assert_eq!(buffer, [7, 7, 7, 7]);
    }

    #[test]
    fn lcg_output_is_not_a_repeated_byte() {
        // Sanity that the byte really comes from the top of the state: taking it from
        // the bottom would produce visible structure, and taking it from a state that
        // never advanced would produce one repeated value.
        let mut buffer = [0_u8; 256];
        lcg_fill(0xDEAD_BEEF, &mut buffer);
        assert!(buffer.iter().any(|&byte| byte != buffer[0]));

        // A low-order bit that alternated with period two would make every other byte
        // share a parity; count distinct values instead, which catches that and more.
        let mut seen = buffer.to_vec();
        seen.sort_unstable();
        seen.dedup();
        assert!(
            seen.len() > 64,
            "256 pseudo-random bytes yielded only {} distinct values",
            seen.len()
        );
    }

    #[test]
    fn check_err_is_silent_on_success() {
        check_err(ReturnCode::OK, "this must not panic");
    }

    #[test]
    #[should_panic(expected = "inflate error: -3")]
    fn check_err_reports_the_message_and_the_numeric_code() {
        // The numeric code is what CHECK_ERR prints, so it must be in the message.
        check_err(ReturnCode::DATA_ERROR, "inflate");
    }

    #[test]
    fn a_fresh_tracker_has_no_history() {
        let tracker = TrackingAllocator::new();
        assert_eq!(tracker.total(), 0);
        assert_eq!(tracker.high_water(), 0);
        assert_eq!(tracker.not_lifo(), 0);
        assert_eq!(tracker.rogue(), 0);

        let report = tracker.finish();
        assert!(report.is_clean());
        assert_eq!(report.leaked_bytes, 0);
        assert_eq!(report.leaked_blocks, 0);
        assert_eq!(report.high_water, 0);
    }

    #[test]
    fn an_allocation_is_filled_with_the_sentinel_and_counted() {
        // The fill value is the one mem_alloc writes at test/infcover.c L87.
        assert_eq!(SENTINEL_FILL, 0xa5);

        let tracker = TrackingAllocator::new();
        let buffer = tracker.allocate_bytes(48, 1).unwrap();

        assert_eq!(buffer.len(), 48);
        assert!(
            buffer.as_slice().iter().all(|&byte| byte == 0xa5),
            "every byte of a fresh block must be 0xa5, never zero"
        );
        assert_eq!(tracker.total(), 48);
        assert!(tracker.high_water() >= 48);

        tracker.deallocate_bytes(&mut Some(buffer));
        tracker.assert_clean();
    }

    #[test]
    fn the_byte_count_is_the_product_of_items_and_size() {
        // ZALLOC passes items and size separately and the hook multiplies them.
        let tracker = TrackingAllocator::new();
        let buffer = tracker.allocate_bytes(32, 3).unwrap();
        assert_eq!(buffer.len(), 96);
        assert_eq!(tracker.total(), 96);
        tracker.deallocate_bytes(&mut Some(buffer));
        tracker.assert_clean();
    }

    #[test]
    fn u16_blocks_are_filled_bytewise_and_accounted_in_bytes() {
        let tracker = TrackingAllocator::new();
        let buffer = tracker.allocate_u16s(64).unwrap();

        assert_eq!(buffer.len(), 64);
        // Both bytes of the pattern are the sentinel, so the u16 reads as 0xa5a5 in
        // either byte order.
        assert!(buffer.as_slice().iter().all(|&value| value == 0xa5a5));
        // Accounting is in bytes, matching mem_alloc's count * size.
        assert_eq!(tracker.total(), 128);

        tracker.deallocate_u16s(&mut Some(buffer));
        assert_eq!(tracker.total(), 0);
        tracker.assert_clean();
    }

    #[test]
    fn a_release_lowers_the_total_but_never_the_high_water_mark() {
        let tracker = TrackingAllocator::new();
        let buffer = tracker.allocate_bytes(1024, 1).unwrap();
        assert_eq!(tracker.total(), 1024);
        assert_eq!(tracker.high_water(), 1024);

        tracker.deallocate_bytes(&mut Some(buffer));
        assert_eq!(tracker.total(), 0);
        assert_eq!(
            tracker.high_water(),
            1024,
            "the high water mark must be monotonic, so that it survives teardown"
        );
        assert_eq!(tracker.not_lifo(), 0);
        assert_eq!(tracker.rogue(), 0);

        // And it survives the end of tracking, which is what makes it a measurement.
        let report = tracker.finish();
        assert!(report.is_clean());
        assert_eq!(report.high_water, 1024);
        assert_eq!(tracker.high_water(), 1024);
    }

    #[test]
    fn releasing_in_reverse_order_is_clean() {
        let tracker = TrackingAllocator::new();
        let first = tracker.allocate_bytes(16, 1).unwrap();
        let second = tracker.allocate_bytes(32, 1).unwrap();
        assert_eq!(tracker.total(), 48);

        // Last in, first out -- the order deflateEnd releases in.
        tracker.deallocate_bytes(&mut Some(second));
        assert_eq!(tracker.not_lifo(), 0);
        tracker.deallocate_bytes(&mut Some(first));
        assert_eq!(tracker.not_lifo(), 0);

        assert_eq!(tracker.total(), 0);
        assert_eq!(tracker.high_water(), 48);
        tracker.assert_clean();
    }

    #[test]
    fn releasing_in_allocation_order_is_reported_as_not_lifo() {
        let tracker = TrackingAllocator::new();
        let first = tracker.allocate_bytes(16, 1).unwrap();
        let second = tracker.allocate_bytes(32, 1).unwrap();

        // First in, first out: the block released is not the most recent one, which
        // is what mem_free counts at test/infcover.c L136.
        tracker.deallocate_bytes(&mut Some(first));
        assert_eq!(tracker.not_lifo(), 1);
        assert_eq!(tracker.total(), 32);

        // The second release is now of the most recent block, so it is clean.
        tracker.deallocate_bytes(&mut Some(second));
        assert_eq!(tracker.not_lifo(), 1);
        assert_eq!(tracker.total(), 0);
        assert_eq!(tracker.rogue(), 0);

        // Out-of-order releases are an anomaly, so the run is not clean.
        let report = tracker.finish();
        assert!(!report.is_clean());
        assert_eq!(report.not_lifo, 1);
        assert_eq!(report.leaked_bytes, 0);
    }

    #[test]
    fn releasing_a_block_the_tracker_never_handed_out_is_rogue() {
        let tracker = TrackingAllocator::new();

        // A block from the global allocator directly, which this tracker never
        // recorded. It shares the tracker's identity, so it is genuinely released
        // rather than refused and nothing leaks.
        let stray: Buffer<'_, u8> = Buffer::try_global(24, 0).unwrap();
        tracker.deallocate_bytes(&mut Some(stray));

        assert_eq!(tracker.rogue(), 1);
        // A rogue release changes nothing else, per mem_free L149-L150.
        assert_eq!(tracker.total(), 0);
        assert_eq!(tracker.not_lifo(), 0);
        assert_eq!(tracker.high_water(), 0);

        let report = tracker.finish();
        assert!(!report.is_clean());
        assert_eq!(report.rogue, 1);
    }

    #[test]
    fn the_limit_forces_failure_and_records_nothing() {
        let tracker = TrackingAllocator::new();
        tracker.set_limit(64);

        // Over the ceiling: the request fails the way zalloc returning Z_NULL does.
        assert!(tracker.allocate_bytes(65, 1).is_none());
        // And leaves the books untouched, so a later assertion is not polluted.
        assert_eq!(tracker.total(), 0);
        assert_eq!(tracker.high_water(), 0);
        assert_eq!(tracker.not_lifo(), 0);
        assert_eq!(tracker.rogue(), 0);

        // Exactly at the ceiling still succeeds: the predicate is `total + len >
        // limit`, not `>=`.
        let exact = tracker.allocate_bytes(64, 1).unwrap();
        assert_eq!(tracker.total(), 64);

        // A further byte would breach it, and the u16 shape is capped identically.
        assert!(tracker.allocate_bytes(1, 1).is_none());
        assert!(tracker.allocate_u16s(1).is_none());
        tracker.deallocate_bytes(&mut Some(exact));

        // Zero disarms, exactly as mem_limit(&strm, 0) does at L329.
        tracker.set_limit(0);
        let unlimited = tracker.allocate_bytes(4096, 1).unwrap();
        assert_eq!(tracker.total(), 4096);
        tracker.deallocate_bytes(&mut Some(unlimited));

        tracker.assert_clean();
        assert_eq!(tracker.high_water(), 4096);
    }

    #[test]
    fn a_limit_of_one_byte_fails_every_request_the_library_makes() {
        // The arming value test/infcover.c uses at L326 to force Z_MEM_ERROR.
        let tracker = TrackingAllocator::new();
        tracker.set_limit(1);
        assert!(tracker.allocate_bytes(1, 2).is_none());
        assert!(tracker.allocate_u16s(1).is_none());
        tracker.assert_clean();
    }

    #[test]
    fn a_tracker_can_be_used_through_a_shared_reference() {
        // A suite hands the library `&tracker` and keeps the original to interrogate;
        // a shared reference to an allocator is itself an allocator.
        let tracker = TrackingAllocator::new();
        let borrowed: &TrackingAllocator = &tracker;
        let buffer = borrowed.allocate_bytes(8, 4).unwrap();
        assert_eq!(tracker.total(), 32);
        borrowed.deallocate_bytes(&mut Some(buffer));
        tracker.assert_clean();
    }

    #[test]
    fn assert_clean_accepts_a_balanced_sequence() {
        let tracker = TrackingAllocator::new();
        // The four shapes deflateInit allocates, released in reverse order.
        let window = tracker.allocate_bytes(256, 2).unwrap();
        let prev = tracker.allocate_u16s(256).unwrap();
        let head = tracker.allocate_u16s(512).unwrap();
        let pending = tracker.allocate_bytes(256, 4).unwrap();
        assert_eq!(tracker.total(), 512 + 512 + 1024 + 1024);

        tracker.deallocate_bytes(&mut Some(pending));
        tracker.deallocate_u16s(&mut Some(head));
        tracker.deallocate_u16s(&mut Some(prev));
        tracker.deallocate_bytes(&mut Some(window));

        tracker.assert_clean();
    }

    #[test]
    #[should_panic(expected = "bytes in 1 blocks not freed")]
    fn assert_clean_rejects_a_leak() {
        let tracker = TrackingAllocator::new();
        let buffer = tracker.allocate_bytes(32, 1).unwrap();
        // Dropped without being released, so the block's memory returns to Rust but
        // the tracker's books still show it outstanding -- which is exactly the
        // discrepancy mem_done reports.
        drop(buffer);
        tracker.assert_clean();
    }

    #[test]
    #[should_panic(expected = "releases not LIFO")]
    fn assert_clean_rejects_an_out_of_order_release() {
        let tracker = TrackingAllocator::new();
        let first = tracker.allocate_bytes(16, 1).unwrap();
        let second = tracker.allocate_bytes(16, 1).unwrap();
        tracker.deallocate_bytes(&mut Some(first));
        tracker.deallocate_bytes(&mut Some(second));
        tracker.assert_clean();
    }

    #[test]
    fn finish_ends_tracking_but_preserves_the_diagnosis() {
        let tracker = TrackingAllocator::new();
        let leaked = tracker.allocate_bytes(64, 1).unwrap();
        drop(leaked);

        let first = tracker.finish();
        assert_eq!(first.leaked_bytes, 64);
        assert_eq!(first.leaked_blocks, 1);
        assert_eq!(first.high_water, 64);

        // Tracking has ended, so there is nothing further outstanding ...
        let second = tracker.finish();
        assert_eq!(second.leaked_bytes, 0);
        assert_eq!(second.leaked_blocks, 0);
        // ... but the history is still there to be read.
        assert_eq!(second.high_water, 64);
        assert_eq!(tracker.high_water(), 64);
    }

    #[test]
    fn a_zero_length_request_is_tracked_without_colliding() {
        // The library never asks for zero bytes, but two zero-length Rust allocations
        // share a dangling address, so the tracker must not mistake one for the other.
        let tracker = TrackingAllocator::new();
        let first = tracker.allocate_bytes(0, 1).unwrap();
        let second = tracker.allocate_bytes(0, 4).unwrap();
        assert!(first.is_empty());
        assert!(second.is_empty());
        assert_eq!(tracker.total(), 0);

        tracker.deallocate_bytes(&mut Some(second));
        tracker.deallocate_bytes(&mut Some(first));

        // Both were recognised: neither release was rogue, and neither was counted
        // out of order.
        assert_eq!(tracker.rogue(), 0);
        assert_eq!(tracker.not_lifo(), 0);
        tracker.assert_clean();
    }
}
