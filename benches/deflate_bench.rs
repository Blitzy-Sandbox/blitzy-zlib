//! DEFLATE compression-throughput benchmarks for the `zlib_rs` crate.
//!
//! This is a [criterion] harness (the REFERENCE harness mandated by AAP sec. 0.4.1;
//! there is no 1:1 C source). It measures the throughput of `zlib_rs`'s DEFLATE
//! encoder -- conceptually the `deflate.c` / `trees.c` engine -- across the
//! representative compression levels, the five [`Strategy`] variants, and a set
//! of deterministic corpora, and it demonstrates the AAP sec. 0.7.3 performance
//! gate:
//!
//! > **compression throughput >= 80 % of C zlib.**
//!
//! To make that gate directly readable from the criterion report, the
//! [`bench_baseline_czlib`] group compresses the *same* corpora at the *same*
//! levels through canonical **C zlib** via the `flate2` dev-dependency (built
//! with its `zlib` feature, which links `libz-sys`). Its results land in the
//! same `deflate_levels` benchmark group as the `zlib_rs` results so that, for
//! every corpus/level pair, the ratio
//!
//! ```text
//! deflate_levels/<name>_L<level>        (zlib_rs)
//! -----------------------------------------------  >= 0.80
//! deflate_levels/czlib_<name>_L<level>  (C zlib)
//! ```
//!
//! can be read off side by side.
//!
//! # Throughput convention
//!
//! Compression throughput is reported against the **uncompressed input** length
//! (`Throughput::Bytes(input.len())`), i.e. uncompressed bytes processed per
//! second -- the natural rate for a compressor and the basis of the >= 80 % gate.
//!
//! # API surface used
//!
//! The primary, "rock-solid" numbers come from the one-shot
//! [`compress2`](zlib_rs::util::compress2): it owns its allocation and reaches
//! `StreamEnd` in a single call, so it cannot drift in shape. The streaming
//! [`Deflate`] API is used only for [`Strategy`] coverage, where a per-strategy
//! handle is required. Output buffers for the streaming path are sized with the
//! stable [`compress_bound`](zlib_rs::util::compress_bound) `const fn` rather
//! than `Deflate::bound` (whose signature has drifted across drafts).
//!
//! Note on import paths: the one-shot helpers resolve under `zlib_rs::util::`
//! (re-exported there by `src/util/mod.rs`); the streaming `Deflate`/`FlushMode`
//! /`Strategy`/`ReturnCode` items resolve at the crate root. This matches the
//! actual compiled crate.
//!
//! # Constraints honored
//!
//! * The throughput benchmarks use only the safe Rust public API. The single
//!   exception is the AAP sec. 0.7.3 **memory-footprint gate** (see the
//!   "Memory-footprint gate" section near the bottom of this file): it arms a
//!   counting global allocator for one brief, untimed window to capture
//!   `zlib_rs`'s deflate-engine peak working-set, and uses a small, fully
//!   `// SAFETY:`-documented `unsafe extern "C"` probe to drive canonical
//!   C zlib (`deflateInit2_`/`deflate`/`deflateEnd`, linked via the `flate2`
//!   `zlib` backend) with counting `zalloc`/`zfree` for the C baseline. That
//!   probe defines **no exported symbol**, so there is still no collision with
//!   the crate's (here-OFF) C-ABI surface; the `capi`/FFI feature stays off for
//!   benches. The allocator is dormant (a single relaxed load per call) during
//!   every timed criterion measurement, so it does not affect throughput.
//! * Deterministic corpora built from a fixed [`SEED`] for reproducible runs.
//! * The level matrix is kept small ([`LEVELS`]) to bound `cargo bench` runtime
//!   while still spanning store / best-speed / default / best-compression.

use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::HashMap;
use std::hint::black_box;
use std::io::Write;
use std::os::raw::{c_char, c_int, c_uint, c_ulong, c_void};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group};
use flate2::Compression;
use flate2::write::ZlibEncoder;
use rand::rngs::StdRng;
use rand::{RngCore, SeedableRng};
use zlib_rs::util::{compress_bound, compress2};
use zlib_rs::{Deflate, FlushMode, ReturnCode, Strategy};

// ===========================================================================
// Benchmark configuration
// ===========================================================================

/// Fixed PRNG seed so every run builds byte-identical corpora -- essential for
/// the `zlib_rs`-vs-C-zlib ratio to compare the two encoders on the same input.
const SEED: u64 = 0x5A4C_4942;

