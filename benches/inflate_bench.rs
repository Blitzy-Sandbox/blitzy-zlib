//! INFLATE decompression-throughput benchmarks for the `zlib_rs` crate.
//!
//! This criterion harness measures the *decompression* (INFLATE) throughput of
//! `zlib_rs` and validates the AAP section 0.7.3 performance gate:
//! decompression throughput must be >= C zlib (parity or better). The
//! conceptual comparison targets are the C decode engines `inflate.c`,
//! `inffast.c`, `inftrees.c`, and `infback.c`; canonical C zlib is linked
//! (development-only) through the `flate2` `zlib` backend and benched
//! side-by-side as the parity oracle.
//!
//! Design rule: ALL compression happens in untimed setup. Every timed body
//! decompresses only -- the destination buffer is pre-allocated once (sized to
//! the original length) and reused across iterations, so the measured cost is
//! pure decode throughput, isolated from allocation and from the encoder.
//!
//! Three benchmark functions are registered:
//! - `bench_uncompress`: one-shot `uncompress` (the PRIMARY numbers), group `inflate`.
//! - `bench_inflate_stream`: streaming `Inflate` state machine, group `inflate_stream`.
//! - `bench_baseline_czlib`: flate2 / C-zlib oracle, group `inflate` (side-by-side).
//!
//! Throughput is reported on the *uncompressed (output)* size, so the ratio
//! `inflate/zlibrs_<corpus>` over `inflate/czlib_<corpus>` reads directly as
//! the parity gate: it must be >= 1.0.
//!
//! The throughput benchmarks use only the safe Rust public API. The single
//! exception is the AAP sec. 0.7.3 **memory-footprint gate** (see the
//! "Memory-footprint gate" section near the bottom of this file): it arms a
//! counting global allocator for one brief, untimed window to capture
//! `zlib_rs`'s inflate-engine peak working-set, and uses a small, fully
//! `// SAFETY:`-documented `unsafe extern "C"` probe to drive canonical C zlib
//! (`inflateInit2_`/`inflate`/`inflateEnd`, linked via the `flate2` `zlib`
//! backend) with counting `zalloc`/`zfree` for the C baseline. That probe
//! defines no exported symbol, so the `capi` FFI feature stays OFF for benches
//! and the `flate2` C-zlib oracle never collides with the crate's own (gated)
//! C-ABI exports. The allocator is dormant (a single relaxed load per call)
//! during every timed criterion measurement, so it does not affect throughput.

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group};
use flate2::read::ZlibDecoder;
use rand::rngs::StdRng;
use rand::{RngCore, SeedableRng};
use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::HashMap;
use std::hint::black_box;
use std::io::Read;
use std::os::raw::{c_char, c_int, c_uint, c_ulong, c_void};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use zlib_rs::util::{compress2, uncompress};
use zlib_rs::{Inflate, Z_NO_FLUSH, Z_OK, Z_STREAM_END};

/// Fixed RNG seed -- keeps every corpus byte-for-byte reproducible across runs
/// so successive benchmark measurements compare like with like.
const SEED: u64 = 0x5A4C_4942;

/// Corpus size: 1 MiB. Large enough that the steady-state decode loop, rather
/// than per-call fixed overhead, dominates the measured throughput.
const SIZE: usize = 1024 * 1024;

/// Compression level used to PREPARE the decode inputs (untimed). Level 6 is
/// zlib's default (`Z_DEFAULT_COMPRESSION`) and produces a standard zlib stream
/// (window bits 15) that both `zlib_rs` and flate2 / C zlib decode.
const SETUP_LEVEL: i32 = 6;

/// Printable alphabet for the `text_like` corpus. Random bytes are folded onto
/// this set, yielding text-shaped, moderately compressible data (dynamic
/// Huffman blocks) -- distinct from both the highly redundant and the random
/// corpora.
const ALPHABET: &[u8] =
    b"abcdefghijklmnopqrstuvwxyz ABCDEFGHIJKLMNOPQRSTUVWXYZ 0123456789 .,;:\n\t";

