// This suite measures memory, so it only means anything when the exported C surface and the gz layer
// are both compiled. `--no-default-features` is a supported configuration for this crate, and with
// either feature off there is no `gzopen` symbol to link against, so the whole binary compiles away.
#![cfg(all(feature = "libz-compat", feature = "gz"))]
// The workspace denies the panic-prone family in `[workspace.lints.clippy]`, which is right for
// `src/**` -- a panic there would abort a C caller's process -- and wrong for a test, whose
// assertions panic by design. `clippy.toml` keys `allow-unwrap-in-tests` and friends on `#[test]`
// context only, so the file-scope helpers below still need this. Nothing else is relaxed.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

//! Per-stream memory accounting for the `gzFile` layer, reported against the C implementation.
//!
//! AAP 0.8.4 sets the gate this suite enforces: per-stream memory "within 15% of C zlib", measured
//! through "an instrumented `zalloc` high-water counter, mirroring the `mem_high()` technique in
//! `test/infcover.c`". For the `gzFile` layer that technique cannot be applied as written, and the
//! reason is a property of C's own design rather than of this port:
//!
//! **A `gzFile` has no caller-supplied allocator.** The `gzopen` family takes no `zalloc`, and C's gz
//! layer deliberately clears the ones on its embedded stream before initialising an engine --
//! `strm->zalloc = Z_NULL; strm->zfree = Z_NULL; strm->opaque = Z_NULL` at `gzwrite.c` L30-L32 and
//! `gzread.c` L110-L112. So every byte a `gzFile` costs, on both implementations, comes from the
//! process allocator: `malloc` in C, and Rust's global allocator here (`crates/libz-rs-sys/src/gz.rs`
//! stores its state as `GzState<'static, GlobalAllocator>`). The instrument therefore has to be a
//! counting **global** allocator, and that is what [`Counting`] is -- a pass-through wrapper over
//! [`System`] that keeps a per-thread outstanding total and high-water mark.
//!
//! # The two figures, and why both are reported
//!
//! *Idle fixed state* is what a stream costs before it does any work: in C one `malloc` of
//! `sizeof(gz_state)` plus one for the path (`gzlib.c` L104 and L200-L204). It is the figure that
//! sizes a program holding many open-but-quiet `gzFile`s.
//!
//! *Active high-water* is the peak while the stream is actually moving bytes, which is dominated by
//! the working buffers and the engine -- roughly 286 KiB for a write stream and 63 KiB for a read
//! stream. It is the figure that sizes a program compressing something.
//!
//! Reporting only the first overstates any difference by two orders of magnitude; reporting only the
//! second hides it entirely. Both are printed by every run and both are asserted.
//!
//! # Where the C numbers come from
//!
//! Every C figure below is either a measured `sizeof` on this target or a buffer size read straight
//! out of the C source, and each is named as a constant with its origin cited. Nothing is estimated,
//! and no C code is compiled here -- the reference sizes are stable properties of the in-tree headers
//! on this target, and the differential harness is where behaviour (rather than footprint) is
//! compared against the C library itself.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::ffi::{c_char, c_int, c_uint, c_void, CString};
use std::fs;
use std::path::PathBuf;

// Forces the rlib onto the link line. The gz entry points are `#[no_mangle] pub extern "C"` inside a
// private module, so they are reachable only as C symbols -- which is the point: this measures what a
// C consumer's process actually pays, through the same symbols `test/minigzip.c` calls.
use z as _;

extern "C" {
    fn gzopen(path: *const c_char, mode: *const c_char) -> *mut c_void;
    fn gzwrite(file: *mut c_void, buf: *const c_void, len: c_uint) -> c_int;
    fn gzread(file: *mut c_void, buf: *mut c_void, len: c_uint) -> c_int;
    fn gzclose(file: *mut c_void) -> c_int;
}

// ---------------------------------------------------------------------------------------------
// The instrument
// ---------------------------------------------------------------------------------------------

// Per-thread rather than process-wide, because the test harness runs tests on parallel threads and a
// shared counter would measure whatever else happened to be running. Every allocation a `gzFile`
// makes happens on the thread that drives it, so per-thread accounting is both quieter and exact for
// the window each test measures.
//
// `const`-initialised on purpose: a `thread_local!` with a lazy initialiser would allocate on first
// access, and allocating from inside the allocator is unbounded recursion. `Cell<usize>` has no
// destructor, so no thread-exit callback is registered either.
thread_local! {
    static OUTSTANDING: Cell<usize> = const { Cell::new(0) };
    static PEAK: Cell<usize> = const { Cell::new(0) };
}