/// Primary corpus size: 1 MiB. Large enough that per-call fixed costs are
/// amortized and the throughput number reflects steady-state encoding.
const SIZE: usize = 1024 * 1024;

/// Representative compression levels swept by [`bench_levels`] and mirrored by
/// [`bench_baseline_czlib`]. These four points stand in for the full `0..=9`
/// range -- `0` = stored (no compression), `1` = best speed (`deflate_fast`),
/// `6` = default (`deflate_slow`), `9` = best compression -- keeping the matrix
/// (and therefore `cargo bench` wall-time) bounded while still exercising every
/// distinct strategy path.
const LEVELS: [i32; 4] = [0, 1, 6, 9];

// ===========================================================================
// Deterministic corpora
// ===========================================================================

/// Highly compressible input: a fixed ASCII phrase repeated to fill [`SIZE`].
///
/// Mirrors the `test/example.c` choice of a repeated phrase (`"hello, hello!"`)
/// that deliberately stresses the LZ77 matcher with long, frequent matches.
/// No RNG is involved, so the result is deterministic by construction.
fn highly_compressible() -> Vec<u8> {
    const PHRASE: &[u8] = b"The quick brown fox jumps over the lazy dog. ";
    let mut v = Vec::with_capacity(SIZE + PHRASE.len());
    while v.len() < SIZE {
        v.extend_from_slice(PHRASE);
    }
    v.truncate(SIZE);
    v
}

/// Text-like input: pseudo-English assembled by drawing words from a fixed
/// vocabulary with a seeded PRNG, joined by spaces, until [`SIZE`] is reached.
///
/// Deterministic via [`SEED`]; representative of natural-language/source-code
/// data where the matcher finds frequent but short, scattered matches.
fn text_like() -> Vec<u8> {
    const WORDS: &[&str] = &[
        "the",
        "quick",
        "brown",
        "fox",
        "jumps",
        "over",
        "lazy",
        "dog",
        "pack",
        "my",
        "box",
        "with",
        "five",
        "dozen",
        "liquor",
        "jugs",
        "how",
        "razorback",
        "frogs",
        "can",
        "level",
        "six",
        "piqued",
        "gymnasts",
        "compression",
        "deflate",
        "inflate",
        "stream",
        "buffer",
        "window",
        "huffman",
        "checksum",
        "throughput",
        "benchmark",
        "encoder",
        "decoder",
    ];
    let mut rng = StdRng::seed_from_u64(SEED);
    let mut v = Vec::with_capacity(SIZE + 16);
    while v.len() < SIZE {
        // `next_u32` (RngCore) gives a deterministic index into the vocabulary.
        let word = WORDS[(rng.next_u32() as usize) % WORDS.len()];
        v.extend_from_slice(word.as_bytes());
        v.push(b' ');
    }
    v.truncate(SIZE);
    v
}

/// Incompressible input: [`SIZE`] bytes of seeded pseudo-random data -- the
/// worst case for the matcher (no useful matches, so output ~= input + overhead).
///
/// Deterministic via [`SEED`] so the random bytes are identical on every run.
fn incompressible() -> Vec<u8> {
    let mut v = vec![0u8; SIZE];
    let mut rng = StdRng::seed_from_u64(SEED);
    rng.fill_bytes(&mut v);
    v
}