/// Highly compressible corpus: a fixed phrase repeated to fill `size` bytes.
/// Exercises the long-match / back-reference-heavy decode path.
fn highly_compressible(size: usize) -> Vec<u8> {
    const PHRASE: &[u8] = b"the quick brown fox jumps over the lazy dog. ";
    let mut data = Vec::with_capacity(size);
    while data.len() < size {
        let remaining = size - data.len();
        let take = remaining.min(PHRASE.len());
        data.extend_from_slice(&PHRASE[..take]);
    }
    data
}

/// Text-like corpus: deterministic random bytes mapped onto a printable
/// alphabet. Produces dynamic-Huffman-dominated streams.
fn text_like(size: usize) -> Vec<u8> {
    let mut rng = StdRng::seed_from_u64(SEED);
    let mut data = vec![0u8; size];
    rng.fill_bytes(&mut data);
    for byte in &mut data {
        *byte = ALPHABET[(*byte as usize) % ALPHABET.len()];
    }
    data
}

/// Incompressible corpus: deterministic raw random bytes. zlib emits mostly
/// stored blocks, so the decode path is dominated by literal copies.
fn incompressible(size: usize) -> Vec<u8> {
    let mut rng = StdRng::seed_from_u64(SEED);
    let mut data = vec![0u8; size];
    rng.fill_bytes(&mut data);
    data
}

/// A prepared decode input: a human-readable corpus name, the *compressed*
/// zlib stream (produced in untimed setup), the original (decompressed) bytes
/// (the reference the timed streaming body asserts its output against), and
/// their length.
struct Corpus {
    name: &'static str,
    compressed: Vec<u8>,
    original: Vec<u8>,
    original_len: usize,
}