/// A pass-through global allocator that records the current and peak outstanding bytes.
///
/// [`System`] does the work; this only counts. `realloc` and `alloc_zeroed` are deliberately left to
/// the default [`GlobalAlloc`] implementations, which route through this type's own `alloc` and
/// `dealloc`, so a growing [`Vec`] is accounted for without any extra bookkeeping here.
struct Counting;

/// Adds `bytes` to the outstanding total and raises the high-water mark if it moved.
///
/// `try_with` rather than `with`: during thread-local destruction at thread exit the slot may already
/// be gone, and an allocator that panicked there would abort the process. A lost count at thread exit
/// cannot affect any measurement, all of which complete inside a test body.
fn record_alloc(bytes: usize) {
    let _ = OUTSTANDING.try_with(|outstanding| {
        let now = outstanding.get().saturating_add(bytes);
        outstanding.set(now);
        let _ = PEAK.try_with(|peak| {
            if now > peak.get() {
                peak.set(now);
            }
        });
    });
}

/// Removes `bytes` from the outstanding total, saturating so that a stray free cannot wrap it.
fn record_free(bytes: usize) {
    let _ = OUTSTANDING.try_with(|outstanding| {
        outstanding.set(outstanding.get().saturating_sub(bytes));
    });
}

// SAFETY: every method forwards to `System`, the allocator this type wraps, with the layout it
// was given and unchanged -- so the implementation inherits `System`'s own compliance with the
// trait's contract (a valid non-null block or null, correctly aligned, released only through the
// matching layout). The accounting reads the layout and never touches the block.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller of `GlobalAlloc::alloc` guarantees a non-zero-sized layout, which is
        // exactly what `System::alloc` requires; the layout is forwarded unchanged.
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            record_alloc(layout.size());
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        record_free(layout.size());
        // SAFETY: the caller guarantees `pointer` came from this allocator with this layout, and
        // every block this allocator hands out came from `System` with the same layout.
        unsafe { System.dealloc(pointer, layout) };
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// The outstanding and peak byte counts for the current thread, both relative to a baseline.
#[derive(Debug, Clone, Copy)]
struct Usage {
    /// Bytes allocated and not yet freed, above the baseline.
    outstanding: usize,
    /// The highest the outstanding total reached since the baseline was taken.
    peak: usize,
}

/// Starts a measurement window: returns the current outstanding total and resets the peak to it.
///
/// Anything the test itself allocated before this call -- payload buffers, path strings -- is inside
/// the baseline and therefore excluded from every figure reported afterwards.
fn begin() -> usize {
    let baseline = OUTSTANDING.with(Cell::get);
    PEAK.with(|peak| peak.set(baseline));
    baseline
}

/// Reads the window's figures, both relative to `baseline`.
fn measure(baseline: usize) -> Usage {
    Usage {
        outstanding: OUTSTANDING.with(Cell::get).saturating_sub(baseline),
        peak: PEAK.with(Cell::get).saturating_sub(baseline),
    }
}

// ---------------------------------------------------------------------------------------------
// The C reference figures
// ---------------------------------------------------------------------------------------------

/// `#[repr(C)]` mirrors of the three private C state structs, so that every `sizeof` this suite
/// divides by is the one **the target being tested** would really have.
///
/// # Why mirrors rather than three literals
///
/// The literals this module replaces — 248, 5968 and 7160 — were measured with a C probe on
/// `x86_64-unknown-linux-gnu` and were then applied on **every** target, including the LLP64
/// Windows row the CI matrix runs. That is not a reporting inaccuracy, it is a hole in the gate:
/// the figures are the *denominator* of `rust * 100 <= c * 115`, C's `gz_state` embeds a whole
/// `z_stream` and C's `deflate_state`/`inflate_state` carry `unsigned long`s and pointers
/// throughout, so all three shrink where `unsigned long` or a pointer is narrower. A denominator
/// borrowed from a wider model is a budget that is too generous, and a Rust stream genuinely past
/// 115 % of the real C footprint passed.
///
/// # Why this is a probe and not a guess
///
/// `#[repr(C)]` field placement is a function of nothing but field width and field alignment, so a
/// faithful mirror laid out by the Rust compiler for a target *is* what the C compiler would lay
/// out for that target. The mirrors are validated rather than asserted to be faithful:
/// [`the_c_mirrors_reproduce_the_measured_lp64_sizes`] holds them against the three numbers the C
/// probe actually measured, so on the one model where ground truth exists the mirrors must
/// reproduce it exactly — and a mirror that reproduces 248/5968/7160 on LP64 is a mirror whose
/// declaration order and field widths match the headers.
///
/// Declared from the headers, member for member and in declaration order:
/// `gzguts.h` L170-L203 (`gz_state`), `deflate.h` L104-L288 (`deflate_state`) and
/// `inflate.h` L82-L126 (`struct inflate_state`). `LIT_MEM` is deliberately absent because
/// `deflate.h` L28 leaves it undefined, which is the configuration the reference build ships;
/// `ZLIB_DEBUG` is absent for the same reason. Both would change the size, and the LP64 validation
/// is what proves the shipped configuration is the one mirrored.
///
/// Nothing here is ever instantiated — only `size_of` is taken — so the pointer members need no
/// referent and the arrays cost nothing.
#[allow(dead_code)]
mod c_probe {
    use std::ffi::{c_char, c_int, c_long, c_uchar, c_uint, c_ulong, c_ushort, c_void};