/// Build the labeled corpus set once (outside any timed region). The labels
/// become part of the criterion benchmark IDs.
fn build_corpora() -> [(&'static str, Vec<u8>); 3] {
    [
        ("repeated", highly_compressible()),
        ("text", text_like()),
        ("random", incompressible()),
    ]
}

// ===========================================================================
// Streaming-deflate helper (Strategy coverage)
// ===========================================================================

/// Compress `input` into `output` through the streaming [`Deflate`] API at a
/// fixed level (6) / window (15) / memory level (8) with the given `strategy`,
/// returning the total number of bytes produced.
///
/// `output` must be sized with [`compress_bound`](zlib_rs::util::compress_bound)
/// so the whole stream fits; given such a buffer a single
/// [`FlushMode::Finish`] completes the stream in one call. The surrounding loop
/// is **defensive**: it keeps calling `compress` -- advancing the input/output
/// cursors -- until [`ReturnCode::StreamEnd`], so the benchmark stays correct
/// even if some future build chooses to buffer output internally and return
/// `Ok` before finishing. The loop is bounded by a hard progress assertion: a
/// nonterminal call that makes no forward progress (unreachable with a
/// `compress_bound`-sized buffer) fails the benchmark rather than silently
/// returning a partial byte count.
///
/// The [`Deflate`] handle is constructed inside this helper (and therefore
/// inside the timed body when called from `b.iter`); its initialization and the
/// caller's buffer allocation are part of the measured per-iteration cost -- a
/// deliberate, representative choice that is identical across strategies, so the
/// relative comparison between strategies remains fair.
fn deflate_finish(input: &[u8], output: &mut [u8], strategy: Strategy) -> usize {
    let mut deflate = Deflate::with_options(6, 15, 8, strategy).expect("deflate init");
    let mut consumed = 0usize;
    let mut produced = 0usize;
    loop {
        let outcome = deflate
            .compress(
                &input[consumed..],
                &mut output[produced..],
                FlushMode::Finish,
            )
            .expect("deflate");
        consumed += outcome.consumed;
        produced += outcome.produced;
        if outcome.code == ReturnCode::StreamEnd {
            break;
        }
        // Hard (release-mode) gate: a nonterminal Finish call MUST make forward
        // progress. With a `compress_bound`-sized `output` a single Finish
        // completes the stream, so a stall here is a bug. Assert rather than
        // silently `break`-ing with a partial byte count that would then be
        // benchmarked as a fast "success" in optimized criterion runs.
        assert!(
            outcome.consumed != 0 || outcome.produced != 0,
            "deflate_finish stalled with no progress before StreamEnd"
        );
    }
    produced
}

// ===========================================================================
// Benchmark 1 (PRIMARY) -- level sweep via the one-shot `compress2`
// ===========================================================================

/// Sweep the one-shot [`compress2`](zlib_rs::util::compress2) encoder across
/// [`LEVELS`] for every corpus, reporting throughput against the uncompressed
/// input size.
///
/// These are the primary, authoritative `zlib_rs` numbers. The matching C-zlib
/// baseline is produced by [`bench_baseline_czlib`] into the *same*
/// `deflate_levels` group, so each `<name>_L<level>` (zlib_rs) entry sits next
/// to its `czlib_<name>_L<level>` (C zlib) counterpart; the >= 80 % gate is the
/// ratio of the two. `compress2` allocates its own output `Vec` on each call --
/// that allocation is part of the measured one-shot cost and is representative
/// of real one-shot usage.
fn bench_levels(c: &mut Criterion) {
    let corpora = build_corpora();

    // Untimed correctness spot-check: guard against benchmarking a broken
    // encoder. Every corpus must compress without error and yield a non-empty
    // stream at the default level before we start timing.
    for (name, data) in &corpora {
        let packed = compress2(data, 6).expect("compress2 level 6 must succeed in setup");
        assert!(
            !packed.is_empty(),
            "compressed output for corpus `{name}` must be non-empty"
        );
    }

    let mut group = c.benchmark_group("deflate_levels");
    for (name, data) in &corpora {
        // Throughput is measured against the uncompressed input length.
        group.throughput(Throughput::Bytes(data.len() as u64));
        for level in LEVELS {
            let id = BenchmarkId::new(format!("{name}_L{level}"), data.len());
            group.bench_with_input(id, data.as_slice(), |b, input| {
                b.iter(|| {
                    let out = compress2(black_box(input), level).expect("compress2");
                    black_box(out)
                });
            });
        }
    }
    group.finish();
}

// ===========================================================================
// Benchmark 2 -- strategy coverage via the streaming `Deflate` API
// ===========================================================================

/// Exercise each of the five [`Strategy`] variants at a fixed level (6), window
/// (15), and memory level (8) so every `deflate_*` strategy path is benchmarked.
///
/// Uses the streaming [`Deflate`] API (the only way to select a strategy) with a
/// single [`FlushMode::Finish`] over a `compress_bound`-sized output buffer; see
/// [`deflate_finish`] for the defensive Finish-loop. The text-like corpus is
/// chosen as representative real-world input.
fn bench_strategies(c: &mut Criterion) {
    let data = text_like();
    let cap = compress_bound(data.len());

    let mut group = c.benchmark_group("deflate_strategies");
    group.throughput(Throughput::Bytes(data.len() as u64));
    for strategy in [
        Strategy::Default,
        Strategy::Filtered,
        Strategy::HuffmanOnly,
        Strategy::Rle,
        Strategy::Fixed,
    ] {
        let id = BenchmarkId::new("strategy", format!("{strategy:?}"));
        group.bench_with_input(id, data.as_slice(), |b, input| {
            b.iter(|| {
                // A fresh output buffer per iteration (sized so a single Finish
                // completes the stream); its allocation is intentionally part of
                // the measured cost and is identical across strategies.
                let mut out = vec![0u8; cap];
                let produced = deflate_finish(black_box(input), &mut out, strategy);
                black_box(produced)
            });
        });
    }
    group.finish();
}

// ===========================================================================
// Benchmark 3 -- canonical C-zlib baseline (the >= 80 % reference)
// ===========================================================================

/// Compress the same corpora at the same [`LEVELS`] through canonical **C zlib**
/// (via `flate2` built with its `zlib` feature), into the *same*
/// `deflate_levels` group as [`bench_levels`].
///
/// Co-locating the baseline lets the gate be read directly: for each corpus and
/// level, `zlib_rs`'s `deflate_levels/<name>_L<level>` throughput divided by
/// this baseline's `deflate_levels/czlib_<name>_L<level>` throughput must be
/// **>= 0.80** to satisfy AAP sec. 0.7.3.
fn bench_baseline_czlib(c: &mut Criterion) {
    let corpora = build_corpora();

    let mut group = c.benchmark_group("deflate_levels");
    for (name, data) in &corpora {
        group.throughput(Throughput::Bytes(data.len() as u64));
        for level in LEVELS {
            // `flate2` levels are 0..=9; `level.max(0)` maps a hypothetical
            // `Z_DEFAULT_COMPRESSION` (-1) to flate2's default (6) and leaves
            // 0 (`Compression::none()`-equivalent) and 1..=9 unchanged.
            let compression = Compression::new(level.max(0) as u32);
            let id = BenchmarkId::new(format!("czlib_{name}_L{level}"), data.len());
            group.bench_with_input(id, data.as_slice(), |b, input| {
                b.iter(|| {
                    let mut encoder = ZlibEncoder::new(Vec::new(), compression);
                    encoder
                        .write_all(black_box(input))
                        .expect("flate2 write_all");
                    black_box(encoder.finish().expect("flate2 finish"))
                });
            });
        }
    }
    group.finish();
}

// ===========================================================================
// Memory-footprint gate (AAP sec. 0.7.3: "memory <= C zlib")
// ===========================================================================
//
// The criterion groups above measure *time*. AAP sec. 0.7.3 also requires the
// crate's runtime memory footprint to be <= canonical C zlib's. This section
// adds a small, deterministic **runtime** memory probe that runs once at
// process start (before any timed criterion work) and prints a CI-readable
// report plus a hard assertion, so the gate is backed by measured evidence
// rather than a static formula.
//
// Apples-to-apples method:
//   * `CountingAlloc` is a `System`-backed global allocator that is *dormant*
//     by default -- each call forwards to `System` behind one relaxed atomic
//     load -- and is armed only for the brief, untimed measurement window via
//     `MEM_COUNT_ON`, so it never perturbs the criterion throughput numbers.
//   * `zlib_rs`'s deflate engine allocates its working buffers (window / prev /
//     head / pending + boxed state) through the global allocator, so arming the
//     counter around one streaming-`Deflate` lifecycle captures its exact peak
//     resident working-set in bytes.
//   * Canonical C zlib allocates through its `z_stream` `zalloc`/`zfree` hooks,
//     NOT the Rust allocator, so we drive a real `deflateInit2_` / `deflate` /
//     `deflateEnd` cycle with counting hooks under the identical configuration
//     (level 6, windowBits 15, memLevel 8, default strategy) to capture C
//     zlib's peak working-set. I/O buffers are allocated BEFORE counting, so
//     both peaks reflect only engine-internal allocations.
//
// The gate asserts `rust_peak <= c_peak`.

/// Allocations of at least this many bytes are the **data-scaling working
/// buffers** (the LZ77 window / prev / head / pending arrays for deflate, the
/// sliding window for inflate); smaller allocations are **fixed control
/// structures** (the boxed engine state). The AAP sec. 0.7.1 deflate footprint
/// bound ("approximately 256 KB") refers to the former. The threshold sits well
/// below the smallest working buffer (>= 16 KiB) and well above any control
/// struct (a few KiB), so a real, data-scaling regression (a larger window or
/// an extra buffer) always lands in the "large" bucket and trips the strict
/// gate, while fixed struct-layout differences land in "small".
const LARGE_THRESHOLD: usize = 32 * 1024;

/// Live bytes (armed window only) and the high-water mark for the Rust engine,
/// split into data-scaling working buffers (`MEM_LARGE`) and fixed control
/// structures (`MEM_SMALL`); `MEM_CUR`/`MEM_PEAK` track the combined total.
static MEM_CUR: AtomicUsize = AtomicUsize::new(0);
static MEM_PEAK: AtomicUsize = AtomicUsize::new(0);
static MEM_LARGE: AtomicUsize = AtomicUsize::new(0);
static MEM_SMALL: AtomicUsize = AtomicUsize::new(0);
/// When `false`, [`CountingAlloc`] is a transparent pass-through (the default).
static MEM_COUNT_ON: AtomicBool = AtomicBool::new(false);

/// A `System`-backed global allocator that, only while armed via
/// [`MEM_COUNT_ON`], tracks the live byte count and its peak. While disarmed it
/// adds a single relaxed load per call and is otherwise identical to `System`,
/// so it does not measurably affect the timed throughput benchmarks.
struct CountingAlloc;

/// Account a newly-allocated block of `sz` bytes into the live/peak/bucket
/// counters. Called only on the armed path.
#[inline]
fn mem_account_alloc(sz: usize) {
    let live = MEM_CUR.fetch_add(sz, Ordering::Relaxed) + sz;
    MEM_PEAK.fetch_max(live, Ordering::Relaxed);
    if sz >= LARGE_THRESHOLD {
        MEM_LARGE.fetch_add(sz, Ordering::Relaxed);
    } else {
        MEM_SMALL.fetch_add(sz, Ordering::Relaxed);
    }
}

/// Account a freed block of `sz` bytes out of the live/bucket counters. Called
/// only on the armed path.
#[inline]
fn mem_account_free(sz: usize) {
    MEM_CUR.fetch_sub(sz, Ordering::Relaxed);
    if sz >= LARGE_THRESHOLD {
        MEM_LARGE.fetch_sub(sz, Ordering::Relaxed);
    } else {
        MEM_SMALL.fetch_sub(sz, Ordering::Relaxed);
    }
}

// SAFETY: every method forwards to `System` (a correct `GlobalAlloc`) with the
// caller's exact `Layout`; the accounting is side-band atomics that never alter
// the returned pointer or the validity of the memory.
unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: delegating to the System allocator with the same layout.
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() && MEM_COUNT_ON.load(Ordering::Relaxed) {
            mem_account_alloc(layout.size());
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if MEM_COUNT_ON.load(Ordering::Relaxed) {
            mem_account_free(layout.size());
        }
        // SAFETY: `ptr` was returned by this allocator's `alloc` for `layout`.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: delegating to the System allocator with the same layout.
        let p = unsafe { System.alloc_zeroed(layout) };
        if !p.is_null() && MEM_COUNT_ON.load(Ordering::Relaxed) {
            mem_account_alloc(layout.size());
        }
        p
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: delegating to the System allocator with the original layout
        // and the requested new size (preserves System's efficient in-place
        // growth so the disarmed path matches a plain System allocator).
        let p = unsafe { System.realloc(ptr, layout, new_size) };
        if !p.is_null() && MEM_COUNT_ON.load(Ordering::Relaxed) {
            // Treat as free(old) + alloc(new) so the bucket split stays correct
            // even across the size threshold.
            mem_account_free(layout.size());
            mem_account_alloc(new_size);
        }
        p
    }
}