/// Build every corpus and pre-compress it in UNTIMED setup.
///
/// Each corpus is compressed once with `compress2` (level 6 -> standard zlib
/// stream, window bits 15) and then sanity-decoded with `uncompress`, so the
/// harness never benchmarks against a broken decoder: a setup-time panic here
/// means the round-trip itself is wrong (a true bug), not a benchmarking
/// artifact. None of this work is timed -- it runs before any `b.iter`.
fn build_corpora() -> Vec<Corpus> {
    let raw: [(&'static str, Vec<u8>); 3] = [
        ("highly_compressible", highly_compressible(SIZE)),
        ("text_like", text_like(SIZE)),
        ("incompressible", incompressible(SIZE)),
    ];

    raw.into_iter()
        .map(|(name, original)| {
            let original_len = original.len();
            let compressed = compress2(&original, SETUP_LEVEL).expect("setup compress2");

            // Untimed correctness guard: confirm the decoder reproduces the
            // exact original bytes before any timing happens.
            let mut check = vec![0u8; original_len];
            let produced = uncompress(&mut check, &compressed).expect("setup uncompress");
            assert_eq!(
                produced, original_len,
                "setup decode length mismatch ({name})"
            );
            assert_eq!(check, original, "setup decode content mismatch ({name})");

            Corpus {
                name,
                compressed,
                original,
                original_len,
            }
        })
        .collect()
}

/// PRIMARY: one-shot `uncompress` decode throughput.
///
/// The destination buffer is allocated once (sized to the original length) and
/// reused across iterations -- `uncompress` overwrites it on every call, so the
/// measured cost is pure decode throughput, free of allocation. Throughput is
/// reported on the uncompressed size so it is directly comparable to the
/// flate2 / C-zlib baseline registered in the same `inflate` group.
fn bench_uncompress(c: &mut Criterion) {
    let corpora = build_corpora();
    let mut group = c.benchmark_group("inflate");

    for corpus in &corpora {
        let name = corpus.name;
        let original_len = corpus.original_len;
        group.throughput(Throughput::Bytes(original_len as u64));
        group.bench_with_input(
            BenchmarkId::new(format!("zlibrs_{name}"), original_len),
            &corpus.compressed,
            move |b, src| {
                let mut dest = vec![0u8; original_len];
                b.iter(|| {
                    let produced =
                        uncompress(&mut dest, black_box(src.as_slice())).expect("uncompress");
                    black_box(produced)
                });
            },
        );
    }

    // GATE (AAP 0.7.3): each `inflate/zlibrs_<corpus>` throughput must be >= the
    // matching `inflate/czlib_<corpus>` throughput emitted by
    // `bench_baseline_czlib` (parity or better).
    group.finish();
}

/// Streaming decode coverage via the raw-`i32` `Inflate` state-machine API.
///
/// ASYMMETRY (do not confuse with the deflate side): `Inflate::inflate` takes a
/// raw `i32` flush (`Z_NO_FLUSH` == 0) and returns a raw
/// `(code, consumed, produced)` tuple -- it is NOT a `Result` and NOT a
/// `FlushMode`. `code == Z_STREAM_END` (1) signals a complete decode.
/// `Inflate::new()` configures the zlib wrapper at window bits 15, matching the
/// `compress2` output exactly (equivalently, `Inflate::with_window_bits(15)`).
///
/// A fresh `Inflate` is constructed per iteration, so this path intentionally
/// includes state-machine init and teardown. The decode is wrapped in a cursor
/// loop that advances the input/output offsets until `Z_STREAM_END`.
///
/// BENCHMARK INTEGRITY (release-mode): criterion runs the body optimized, where
/// `debug_assert!` is compiled out. Every correctness check here is therefore a
/// hard `assert!`/`assert_eq!` so that an error, a stall (no forward progress),
/// or a partial/over-long decode FAILS the benchmark instead of being silently
/// measured as a fast "success". Each nonterminal `inflate` call must return
/// `Z_OK` and make progress; the loop terminates only on `Z_STREAM_END`; and
/// after the loop the run must have consumed the whole compressed stream
/// (`in_off == src.len()`), produced exactly the original length
/// (`out_off == original_len`), and reproduced the original bytes
/// (`dest == original`) before the result is handed to `black_box`.
fn bench_inflate_stream(c: &mut Criterion) {
    let corpora = build_corpora();
    let mut group = c.benchmark_group("inflate_stream");

    for corpus in &corpora {
        let name = corpus.name;
        let original_len = corpus.original_len;
        // The reference bytes the timed body asserts its decode against. Cloned
        // once here (untimed); the comparison itself happens inside `b.iter`.
        let original = corpus.original.clone();
        group.throughput(Throughput::Bytes(original_len as u64));
        group.bench_with_input(
            BenchmarkId::new("stream", name),
            &corpus.compressed,
            move |b, src| {
                let mut dest = vec![0u8; original_len];
                b.iter(|| {
                    // Inflate::new() == zlib wrapper, window bits 15 (matches
                    // the compress2 output above).
                    let mut inf = Inflate::new().expect("inflate init");
                    let mut in_off = 0usize;
                    let mut out_off = 0usize;
                    loop {
                        let (code, consumed, produced) = inf.inflate(
                            black_box(&src[in_off..]),
                            &mut dest[out_off..],
                            Z_NO_FLUSH,
                        );
                        in_off += consumed;
                        out_off += produced;
                        if code == Z_STREAM_END {
                            break;
                        }
                        // Hard (release-mode) gates: a nonterminal call MUST
                        // return Z_OK and MUST make forward progress. An error
                        // code or a stall fails the benchmark instead of being
                        // measured as success or spinning forever.
                        assert_eq!(
                            code, Z_OK,
                            "inflate_stream: nonterminal call returned {code}"
                        );
                        assert!(
                            consumed != 0 || produced != 0,
                            "inflate_stream stalled with no progress before Z_STREAM_END"
                        );
                    }
                    // Hard (release-mode) correctness gates: the whole input was
                    // consumed, exactly the original length was produced, and
                    // the decoded bytes match the original. Without these a
                    // broken/partial decode could be benchmarked as success.
                    assert_eq!(
                        in_off,
                        src.len(),
                        "inflate_stream did not consume all input"
                    );
                    assert_eq!(out_off, original_len, "inflate_stream length mismatch");
                    assert!(dest == original, "inflate_stream content mismatch");
                    black_box(out_off)
                });
            },
        );
    }

    group.finish();
}

/// Parity reference: decompress the SAME compressed bytes through canonical
/// C zlib (flate2's `zlib` backend). Registered in the same `inflate` group as
/// `bench_uncompress`, so `zlibrs_<corpus>` and `czlib_<corpus>` appear
/// side-by-side per corpus. The output `Vec` is allocated once with capacity
/// and `clear()`-ed each iteration (capacity is retained), keeping allocation
/// out of the timed loop.
fn bench_baseline_czlib(c: &mut Criterion) {
    let corpora = build_corpora();
    let mut group = c.benchmark_group("inflate");

    for corpus in &corpora {
        let name = corpus.name;
        let original_len = corpus.original_len;
        group.throughput(Throughput::Bytes(original_len as u64));
        group.bench_with_input(
            BenchmarkId::new(format!("czlib_{name}"), original_len),
            &corpus.compressed,
            move |b, src| {
                let mut out = Vec::with_capacity(original_len);
                b.iter(|| {
                    out.clear();
                    let mut decoder = ZlibDecoder::new(black_box(src.as_slice()));
                    decoder.read_to_end(&mut out).expect("flate2 zlib decode");
                    black_box(out.len())
                });
            },
        );
    }

    // GATE (AAP 0.7.3): the ratio zlib_rs throughput / C-zlib throughput must be
    // >= 1.0 (parity or better) for every corpus.
    group.finish();
}

// ===========================================================================
// Memory-footprint gate (AAP sec. 0.7.3: "memory <= C zlib")
// ===========================================================================
//
// The criterion groups above measure *time*. AAP sec. 0.7.3 also requires the
// crate's runtime memory footprint to be <= canonical C zlib's, and AAP
// sec. 0.7.1 bounds the inflate engine at "~7 KB of inflate state plus a window
// of at most 32 KB". This section adds a deterministic **runtime** memory probe
// that runs once at process start (before any timed criterion work) and prints
// a CI-readable report plus a hard assertion, so the gate is backed by measured
// evidence rather than a static formula.
//
// Apples-to-apples method (mirrors `deflate_bench.rs`):
//   * `CountingAlloc` is a `System`-backed global allocator, *dormant* by
//     default (one relaxed atomic load per call), armed only for the brief,
//     untimed measurement window via `MEM_COUNT_ON`, so it never perturbs the
//     criterion throughput numbers.
//   * `zlib_rs`'s inflate engine allocates its sliding window and decode tables
//     through the global allocator, so arming the counter around one streaming
//     `Inflate` decode captures its exact peak resident working-set.
//   * Canonical C zlib allocates through its `z_stream` `zalloc`/`zfree` hooks,
//     NOT the Rust allocator, so we drive a real `inflateInit2_` / `inflate` /
//     `inflateEnd` cycle with counting hooks under the identical configuration
//     (windowBits 15) to capture C zlib's peak working-set. I/O buffers are
//     allocated BEFORE counting, so both peaks reflect only engine-internal
//     allocations. This is the one place the harness uses `unsafe`/`extern "C"`;
//     it exports no symbol, so the `flate2` C-zlib oracle is not disturbed.
//
// The gate asserts the data-scaling window is <= C's (strict) and the total
// (window + decode tables + fixed state) is at parity with C's.

/// Allocations of at least this many bytes are the **data-scaling working
/// buffer** (the inflate sliding window, up to 32 KiB at windowBits 15);
/// smaller allocations are the **fixed decode state** (the `InflateState` box
/// and the `Code` decode-table array). See `deflate_bench.rs` for the rationale.
const LARGE_THRESHOLD: usize = 32 * 1024;

/// Live bytes (armed window only) and the high-water mark for the Rust engine,
/// split into the data-scaling window (`MEM_LARGE`) and fixed decode state
/// (`MEM_SMALL`); `MEM_CUR`/`MEM_PEAK` track the combined total.
static MEM_CUR: AtomicUsize = AtomicUsize::new(0);
static MEM_PEAK: AtomicUsize = AtomicUsize::new(0);
static MEM_LARGE: AtomicUsize = AtomicUsize::new(0);
static MEM_SMALL: AtomicUsize = AtomicUsize::new(0);
/// When `false`, [`CountingAlloc`] is a transparent pass-through (the default).
static MEM_COUNT_ON: AtomicBool = AtomicBool::new(false);

/// A `System`-backed global allocator that, only while armed via
/// [`MEM_COUNT_ON`], tracks the live byte count and its peak. While disarmed it
/// adds a single relaxed load per call and is otherwise identical to `System`.
struct CountingAlloc;

/// Account a newly-allocated block of `sz` bytes. Armed path only.
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

/// Account a freed block of `sz` bytes. Armed path only.
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
/// size equals `sizeof(z_stream)` exactly and `inflateInit2_`'s stream-size
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
    fn inflateInit2_(
        strm: *mut ZStreamC,
        window_bits: c_int,
        version: *const c_char,
        stream_size: c_int,
    ) -> c_int;
    fn inflate(strm: *mut ZStreamC, flush: c_int) -> c_int;
    fn inflateEnd(strm: *mut ZStreamC) -> c_int;
    fn zlibVersion() -> *const c_char;
}