    /// `z_off64_t` (`zconf.h` L494-L531): 64 bits wherever large-file support exists.
    type COff64 = i64;

    /// `alloc_func` / `free_func` (`zlib.h` L85-L86), as one pointer-wide slot each.
    type CHook = Option<unsafe extern "C" fn()>;

    /// `z_stream` (`zlib.h` L90-L110). Embedded whole in `gz_state`, so its width drives that
    /// struct's.
    #[repr(C)]
    pub struct CZStream {
        next_in: *const c_uchar,
        avail_in: c_uint,
        total_in: c_ulong,
        next_out: *mut c_uchar,
        avail_out: c_uint,
        total_out: c_ulong,
        msg: *const c_char,
        state: *mut c_void,
        zalloc: CHook,
        zfree: CHook,
        opaque: *mut c_void,
        data_type: c_int,
        adler: c_ulong,
        reserved: c_ulong,
    }

    /// `gz_header` (`zlib.h` L118-L133). Reached only as a pointer member below.
    #[repr(C)]
    pub struct CGzHeader {
        text: c_int,
        time: c_ulong,
        xflags: c_int,
        os: c_int,
        extra: *mut c_uchar,
        extra_len: c_uint,
        extra_max: c_uint,
        name: *mut c_uchar,
        name_max: c_uint,
        comment: *mut c_uchar,
        comm_max: c_uint,
        hcrc: c_int,
        done: c_int,
    }

    /// `struct gzFile_s` (`zlib.h` L1956-L1960) — `gz_state`'s first member.
    #[repr(C)]
    pub struct CGzFileS {
        have: c_uint,
        next: *mut c_uchar,
        pos: COff64,
    }

    /// `gz_state` (`gzguts.h` L170-L203). The one `malloc` `gz_open` makes for the state itself.
    #[repr(C)]
    pub struct CGzState {
        x: CGzFileS,
        mode: c_int,
        fd: c_int,
        path: *mut c_char,
        size: c_uint,
        want: c_uint,
        input: *mut c_uchar,
        out: *mut c_uchar,
        direct: c_int,
        junk: c_int,
        how: c_int,
        again: c_int,
        start: COff64,
        eof: c_int,
        past: c_int,
        level: c_int,
        strategy: c_int,
        reset: c_int,
        skip: COff64,
        err: c_int,
        msg: *mut c_char,
        strm: CZStream,
    }

    /// `HEAP_SIZE` = `2 * L_CODES + 1` = 573 (`deflate.h` L48).
    const HEAP_SIZE: usize = 2 * 286 + 1;
    /// `2 * D_CODES + 1` = 61 (`deflate.h` L206).
    const D_TREE_SIZE: usize = 2 * 30 + 1;
    /// `2 * BL_CODES + 1` = 39 (`deflate.h` L209).
    const BL_TREE_SIZE: usize = 2 * 19 + 1;
    /// `MAX_BITS + 1` = 16 (`deflate.h` L216).
    const BL_COUNT_SIZE: usize = 16;

    /// `struct ct_data_s` (`deflate.h` L74-L84): two `ush` unions.
    #[repr(C)]
    pub struct CCtData {
        fc: c_ushort,
        dl: c_ushort,
    }

    /// `struct tree_desc_s` (`deflate.h` L88-L92).
    #[repr(C)]
    pub struct CTreeDesc {
        dyn_tree: *mut CCtData,
        max_code: c_int,
        stat_desc: *const c_void,
    }