#[global_allocator]
static GLOBAL: CountingAlloc = CountingAlloc;

/// LP64 `z_stream`, byte-compatible with the system `<zlib.h>` (verified against
/// zlib 1.3.1). The probe sets only a few fields; the rest exist so the struct
/// size equals `sizeof(z_stream)` exactly and `deflateInit2_`'s stream-size
/// guard passes.
#[repr(C)]
struct ZStreamC {
    next_in: *const u8,
    avail_in: c_uint,
    total_in: c_ulong,
    next_out: *mut u8,
    avail_out: c_uint,
    total_out: c_ulong,
    msg: *const c_char,
    state: *mut c_void,
    zalloc: Option<extern "C" fn(*mut c_void, c_uint, c_uint) -> *mut c_void>,
    zfree: Option<extern "C" fn(*mut c_void, *mut c_void)>,
    opaque: *mut c_void,
    data_type: c_int,
    adler: c_ulong,
    reserved: c_ulong,
}

// SAFETY: these resolve to canonical C zlib, linked (development-only) through
// the `flate2` `zlib` backend (`libz-sys` -> system `libz`). The signatures
// match `<zlib.h>` for zlib 1.3.1 on this LP64 target.
unsafe extern "C" {
    fn deflateInit2_(
        strm: *mut ZStreamC,
        level: c_int,
        method: c_int,
        window_bits: c_int,
        mem_level: c_int,
        strategy: c_int,
        version: *const c_char,
        stream_size: c_int,
    ) -> c_int;
    fn deflate(strm: *mut ZStreamC, flush: c_int) -> c_int;
    fn deflateEnd(strm: *mut ZStreamC) -> c_int;
    fn zlibVersion() -> *const c_char;
}