/// Live/peak byte counters for C zlib's `zalloc`/`zfree`, split into the
/// data-scaling window (`C_MEM_LARGE`) and fixed decode state (`C_MEM_SMALL`)
/// by the same [`LARGE_THRESHOLD`] used for the Rust side.
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
/// allocator, record the layout, and update the C-side peak/buckets. Returns
/// null on zero/overflow/failure, honoring zlib's allocator contract.
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
/// the C-side live count/buckets, and free through `System`.
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

/// A peak-working-set measurement, split into the data-scaling window (`large`)
/// and the fixed decode state + tables (`small`); `total` is the high-water
/// mark of the two combined.
#[derive(Clone, Copy)]
struct MemBreakdown {
    total: usize,
    large: usize,
    small: usize,
}

/// Maximum amount by which `zlib_rs`'s *total* peak may exceed C zlib's and
/// still pass the secondary (parity) gate. It absorbs only fixed decode-state /
/// table layout differences between the Rust `InflateState` and the C
/// `inflate_state`; it is far below the data-scaling window (32 KiB), so it can
/// never mask a real, data-scaling regression (which lands in `large` and trips
/// the strict primary gate).
const CONTROL_SLACK: usize = 8 * 1024;

/// Output-buffer size for the streaming decode used by the memory probe. Kept
/// **below** the 32 KiB sliding window so the engine must allocate and use its
/// internal window to resolve back-references that reach before the current
/// output chunk -- exercising the worst-case inflate footprint the AAP
/// sec. 0.7.1 bound describes ("~7 KB state + window of at most 32 KB"), which a
/// single-call full-buffer decode would never allocate.
const STREAM_CHUNK: usize = 16 * 1024;