    /// `deflate_state` (`deflate.h` L104-L288), in the shipped configuration: `LIT_MEM`
    /// undefined, so one `sym_buf` rather than a `d_buf`/`l_buf` pair, and `ZLIB_DEBUG`
    /// undefined, so no `compressed_len`/`bits_sent`.
    #[repr(C)]
    pub struct CDeflateState {
        strm: *mut CZStream,
        status: c_int,
        pending_buf: *mut c_uchar,
        pending_buf_size: c_ulong,
        pending_out: *mut c_uchar,
        pending: c_ulong,
        wrap: c_int,
        gzhead: *mut CGzHeader,
        gzindex: c_ulong,
        method: c_uchar,
        last_flush: c_int,
        w_size: c_uint,
        w_bits: c_uint,
        w_mask: c_uint,
        window: *mut c_uchar,
        window_size: c_ulong,
        prev: *mut c_ushort,
        head: *mut c_ushort,
        ins_h: c_uint,
        hash_size: c_uint,
        hash_bits: c_uint,
        hash_mask: c_uint,
        hash_shift: c_uint,
        block_start: c_long,
        match_length: c_uint,
        prev_match: c_uint,
        match_available: c_int,
        strstart: c_uint,
        match_start: c_uint,
        lookahead: c_uint,
        prev_length: c_uint,
        max_chain_length: c_uint,
        max_lazy_match: c_uint,
        level: c_int,
        strategy: c_int,
        good_match: c_uint,
        nice_match: c_int,
        dyn_ltree: [CCtData; HEAP_SIZE],
        dyn_dtree: [CCtData; D_TREE_SIZE],
        bl_tree: [CCtData; BL_TREE_SIZE],
        l_desc: CTreeDesc,
        d_desc: CTreeDesc,
        bl_desc: CTreeDesc,
        bl_count: [c_ushort; BL_COUNT_SIZE],
        heap: [c_int; HEAP_SIZE],
        heap_len: c_int,
        heap_max: c_int,
        depth: [c_uchar; HEAP_SIZE],
        sym_buf: *mut c_uchar,
        lit_bufsize: c_uint,
        sym_next: c_uint,
        sym_end: c_uint,
        opt_len: c_ulong,
        static_len: c_ulong,
        matches: c_uint,
        insert: c_uint,
        bi_buf: c_ushort,
        bi_valid: c_int,
        bi_used: c_int,
        high_water: c_ulong,
        slid: c_int,
    }

    /// `ENOUGH` = `ENOUGH_LENS + ENOUGH_DISTS` = 1444 (`inftrees.h` L49-L51).
    const ENOUGH: usize = 852 + 592;

    /// `code` (`inftrees.h` L24-L28): four bytes on every target.
    #[repr(C)]
    pub struct CCode {
        op: c_uchar,
        bits: c_uchar,
        val: c_ushort,
    }

    /// `struct inflate_state` (`inflate.h` L82-L126). `inflate_mode` is an all-non-negative C
    /// enum, which GCC and Clang give `int` width, hence `c_int` for `mode`.
    #[repr(C)]
    pub struct CInflateState {
        strm: *mut CZStream,
        mode: c_int,
        last: c_int,
        wrap: c_int,
        havedict: c_int,
        flags: c_int,
        dmax: c_uint,
        check: c_ulong,
        total: c_ulong,
        head: *mut CGzHeader,
        wbits: c_uint,
        wsize: c_uint,
        whave: c_uint,
        wnext: c_uint,
        window: *mut c_uchar,
        hold: c_ulong,
        bits: c_uint,
        length: c_uint,
        offset: c_uint,
        extra: c_uint,
        lencode: *const CCode,
        distcode: *const CCode,
        lenbits: c_uint,
        distbits: c_uint,
        ncode: c_uint,
        nlen: c_uint,
        ndist: c_uint,
        have: c_uint,
        next: *mut CCode,
        lens: [c_ushort; 320],
        work: [c_ushort; 288],
        codes: [CCode; ENOUGH],
        sane: c_int,
        back: c_int,
        was: c_uint,
    }
}