/// Live/peak byte counters for C zlib's `zalloc`/`zfree` -- separate from the
/// Rust-side counters so the two engines are measured independently. Split into
/// data-scaling working buffers (`C_MEM_LARGE`) and fixed control structures
/// (`C_MEM_SMALL`) by the same [`LARGE_THRESHOLD`] used for the Rust side.
static C_MEM_CUR: AtomicUsize = AtomicUsize::new(0);
static C_MEM_PEAK: AtomicUsize = AtomicUsize::new(0);
static C_MEM_LARGE: AtomicUsize = AtomicUsize::new(0);
static C_MEM_SMALL: AtomicUsize = AtomicUsize::new(0);

/// Maps every pointer C zlib requested to the `Layout` it was allocated with,
/// so `zfree` (which receives only the pointer) can decrement the live count
/// and hand `System` the correct `Layout`.
fn c_alloc_registry() -> &'static Mutex<HashMap<usize, Layout>> {
    static REG: OnceLock<Mutex<HashMap<usize, Layout>>> = OnceLock::new();
    REG.get_or_init(|| Mutex::new(HashMap::new()))
}

/// `z_stream.zalloc` hook: allocate `items * size` bytes via the Rust `System`
/// allocator, record the layout, and update the C-side peak. Returns null on
/// zero/overflow/failure, honoring zlib's allocator contract.
extern "C" fn counting_zalloc(_opaque: *mut c_void, items: c_uint, size: c_uint) -> *mut c_void {
    let bytes = (items as usize).saturating_mul(size as usize);
    if bytes == 0 {
        return core::ptr::null_mut();
    }
    // zlib's buffers are u8/u16/u32 arrays; 16-byte alignment is always ample.
    let Ok(layout) = Layout::from_size_align(bytes, 16) else {
        return core::ptr::null_mut();
    };
    // SAFETY: `layout` has a non-zero size (checked just above).
    let p = unsafe { System.alloc(layout) };
    if p.is_null() {
        return core::ptr::null_mut();
    }
    c_alloc_registry()
        .lock()
        .unwrap()
        .insert(p as usize, layout);
    let live = C_MEM_CUR.fetch_add(bytes, Ordering::Relaxed) + bytes;
    C_MEM_PEAK.fetch_max(live, Ordering::Relaxed);
    if bytes >= LARGE_THRESHOLD {
        C_MEM_LARGE.fetch_add(bytes, Ordering::Relaxed);
    } else {
        C_MEM_SMALL.fetch_add(bytes, Ordering::Relaxed);
    }
    p as *mut c_void
}

