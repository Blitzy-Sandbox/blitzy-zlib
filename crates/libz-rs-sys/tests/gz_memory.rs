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
use std::process;

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

/// `sizeof(gz_state)` on this target: the one `malloc` `gz_open` makes for the state itself
/// (`gzlib.c` L104, `state = (gz_statep)malloc(sizeof(gz_state))`).
///
/// Measured with a C probe against the in-tree `gzguts.h`: 248 bytes on LP64. This is the figure the
/// idle-state gate is set from, and the same one `crates/libz-rs-sys/src/gz.rs` asserts its own
/// `GzBlock` against at compile time.
const C_GZ_STATE: usize = 248;

/// `sizeof(deflate_state)`, allocated by `deflateInit2_` through `ZALLOC` (`deflate.c` L431).
/// Measured: 5968 bytes on LP64.
const C_DEFLATE_STATE: usize = 5968;

/// `sizeof(struct inflate_state)`, allocated by `inflateInit2_` through `ZALLOC`
/// (`inflate.c` L172). Measured: 7160 bytes on LP64.
const C_INFLATE_STATE: usize = 7160;

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

/// A scratch file removed when the guard is dropped, including while a failing test unwinds.
struct TempPath(PathBuf);

impl TempPath {
    fn new(tag: &str) -> Self {
        let mut path = std::env::temp_dir();
        path.push(format!("blitzy-gz-memory-{}-{tag}", process::id()));
        let _ = fs::remove_file(&path);
        Self(path)
    }

    fn as_cstring(&self) -> CString {
        CString::new(self.0.to_str().expect("a temp path must be UTF-8")).unwrap()
    }
}

impl Drop for TempPath {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
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
    let c_idle = C_GZ_STATE + c_path;
    // `gz_init`: `in` is `want << 1` and `out` is `want` (`gzwrite.c` L16 and L26), plus the
    // compressor `deflateInit2` builds (`gzwrite.c` L35-L36).
    let c_write_peak = c_idle + (C_WANT << 1) + C_WANT + C_DEFLATE_STATE + C_DEFLATE_BUFFERS;

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
    let c_read_peak = c_idle + C_WANT + (C_WANT << 1) + C_INFLATE_STATE + C_INFLATE_WINDOW;

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