/// `sizeof(gz_state)` **on the target being tested**: the one `malloc` `gz_open` makes for the
/// state itself (`gzlib.c` L104, `state = (gz_statep)malloc(sizeof(gz_state))`).
///
/// ★ Derived from [`c_probe::CGzState`], not written as a literal, and that is the whole point of
/// the probe module. This figure is the *denominator* of a 115 % budget, so a number measured on
/// one integer model and applied to another does not merely mis-report: on a model where the real
/// C state is smaller it makes the budget **larger than it should be**, and a Rust stream genuinely
/// over the limit passes. The literal 248 was an LP64 measurement, and C's `gz_state` embeds a
/// whole `z_stream` plus two `z_off64_t`s, so it is 224 on LLP64 and 160 on ILP32 — which is
/// exactly how much slack the Windows and 32-bit rows were being handed.
const fn c_gz_state() -> usize {
    size_of::<c_probe::CGzState>()
}

/// `sizeof(deflate_state)`, allocated by `deflateInit2_` through `ZALLOC` (`deflate.c` L431).
///
/// Derived from [`c_probe::CDeflateState`] for the reason [`c_gz_state`] gives: 5968 on LP64,
/// 5920 on LLP64 and 5836 on ILP32.
const fn c_deflate_state() -> usize {
    size_of::<c_probe::CDeflateState>()
}

/// `sizeof(struct inflate_state)`, allocated by `inflateInit2_` through `ZALLOC`
/// (`inflate.c` L172).
///
/// Derived from [`c_probe::CInflateState`]: 7160 on LP64, 7152 on LLP64 and 7120 on ILP32.
const fn c_inflate_state() -> usize {
    size_of::<c_probe::CInflateState>()
}

/// `GZBUFSIZE` (`gzguts.h` L156): the default `state->want`, and the unit every gz buffer is sized
/// in.
const C_WANT: usize = 8192;

/// [`C_WANT`] as the `unsigned` a `gzread` length is, so the read loop needs no conversion.
const C_WANT_U32: c_uint = 8192;

/// The four `ZALLOC` blocks `deflateInit2_` makes for `windowBits = 15`, `memLevel = 8` -- the
/// settings C's `gz_init` passes (`gzwrite.c` L35-L36, `MAX_WBITS + 16` and `DEF_MEM_LEVEL`).
///
/// `window` is `w_size * 2` bytes, `prev` is `w_size * sizeof(Pos)`, `head` is
/// `hash_size * sizeof(Pos)` and `pending_buf` is `lit_bufsize * LIT_BUFS` (`deflate.c` L457-L471).
/// With `w_size = 1 << 15`, `hash_size = 1 << 15`, `lit_bufsize = 1 << 14` and `LIT_BUFS = 4` that is
/// four blocks of 64 KiB each.
const C_DEFLATE_BUFFERS: usize = 4 * 65536;

/// The inflate window, `ZALLOC(strm, 1U << state->wbits, 1)` (`inflate.c` L400-L403), allocated on
/// demand for `wbits = 15`.
const C_INFLATE_WINDOW: usize = 1 << 15;

/// AAP 0.8.4's gate: per-stream memory within 15% of C. Applied as `rust * 100 <= c * 115` so that
/// the comparison is exact integer arithmetic with no rounding to argue about.
fn within_budget(rust: usize, c: usize) -> bool {
    rust.saturating_mul(100) <= c.saturating_mul(115)
}

/// Formats a figure as a percentage of the C reference, for the report each run prints.
fn percent_of(rust: usize, c: usize) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    let (rust, c) = (rust as f64, c as f64);
    rust * 100.0 / c
}

/// A private directory for this suite's scratch file, and the file's path inside it.
///
/// The measurement below opens the file with `gzopen(path, "wb")`, which is
/// `open(..., O_CREAT | O_TRUNC, ...)` with no `O_EXCL` (`gzlib.c` L228-L244). A predictable name in
/// a shared, world-writable directory is therefore an attack surface and not merely a collision
/// risk -- a process id is visible in `/proc`, drawn from a small space and reused, so another user
/// can plant a symlink at the name and have the truncating open follow it. The *directory* carries
/// the property instead, created unguessable (128 bits from two OS-seeded
/// [`RandomState`](std::hash::RandomState) draws), owner-only in the same syscall that creates it
/// (`mode(0o700)`), and exclusively (`recursive(false)`, so an occupied name is refused rather than
/// adopted -- nothing here removes a path it did not itself create).
///
/// `crates/libz-rs-sys/tests/gz_printf.rs` sets the reasoning out in full on its own `TempPath`, and
/// `the_scratch_directory_is_private_unguessable_and_exclusively_created` there asserts each of the
/// three bullets; this is the same type with a different name prefix.
///
/// [`Drop`] removes the tree, so cleanup survives a failing assertion unwinding through it. No
/// `tempfile` dev-dependency: AAP 0.7.1(i) keeps the dependency table minimal.
struct TempPath {
    /// The private directory. Removed, with its contents, when this value is dropped.
    dir: PathBuf,
    /// The scratch file inside it.
    file: PathBuf,
}