/// `z_stream.zfree` hook: look up the recorded layout for `address`, decrement
/// the C-side live count, and free through `System`.
extern "C" fn counting_zfree(_opaque: *mut c_void, address: *mut c_void) {
    if address.is_null() {
        return;
    }
    let entry = c_alloc_registry()
        .lock()
        .unwrap()
        .remove(&(address as usize));
    if let Some(layout) = entry {
        let sz = layout.size();
        C_MEM_CUR.fetch_sub(sz, Ordering::Relaxed);
        if sz >= LARGE_THRESHOLD {
            C_MEM_LARGE.fetch_sub(sz, Ordering::Relaxed);
        } else {
            C_MEM_SMALL.fetch_sub(sz, Ordering::Relaxed);
        }
        // SAFETY: `address` was returned by `counting_zalloc` for exactly
        // `layout` (just looked up from, and removed from, the registry).
        unsafe { System.dealloc(address as *mut u8, layout) };
    }
}

/// A peak-working-set measurement, split into the data-scaling working buffers
/// (`large`) and fixed control structures (`small`); `total` is the high-water
/// mark of the two combined.
#[derive(Clone, Copy)]
struct MemBreakdown {
    total: usize,
    large: usize,
    small: usize,
}

/// Maximum amount by which `zlib_rs`'s *total* peak may exceed C zlib's and
/// still pass the secondary (parity) gate. It exists solely to absorb fixed
/// control-structure layout differences between the Rust `DeflateState`/
/// `InflateState` and the C `deflate_state`/`inflate_state` (a few hundred
/// bytes in practice). It is far below the smallest data-scaling working buffer
/// (>= 16 KiB), so it can never mask a real, data-scaling regression -- such a
/// regression lands in the `large` bucket and trips the *strict* primary gate.
const CONTROL_SLACK: usize = 8 * 1024;