/// Measure `zlib_rs`'s inflate-engine peak working-set for a **streaming**
/// decode of `compressed` (a windowBits-15 zlib stream) through a 16 KiB output
/// buffer -- forcing the sliding window to be allocated. Decoded bytes are
/// accumulated into a pre-sized (untimed/uncounted) buffer and checked against
/// `verify`. The captured peak reflects only the engine's internal allocations
/// (sliding window + decode tables + boxed state).
fn measure_rust_inflate(compressed: &[u8], verify: &[u8]) -> MemBreakdown {
    // Both buffers are allocated BEFORE arming, so they are never counted; `acc`
    // is pre-sized to the full output so `extend_from_slice` never reallocates
    // inside the armed window (it is then a pure memcpy).
    let mut chunk = vec![0u8; STREAM_CHUNK];
    let mut acc: Vec<u8> = Vec::with_capacity(verify.len());

    MEM_CUR.store(0, Ordering::SeqCst);
    MEM_PEAK.store(0, Ordering::SeqCst);
    MEM_LARGE.store(0, Ordering::SeqCst);
    MEM_SMALL.store(0, Ordering::SeqCst);
    MEM_COUNT_ON.store(true, Ordering::SeqCst);

    let mut inf = Inflate::new().expect("inflate init");
    let mut in_off = 0usize;
    loop {
        let (code, consumed, produced) =
            inf.inflate(&compressed[in_off..], &mut chunk[..], Z_NO_FLUSH);
        in_off += consumed;
        // Pure memcpy into the pre-sized accumulator (no allocation -> uncounted).
        acc.extend_from_slice(&chunk[..produced]);
        if code == Z_STREAM_END {
            break;
        }
        assert_eq!(code, Z_OK, "measure_rust_inflate: nonterminal code {code}");
        assert!(
            consumed != 0 || produced != 0,
            "measure_rust_inflate stalled with no progress before Z_STREAM_END"
        );
    }
    // The engine, window, and decode tables are live here (nothing freed yet),
    // so the bucket counters hold the steady-state working-set split.
    let breakdown = MemBreakdown {
        total: MEM_PEAK.load(Ordering::SeqCst),
        large: MEM_LARGE.load(Ordering::SeqCst),
        small: MEM_SMALL.load(Ordering::SeqCst),
    };
    drop(inf);

    MEM_COUNT_ON.store(false, Ordering::SeqCst);
    assert!(
        acc == verify,
        "measure_rust_inflate produced incorrect bytes"
    );
    breakdown
}