impl TempPath {
    /// How many unguessable names to try before giving up. A collision means another process holds
    /// that exact 128-bit name; the bound keeps a directory that refuses every create from spinning.
    const ATTEMPTS: usize = 16;

    fn new(tag: &str) -> Self {
        use std::hash::{BuildHasher, RandomState};
        #[cfg(unix)]
        use std::os::unix::fs::DirBuilderExt;

        let base = std::env::temp_dir();
        for _ in 0..Self::ATTEMPTS {
            let high = u128::from(RandomState::new().hash_one(0_u64));
            let low = u128::from(RandomState::new().hash_one(u64::MAX));
            let dir = base.join(format!("blitzy_gz_memory_{:032x}", (high << 64) | low));

            let mut builder = fs::DirBuilder::new();
            builder.recursive(false);
            #[cfg(unix)]
            builder.mode(0o700);

            match builder.create(&dir) {
                Ok(()) => {
                    let file = dir.join(format!("{tag}.gz"));
                    return Self { dir, file };
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => panic!(
                    "cannot create a private scratch directory under {}: {error}",
                    base.display()
                ),
            }
        }
        panic!(
            "{} unguessable names under {} were all taken",
            Self::ATTEMPTS,
            base.display()
        )
    }

    fn as_cstring(&self) -> CString {
        CString::new(self.file.to_str().expect("a temp path must be UTF-8")).unwrap()
    }
}

impl Drop for TempPath {
    fn drop(&mut self) {
        // Best effort, and only ever on a directory this process created.
        let _ = fs::remove_dir_all(&self.dir);
    }
}

// ---------------------------------------------------------------------------------------------
// The measurement
// ---------------------------------------------------------------------------------------------

/// Reports and gates the two per-stream memory figures for a write stream and a read stream.
///
/// The shape of each half is the one `test/infcover.c` uses for its own allocation accounting: take a
/// baseline, do the work, read the peak, then close and require the books to balance. The final
/// balance check is the part that makes the peak trustworthy -- a stream that leaked half its buffers
/// would otherwise report a flattering high-water and pass.
///
/// Three numbers are asserted per half:
///
/// * **idle** -- outstanding bytes with the file open and nothing done yet, against C's
///   `sizeof(gz_state)` plus its path copy.
/// * **peak** -- the high-water while compressing or decompressing, against the sum of C's own
///   allocations for the same configuration.
/// * **leaked** -- outstanding bytes after `gzclose`, which must be exactly zero.
///
/// Requires the filesystem, so it is skipped under Miri; the facade's Miri coverage runs over the
/// safe core (AAP 0.6.4.5) and this suite measures the process allocator, which is not what Miri is
/// for.
#[test]
#[cfg_attr(miri, ignore)]
fn per_stream_memory_is_reported_and_within_the_c_budget() {
    // Everything the test itself needs is allocated up front, before any baseline is taken, so no
    // figure below includes it. 256 KiB of compressible text is large enough to force the read side
    // to allocate its window, which a short payload would leave out of the comparison.
    let temp = TempPath::new("stream");
    let path = temp.as_cstring();
    let write_mode = CString::new("wb").unwrap();
    let read_mode = CString::new("rb").unwrap();
    let payload: Vec<u8> = (0..256 * 1024)
        .map(|index| b"the quick brown fox jumps over the lazy dog "[index % 43])
        .collect();
    let mut readback = vec![0_u8; payload.len()];
    // C copies the path with `malloc(strlen(path) + 1)` (`gzlib.c` L200-L204), so the reference
    // figures include it; `path.as_bytes()` excludes the NUL, hence the `+ 1`.
    let c_path = path.as_bytes().len() + 1;

    // ----- the write stream -------------------------------------------------------------------
    let c_idle = c_gz_state() + c_path;
    // `gz_init`: `in` is `want << 1` and `out` is `want` (`gzwrite.c` L16 and L26), plus the
    // compressor `deflateInit2` builds (`gzwrite.c` L35-L36).
    let c_write_peak = c_idle + (C_WANT << 1) + C_WANT + c_deflate_state() + C_DEFLATE_BUFFERS;

    let baseline = begin();
    // SAFETY: two live `CString`s, each valid for reads through its terminator for the whole
    // call; the library copies the path it keeps, so neither is retained.
    let file = unsafe { gzopen(path.as_ptr(), write_mode.as_ptr()) };
    assert!(!file.is_null(), "gzopen for writing must succeed");
    let write_idle = measure(baseline).outstanding;

    // The whole payload in one call, which is larger than the buffer and so drives the fill/compress
    // loop rather than a single buffered copy.
    let length = c_uint::try_from(payload.len()).unwrap();
    // SAFETY: `file` is the live handle opened above and not yet closed; `payload` is a live
    // slice and `length` is its own length, so the library reads only bytes it was given.
    let written = unsafe { gzwrite(file, payload.as_ptr().cast::<c_void>(), length) };
    assert_eq!(
        written,
        c_int::try_from(payload.len()).unwrap(),
        "gzwrite must accept the whole payload"
    );
    let write_peak = measure(baseline).peak;

    // SAFETY: `file` is the live handle, closed exactly once here and never used again.
    let closed = unsafe { gzclose(file) };
    assert_eq!(closed, 0, "gzclose must report Z_OK");
    let write_leaked = measure(baseline).outstanding;

    // ----- the read stream --------------------------------------------------------------------
    // `gz_look`: `in` is `want` and `out` is `want << 1` (`gzread.c` L99-L100), plus the
    // decompressor `inflateInit2` builds (`gzread.c` L115) and the window it allocates on demand.
    let c_read_peak = c_idle + C_WANT + (C_WANT << 1) + c_inflate_state() + C_INFLATE_WINDOW;

    let baseline = begin();
    // SAFETY: as the write open above -- two live `CString`s, neither retained.
    let file = unsafe { gzopen(path.as_ptr(), read_mode.as_ptr()) };
    assert!(!file.is_null(), "gzopen for reading must succeed");
    let read_idle = measure(baseline).outstanding;

    // Read the whole stream back in `want`-sized chunks. Reading across calls is what makes the
    // decompressor need its window at all -- a single call that consumed the entire member could
    // finish without one -- so a chunked read is the configuration the C figure describes.
    let mut filled = 0_usize;
    loop {
        let room = c_uint::try_from(readback.len() - filled)
            .unwrap()
            .min(C_WANT_U32);
        if room == 0 {
            break;
        }
        // SAFETY: `file` is the live read handle; the destination is the remaining tail of a
        // live local buffer and `room` is that tail's own length, so nothing is written past it.
        let read = unsafe { gzread(file, readback[filled..].as_mut_ptr().cast::<c_void>(), room) };
        assert!(read >= 0, "gzread must not fail");
        if read == 0 {
            break;
        }
        filled += usize::try_from(read).unwrap();
    }
    assert_eq!(filled, payload.len(), "the whole payload must read back");
    assert_eq!(readback, payload, "and read back byte for byte");
    let read_peak = measure(baseline).peak;

    // SAFETY: `file` is the live read handle, closed exactly once here.
    let closed = unsafe { gzclose(file) };
    assert_eq!(closed, 0, "gzclose must report Z_OK");
    let read_leaked = measure(baseline).outstanding;

    // ----- the report -------------------------------------------------------------------------
    // Printed on every run, not only on failure: AAP 0.8.4 asks for these figures, and a gate that
    // passes silently reports nothing.
    println!("gzFile per-stream memory, this port versus C zlib on this target");
    println!(
        "  write idle  {write_idle:>7} B   C {c_idle:>7} B   {:>6.2}%",
        percent_of(write_idle, c_idle)
    );
    println!(
        "  write peak  {write_peak:>7} B   C {c_write_peak:>7} B   {:>6.2}%",
        percent_of(write_peak, c_write_peak)
    );
    println!(
        "  read  idle  {read_idle:>7} B   C {c_idle:>7} B   {:>6.2}%",
        percent_of(read_idle, c_idle)
    );
    println!(
        "  read  peak  {read_peak:>7} B   C {c_read_peak:>7} B   {:>6.2}%",
        percent_of(read_peak, c_read_peak)
    );

    // ----- the gates --------------------------------------------------------------------------
    assert_eq!(
        write_leaked, 0,
        "a closed write stream must free everything"
    );
    assert_eq!(read_leaked, 0, "a closed read stream must free everything");
    assert!(
        write_idle > 0 && read_idle > 0,
        "the instrument must have observed the open itself"
    );
    assert!(
        within_budget(write_idle, c_idle),
        "idle write state {write_idle} B exceeds 115% of C's {c_idle} B"
    );
    assert!(
        within_budget(read_idle, c_idle),
        "idle read state {read_idle} B exceeds 115% of C's {c_idle} B"
    );
    assert!(
        within_budget(write_peak, c_write_peak),
        "write high-water {write_peak} B exceeds 115% of C's {c_write_peak} B"
    );
    assert!(
        within_budget(read_peak, c_read_peak),
        "read high-water {read_peak} B exceeds 115% of C's {c_read_peak} B"
    );
}

/// The `#[repr(C)]` C mirrors reproduce the sizes a C probe measured, per integer model.
///
/// ★ This is what licenses [`c_gz_state`], [`c_deflate_state`] and [`c_inflate_state`] as
/// *reference* figures rather than as plausible-looking arithmetic. On LP64 there is ground truth
/// — a C probe compiled against the in-tree `gzguts.h`, `deflate.h` and `inflate.h` on
/// `x86_64-unknown-linux-gnu` reported 248, 5968 and 7160 — and a mirror that reproduces all three
/// exactly is a mirror whose declaration order, field widths and field alignments match the
/// headers. `#[repr(C)]` placement depends on nothing else, so the same mirrors laid out for
/// another target give that target's C sizes.
///
/// The other two models are then pinned at the numbers those same mirrors produce when compiled for
/// them, so a drift is a named failure rather than a silently different denominator:
///
/// | Model | `gz_state` | `deflate_state` | `struct inflate_state` |
/// |---|---|---|---|
/// | LP64 | 248 | 5968 | 7160 |
/// | LLP64 | 224 | 5920 | 7152 |
/// | ILP32 | 160 | 5836 | 7120 |
///
/// A model none of the three tiers recognises fails outright rather than skipping. That is
/// deliberate and is the opposite of how a *layout* tier behaves: an unpinned layout still has
/// `tests/abi_layout.rs`'s relational tier behind it, whereas an unpinned budget denominator has
/// nothing behind it at all — the gate would still compute a number and still pass or fail on it,
/// with no one having checked which. Add a row rather than let that happen.
#[test]
fn the_c_mirrors_reproduce_the_measured_lp64_sizes() {
    let pointer = size_of::<*const c_void>();
    let ulong = size_of::<std::ffi::c_ulong>();

    // Every model must at least agree about the two structural facts the mirrors rest on.
    assert!(
        c_gz_state() > size_of::<c_probe::CZStream>(),
        "gz_state embeds a whole z_stream and more besides"
    );
    assert!(
        c_inflate_state() > c_deflate_state(),
        "struct inflate_state is the larger of the two engines on every target"
    );

    let (model, gz, def, inf) = match (pointer, ulong) {
        (8, 8) => ("LP64", 248_usize, 5968_usize, 7160_usize),
        (8, 4) => ("LLP64", 224, 5920, 7152),
        (4, 4) => ("ILP32", 160, 5836, 7120),
        _ => panic!(
            "unrecognised integer model: {pointer}-byte pointer, {ulong}-byte unsigned long. \
             The C reference sizes this suite divides by are pinned per model, and a budget \
             denominator nothing has checked is worse than no budget: add a row above rather \
             than measuring against another model's figures."
        ),
    };

    println!(
        "gz_memory: {model} C reference sizes -- gz_state {} B, deflate_state {} B, \
         inflate_state {} B",
        c_gz_state(),
        c_deflate_state(),
        c_inflate_state()
    );

    assert_eq!(
        c_gz_state(),
        gz,
        "sizeof(gz_state) under {model}: the mirror and the header must agree"
    );
    assert_eq!(
        c_deflate_state(),
        def,
        "sizeof(deflate_state) under {model}: the mirror and the header must agree"
    );
    assert_eq!(
        c_inflate_state(),
        inf,
        "sizeof(struct inflate_state) under {model}: the mirror and the header must agree"
    );

    // The two mirrors C exposes publicly are already pinned exactly by
    // `tests/abi_layout.rs`; asserting them against the facade's own types here is what
    // proves the probe module is describing the same ABI that suite measures, rather than a
    // parallel set of declarations that happens to compile.
    assert_eq!(size_of::<c_probe::CZStream>(), size_of::<z::z_stream>());
    assert_eq!(size_of::<c_probe::CGzHeader>(), size_of::<z::gz_header>());
    assert_eq!(size_of::<c_probe::CGzFileS>(), size_of::<z::gzFile_s>());
    assert_eq!(size_of::<c_probe::CCode>(), size_of::<z::code>());
}