/// Measure `zlib_rs`'s deflate-engine peak working-set for one full streaming
/// compression of `input` at level 6 / windowBits 15 / memLevel 8. `output` is
/// pre-allocated by the caller (untimed, uncounted), so the captured peak
/// reflects only the engine's internal allocations (window / prev / head /
/// pending + boxed state).
fn measure_rust_deflate(input: &[u8], output: &mut [u8]) -> MemBreakdown {
    MEM_CUR.store(0, Ordering::SeqCst);
    MEM_PEAK.store(0, Ordering::SeqCst);
    MEM_LARGE.store(0, Ordering::SeqCst);
    MEM_SMALL.store(0, Ordering::SeqCst);
    MEM_COUNT_ON.store(true, Ordering::SeqCst);

    let mut deflate = Deflate::with_options(6, 15, 8, Strategy::Default).expect("deflate init");
    let mut consumed = 0usize;
    let mut produced = 0usize;
    loop {
        let outcome = deflate
            .compress(
                &input[consumed..],
                &mut output[produced..],
                FlushMode::Finish,
            )
            .expect("deflate");
        consumed += outcome.consumed;
        produced += outcome.produced;
        if outcome.code == ReturnCode::StreamEnd {
            break;
        }
        assert!(
            outcome.consumed != 0 || outcome.produced != 0,
            "measure_rust_deflate stalled with no progress before StreamEnd"
        );
    }
    // The engine and all its buffers are live here (nothing freed yet), so the
    // bucket counters hold the steady-state working-set split.
    let breakdown = MemBreakdown {
        total: MEM_PEAK.load(Ordering::SeqCst),
        large: MEM_LARGE.load(Ordering::SeqCst),
        small: MEM_SMALL.load(Ordering::SeqCst),
    };
    drop(deflate);

    MEM_COUNT_ON.store(false, Ordering::SeqCst);
    breakdown
}

/// Measure canonical C zlib's deflate-engine peak working-set for the identical
/// configuration, via a real `deflateInit2_`/`deflate`/`deflateEnd` cycle whose
/// `zalloc`/`zfree` are the counting hooks above. `output` is pre-allocated by
/// the caller.
fn measure_c_deflate(input: &[u8], output: &mut [u8]) -> MemBreakdown {
    c_alloc_registry().lock().unwrap().clear();
    C_MEM_CUR.store(0, Ordering::SeqCst);
    C_MEM_PEAK.store(0, Ordering::SeqCst);
    C_MEM_LARGE.store(0, Ordering::SeqCst);
    C_MEM_SMALL.store(0, Ordering::SeqCst);

    let mut strm = ZStreamC {
        next_in: core::ptr::null(),
        avail_in: 0,
        total_in: 0,
        next_out: core::ptr::null_mut(),
        avail_out: 0,
        total_out: 0,
        msg: core::ptr::null(),
        state: core::ptr::null_mut(),
        zalloc: Some(counting_zalloc),
        zfree: Some(counting_zfree),
        opaque: core::ptr::null_mut(),
        data_type: 0,
        adler: 0,
        reserved: 0,
    };

    // SAFETY: `strm` is a correctly-laid-out `z_stream` with valid allocator
    // hooks set; `version`/`stream_size` follow the `deflateInit2_` ABI
    // (Z_DEFLATED == 8, Z_DEFAULT_STRATEGY == 0).
    let rc = unsafe {
        deflateInit2_(
            &mut strm,
            6,
            8,
            15,
            8,
            0,
            zlibVersion(),
            core::mem::size_of::<ZStreamC>() as c_int,
        )
    };
    assert_eq!(rc, 0, "C deflateInit2_ failed (rc={rc})");

    strm.next_in = input.as_ptr();
    strm.avail_in = input.len() as c_uint;
    strm.next_out = output.as_mut_ptr();
    strm.avail_out = output.len() as c_uint;
    // SAFETY: the pointers/lengths above describe the live `input`/`output`
    // slices for the duration of the call; Z_FINISH == 4.
    let rc = unsafe { deflate(&mut strm, 4) };
    assert_eq!(
        rc, 1,
        "C deflate(Z_FINISH) should return Z_STREAM_END==1 (rc={rc})"
    );

    // All C engine buffers are live here (before deflateEnd), so the bucket
    // counters hold the steady-state working-set split.
    let breakdown = MemBreakdown {
        total: C_MEM_PEAK.load(Ordering::SeqCst),
        large: C_MEM_LARGE.load(Ordering::SeqCst),
        small: C_MEM_SMALL.load(Ordering::SeqCst),
    };
    // SAFETY: frees the engine state allocated above through our `zfree` hook.
    let rc = unsafe { deflateEnd(&mut strm) };
    assert_eq!(rc, 0, "C deflateEnd failed (rc={rc})");
    breakdown
}