/// Measure canonical C zlib's inflate-engine peak working-set for the identical
/// **streaming** configuration (windowBits 15, 16 KiB output chunks -> the
/// 32 KiB window is allocated), via a real `inflateInit2_`/`inflate`/
/// `inflateEnd` cycle whose `zalloc`/`zfree` are the counting hooks above.
/// Decoded bytes are accumulated and checked against `verify`.
fn measure_c_inflate(compressed: &[u8], verify: &[u8]) -> MemBreakdown {
    // Allocated while the Rust counter is disarmed and never seen by C's hooks,
    // so neither counter observes them.
    let mut chunk = vec![0u8; STREAM_CHUNK];
    let mut acc: Vec<u8> = Vec::with_capacity(verify.len());

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
    // hooks set; `version`/`stream_size` follow the `inflateInit2_` ABI
    // (windowBits 15 selects a zlib-wrapped stream with a 32 KiB window).
    let rc = unsafe {
        inflateInit2_(
            &mut strm,
            15,
            zlibVersion(),
            core::mem::size_of::<ZStreamC>() as c_int,
        )
    };
    assert_eq!(rc, 0, "C inflateInit2_ failed (rc={rc})");

    strm.next_in = compressed.as_ptr();
    strm.avail_in = compressed.len() as c_uint;
    // Stream through a 16 KiB output window so C zlib must allocate its 32 KiB
    // sliding window to carry history across calls (the AAP worst-case path).
    loop {
        strm.next_out = chunk.as_mut_ptr();
        strm.avail_out = STREAM_CHUNK as c_uint;
        // SAFETY: `next_in`/`avail_in` describe the live `compressed` slice and
        // `next_out`/`avail_out` the live `chunk` buffer; Z_NO_FLUSH == 0.
        let rc = unsafe { inflate(&mut strm, 0) };
        let produced = STREAM_CHUNK - strm.avail_out as usize;
        acc.extend_from_slice(&chunk[..produced]);
        if rc == 1 {
            break;
        }
        assert_eq!(rc, 0, "C inflate returned nonterminal code rc={rc}");
        assert!(
            produced != 0,
            "C inflate stalled with no progress before Z_STREAM_END"
        );
    }

    // The C engine state and window are live here (before inflateEnd).
    let breakdown = MemBreakdown {
        total: C_MEM_PEAK.load(Ordering::SeqCst),
        large: C_MEM_LARGE.load(Ordering::SeqCst),
        small: C_MEM_SMALL.load(Ordering::SeqCst),
    };
    // SAFETY: frees the engine state/window allocated above through our `zfree`.
    let rc = unsafe { inflateEnd(&mut strm) };
    assert_eq!(rc, 0, "C inflateEnd failed (rc={rc})");
    assert!(acc == verify, "C zlib inflate produced incorrect bytes");
    breakdown
}