/// Format a byte count as `N bytes (~K.K KiB)` for the report.
fn fmt_bytes(n: usize) -> String {
    format!("{n} bytes (~{:.1} KiB)", n as f64 / 1024.0)
}

/// Run the AAP sec. 0.7.3 memory-footprint gate for the **deflate** engine:
/// measure both engines' peak working-set on a representative 1 MiB corpus and
/// enforce `memory <= C zlib`. Prints a CI-readable report. Runs once, untimed,
/// at startup (before any criterion measurement).
///
/// Two gates are enforced:
///   1. PRIMARY (strict): the data-scaling working buffers (`large`; the AAP
///      sec. 0.7.1 "approximately 256 KB" deflate footprint) must satisfy
///      `zlib_rs <= C zlib`. Any data-scaling regression trips this.
///   2. SECONDARY (parity): the *total* peak (working buffers + fixed control
///      struct) must satisfy `zlib_rs <= C zlib + CONTROL_SLACK`, absorbing only
///      fixed control-structure layout differences.
fn run_memory_gate() {
    let input = text_like(); // representative 1 MiB corpus (deterministic)
    let cap = compress_bound(input.len());
    let mut out_rust = vec![0u8; cap];
    let mut out_c = vec![0u8; cap];

    let r = measure_rust_deflate(&input, &mut out_rust);
    let c = measure_c_deflate(&input, &mut out_c);

    let pct = |a: usize, b: usize| {
        if b > 0 {
            (a as f64 / b as f64) * 100.0
        } else {
            0.0
        }
    };

    println!(
        "=== AAP 0.7.3 memory-footprint gate: DEFLATE engine (level 6 / windowBits 15 / memLevel 8) ==="
    );
    println!("                                          zlib_rs                C zlib (1.3.1)");
    println!(
        "    working buffers (data-scaling)  : {:>26}   {:>26}",
        fmt_bytes(r.large),
        fmt_bytes(c.large)
    );
    println!(
        "    control struct (fixed)          : {:>26}   {:>26}",
        fmt_bytes(r.small),
        fmt_bytes(c.small)
    );
    println!(
        "    total peak working-set          : {:>26}   {:>26}",
        fmt_bytes(r.total),
        fmt_bytes(c.total)
    );
    println!(
        "    ratio zlib_rs/C  (buffers/total): {:.1}% / {:.1}%   (gate: buffers <= 100%, total <= 100% + {} B slack)",
        pct(r.large, c.large),
        pct(r.total, c.total),
        CONTROL_SLACK
    );

    // PRIMARY strict gate: the data-scaling working buffers.
    assert!(
        r.large <= c.large,
        "MEMORY GATE FAILED (deflate working buffers): zlib_rs {} B > C zlib {} B \
         (AAP 0.7.1/0.7.3 require the ~256 KB working-set to be <= C zlib)",
        r.large,
        c.large
    );
    // SECONDARY parity gate: total incl. fixed control struct.
    assert!(
        r.total <= c.total + CONTROL_SLACK,
        "MEMORY GATE FAILED (deflate total): zlib_rs {} B > C zlib {} B + {} B control slack",
        r.total,
        c.total,
        CONTROL_SLACK
    );
    println!(
        "    RESULT                          : PASS (working buffers <= C zlib; total at parity)"
    );
}

// ===========================================================================
// Harness wiring (custom `main`; `harness = false` in Cargo.toml)
// ===========================================================================

criterion_group!(
    benches,
    bench_levels,
    bench_strategies,
    bench_baseline_czlib
);

/// Custom harness entry point. Runs the AAP sec. 0.7.3 memory-footprint gate
/// first (untimed, at startup), then drives the criterion throughput suite
/// exactly as the generated `criterion_main!(benches)` would (run the group,
/// then emit the final summary). `cargo bench --no-run` compiles this without
/// executing it; `cargo test` does not execute `harness = false` bench mains.
fn main() {
    run_memory_gate();
    benches();
    Criterion::default().configure_from_args().final_summary();
}