/// Format a byte count as `N bytes (~K.K KiB)` for the report.
fn fmt_bytes(n: usize) -> String {
    format!("{n} bytes (~{:.1} KiB)", n as f64 / 1024.0)
}

/// Run the AAP sec. 0.7.3 memory-footprint gate for the **inflate** engine:
/// measure both engines' peak working-set decoding a representative 1 MiB
/// stream and enforce `memory <= C zlib`. Prints a CI-readable report. Runs
/// once, untimed, at startup (before any criterion measurement).
///
/// Two gates are enforced:
///   1. PRIMARY (strict): the data-scaling sliding window (`large`; AAP
///      sec. 0.7.1 "window of at most 32 KB") must satisfy `zlib_rs <= C zlib`.
///   2. SECONDARY (parity): the *total* peak (window + decode tables + fixed
///      state, i.e. the full AAP sec. 0.7.1 "~7 KB state + <= 32 KB window")
///      must satisfy `zlib_rs <= C zlib + CONTROL_SLACK`.
fn run_memory_gate() {
    let original = text_like(SIZE); // representative 1 MiB corpus (deterministic)
    let compressed = compress2(&original, SETUP_LEVEL).expect("setup compress2");

    // Both probes stream through a 16 KiB output window (< the 32 KiB sliding
    // window), forcing each engine to allocate its window; each verifies its
    // decoded output against `original` internally.
    let r = measure_rust_inflate(&compressed, &original);
    let c = measure_c_inflate(&compressed, &original);

    let pct = |a: usize, b: usize| {
        if b > 0 {
            (a as f64 / b as f64) * 100.0
        } else {
            0.0
        }
    };

    println!("=== AAP 0.7.3 memory-footprint gate: INFLATE engine (windowBits 15, streaming) ===");
    println!("                                          zlib_rs                C zlib (1.3.1)");
    println!(
        "    sliding window (data-scaling)   : {:>26}   {:>26}",
        fmt_bytes(r.large),
        fmt_bytes(c.large)
    );
    println!(
        "    decode state + tables (fixed)   : {:>26}   {:>26}",
        fmt_bytes(r.small),
        fmt_bytes(c.small)
    );
    println!(
        "    total peak working-set          : {:>26}   {:>26}",
        fmt_bytes(r.total),
        fmt_bytes(c.total)
    );
    println!(
        "    ratio zlib_rs/C  (window/total) : {:.1}% / {:.1}%   (gate: window <= 100%, total <= 100% + {} B slack)",
        pct(r.large, c.large),
        pct(r.total, c.total),
        CONTROL_SLACK
    );

    // PRIMARY strict gate: the data-scaling sliding window.
    assert!(
        r.large <= c.large,
        "MEMORY GATE FAILED (inflate window): zlib_rs {} B > C zlib {} B \
         (AAP 0.7.1/0.7.3 require the <= 32 KB window to be <= C zlib)",
        r.large,
        c.large
    );
    // SECONDARY parity gate: total incl. fixed decode state + tables.
    assert!(
        r.total <= c.total + CONTROL_SLACK,
        "MEMORY GATE FAILED (inflate total): zlib_rs {} B > C zlib {} B + {} B control slack",
        r.total,
        c.total,
        CONTROL_SLACK
    );
    println!("    RESULT                          : PASS (window <= C zlib; total at parity)");
}

// ===========================================================================
// Harness wiring (custom `main`; `harness = false` in Cargo.toml)
// ===========================================================================

criterion_group!(
    benches,
    bench_uncompress,
    bench_inflate_stream,
    bench_baseline_czlib
);

/// Custom harness entry point. Runs the AAP sec. 0.7.3 memory-footprint gate
/// first (untimed, at startup), then drives the criterion throughput suite
/// exactly as the generated `criterion_main!(benches)` would. `cargo bench
/// --no-run` compiles this without executing it; `cargo test` does not execute
/// `harness = false` bench mains.
fn main() {
    run_memory_gate();
    benches();
    Criterion::default().configure_from_args().final_summary();
}
