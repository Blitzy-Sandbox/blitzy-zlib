//! Compression throughput and per-stream memory: the Rust port against the in-process C oracle.
//!
//! This is the third and richest of the three suites AAP §0.3.1 places at `benches/`, and it is
//! the only one that answers two questions rather than one:
//!
//! 1. *Does the port compress within 10% of the speed of the C reference at levels 1, 6 and 9?*
//! 2. *Does one stream of the port cost within 15% of the memory one stream of the C reference
//!    costs?*
//!
//! Both are AAP §0.8.4 gates, and AAP §0.6.4.6 requires the first to be read with both
//! implementations running in **one process on one buffer**, so that two build artifacts, two
//! page-cache states and two process start-ups cannot be mistaken for a throughput difference. The
//! second is measured the way AAP §0.6.4.6 names: an instrumented `zalloc` high-water counter,
//! mirroring the `mem_high` technique that `test/infcover.c` has already proven against this
//! library's allocation patterns.
//!
//! The idioms come from `benches/checksum_bench.rs`, which established them, and from
//! `benches/inflate_bench.rs`, which added stream state: committed-corpus loading, named call
//! gates that own a single `unsafe` block each, agreement checks taken strictly outside every
//! timed region, `RAII` guards so no path can leak a stream, graceful skipping in place of
//! assertions, and the `zlib-rs` / `c-oracle` row labels. What this suite adds on top is the
//! allocator instrumentation of [`AllocCounter`], which exists nowhere else in the workspace.
//!
//! # Why the gated levels are 1, 6 and 9
//!
//! Not an arbitrary sample. `deflate.c`'s `configuration_table[10]` (L112-L124) binds each level
//! to one of five strategy functions and to four tuning parameters
//! `{good_length, max_lazy, nice_length, max_chain}`:
//!
//! ```text
//! /* 0 */ {0,    0,   0,    0, deflate_stored}   store only
//! /* 1 */ {4,    4,   8,    4, deflate_fast}     max speed, no lazy matches
//! /* 2 */ {4,    5,  16,    8, deflate_fast}
//! /* 3 */ {4,    6,  32,   32, deflate_fast}
//! /* 4 */ {4,    4,  16,   16, deflate_slow}     lazy matches
//! /* 5 */ {8,   16,  32,   32, deflate_slow}
//! /* 6 */ {8,   16, 128,  128, deflate_slow}
//! /* 7 */ {8,   32, 128,  256, deflate_slow}
//! /* 8 */ {32, 128, 258, 1024, deflate_slow}
//! /* 9 */ {32, 258, 258, 4096, deflate_slow}     max compression
//! ```
//!
//! So `{1, 6, 9}` spans **both** algorithm families — level 1 is `deflate_fast` (`deflate.c`
//! L1857), levels 6 and 9 are `deflate_slow` (L1956) — and the three orders of magnitude of
//! `max_chain`: 4, 128 and 4096. Those are the three points at which a Rust-versus-C divergence
//! would manifest differently. At level 1 the measurement is dominated by hashing and window
//! bookkeeping (`deflate.c` L187 `slide_hash`, L252 `fill_window`); at level 9 it is dominated by
//! `longest_match`'s chain walk (L1389) and the `TOO_FAR` lazy-match rejection (L89, applied at
//! L1996-L2007). A port that were fast at one and slow at the other would pass a single-level gate
//! and still be wrong.
//!
//! Level 0 (`deflate_stored`, `deflate.c` L1668) is measured too, in [`deflate_stored_floor`], as
//! an explicitly **informational** `memcpy`-bound floor: it is outside the gate because AAP §0.8.4
//! names levels 1, 6 and 9, and because a level that performs no match search says nothing about
//! the match finder. `deflate_rle` (L2084) and `deflate_huff` (L2155) are reached through the
//! `Z_RLE` and `Z_HUFFMAN_ONLY` rows of [`deflate_strategy`], also informational.
//!
//! # What is measured
//!
//! Ten group functions. Every throughput group pairs the port and the reference as two rows under
//! one benchmark id so that criterion prints them side by side, and every such row carries
//! `Throughput::Bytes(uncompressed_len)` so the reported rate is *input* bytes per second — which
//! is the quantity the gate is expressed in.
//!
//! * **`deflate_steady_state`** — ★ **this is the group the ≤10% throughput gate is read from.**
//!   One encoder is initialised once per row and `deflateReset` returns it to the start of a
//!   stream between iterations, so what is timed is the encode plus the cheap half of the stream
//!   lifecycle and *not* the allocator. Axis: each committed fixture at levels 1, 6 and 9.
//! * **`deflate_lifecycle`** — the same work with `deflateInit2_` and `deflateEnd` inside the
//!   timed region, so one iteration is a whole stream from nothing to nothing. Kept separate from
//!   the gate rather than folded into it, because a deflate stream's allocation is large — a
//!   32 `KiB` window, a `prev` array, a `head` array and the symbol buffers, all sized from
//!   `windowBits` and `memLevel` — and on a small payload it would swamp the encode it is supposed
//!   to be measuring. Read the two groups together: the difference between them *is* the
//!   per-stream setup cost, and the ids are spelled the same way in both so the subtraction is
//!   direct.
//! * **`deflate_container`** — the three container formats, selected by `windowBits`: 15 for zlib
//!   (RFC 1950, a two-byte header and an Adler-32 trailer), −15 for raw DEFLATE (RFC 1951, no
//!   wrapper at all) and 31 for gzip (RFC 1952, a ten-byte minimum header and a CRC-32 trailer).
//!   All three are measured because the checksum work differs between them, and the checksum is
//!   exactly where the `simd` feature shows up.
//! * **`deflate_feeding`** — single-shot against chunked, and chunked at three chunk sizes. AAP
//!   §0.6.4.2 records that chunk boundaries interact with flush handling and pending-buffer state,
//!   and the codegen lever AAP §0.3.2.4 describes is explicitly chunk-size sensitive — roughly 10%
//!   at 16-byte chunks against a couple of percent at 1024 — so a 16-byte and a 1024-byte case sit
//!   beside the reference driver's 32768.
//! * **`deflate_mem_level`** — `memLevel` 1 and 9 against the default 8 the gate group uses. This
//!   is the hash-table-size axis: `memLevel` sets `hash_bits` and the symbol-buffer size
//!   (`deflate.h` L104-L288, `LIT_BUFS` at L225-L229), so a smaller table means more collisions
//!   and longer chains for `longest_match` to walk.
//! * **`deflate_strategy`** — `Z_FILTERED`, `Z_RLE`, `Z_HUFFMAN_ONLY` and `Z_FIXED` against the
//!   `Z_DEFAULT_STRATEGY` the gate group uses. Informational: these select different code paths
//!   entirely, and a caller who has chosen one is not measuring the same thing the gate is about.
//! * **`compress_oneshot`** — `compress2` and `compress2_z` against `c_compress2` and
//!   `c_compress2_z`, sized with `compressBound`. A distinct and heavily used entry path:
//!   `compress.c` runs a whole stream internally and supplies `Z_DEFLATED`, `MAX_WBITS`,
//!   `DEF_MEM_LEVEL` and `Z_DEFAULT_STRATEGY` itself, so its cost profile is the lifecycle
//!   group's rather than the steady state's.
//! * **`deflate_stored_floor`** — level 0 only. Informational, as described above.
//! * **`deflate_bound`** — `deflateBound`, `deflateBound_z`, `compressBound` and `compressBound_z`
//!   against their `c_` counterparts, per call and with **no** `Throughput`, because these
//!   functions return a number and copy nothing. See "Bound agreement" below for why they are
//!   also a correctness guard on every other group.
//! * **`deflate_memory`** — the ≤15% memory gate. Registers **no criterion row at all**: it runs
//!   the allocation-tracked encodes in its own untimed pass and prints the `MEMORY` lines. See
//!   "The memory gate" below.
//! * **`deflate_degenerate`** — `empty.bin`, `single_byte.bin`, `dictionary.bin`, `gray_list.bin`
//!   and `hello.bin`: the payloads whose *per-call overhead* is the whole measurement. This group
//!   sets no `Throughput` deliberately, and that is not a formality — a zero-byte fixture would
//!   otherwise hand criterion a `Throughput::Bytes(0)` to divide by, and because a criterion
//!   group's throughput setting is sticky across rows, a zero-length row mixed into a throughput
//!   group would silently inherit the previous row's divisor. Latency rows belong in a latency
//!   group.
//! * **`deflate_silesia`** — the gate measurement over the opt-in tier-2 corpus, skipped entirely
//!   and with one clear note when that corpus is not present.
//!
//! # Provenance
//!
//! The subject is `deflate.c` (2,185 lines) and, through it, `trees.c` (1,119 lines): the five
//! strategy functions at L1668 (`deflate_stored`), L1857 (`deflate_fast`), L1956 (`deflate_slow`),
//! L2084 (`deflate_rle`) and L2155 (`deflate_huff`); the match finder at L1389 (`longest_match`);
//! the window machinery at L187 (`slide_hash`) and L252 (`fill_window`); the per-level table at
//! L112-L124; and `deflateBound` at `zlib.h` L768. `compress.c` (99 lines) supplies the one-shot
//! group. `trees.c` L966 (`detect_data_type`) is why `text.txt`, `binary.bin` and `gray_list.bin`
//! are separate fixtures.
//!
//! The harness *shape* is the repository's own benchmark driver,
//! `contrib/testzlib/testzlib.c` (275 lines), which AAP §0.4.1.5 names as this target's source.
//! Four things are taken from it and three are deliberately left behind:
//!
//! * the 32768-byte default chunk (L147-L148, `0x8000` for both directions);
//! * the `memset(&zcpr, 0, sizeof(z_stream))` before initialisation (L197);
//! * `next_in` and `next_out` set **once** before the loop (L200-L201), because the library
//!   advances both itself;
//! * the compression loop at L204-L213 — each round offer `min(remaining, chunk)` input and a
//!   chunk of output space, call `deflate` with `Z_FINISH` when this round's input is the last of
//!   it and `Z_SYNC_FLUSH` otherwise, and continue while the status is `Z_OK`; then read the
//!   produced length from `total_out` (L215) and end the stream (L216);
//! * **not** its output-buffer guard `size + size / 0x10 + 0x200` (L183), which is a heuristic.
//!   `deflateBound` and `compressBound` are the library's own exact bounds, AAP §0.6.2.2 requires
//!   the port's and the reference's values to be numerically identical, and a bound that is too
//!   small becomes a buffer overflow in *caller* code. See "Bound agreement" below;
//! * **not** its three hand-rolled clocks (`GetTickCount`, `QueryPerformanceCounter` and an
//!   `rdtsc` pair), which criterion's warmed, resampled, outlier-aware sampling replaces;
//! * **not** its `ReadFileMemory` (L119-L143), which becomes `std::fs::read` and must tolerate a
//!   missing file.
//!
//! That driver is Windows-only — it includes `windows.h`, contains inline `_asm` and calls
//! `Int64ShrlMod32` — so it is a design reference and is never ported literally, never compiled
//! and never linked.
//!
//! The allocator instrumentation is `test/infcover.c` L56-L200, transcribed rather than
//! reinvented: `mem_item` (L56-L60), `mem_zone` with its `total`/`highwater`/`notlifo`/`rogue`
//! members (L63-L68), `mem_alloc`'s `count * (size_t) size` sizing and its deliberate
//! `memset(ptr, 0xa5, len)` fill (L71-L109), `mem_free`'s head-first search with the non-`LIFO`
//! and rogue tallies (L112-L154), `mem_setup`'s installation order (L158-L173) and `mem_high`'s
//! report (L192-L197).
//!
//! # This is not a correctness gate
//!
//! Every case is encoded once by each implementation before either is timed; the two compressed
//! lengths are compared, and each result is round-tripped back to the original bytes. That check
//! is a guard on the *measurement*, not a test: a throughput number for an encoder that produced
//! the wrong bytes is worse than no number. On a mismatch this file prints a diagnostic naming the
//! fixture, the level, the `windowBits`, the `memLevel`, the strategy and the feeding mode, skips
//! that one case, and carries on — because a suite that aborted on case three would say nothing
//! about the other hundred and forty.
//!
//! Note carefully what is **not** claimed here: comparing two *lengths* is not byte identity.
//! Byte identity across the full encoder matrix — level × `windowBits` × `memLevel` × strategy ×
//! flush × corpus — is owned by `crates/zlib-rs-differential/tests/byte_identical.rs`, stream
//! interoperability in both directions by `tests/roundtrip_interop.rs`, and the transcribed
//! constants by `tests/table_equality.rs`. Nothing here duplicates them, and a failure there is a
//! failed build while a mismatch here is one unmeasured row.
//!
//! # The ≤10% throughput gate, and the summary format CI consumes
//!
//! criterion is a measurement tool and does not fail a build, and the pass/fail decision belongs
//! to the criterion regression job that AAP §0.4.1.6 places in `.github/workflows/rust.yml`. So
//! this file *reports* both gates and never enforces either: nothing here panics, exits or aborts
//! on a regression.
//!
//! Alongside criterion's own estimates, each case is measured by a small bounded self-timed pass
//! and one line per case is written to **stderr** in a stable, greppable form:
//!
//! ```text
//! deflate_bench: RATIO group=<group> case=<case> bytes=<n> port_ns=<f> oracle_ns=<f> ratio=<f> limit=1.10 gate=counted|informational verdict=within|over
//! ```
//!
//! and one aggregate line per group:
//!
//! ```text
//! deflate_bench: RATIO-SUMMARY group=<group> cases=<n> gated=<k> over=<m> informational=<i> limit=1.10
//! ```
//!
//! The field order is fixed and the keys are stable. `bytes` is the uncompressed length, which is
//! what the throughput is per. `verdict=over` marks a case slower than `limit`. `cases` is
//! `gated + informational`, and **`over` counts only gated cases**, so a workflow that reads
//! `over` from the `deflate_steady_state` summary line is reading exactly the AAP §0.8.4
//! compression gate. The informational groups named above emit the same lines and are excluded
//! from the gate by group name, not by suppressing their numbers.
//!
//! ★ `gate=informational` also separates a throughput comparison from a per-call-overhead
//! comparison, and it is a statement about what the gate *means* rather than a way of avoiding it.
//! A payload below [`GATE_MIN_BYTES`] — one page — is encoded in a few hundred nanoseconds of
//! which almost none is encoding, so its ratio measures entry validation and stream reset. That
//! number is worth having and is printed in full with its own verdict; it is simply not the
//! bytes-per-second bound AAP §0.8.4 states, which AAP §0.6.4.6 reads from the multi-megabyte
//! Silesia members.
//!
//! Two further properties of these numbers matter to anyone reading them:
//!
//! * They are **indicative**, not authoritative. The pass is bounded to a few milliseconds per
//!   side, takes the minimum of three rounds, and reports one figure rather than a confidence
//!   interval. criterion's own estimate remains the authoritative measurement, and a regression
//!   job that wants statistics should read it from
//!   `target/criterion/<group>/<label>/<case>/new/estimates.json` — note that a
//!   `BenchmarkId::new(label, case)` becomes *two* path components under the group, not one.
//! * They are measured **before** the criterion rows for the same case, so the calibration also
//!   serves as an identical warm-up for both sides.
//!
//! # The memory gate, and its summary format
//!
//! AAP §0.8.4 bounds per-stream memory at 115% of the reference and AAP §0.6.4.6 names the
//! measurement technique: an instrumented `zalloc` counter, "the same technique `test/infcover.c`
//! already uses via `mem_high` for high-water tracking". [`AllocCounter`] is that counter and
//! [`tracked_alloc`] / [`tracked_free`] are the hooks; both sides get their own instance, since
//! the two implementations' `alloc_func` and `free_func` aliases resolve to the same underlying
//! `extern "C"` signature.
//!
//! ```text
//! deflate_bench: MEMORY group=<group> case=<case> port_bytes=<n> oracle_bytes=<n> ratio=<f> limit=1.15 verdict=within|over
//! deflate_bench: MEMORY-SUMMARY group=<group> cases=<n> over=<m> limit=1.15
//! deflate_bench: ALLOC-BALANCE side=<label> case=<case> allocations=<n> frees=<n> live_bytes=<n> notlifo=<n> rogue=<n> verdict=clean|dirty
//! ```
//!
//! `port_bytes` and `oracle_bytes` are high-water marks in bytes, `ratio` is `port / oracle`, and
//! a workflow reads `over` from the `MEMORY-SUMMARY` line exactly as it reads the throughput gate
//! from `RATIO-SUMMARY`. The `ALLOC-BALANCE` line is emitted once per side per case and is the
//! leak check: `verdict=dirty` means `deflateEnd` did not return every block, or a block came back
//! out of order, or a pointer this counter never handed out was freed through it. A leak here
//! would also surface in the nightly `AddressSanitizer` job, which is the verification gate this
//! crate does sit inside.
//!
//! ★ **The tracked pass is never inside a timed region.** The counter maintains a registry and
//! fills every block with `0xa5`, both of which cost time; letting that bookkeeping into a
//! criterion closure is the single easiest way for this file to publish a misleading number. The
//! throughput groups therefore run with `zalloc`, `zfree` and `opaque` left null — C's `Z_NULL`,
//! which selects each library's own internal allocator — and [`deflate_memory`] is a separate
//! group function that registers no row.
//!
//! # Running it
//!
//! ```text
//! cargo bench -p zlib-rs-differential --bench deflate_bench
//! cargo bench -p zlib-rs-differential --bench deflate_bench -- --test    # one iteration each
//! cargo bench -p zlib-rs-differential --bench deflate_bench -- deflate_steady_state
//! ```
//!
//! The throughput groups run with an explicit 50-sample, 1-second warm-up, 3-second measurement
//! budget rather than criterion's 100/3/5 defaults, and the sub-microsecond groups run shorter
//! still; the reasoning is recorded at [`configure`] and [`configure_short`]. `--test` executes
//! every case exactly once in a few seconds and is the cheapest end-to-end proof that the harness,
//! the oracle linkage, the version-and-size handshake, the allocator hooks and the corpus paths
//! are all correct. Filtering by group name is the way to spend the whole budget on one axis.
//!
//! Comparing two whole configurations — a `simd` build against a scalar one, say — is what
//! criterion's baselines are for, and the row labels here are stable across configurations
//! precisely so that it works:
//!
//! ```text
//! cargo bench -p zlib-rs-differential --bench deflate_bench -- --save-baseline scalar
//! cargo bench -p zlib-rs-differential --bench deflate_bench --features simd \
//!     -- --baseline-lenient scalar
//! ```
//!
//! The `simd` feature is meaningful here even though this suite measures the encoder: `deflate`
//! runs an Adler-32 or a CRC-32 over everything it consumes, so the checksum backend sits on the
//! encoder's hot path — while remaining unable to change a single emitted byte.
//!
//! # Code generation, and the optimizations that are off the table
//!
//! Cargo's `bench` profile inherits `[profile.release]`, which the repository root sets to
//! `lto = "fat"`, `codegen-units = 1` and `opt-level = 3`, so these measurements are taken against
//! the same code generation as the shipped library. `panic = "abort"` is ignored for bench targets
//! — cargo does not honour the key there — which is expected and harmless. Profiles are honoured
//! only in the workspace root, so nothing here declares one and none is needed.
//!
//! `-Cllvm-args=-enable-dfa-jump-thread` is the output-neutral codegen lever AAP §0.3.2.4 measured
//! at roughly a 10% gain for small chunked input, which is exactly the shape the 16-byte and
//! 1024-byte rows of `deflate_feeding` expose. It is passed through `RUSTFLAGS` at measurement
//! time and is **never** committed to a manifest, because it is an LLVM-internal flag that a
//! non-LLVM toolchain would reject:
//!
//! ```text
//! RUSTFLAGS="-Cllvm-args=-enable-dfa-jump-thread" \
//!     cargo bench -p zlib-rs-differential --bench deflate_bench
//! ```
//!
//! ★ This is the file where AAP §0.7.1(c) bites hardest, so it is restated here for whoever reads
//! a disappointing number and starts looking for a fix. **A benchmark must never change what the
//! encoder emits, and neither may anything a benchmark motivates.** Specifically:
//!
//! * The per-level `configuration_table` values are not tuning knobs. They are the reference's
//!   values and they decide which matches are found at all.
//! * The non-default compile-time knobs AAP §0.6.2.1 lists — `FASTEST`, `LIT_MEM`,
//!   `UNALIGNED_OK`, `FORCE_STATIC`, `FORCE_STORED` — change emitted bytes and are not enabled
//!   here, not measured here, and not to be reached for.
//! * Encoder-level improvements — better match selection, smarter lazy-match thresholds, an
//!   altered hash-chain traversal order, `zlib-ng`-style hashing — are **prohibited regardless of
//!   how much faster they would be**, because each one changes emitted bytes and breaks the
//!   byte-identity criterion that `tests/byte_identical.rs` gates.
//!
//! Admissible optimization is output-neutral only: vectorized checksums behind the `simd` feature,
//! memory-copy strategy, branch layout, fat LTO with a single codegen unit at `opt-level` 3, and
//! the codegen flag above.
//!
//! # Bound agreement
//!
//! `deflateBound` (`zlib.h` L768), `deflateBound_z`, `compressBound` and `compressBound_z` are not
//! merely informational: callers size their output buffers with what they return, so a value that
//! is too small becomes a buffer overflow in caller code and a value that is too large breaks
//! tests asserting exact sizes (AAP §0.6.2.2). Every case in this file therefore compares the
//! port's number against the reference's during setup, and on a divergence prints a loud
//! diagnostic and skips the case rather than benchmarking with a wrongly sized buffer. The
//! authoritative numeric gate remains `tests/byte_identical.rs`; this is a cheap guard, not a
//! second gate.
//!
//! # Inputs — two tiers, and no network, ever
//!
//! **Tier 1 — the committed minimal corpus, always available.** Read by exact filename from
//! `<CARGO_MANIFEST_DIR>/corpus/minimal`, whose inventory `corpus/README.md` pins. Every fixture
//! is worth compressing and each is here for a stated reason: `repetitive.bin` (16 `KiB`) drives
//! long matches and `nice_length`/`max_chain` saturation; `random.bin` (8 `KiB`) is
//! incompressible and forces stored-block selection (`trees.c` L1047-L1048); `text.txt` and
//! `gray_list.bin` exercise the two `Z_TEXT`/fall-through arms of `detect_data_type` (`trees.c`
//! L966) and `binary.bin` the block-list arm; `window_boundary.bin` (65 `KiB`) is the only
//! committed fixture past the 32 `KiB` window and therefore the only one that drives `fill_window`
//! and `slide_hash` across a boundary; `hello.bin` (14 bytes) and `dictionary.bin` (6 bytes) are
//! the payloads the existing C suite has always used (`test/example.c` L35 and L40); and
//! `empty.bin` and `single_byte.bin` are the degenerate cases — the empty stored block and the
//! one-distinct-symbol Huffman fixup at `trees.c` L650-L661 — whose per-call overhead is worth
//! seeing.
//!
//! ★ For a `[[bench]]` target `CARGO_MANIFEST_DIR` expands to the **host package's** directory —
//! `crates/zlib-rs-differential`, the crate whose manifest carries the `[[bench]]` entry that
//! attaches this file — and *not* to the `benches/` directory this file lives in. That is
//! genuinely surprising, so both paths derived from it are named once and only once, in
//! [`minimal_corpus_dir`] and [`repository_root`].
//!
//! **Tier 2 — Silesia, opt-in only.** `corpus/README.md` publishes one canonical resolution order
//! and `corpus/fetch_silesia.sh` implements the same one: `$ZLIB_RS_SILESIA_DIR` when it is set
//! and non-empty, otherwise `<repo-root>/target/silesia`. [`silesia_dir`] implements exactly that
//! and nothing else, and it is behaviourally identical to the probe in `benches/inflate_bench.rs`
//! so that the two files can never disagree about where the corpus lives. Absence is normal, not
//! an error: when the directory is missing or holds no readable member this file prints one note
//! naming `crates/zlib-rs-differential/corpus/fetch_silesia.sh`, skips the Silesia group, and the
//! run still succeeds.
//!
//! **Nothing here touches the network, spawns a process, or executes that script.** AAP §0.6.4.4
//! requires `cargo test` and CI to be network-free, and `corpus/README.md` states that the script
//! is invoked by a human and by nothing else — it is referenced by no manifest, no build script,
//! no test and no workflow, and this file keeps it that way. No crate is introduced for any of
//! this either: `std::fs`, `std::path` and `std::alloc` suffice.
//!
//! # Hygiene
//!
//! `crates/zlib-rs-differential` is dev-only and appears in neither shipped crate's
//! `[dependencies]` (AAP §0.6.4.1), which is what makes `unsafe` acceptable in this file at all:
//! it exists solely to call the oracle's `extern "C"` declarations, the facade's exported C ABI
//! and the two allocator hooks C calls back into. Every such call is funnelled through one named
//! gate that owns a single `unsafe` block, and every block states the invariant that discharges
//! it. AAP §0.8.1 directive 5 and §0.7.1(a) bind the *shipped* crates, and they are untouched.
//! `extern "C-unwind"` appears nowhere, nothing here is `#[no_mangle]`, and no `extern "C"` block
//! and no `#[link]` attribute is declared — `crates/zlib-rs-differential/src/oracle.rs` owns every
//! oracle declaration and `crates/zlib-rs-differential/build.rs` already emits the link
//! directives.
//!
//! Every stream is ended on every path, including the skip paths, by the two `RAII` guards
//! [`PortDeflate`] and [`OracleDeflate`]; a leaked deflate stream leaks its window, its `prev` and
//! `head` arrays and its pending buffer along with it. Miri is scoped to `-p zlib-rs` and cannot
//! execute the C oracle, so this file is outside it by construction.
//!
//! A `harness = false` bench compiles without `--test`, so `cfg(test)` is false here and these
//! functions are not `#[test]`. `clippy.toml`'s `allow-unwrap-in-tests`, `allow-expect-in-tests`
//! and `allow-panic-in-tests` therefore do not reach this file and the workspace's denied panic
//! family is fully in force. Nothing below unwraps, expects, panics or indexes: a fallible step
//! logs to stderr and returns early, which is also the behaviour a benchmark should have. Lengths
//! are converted with `try_from` rather than `as`, so a length that could not be represented is
//! reported and skipped instead of silently truncated — and `uLong` is `c_ulong`, eight bytes on
//! LP64 but four on LLP64 Windows (AAP §0.6.3.3), so that is a portability requirement rather
//! than a style preference.

// `unsafe_op_in_unsafe_fn` is denied here for the same reason `crates/libz-rs-sys` denies it: the
// two allocator hooks below are `unsafe extern "C" fn` items, and without this attribute their
// bodies would be implicitly unsafe, an inner `unsafe` block would be flagged as unnecessary, and
// the `// SAFETY:` comment the workspace's `undocumented_unsafe_blocks` deny-level lint requires
// would have nowhere to attach. With it, every unsafe operation in this file — in a safe function
// or an unsafe one — sits inside an explicitly scoped block that carries its invariant.
#![deny(unsafe_op_in_unsafe_fn)]

use core::ffi::{c_char, c_int, c_uint, c_void};
use core::mem::{align_of, size_of};
use std::alloc::{alloc, dealloc, Layout};
use std::ffi::{CStr, OsStr};
use std::fs;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::sync::{Once, OnceLock};
use std::time::{Duration, Instant};

use criterion::measurement::WallTime;
use criterion::{
    criterion_group, criterion_main, BenchmarkGroup, BenchmarkId, Criterion, Throughput,
};

// The facade -- the artifact whose throughput and footprint are the acceptance criteria. The
// spelling is `libz_rs_sys`, never `z`: `crates/libz-rs-sys` sets `[lib] name = "z"` so that cargo
// emits `libz.so`/`libz.a`, and `crates/zlib-rs-differential/Cargo.toml` uses cargo's
// dependency-rename form to give the Rust path a useful name. That manifest is the authority.
//
// The `Z_*` constants come from here for BOTH implementations, and deliberately so:
// `src/oracle.rs` declares functions and type mirrors but publishes no constants, and these are
// `zlib.h` `#define`s rather than anything either library owns. `crates/libz-rs-sys/src/lib.rs`
// cites the header line for each one.
use libz_rs_sys::{
    MAX_MEM_LEVEL, MAX_WBITS, ZLIB_VERSION, Z_BEST_COMPRESSION, Z_BEST_SPEED, Z_DEFAULT_STRATEGY,
    Z_DEFLATED, Z_FILTERED, Z_FINISH, Z_FIXED, Z_HUFFMAN_ONLY, Z_NO_COMPRESSION, Z_OK, Z_RLE,
    Z_STREAM_END, Z_SYNC_FLUSH,
};

// The C reference. Every name here is declared in exactly one place --
// `crates/zlib-rs-differential/src/oracle.rs` -- and this file adds no `extern "C"` block and no
// `#[link]` attribute of its own.
use zlib_rs_differential::oracle;

// =================================================================================================
//  Row labels
// =================================================================================================

/// Row label for the C reference implementation.
///
/// A row under this name is `deflate.c`, `trees.c` and `compress.c` themselves, compiled from the
/// in-tree sources by `crates/zlib-rs-differential/build.rs` with every export renamed to a `c_`
/// prefix so that both implementations can be linked into one binary.
const ORACLE_LABEL: &str = "c-oracle";

/// Row label for the port, reached through its exported C ABI.
///
/// ★ Stable across configurations, deliberately. criterion keys a saved baseline by benchmark id,
/// so a label that changed with the feature set would make the two runs of the
/// `--save-baseline` / `--baseline-lenient` procedure non-comparable. The configuration is
/// reported in the startup banner and carried by the baseline name instead; see
/// [`report_configuration`].
const PORT_LABEL: &str = "zlib-rs";

/// The stable prefix every stderr line from this suite carries, so that a log can be filtered.
const LOG_PREFIX: &str = "deflate_bench:";

/// The greppable per-case throughput line's leading key. Documented in the module header, because
/// the criterion regression job of AAP §0.4.1.6 consumes it.
const RATIO_KEY: &str = "RATIO";

/// The greppable per-group throughput aggregate line's leading key.
const RATIO_SUMMARY_KEY: &str = "RATIO-SUMMARY";

/// The greppable per-case memory line's leading key.
const MEMORY_KEY: &str = "MEMORY";

/// The greppable per-group memory aggregate line's leading key.
const MEMORY_SUMMARY_KEY: &str = "MEMORY-SUMMARY";

/// The greppable per-side allocator balance line's leading key -- the leak check.
const BALANCE_KEY: &str = "ALLOC-BALANCE";

// =================================================================================================
//  The two gates
// =================================================================================================

/// The throughput gate of AAP §0.8.4: the port may be at most 10% slower than the C reference at
/// levels 1, 6 and 9.
///
/// Expressed as a ratio of per-operation times, so 1.10 is the limit and a larger number is a
/// regression. Reported, never enforced -- see the module header.
const GATE_RATIO: f64 = 1.10;

/// The memory gate of AAP §0.8.4: one stream of the port may cost at most 15% more than one stream
/// of the C reference.
///
/// Compared as a ratio of `zalloc` high-water marks in bytes, which is what
/// `test/infcover.c`'s `mem_high` (L192-L197) reports and what AAP §0.6.4.6 names as the
/// measurement.
const GATE_MEMORY_RATIO: f64 = 1.15;

/// The smallest payload whose throughput ratio is counted against [`GATE_RATIO`]: one 4096-byte
/// page.
///
/// ★ Not a loophole, and worth reading before changing. The gate is stated as a *throughput* bound,
/// and throughput is bytes per second. Encoding an `N`-byte payload costs a fixed amount plus `N`
/// divided by the true rate, so the rate a measurement recovers approaches the true rate only when
/// the second term dominates. Below a page it does not: the 14-byte and 23-byte committed fixtures
/// are encoded in a few hundred nanoseconds, essentially all of it entry validation, stream reset
/// and the block flush it takes to reach the trailer. Their ratio is a real and useful number --
/// it is the per-call overhead ratio -- but it is not the quantity AAP §0.8.4 bounds, and AAP
/// §0.6.4.6 reads that quantity from the multi-megabyte Silesia members.
///
/// So every case still gets a `RATIO` line with its own `verdict`, and nothing is hidden. What the
/// floor changes is the `gate=` field: a case at or above it is `counted` and enters the group's
/// `over` tally, and a case below it is `informational` and is tallied separately. A reader who
/// wants the small-payload comparison reads the lines; a workflow that wants the gate reads
/// `over` on the `RATIO-SUMMARY` line.
///
/// 4096 rather than a rounder-sounding number because it is a page, it is the same "long enough to
/// amortize, short enough to stay in L1" length `benches/checksum_bench.rs` uses for its backend
/// sweep and `benches/inflate_bench.rs` uses for the same floor, and it admits every committed
/// fixture whose encode is rate-dominated -- `random.bin` at 8 `KiB`, `repetitive.bin` at
/// 16 `KiB`, `window_boundary.bin` at 65 `KiB` -- and every Silesia member.
const GATE_MIN_BYTES: usize = 4096;

// =================================================================================================
//  Encoder parameters
// =================================================================================================

/// The three compression levels the AAP §0.8.4 gate names.
///
/// The derivation is in the module header: `deflate.c`'s `configuration_table[10]` (L112-L124)
/// binds level 1 to `deflate_fast` with `max_chain` 4, level 6 to `deflate_slow` with `max_chain`
/// 128 and level 9 to `deflate_slow` with `max_chain` 4096, so these three span both algorithm
/// families and three orders of magnitude of chain length.
///
/// Spelled with the header's own constants rather than with bare literals: `Z_BEST_SPEED` and
/// `Z_BEST_COMPRESSION` (`zlib.h` L189-L190) *are* 1 and 9, so writing them this way states that the
/// gated set is the two ends of the documented range plus the default in between, and it cannot drift
/// from the header.
const GATE_LEVELS: [c_int; 3] = [Z_BEST_SPEED, DEFAULT_LEVEL, Z_BEST_COMPRESSION];

/// Level 0 -- `deflate_stored` (`deflate.c` L1668). Informational, and outside the gate.
///
/// It performs no match search at all, so it is a `memcpy`-bound floor rather than a measurement of
/// the encoder. Measured in [`deflate_stored_floor`] and labelled there.
const STORED_LEVEL: c_int = Z_NO_COMPRESSION;

/// The level used wherever the level is held fixed so another axis can vary.
///
/// 6 is `Z_DEFAULT_COMPRESSION`'s resolved value (`deflate.c` L399-L400) and the level the
/// overwhelming majority of callers actually get.
const DEFAULT_LEVEL: c_int = 6;

/// `DEF_MEM_LEVEL` -- what `compress2` and `gz_init` pass (`zutil.h` L81).
///
/// The gate groups hold `memLevel` here, because AAP §0.7.1(c) confines this suite to observing the
/// configuration the shipped library is built with.
const DEFAULT_MEM_LEVEL: c_int = 8;

/// The smallest `memLevel` `deflateInit2_` accepts.
///
/// `zconf.h` names the ceiling as `MAX_MEM_LEVEL` but has no constant for the floor, so this is the
/// one bound in the file that must be a literal. `deflate.c` L404 is the authority: it rejects
/// `memLevel < 1` with `Z_STREAM_ERROR` alongside `memLevel > MAX_MEM_LEVEL`.
const MIN_MEM_LEVEL: c_int = 1;

/// The two `memLevel` values [`deflate_mem_level`] measures against the default 8.
///
/// 1 and 9 are the ends of the range `zconf.h` L273-L277 permits (`MAX_MEM_LEVEL` is 9), and
/// `deflateInit2_` range-checks against exactly that. `memLevel` sets `hash_bits` and the
/// symbol-buffer size (`deflate.h` L104-L288, `LIT_BUFS` at L225-L229), so 1 means a 256-entry hash
/// table with correspondingly longer chains for `longest_match` to walk and 9 means a 64 `Ki`-entry
/// one. It is also the axis a Rust-side over-allocation would show up on, which is why
/// [`deflate_memory`] sweeps the same three values.
/// Derived from the bounds themselves rather than written as literals, so that the axis is the range
/// and cannot drift from `zconf.h`.
const AXIS_MEM_LEVELS: [c_int; 2] = [MIN_MEM_LEVEL, MAX_MEM_LEVEL];

/// The `memLevel` values the memory gate sweeps: the two extremes and the default between them.
const MEMORY_MEM_LEVELS: [c_int; 3] = [MIN_MEM_LEVEL, DEFAULT_MEM_LEVEL, MAX_MEM_LEVEL];

/// The four non-default strategies, in `zlib.h`'s own declaration order.
///
/// Informational, all four. Each selects a different code path -- `Z_RLE` reaches `deflate_rle`
/// (`deflate.c` L2084), `Z_HUFFMAN_ONLY` reaches `deflate_huff` (L2155), `Z_FILTERED` changes the
/// `TOO_FAR` rejection at L1996-L2007, and `Z_FIXED` forces static trees in `trees.c`'s block-type
/// selection -- so a caller who has chosen one is not measuring what the gate is about.
/// `Z_DEFAULT_STRATEGY` is absent because the gate groups already measure it.
const AXIS_STRATEGIES: [c_int; 4] = [Z_FILTERED, Z_HUFFMAN_ONLY, Z_RLE, Z_FIXED];

/// What `deflateInit2_` adds to `windowBits` to request the gzip wrapper instead of the zlib one.
///
/// 16, exactly as `zlib.h` documents for both `deflateInit2_` and `inflateInit2_`. Named rather than
/// folded into a literal 31 so that [`Container::Gzip`] reads as "the maximum window, wrapped in
/// gzip" instead of as an unexplained number.
const GZIP_WINDOW_BITS_ADDEND: c_int = 16;

/// One of the three container formats, and the `windowBits` that selects it.
///
/// All three are measured because the wrapper work differs: a zlib stream writes a two-byte header
/// and a four-byte Adler-32 trailer, a raw stream writes neither, and a gzip stream writes a
/// ten-byte minimum header and a CRC-32-plus-length trailer. The checksum is the part that matters
/// to a rate -- `deflate` runs one over everything it consumes -- and it is exactly where the
/// `simd` feature shows up.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Container {
    /// RFC 1950 zlib: `windowBits` 15, the default wrapper, Adler-32.
    Zlib,
    /// RFC 1951 raw DEFLATE: `windowBits` −15, no wrapper and no checksum.
    Raw,
    /// RFC 1952 gzip: `windowBits` 31, that is 15 with the `+16` gzip addend, CRC-32.
    Gzip,
}

impl Container {
    /// The `windowBits` argument that selects this container for `deflateInit2_`.
    ///
    /// Built from `MAX_WBITS` (`zconf.h` L287) rather than written as 15, −15 and 31, because the
    /// three forms *are* transformations of it: `zlib.h` documents that negating `windowBits` requests
    /// the raw format and that adding [`GZIP_WINDOW_BITS_ADDEND`] requests gzip. Spelling them this
    /// way means the suite tracks the header instead of restating it, which is what AAP §0.7.1(b)
    /// asks of a consumer of an immutable contract.
    const fn window_bits(self) -> c_int {
        match self {
            Self::Zlib => MAX_WBITS,
            Self::Raw => -MAX_WBITS,
            Self::Gzip => MAX_WBITS + GZIP_WINDOW_BITS_ADDEND,
        }
    }

    /// The short, filesystem-safe token this container contributes to a benchmark id.
    ///
    /// No `/` anywhere in an id: criterion turns an id into a path under `target/criterion`, so a
    /// separator would silently become a directory level.
    const fn tag(self) -> &'static str {
        match self {
            Self::Zlib => "zlib",
            Self::Raw => "raw",
            Self::Gzip => "gzip",
        }
    }
}

/// The container used wherever the container is held fixed so another axis can vary.
const DEFAULT_CONTAINER: Container = Container::Zlib;

/// All three containers, in the order a reader wants them: the default, then the two variants.
const CONTAINERS: [Container; 3] = [Container::Zlib, Container::Raw, Container::Gzip];

/// One complete `deflateInit2_` configuration, minus the two arguments that never vary.
///
/// `method` is always `Z_DEFLATED` -- it is the only method `zlib.h` defines -- and `version` and
/// `stream_size` are the handshake rather than a parameter. Bundled into one `Copy` struct rather
/// than passed as four arguments so that the drivers below stay well inside `clippy.toml`'s
/// eight-argument threshold and so that a case's identity and its configuration cannot drift apart.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Config {
    /// `level`, 0 through 9. See [`GATE_LEVELS`].
    level: c_int,
    /// `windowBits`, from [`Container::window_bits`].
    window_bits: c_int,
    /// `memLevel`, 1 through `MAX_MEM_LEVEL`.
    mem_level: c_int,
    /// `strategy`, one of the five `Z_*` strategy constants.
    strategy: c_int,
    /// The container the `windowBits` above selects. Carried so a diagnostic can name it.
    container: Container,
}

impl Config {
    /// The gate configuration at `level`: zlib, `memLevel` 8, `Z_DEFAULT_STRATEGY`.
    const fn at_level(level: c_int) -> Self {
        Self {
            level,
            window_bits: DEFAULT_CONTAINER.window_bits(),
            mem_level: DEFAULT_MEM_LEVEL,
            strategy: Z_DEFAULT_STRATEGY,
            container: DEFAULT_CONTAINER,
        }
    }

    /// The default level in `container`.
    const fn in_container(container: Container) -> Self {
        Self {
            level: DEFAULT_LEVEL,
            window_bits: container.window_bits(),
            mem_level: DEFAULT_MEM_LEVEL,
            strategy: Z_DEFAULT_STRATEGY,
            container,
        }
    }

    /// This configuration with `mem_level` replaced.
    const fn with_mem_level(self, mem_level: c_int) -> Self {
        Self { mem_level, ..self }
    }

    /// This configuration with `strategy` replaced.
    const fn with_strategy(self, strategy: c_int) -> Self {
        Self { strategy, ..self }
    }

    /// Everything a diagnostic should name about the encoder's configuration.
    fn describe(self) -> String {
        format!(
            "level={level} windowBits={bits} container={container} memLevel={mem} \
             strategy={strategy}",
            level = self.level,
            bits = self.window_bits,
            container = self.container.tag(),
            mem = self.mem_level,
            strategy = strategy_name(self.strategy),
        )
    }
}

/// The `Z_*` name of a strategy code, for diagnostics and benchmark ids.
///
/// A `_` arm rather than an exhaustive match, because the argument is a `c_int` a caller could set
/// to anything and `deflateInit2_` -- not this function -- is what rejects an invalid one.
const fn strategy_name(strategy: c_int) -> &'static str {
    match strategy {
        Z_DEFAULT_STRATEGY => "default",
        Z_FILTERED => "filtered",
        Z_HUFFMAN_ONLY => "huffman",
        Z_RLE => "rle",
        Z_FIXED => "fixed",
        _ => "unknown",
    }
}

// =================================================================================================
//  How the encoder is fed
// =================================================================================================

/// The reference driver's chunk size: `0x8000`, that is 32768 bytes.
///
/// `contrib/testzlib/testzlib.c` L147-L148 sets both `BlockSizeCompress` and
/// `BlockSizeUncompress` to `0x8000`, and the compression loop at L204-L213 offers exactly that
/// much input and output per round. It is also the 32 `KiB` window size, which makes it the most
/// natural chunk for an encoder to receive.
const CHUNK_DEFAULT: usize = 0x8000;

/// A deliberately tiny chunk, to expose per-call overhead.
///
/// 16 bytes: far below anything the encoder can amortize, so this row measures the cost of crossing
/// the C ABI, re-entering the resumable state machine and flushing a block rather than the cost of
/// compressing. It is also the size at which AAP §0.3.2.4's codegen lever was measured at roughly a
/// 10% gain, which makes it the row that lever should move.
const CHUNK_SMALL: usize = 16;

/// A kilobyte: the smallest chunk at which per-call overhead is comfortably amortized, and a common
/// size for a caller feeding a socket or a file in pieces.
const CHUNK_MEDIUM: usize = 1024;

/// How one encode is driven.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Feeding {
    /// One `deflate(Z_FINISH)` call with the whole input and a `deflateBound`-sized output buffer.
    ///
    /// The encoder's own fast path and the shape `deflateBound` is documented for (`zlib.h`
    /// L768-L774): one pass, one block sequence, no intermediate flush. It is measured separately
    /// rather than mixed into the chunked numbers because a `Z_SYNC_FLUSH` costs a block boundary
    /// and an empty stored block that a single-pass encode never pays.
    SingleShot,
    /// The reference driver's loop, at the given chunk size.
    ///
    /// Faithful to `contrib/testzlib/testzlib.c` L204-L213: `Z_FINISH` on the round that offers the
    /// last of the input and `Z_SYNC_FLUSH` on every earlier round.
    Chunked(usize),
}

impl Feeding {
    /// The short, filesystem-safe token this feeding mode contributes to a benchmark id.
    fn tag(self) -> String {
        match self {
            Self::SingleShot => String::from("single"),
            Self::Chunked(chunk) => format!("chunk{chunk}"),
        }
    }

    /// How many `deflate` rounds this mode will make for `len` bytes of input.
    ///
    /// Used only to size the output buffer -- every `Z_SYNC_FLUSH` round adds a bounded amount of
    /// framing that `deflateBound` does not account for. See [`Prepared::output_len`] for the
    /// derivation. At least one, because a zero-length input is still one `Z_FINISH` call.
    fn rounds(self, len: usize) -> usize {
        match self {
            Self::SingleShot => 1,
            Self::Chunked(chunk) => {
                if chunk == 0 {
                    1
                } else {
                    len.div_ceil(chunk).max(1)
                }
            }
        }
    }
}

/// The feeding mode used wherever the feeding mode is held fixed so another axis can vary.
///
/// Chunked at 32768, because that is what the reference driver does and what a streaming caller
/// does; the single-shot fast path is the special case, not the norm.
const DEFAULT_FEEDING: Feeding = Feeding::Chunked(CHUNK_DEFAULT);

/// Every feeding mode, in decreasing order of how much work each call is given.
const FEEDINGS: [Feeding; 4] = [
    Feeding::SingleShot,
    Feeding::Chunked(CHUNK_DEFAULT),
    Feeding::Chunked(CHUNK_MEDIUM),
    Feeding::Chunked(CHUNK_SMALL),
];

// =================================================================================================
//  Tier 1 -- the committed minimal corpus
// =================================================================================================

/// The committed fixtures the throughput groups compress, by exact filename.
///
/// `corpus/README.md` pins the inventory and requires that fixtures be loaded by exact name: there
/// is no globbing and no directory scan, so a name that does not match is a missing file rather than
/// a skipped case. These five are the ones whose *encode* profiles differ:
///
/// * `repetitive.bin` (16 `KiB`) -- long matches, so `longest_match` (`deflate.c` L1389) saturates
///   `nice_length` early and `max_chain` decides the rate.
/// * `random.bin` (8 `KiB`) -- incompressible, so no match is ever found, every chain is walked to
///   its end, and `trees.c` L1047-L1048 selects stored blocks.
/// * `text.txt` (45 bytes) -- natural-language text, and the `Z_TEXT` arm of `detect_data_type`
///   (`trees.c` L966).
/// * `binary.bin` (23 bytes) -- the block-list arm of the same heuristic, and the shortest row in
///   the throughput groups.
/// * `window_boundary.bin` (65 `KiB`) -- the only committed fixture past the 32 `KiB` window, and
///   therefore the only real-data case that drives `fill_window` (`deflate.c` L252) and
///   `slide_hash` (L187) across a boundary.
///
/// The five degenerate and short fixtures are in [`DEGENERATE_FIXTURE_NAMES`] instead, because a
/// zero-byte payload has no rate and belongs in a latency group.
const FIXTURE_NAMES: [&str; 5] = [
    "repetitive.bin",
    "random.bin",
    "text.txt",
    "binary.bin",
    "window_boundary.bin",
];

/// The fixtures the lifecycle group uses, smallest first.
///
/// The full stream lifecycle is dominated by `deflateInit2_`'s allocation -- a 32 `KiB` window, the
/// `prev` and `head` arrays and the symbol buffers -- and `deflateEnd`'s free, so the interesting
/// axis here is payload size rather than content class: `hello.bin` is 14 bytes and is almost
/// entirely setup cost, while `window_boundary.bin` at 65 `KiB` is almost entirely encode.
/// `hello.bin` is `test/example.c`'s own payload (L35 plus its `NUL`), which makes it the smallest
/// payload the existing suite has always fed.
const LIFECYCLE_FIXTURE_NAMES: [&str; 4] = [
    "hello.bin",
    "binary.bin",
    "repetitive.bin",
    "window_boundary.bin",
];

/// The fixtures whose size makes them worth using when one axis is being varied and the others are
/// held fixed.
///
/// Two, not five: the container, feeding, `memLevel` and strategy groups multiply out, and a
/// five-fixture cross product would quadruple the run time to restate what the level group already
/// shows. `repetitive.bin` is the long-match case and `window_boundary.bin` is the past-the-window
/// case, which are the two shapes those axes actually interact with.
const AXIS_FIXTURE_NAMES: [&str; 2] = ["repetitive.bin", "window_boundary.bin"];

/// The degenerate and very short fixtures, measured as latency rather than as throughput.
///
/// Ordered by length. Each is here for a reason `corpus/README.md` states:
///
/// * `empty.bin` (0 bytes) -- the empty stored block, and the empty-input classification boundary
///   (`doc/txtvsbin.txt` L48-L51, `trees.c` L987-L990). ★ It is the reason this group sets no
///   `Throughput`: `Throughput::Bytes(0)` is a divisor of zero, and a criterion group's throughput
///   setting is sticky across rows, so a zero-length row mixed into a throughput group would
///   silently inherit the previous row's divisor.
/// * `single_byte.bin` (1 byte) -- the degenerate Huffman case, where one distinct symbol forces the
///   two-code fixup at `trees.c` L650-L661.
/// * `dictionary.bin` (6 bytes) and `gray_list.bin` (6 bytes) -- `test/example.c` L40's preset
///   dictionary, and the fixture that reaches the final fall-through of `detect_data_type`.
/// * `hello.bin` (14 bytes) -- `test/example.c` L35, the payload the existing C suite compresses.
const DEGENERATE_FIXTURE_NAMES: [&str; 5] = [
    "empty.bin",
    "single_byte.bin",
    "dictionary.bin",
    "gray_list.bin",
    "hello.bin",
];

/// The fixture the memory gate and the bound group measure.
///
/// One fixture, and it must be this one: the memory gate is about how large a *stream* is, so the
/// input has to be long enough to force the encoder to fill and slide its window rather than to sit
/// in the initial lookahead. `window_boundary.bin` at 65 `KiB` is the only committed fixture past
/// the 32 `KiB` window (`corpus/README.md`), so it is the only one that reaches
/// `fill_window`/`slide_hash` and therefore the only one whose high-water mark reflects a fully
/// exercised stream.
const MEMORY_FIXTURE_NAME: &str = "window_boundary.bin";

/// `<this crate>/corpus/minimal`, resolved from the manifest directory at compile time.
///
/// ★ `CARGO_MANIFEST_DIR` is the **host package's** directory for a bench target -- that is
/// `crates/zlib-rs-differential`, the crate whose manifest carries the `[[bench]]` entry that
/// attaches this file -- and not the `benches/` directory the file sits in. Never an absolute path
/// baked into the source, and never derived from the current directory, which cargo does not
/// guarantee for a bench binary.
fn minimal_corpus_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("corpus")
        .join("minimal")
}

/// The repository root, which is `<CARGO_MANIFEST_DIR>/../..`.
///
/// The only consumer is [`silesia_dir`]'s default branch. Kept as its own named function because the
/// two `..` components are the other half of the `CARGO_MANIFEST_DIR` surprise described on
/// [`minimal_corpus_dir`], and a reader should find both facts stated once each rather than inlined
/// at a use site.
fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

/// One loaded input: the name a benchmark id will carry, and the bytes to compress.
#[derive(Debug)]
struct Fixture {
    /// The exact filename, used verbatim as the fixture component of every benchmark id.
    name: &'static str,
    /// The uncompressed bytes, exactly as committed.
    bytes: Vec<u8>,
}

/// Every fixture from `names` that is present, loaded fresh.
///
/// Absence is not an error here. A fixture that cannot be read is named on stderr and left out of
/// the returned set, so the run continues and the remaining cases still produce numbers.
/// `corpus/README.md` treats a missing committed fixture as a repository problem rather than an
/// optional input, and this diagnostic says so; it is the correctness suites under `tests/` that
/// turn it into a failure.
///
/// Unlike `benches/inflate_bench.rs`, an **empty** fixture is loaded rather than rejected: an
/// encoder has well-defined behaviour on zero bytes and `empty.bin` is one of the latency cases
/// [`DEGENERATE_FIXTURE_NAMES`] measures. It is the group that decides whether a length can carry a
/// throughput annotation, not the loader.
fn load_fixtures(names: &[&'static str]) -> Vec<Fixture> {
    let dir = minimal_corpus_dir();
    let mut loaded = Vec::with_capacity(names.len());

    for name in names {
        let path = dir.join(name);
        match fs::read(&path) {
            Ok(bytes) => loaded.push(Fixture { name, bytes }),
            Err(error) => {
                eprintln!(
                    "{LOG_PREFIX} skipping fixture {name}: cannot read {}: {error}. \
                     corpus/README.md pins this exact filename, so a name that does not match is a \
                     missing file rather than a skipped case -- the remaining cases still run and \
                     nothing is downloaded.",
                    path.display()
                );
            }
        }
    }

    if loaded.is_empty() {
        eprintln!(
            "{LOG_PREFIX} no committed fixture could be read from {}; the affected groups will be \
             empty.",
            dir.display()
        );
    }

    loaded
}

/// The tier-1 corpus, loaded once for the whole process.
///
/// The [`OnceLock`] is what makes "reported once" literal: ten group functions ask for overlapping
/// fixture sets and none of them should reprint a diagnostic. All ten committed fixtures together
/// are under 92 `KiB`, so caching them costs nothing; the tier-2 corpus is hundreds of megabytes and
/// is deliberately *not* cached this way -- see [`silesia_members`].
fn tier1_corpus() -> &'static [Fixture] {
    static CACHE: OnceLock<Vec<Fixture>> = OnceLock::new();

    CACHE.get_or_init(|| {
        // The union of the four name lists, with FIXTURE_NAMES first so that the gate group's
        // fixtures are loaded and reported before any other.
        let mut names: Vec<&'static str> = Vec::with_capacity(
            FIXTURE_NAMES.len()
                + LIFECYCLE_FIXTURE_NAMES.len()
                + DEGENERATE_FIXTURE_NAMES.len()
                + 1,
        );
        for name in FIXTURE_NAMES
            .into_iter()
            .chain(LIFECYCLE_FIXTURE_NAMES)
            .chain(AXIS_FIXTURE_NAMES)
            .chain(DEGENERATE_FIXTURE_NAMES)
            .chain([MEMORY_FIXTURE_NAME])
        {
            if !names.contains(&name) {
                names.push(name);
            }
        }

        load_fixtures(&names)
    })
}

/// The loaded fixtures named by `names`, in the order `names` gives them.
///
/// A name with no loaded fixture behind it is simply absent from the result: [`load_fixtures`] has
/// already explained why on stderr, and repeating it per group would bury the explanation in noise.
fn selected(names: &[&'static str]) -> Vec<&'static Fixture> {
    let corpus = tier1_corpus();
    names
        .iter()
        .filter_map(|name| corpus.iter().find(|fixture| fixture.name == *name))
        .collect()
}

/// The one fixture [`MEMORY_FIXTURE_NAME`] names, or `None` with a note.
fn memory_fixture() -> Option<&'static Fixture> {
    let found = selected(&[MEMORY_FIXTURE_NAME]).first().copied();
    if found.is_none() {
        eprintln!(
            "{LOG_PREFIX} skipping the memory and bound groups: the committed fixture \
             {MEMORY_FIXTURE_NAME} could not be read, and it is the only one past the 32 KiB window."
        );
    }
    found
}

// =================================================================================================
//  Tier 2 -- Silesia, opt-in and never fetched
// =================================================================================================

/// The twelve members `corpus/README.md` pins as the Silesia inventory, in its tabulated order.
///
/// `corpus/fetch_silesia.sh` enforces exactly this list after extraction through its
/// `ZLIB_RS_SILESIA_MEMBERS` default, and rejects an archive that is missing a member or carries an
/// extra one -- so an archive that is not this corpus is refused rather than measured. This file
/// consumes the same list, by exact name, so that it and `benches/inflate_bench.rs` never disagree
/// about what the corpus is.
#[rustfmt::skip]
const SILESIA_MEMBERS: [&str; 12] = [
    "dickens", "mozilla", "mr",      "nci",     "ooffice", "osdb",
    "reymont", "sao",     "samba",   "webster", "x-ray",   "xml",
];

/// The environment variable that overrides where the Silesia corpus lives.
const SILESIA_DIR_VAR: &str = "ZLIB_RS_SILESIA_DIR";

/// Where to obtain the corpus, quoted in the skip note so a reader does not have to go looking.
const SILESIA_SCRIPT: &str = "crates/zlib-rs-differential/corpus/fetch_silesia.sh";

/// The Silesia corpus directory, by the one canonical resolution rule.
///
/// `corpus/README.md`'s "The Silesia tier: path contract" publishes exactly two steps, and
/// `corpus/fetch_silesia.sh`'s `resolve_destination` implements the same two:
///
/// 1. `$ZLIB_RS_SILESIA_DIR`, when it is set and non-empty.
/// 2. Otherwise `<repo-root>/target/silesia`.
///
/// An empty value counts as unset, which is what makes `ZLIB_RS_SILESIA_DIR= cargo bench` mean "use
/// the default" rather than "use the filesystem root" -- the same reading the script's
/// `${ZLIB_RS_SILESIA_DIR:-}` default gives it. The default sits under `target/` because the root
/// `.gitignore` ignores `/target/`, so a fetched corpus can never be committed by accident.
///
/// This is byte-for-byte the rule `benches/inflate_bench.rs` implements, deliberately: the two
/// suites must never disagree about where the corpus lives. This function reads an environment
/// variable and joins paths. It does not create the directory, probe the network, or spawn anything
/// -- and neither does anything else in this file.
fn silesia_dir() -> PathBuf {
    match std::env::var(SILESIA_DIR_VAR) {
        Ok(value) if !value.is_empty() => PathBuf::from(value),
        Ok(_) | Err(_) => repository_root().join("target").join("silesia"),
    }
}

/// The Silesia member files that are present, as paths, or an empty vector with one note.
///
/// The canonical [`SILESIA_MEMBERS`] names are resolved by exact name first, exactly as tier 1 is. A
/// directory holding some of the twelve is measured over those; a directory holding none of them but
/// holding other readable files is measured over the first twelve of those, sorted, with a note
/// saying plainly that it is not the pinned inventory -- that keeps a hand-populated or partially
/// extracted directory usable for a local throughput run without ever pretending it is the corpus
/// AAP §0.8.4 names.
///
/// **Absence is normal, not an error.** `corpus/README.md` is explicit: a benchmark run without a
/// fetched corpus reports, skips and exits zero, and under no circumstances downloads. This function
/// returns an empty vector and prints one note naming [`SILESIA_SCRIPT`]; the caller skips its group
/// and the run still succeeds. The note is printed once per process.
fn silesia_members() -> &'static [PathBuf] {
    static CACHE: OnceLock<Vec<PathBuf>> = OnceLock::new();

    CACHE.get_or_init(|| {
        let dir = silesia_dir();

        let canonical: Vec<PathBuf> = SILESIA_MEMBERS
            .iter()
            .map(|member| dir.join(member))
            .filter(|path| path.is_file())
            .collect();

        if !canonical.is_empty() {
            eprintln!(
                "{LOG_PREFIX} Silesia: {} of the {} pinned members found under {}.",
                canonical.len(),
                SILESIA_MEMBERS.len(),
                dir.display()
            );
            return canonical;
        }

        let salvaged = salvage_silesia_dir(&dir);
        if salvaged.is_empty() {
            eprintln!(
                "{LOG_PREFIX} Silesia not present under {} -- skipping the tier-2 group. This is \
                 normal and not an error: tier 1 covers every correctness gate and this run exits \
                 zero. To measure the corpus AAP §0.8.4 names, fetch it yourself with \
                 {SILESIA_SCRIPT} (opt-in, run by a human, never by cargo or CI), or point \
                 {SILESIA_DIR_VAR} at a directory that already holds it.",
                dir.display()
            );
        }

        salvaged
    })
}

/// Up to twelve readable regular files from `dir`, sorted, when none of the pinned members is there.
///
/// The sort is what makes the selection deterministic across runs and machines; the cap is what
/// keeps a directory that happens to hold thousands of files from turning a benchmark into an
/// afternoon. Hidden entries are skipped so that an editor's or a fetcher's bookkeeping file is
/// never measured as though it were data.
fn salvage_silesia_dir(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };

    let mut found: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .filter(|path| {
            path.file_name()
                .and_then(OsStr::to_str)
                .is_some_and(|name| !name.starts_with('.'))
        })
        .collect();

    if found.is_empty() {
        return found;
    }

    found.sort();
    found.truncate(SILESIA_MEMBERS.len());

    eprintln!(
        "{LOG_PREFIX} Silesia: none of the {} pinned members is present under {}, but {} other \
         readable file(s) are -- measuring those instead. This is NOT the inventory \
         corpus/README.md pins and {SILESIA_SCRIPT} enforces, so the resulting numbers are a local \
         signal and not the AAP §0.8.4 Silesia measurement.",
        SILESIA_MEMBERS.len(),
        dir.display(),
        found.len()
    );

    found
}

// =================================================================================================
//  Checked conversions and the version handshake
// =================================================================================================
//
//  Lengths are converted with `try_from`, never with `as`. Two reasons, and the second is the one
//  that matters: `clippy::pedantic` denies the cast family, and on a target where `uInt` is narrower
//  than `usize` an `as` cast would silently truncate a large case into a small one and report a
//  throughput figure for work that was never done. `uLong` is `c_ulong` -- eight bytes on LP64, four
//  on LLP64 Windows (AAP §0.6.3.3) -- so this is portability rather than pedantry.

/// `(int) sizeof(T)`, where `T` is **that side's own** `z_stream` mirror.
///
/// This is the second half of the handshake the `deflateInit2` macro arranges (`zlib.h`
/// L1935-L1938) and it must never be crossed between sides:
/// `zlib_rs_differential::oracle::z_stream` and the facade's `z_stream` are distinct Rust types that
/// happen to have identical layout, and each library compares the number it is given against its
/// *own* `sizeof`.
///
/// Checked rather than cast. On the impossible failure -- a `z_stream` larger than `INT_MAX`, which
/// would mean the ABI mirror had grown past anything `zlib.h` could describe -- it yields 0, and
/// every `*Init*_` entry point rejects 0 with `Z_VERSION_ERROR` because 0 cannot equal
/// `sizeof(z_stream)`. That is a self-reporting fallback rather than a panic, which is what a
/// benchmark should have.
fn stream_size<T>() -> c_int {
    c_int::try_from(size_of::<T>()).unwrap_or(0)
}

/// The version string both `deflateInit2_` calls are given: `ZLIB_VERSION` from `zlib.h` L44,
/// verbatim.
///
/// This is precisely what the C macro passes -- the *caller's* compile-time constant -- so passing it
/// to the oracle exercises the reference's real major-version check rather than side-stepping it. It
/// is a `&CStr` constant with static storage duration, so `as_ptr` yields a `NUL`-terminated pointer
/// that outlives every call; nothing allocates a `CString`.
///
/// Be precise about what that check is, because it is narrower than it looks. `deflate.c` L394
/// compares **only `version[0]`** -- the major digit -- against the library's own. So a wrong major
/// such as `"2.3.2.1-motley"` makes every `deflateInit2_` answer `Z_VERSION_ERROR` (−6), while the
/// shorthand `"1.3.2"` is *accepted* because its major digit agrees. The shorthand is still wrong,
/// for a different reason: it is not the string this library reports, and `zlibVersion` and the
/// `test/example.c` startup check are about identity rather than about the init gate. Passing the
/// `zlib.h` L44 constant verbatim is what a real caller does and is the only spelling that is right
/// on both counts. [`report_configuration`] prints the reference's own `c_zlibVersion()` beside this
/// constant so that a drift is visible in the output rather than only in a failure.
fn version_ptr() -> *const c_char {
    ZLIB_VERSION.as_ptr()
}

/// The outcome of one encode: the status the library returned and how many bytes it produced.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Encode {
    /// The `deflate` return code from the call that ended the drive.
    status: c_int,
    /// Total bytes written into the output buffer across the whole drive.
    produced: usize,
}

// =================================================================================================
//  Zeroed streams
// =================================================================================================

/// A facade `z_stream` with every field zeroed and the allocator hooks left null.
///
/// The `memset(&zcpr, 0, sizeof(z_stream))` the reference driver performs before initialisation
/// (`contrib/testzlib/testzlib.c` L197). Built field by field rather than through
/// `MaybeUninit::zeroed`, which is both stricter and cheaper: it needs no `unsafe`, and it makes the
/// three members that actually carry meaning visible as the deliberate choices they are. `None` is
/// C's `Z_NULL` for the two hooks, which selects the library's own internal allocator -- and that is
/// what every throughput group wants, because the instrumented allocator of [`AllocCounter`] is only
/// ever installed by the untimed memory pass.
///
/// `crates/libz-rs-sys` deliberately does not implement `Default` for `z_stream`, on the grounds
/// that a zeroed stream is not yet a valid one; this function is the local spelling of the same
/// thing, named so that its single purpose is obvious.
fn zeroed_port_stream() -> libz_rs_sys::z_stream {
    libz_rs_sys::z_stream {
        next_in: core::ptr::null(),
        avail_in: 0,
        total_in: 0,
        next_out: core::ptr::null_mut(),
        avail_out: 0,
        total_out: 0,
        msg: core::ptr::null(),
        state: core::ptr::null_mut(),
        zalloc: None,
        zfree: None,
        opaque: core::ptr::null_mut(),
        data_type: 0,
        adler: 0,
        reserved: 0,
    }
}

/// An oracle `z_stream` with every field zeroed.
///
/// The oracle's mirror does implement `Default`, and its documentation says so in the same terms:
/// `Z_NULL` is 0 for every field that has one, so an all-zero stream is exactly what the documented
/// initialisation sequence expects. Named explicitly rather than reached through
/// `Default::default()` so that the two sides read alike at every call site.
fn zeroed_oracle_stream() -> oracle::z_stream {
    oracle::z_stream::default()
}

// =================================================================================================
//  Window installation
// =================================================================================================
//
//  `contrib/testzlib/testzlib.c` L200-L201 sets `next_in` and `next_out` ONCE, before the loop,
//  because the library advances both itself as it consumes and produces. These two helpers do the
//  same, and the drivers below refresh only `avail_in` and `avail_out` per round -- which is what
//  makes the chunked driver a faithful reproduction rather than an approximation of it.
//
//  A zero-length input becomes a NULL `next_in`, the pairing `zlib.h` L91-L92 explicitly permits and
//  which `empty.bin` exercises. No encode in this suite is offered a zero-length output window,
//  because `deflate` answers `Z_BUF_ERROR` for `avail_out == 0` and a run that tripped that would be
//  measuring the harness.

/// Point the port's stream at `input` and `out`, leaving both `avail_*` counts to the driver.
fn install_port(strm: &mut libz_rs_sys::z_stream, input: &[u8], out: &mut [u8]) {
    strm.next_in = if input.is_empty() {
        core::ptr::null()
    } else {
        input.as_ptr()
    };
    strm.next_out = out.as_mut_ptr();
}

/// Point the oracle's stream at `input` and `out`. Identical rules; a different `z_stream` type.
fn install_oracle(strm: &mut oracle::z_stream, input: &[u8], out: &mut [u8]) {
    strm.next_in = if input.is_empty() {
        core::ptr::null()
    } else {
        input.as_ptr()
    };
    strm.next_out = out.as_mut_ptr();
}

// =================================================================================================
//  The instrumented allocator -- the memory gate's instrument
// =================================================================================================
//
//  A transcription of `test/infcover.c` L56-L200 rather than a reinvention of it, because that
//  allocator is already proven against this library's allocation patterns and AAP §0.6.4.6 names it
//  as the technique. What is kept:
//
//    * `len = count * (size_t) size` (L76), computed with `checked_mul` here because Rust will not
//      silently wrap;
//    * `malloc` then `memset(ptr, 0xa5, len)` (L84-L87) -- deliberately NON-zero, so that any code
//      path assuming zero-initialised memory is exposed. This is the whole point of the fill and it
//      costs nothing;
//    * the registry, the head-first search, and the `notlifo` / `rogue` tallies (L98-L150);
//    * `total` and `highwater` (L103-L105), which is what `mem_high` (L192-L197) reports and what
//      this file compares against the 1.15x gate;
//    * the installation order of `mem_setup` (L158-L173): `opaque` first, then `zalloc`, then
//      `zfree`, and all three BEFORE `deflateInit2_`.
//
//  What differs, and why:
//
//    * `mem_limit` (L176-L181) is absent. Its purpose is to force `Z_MEM_ERROR` at a controlled
//      point, which is a coverage technique for the error paths of `test/infcover.c` and has no
//      place in a measurement -- a stream that failed to allocate has no footprint to compare.
//    * `malloc`/`free` are replaced by `std::alloc::alloc`/`dealloc`, because AAP §0.7.1(i) forbids
//      adding `libc` to reach the C allocator and the Rust global allocator is the honest
//      equivalent. `dealloc` needs the `Layout` the block was created with, which C's `free` does
//      not, so the block carries its own payload length in a header -- see [`BLOCK_HEADER`].
//    * The registry is a `Vec` rather than an intrusive linked list. Same semantics, since the
//      "head" of `infcover`'s list is the most recent allocation and the last element of a `Vec`
//      pushed at the end is the same thing.

/// The alignment every block this allocator hands out is guaranteed to have.
///
/// 16 bytes, which is what a C `malloc` guarantees on every target this workspace supports and
/// therefore what the reference implementation's own allocations have. It matters because the blocks
/// zlib requests are arrays of `unsigned short` (`prev` and `head`), of `Bytef` (the window and the
/// pending buffer) and one `deflate_state` containing pointers and `ct_data` structures; over-aligning
/// is always sound, while under-aligning would not be.
///
/// Asserted against the largest primitive alignment Rust knows about on this target rather than
/// assumed, so that a hypothetical target with a stricter requirement is a build failure here rather
/// than a misaligned access somewhere in `deflate.c`.
const BLOCK_ALIGN: usize = 16;
const _: () = assert!(BLOCK_ALIGN >= align_of::<u128>());
const _: () = assert!(BLOCK_ALIGN >= align_of::<usize>());
const _: () = assert!(BLOCK_ALIGN.is_power_of_two());

/// The `Layout` a payload of `len` bytes is allocated and deallocated with.
///
/// C's `free(ptr)` needs nothing but the pointer; Rust's `dealloc(ptr, layout)` needs the *exact*
/// `Layout` the allocation was made with, and `zfree` is handed only a pointer. So the size has to be
/// recoverable from the pointer, and this allocator recovers it from the side table
/// [`AllocCounter::blocks`] -- which it has to maintain anyway, because
/// `test/infcover.c`'s non-`LIFO` and rogue tallies are lookups in exactly that table. One record
/// rather than two: a size-prefixed block would work equally well and would duplicate a fact the
/// registry already holds.
///
/// The scheme's invariants, stated once and relied on by every `// SAFETY:` comment below:
///
/// 1. Every pointer this allocator returns came from `std::alloc::alloc` with the `Layout` this
///    function produces for the requested `len`.
/// 2. [`AllocCounter::blocks`] holds that pointer paired with that same `len` for exactly as long as
///    the block is outstanding.
/// 3. [`tracked_free`] takes the entry out of the registry, calls this function on the `len` it
///    recorded, and so reconstructs a `Layout` identical to the one the block was created with.
/// 4. A pointer that is not in the registry did not come from rule 1. It is counted as a rogue free
///    and is **not** deallocated, because fabricating a `Layout` for a pointer of unknown provenance
///    would be undefined behaviour.
/// 5. The requested size can never overflow, because [`tracked_alloc`] computes `items * size` with
///    `checked_mul` and answers `NULL` -- zlib's documented allocation-failure signal -- if it wraps.
///
/// `len.max(1)` is the one adjustment: a zero-sized `Layout` is undefined behaviour to pass to
/// `alloc`, so a zero-byte request gets one byte it will never use, which is also what `malloc(0)`
/// does in the reference's own default allocator (`zutil.c` L215). `len` rather than the layout's size
/// is what the counter records, so the adjustment never inflates a reported high-water mark. The
/// alignment is [`BLOCK_ALIGN`] for every block, which is what makes step 3 exact.
fn block_layout(len: usize) -> Option<Layout> {
    Layout::from_size_align(len.max(1), BLOCK_ALIGN).ok()
}

/// The non-zero byte every freshly allocated block is filled with.
///
/// `test/infcover.c` L87 exactly. The value is not arbitrary and must not be changed to zero: its
/// entire purpose is that a port which silently assumed zero-initialised memory would be caught
/// here, as it is caught there. The reference's own default allocator is plain `malloc`
/// (`zutil.c` L215), which is uninitialised rather than zeroed, so this is the stricter of the two
/// and neither implementation may depend on the difference.
const POISON: u8 = 0xa5;

/// One live block, as `test/infcover.c`'s `struct mem_item` (L56-L60) records it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Block {
    /// The payload pointer handed to zlib -- that is, `base + BLOCK_HEADER`.
    payload: *mut u8,
    /// The payload length, which is also what the block's header holds.
    len: usize,
}

/// The `zalloc`/`zfree` bookkeeping of one stream: `test/infcover.c`'s `struct mem_zone`
/// (L63-L68).
///
/// One instance per stream, never shared between the two implementations, and never touched from
/// more than one thread -- see the safety note on [`tracked_alloc`].
#[derive(Debug, Default)]
struct AllocCounter {
    /// Every block currently outstanding, pushed at the end. The last element is `infcover`'s
    /// list head, so a `LIFO` free matches there.
    blocks: Vec<Block>,
    /// Bytes currently outstanding: `mem_zone::total`.
    total: usize,
    /// The largest value `total` ever reached: `mem_zone::highwater`, and the number the memory
    /// gate compares.
    highwater: usize,
    /// How many allocation requests were satisfied.
    allocations: usize,
    /// How many frees were performed. Must equal `allocations` once the stream has been ended.
    frees: usize,
    /// Frees that did not match the most recent outstanding block: `mem_zone::notlifo`.
    notlifo: usize,
    /// Frees of a pointer this counter never handed out: `mem_zone::rogue`.
    rogue: usize,
    /// Allocation requests refused because a size computation would have overflowed, or because the
    /// global allocator returned null. Reported so that a zero high-water mark is never mistaken for
    /// a frugal implementation.
    refusals: usize,
}

impl AllocCounter {
    /// An empty counter. `Default` supplies it; this name exists so call sites read deliberately.
    fn new() -> Self {
        Self::default()
    }

    /// Record a satisfied allocation of `len` bytes at `payload`, updating `total` and `highwater`.
    ///
    /// `test/infcover.c` L98-L105: push at the head of the list, add to the total, and raise the
    /// high-water mark if the total has grown past it. `saturating_add` rather than `+` because a
    /// benchmark must not panic; an overflow here is impossible in practice, since `total` cannot
    /// exceed the address space.
    fn record_alloc(&mut self, payload: *mut u8, len: usize) {
        self.blocks.push(Block { payload, len });
        self.total = self.total.saturating_add(len);
        self.highwater = self.highwater.max(self.total);
        self.allocations = self.allocations.saturating_add(1);
    }

    /// Look up and remove `payload` from the registry, returning its length.
    ///
    /// `test/infcover.c` L123-L150: the most recent block is checked first and matches without
    /// comment; a match anywhere else is a non-`LIFO` free and increments `notlifo`; no match at all
    /// is a rogue free and increments `rogue`. Returning `None` for the rogue case is what stops
    /// [`tracked_free`] from deallocating a pointer whose provenance it does not know.
    fn take_block(&mut self, payload: *mut u8) -> Option<usize> {
        let found = self
            .blocks
            .iter()
            .rposition(|block| block.payload == payload);

        let Some(index) = found else {
            self.rogue = self.rogue.saturating_add(1);
            return None;
        };

        if index + 1 != self.blocks.len() {
            self.notlifo = self.notlifo.saturating_add(1);
        }

        let block = self.blocks.remove(index);
        self.total = self.total.saturating_sub(block.len);
        self.frees = self.frees.saturating_add(1);
        Some(block.len)
    }

    /// Record an allocation this counter refused to satisfy.
    fn record_refusal(&mut self) {
        self.refusals = self.refusals.saturating_add(1);
    }

    /// Whether every block handed out has come back, in order, and nothing foreign came back at all.
    ///
    /// This is `mem_done`'s verdict (`test/infcover.c` L200-L240) reduced to a boolean: no leftover
    /// allocation, a balanced count, no non-`LIFO` free, no rogue free and no refusal.
    fn is_clean(&self) -> bool {
        self.blocks.is_empty()
            && self.total == 0
            && self.allocations == self.frees
            && self.notlifo == 0
            && self.rogue == 0
            && self.refusals == 0
    }

    /// Print the `ALLOC-BALANCE` line for one side of one case.
    ///
    /// Reported, never asserted: a dirty verdict means this file found a leak, and the nightly
    /// `AddressSanitizer` job is where that becomes a failure.
    fn report_balance(&self, label: &str, case: &str) {
        let verdict = if self.is_clean() { "clean" } else { "dirty" };
        eprintln!(
            "{LOG_PREFIX} {BALANCE_KEY} side={label} case={case} allocations={allocations} \
             frees={frees} live_bytes={total} notlifo={notlifo} rogue={rogue} \
             refusals={refusals} verdict={verdict}",
            allocations = self.allocations,
            frees = self.frees,
            total = self.total,
            notlifo = self.notlifo,
            rogue = self.rogue,
            refusals = self.refusals,
        );
    }
}

/// Owns one [`AllocCounter`] at a stable address and hands out the `opaque` pointer for it.
///
/// The counter is boxed rather than held inline for a reason that is about aliasing rather than about
/// convenience: C calls back into [`tracked_alloc`] *during* a `deflateInit2_` or `deflateEnd` call
/// that is itself being made through a raw pointer to the stream. Keeping the counter in its own
/// allocation means the hook's transient `&mut AllocCounter` can never overlap the borrow the call
/// site holds on the stream, because the two are different allocations.
#[derive(Debug)]
struct TrackedAllocator {
    /// The counter. Boxed, so its address is stable across moves of this struct.
    counter: Box<AllocCounter>,
}

impl TrackedAllocator {
    /// A fresh counter with nothing outstanding.
    fn new() -> Self {
        Self {
            counter: Box::new(AllocCounter::new()),
        }
    }

    /// The `opaque` value to install on a stream: the address of the boxed counter.
    ///
    /// `addr_of_mut!` through the `Box` rather than `&mut *self.counter as *mut _`, so that no Rust
    /// mutable reference is materialised here at all -- the pointer is taken straight from the place
    /// and only the hooks ever form a reference from it.
    fn opaque(&mut self) -> *mut c_void {
        core::ptr::addr_of_mut!(*self.counter).cast::<c_void>()
    }

    /// The high-water mark in bytes: `test/infcover.c`'s `mem_high` (L192-L197).
    fn highwater(&self) -> usize {
        self.counter.highwater
    }

    /// Print this side's `ALLOC-BALANCE` line and answer whether the balance is clean.
    fn report_balance(&self, label: &str, case: &str) -> bool {
        self.counter.report_balance(label, case);
        self.counter.is_clean()
    }
}

/// zlib's `alloc_func`: allocate `items * size` bytes through the Rust global allocator, poison them,
/// and record the block.
///
/// `test/infcover.c`'s `mem_alloc` (L71-L109), with `malloc` replaced by `std::alloc::alloc` and the
/// size arithmetic checked. Answering `NULL` is zlib's documented allocation-failure signal, and it
/// is what this function does for every condition it cannot satisfy: an overflowing size, a
/// `Layout` the allocator will not accept, a null `opaque`, or a null from the global allocator.
///
/// This function is `extern "C"` and never `extern "C-unwind"`, so a panic crossing out of it would
/// abort rather than unwind into C. Nothing in the body can panic in any case: every arithmetic
/// operation is checked, the only allocation is guarded, and `Vec::push` is the sole operation that
/// could abort on allocation failure -- which is the same behaviour the Rust global allocator has
/// everywhere else in the process.
///
/// # Safety
///
/// `opaque` must be either null or the address of a live [`AllocCounter`] that outlives the stream it
/// was installed on, and that counter must not be reachable from any other thread for the duration
/// of the call. Both hold by construction here: the only writer is [`TrackedAllocator::opaque`], the
/// counter is boxed so its address is stable, and one stream is driven by one thread at a time.
unsafe extern "C" fn tracked_alloc(
    opaque: *mut c_void,
    items: c_uint,
    size: c_uint,
) -> *mut c_void {
    if opaque.is_null() {
        return core::ptr::null_mut();
    }

    // SAFETY: `opaque` is non-null, as just checked, and this function's contract states that a
    // non-null `opaque` is the address of a live `AllocCounter` that outlives the stream and is
    // reached from one thread at a time. The reference is transient -- it is created here, used
    // below, and dropped when this call returns -- and the counter is a separate allocation from the
    // `z_stream` the caller passed, so it cannot alias any borrow the call site holds.
    let counter = unsafe { &mut *opaque.cast::<AllocCounter>() };

    // `len = count * (size_t) size` -- test/infcover.c L76, but checked twice over: `try_from`
    // because `usize` is not `From<c_uint>` on a 16-bit target, and `checked_mul` because zlib's own
    // callers pass small products and a wrap would hand back a block smaller than the one requested.
    let (Ok(items), Ok(size)) = (usize::try_from(items), usize::try_from(size)) else {
        counter.record_refusal();
        return core::ptr::null_mut();
    };

    let Some(len) = items.checked_mul(size) else {
        counter.record_refusal();
        return core::ptr::null_mut();
    };

    let Some(layout) = block_layout(len) else {
        counter.record_refusal();
        return core::ptr::null_mut();
    };

    // SAFETY: `layout` has a non-zero size -- `block_layout` raises a zero request to one byte
    // precisely so that this holds -- and a power-of-two alignment, which is the whole of `alloc`'s
    // contract. The returned pointer is checked for null immediately below, and nothing is read or
    // written through it before that check.
    let payload = unsafe { alloc(layout) };
    if payload.is_null() {
        counter.record_refusal();
        return core::ptr::null_mut();
    }

    // SAFETY: `payload` is the start of a freshly allocated block of at least `len` bytes, so `len`
    // bytes are writable there, and `u8` has no alignment requirement. This is
    // `memset(ptr, 0xa5, len)` -- test/infcover.c L87 -- and it must not become a zero fill: a port
    // that assumed zero-initialised memory is exactly what it exists to expose. `write_bytes` with a
    // zero count is a documented no-op, which is the `items * size == 0` case.
    unsafe { core::ptr::write_bytes(payload, POISON, len) };

    // Invariant 2: the registry records the pointer with the length its layout was built from, before
    // the pointer escapes to the caller.
    counter.record_alloc(payload, len);
    payload.cast::<c_void>()
}

/// zlib's `free_func`: look `address` up in the registry and return it to the Rust global allocator.
///
/// `test/infcover.c`'s `mem_free` (L112-L154), with one deliberate departure. C's version calls
/// `free(ptr)` even for a pointer it did not recognise, because `free` needs no size and the C
/// standard library can cope; this version does **not**, because `dealloc` needs the exact `Layout`
/// and reconstructing one for a pointer of unknown provenance would be undefined behaviour. A rogue
/// free is therefore counted and dropped, and `verdict=dirty` on the `ALLOC-BALANCE` line is how it
/// is reported.
///
/// # Safety
///
/// `opaque` must satisfy [`tracked_alloc`]'s contract. `address` must be null, or a pointer this same
/// counter returned from [`tracked_alloc`] and has not yet been given here -- which is precisely
/// what zlib's own contract promises, since a block obtained from a stream's `zalloc` is released
/// only through that same stream's `zfree` (AAP §0.6.1 category 4).
unsafe extern "C" fn tracked_free(opaque: *mut c_void, address: *mut c_void) {
    if opaque.is_null() || address.is_null() {
        return;
    }

    // SAFETY: as for `tracked_alloc` -- `opaque` is non-null and, by this function's contract, the
    // address of the same live, single-threaded `AllocCounter`. The reference is transient and the
    // counter is a separate allocation from the `z_stream`.
    let counter = unsafe { &mut *opaque.cast::<AllocCounter>() };

    let payload = address.cast::<u8>();
    let Some(len) = counter.take_block(payload) else {
        // Invariant 4: a rogue free. Counted by `take_block` and deliberately not deallocated -- see
        // the note above on the departure from `mem_free`.
        return;
    };

    let Some(layout) = block_layout(len) else {
        // Unreachable in practice -- `block_layout` already answered `Some` for this same `len`
        // before the block existed -- but a benchmark answers an impossible condition by leaking one
        // block rather than by panicking.
        counter.record_refusal();
        return;
    };

    // SAFETY: invariant 3 of the block scheme, and it is the registry lookup above that establishes
    // it. That lookup found `payload` in the table, which by invariant 1 means `tracked_alloc`
    // obtained it from `alloc` with `block_layout(len)` for the very `len` just returned -- so
    // `layout` is identical to the allocation's own, which is exactly `dealloc`'s requirement. The
    // lookup also *removed* the entry, so this runs at most once per block and cannot double free;
    // and a pointer that was never in the table took the branch above instead.
    unsafe { dealloc(payload, layout) };
}

// =================================================================================================
//  RAII stream guards -- the whole reason no path can leak
// =================================================================================================
//
//  ★ A GUARD MUST NOT BE MOVED ONCE IT IS LIVE, and this is a hard requirement rather than a
//  stylistic one. `deflate.c` L521 stores a pointer back to the stream inside the state block
//  (`s->strm = strm`) and L538's `deflateStateCheck` compares it by identity on entry to every other
//  entry point, so a stream that changed address after `deflateInit2_` would fail every subsequent
//  call with `Z_STREAM_ERROR`. The port applies the same rule for the same reason. So the discipline
//  is uniform: construct the guard, bind it to a local, and only then call `init` through `&mut`.
//  Never `init` inside an expression whose value is then moved.
//
//  The guards mirror `z_stream zcpr;` in the reference driver: an inline stream in the caller's
//  frame, not a heap allocation. Boxing would make moving safe but would add one global-allocator
//  round trip per stream that the C driver does not pay, which the lifecycle group exists to measure
//  precisely.
//
//  Every guard's `Drop` calls `deflateEnd`, so a skip path, an early return and a panicking unwind
//  all release the stream -- and a deflate stream is worth releasing: it owns a 32 KiB window, a
//  `prev` array, a `head` array and the symbol buffers, all sized from `windowBits` and `memLevel`.
//  That is what keeps this file clean under the nightly `AddressSanitizer` job, which is the
//  verification gate this crate does sit inside, and it is what makes the `ALLOC-BALANCE` line of the
//  memory pass meaningful.

/// The port's deflate stream, ended on every path by [`Drop`].
#[derive(Debug)]
struct PortDeflate {
    /// The stream itself. Must not move once [`PortDeflate::live`] is set -- see the section note.
    strm: libz_rs_sys::z_stream,
    /// Whether `deflateInit2_` succeeded and `deflateEnd` therefore owes a call.
    live: bool,
}

impl PortDeflate {
    /// A zeroed, not-yet-initialised stream. Bind the result to a local before calling `init`.
    fn new() -> Self {
        Self {
            strm: zeroed_port_stream(),
            live: false,
        }
    }

    /// Install the instrumented allocator, which must happen **before** `init`.
    ///
    /// `test/infcover.c`'s `mem_setup` (L158-L173) in the same order: `opaque` first, then the two
    /// hooks. `deflateInit2_` makes its very first allocation request through these, so installing
    /// them afterwards would leave the state block itself untracked and the high-water mark short by
    /// the largest single allocation the stream makes.
    fn install_allocator(&mut self, opaque: *mut c_void) {
        self.strm.opaque = opaque;
        self.strm.zalloc = Some(tracked_alloc);
        self.strm.zfree = Some(tracked_free);
    }

    /// `deflateInit2_(strm, level, Z_DEFLATED, windowBits, memLevel, strategy, ZLIB_VERSION,
    /// sizeof(z_stream))` -- `zlib.h` L1907-L1910.
    fn init(&mut self, config: Config) -> c_int {
        // SAFETY: `strm` is a live, zeroed, correctly-typed local of this side's own `z_stream` type
        // that outlives the call; `version` points at `ZLIB_VERSION`, a NUL-terminated `'static`
        // byte string; and `stream_size` is `size_of` of that same type, never the other side's.
        // `deflateInit2_` reads at most one byte of `version` after testing it for null, allocates
        // through the stream's own `zalloc` -- either null, which selects the library's internal
        // allocator, or the pair `install_allocator` set, whose contract the caller has satisfied --
        // writes `state`, and touches neither window, which is why a C caller may leave
        // `next_in`/`next_out` unset until after the call.
        let status = unsafe {
            libz_rs_sys::deflateInit2_(
                core::ptr::addr_of_mut!(self.strm),
                config.level,
                Z_DEFLATED,
                config.window_bits,
                config.mem_level,
                config.strategy,
                version_ptr(),
                stream_size::<libz_rs_sys::z_stream>(),
            )
        };
        self.live = status == Z_OK;
        status
    }

    /// `deflateReset(strm)` -- `zlib.h` L737.
    ///
    /// Returns the stream to the state `deflateInit2_` left it in *without* reallocating: the window,
    /// the hash arrays and the pending buffer are kept and only the counters, the bit buffer and the
    /// tree state are cleared. That is exactly why the steady-state group uses it -- what is then
    /// timed is the encode plus the cheap half of the lifecycle, and not the allocator.
    fn reset(&mut self) -> c_int {
        // SAFETY: as for `init`, minus the version handshake. `strm` is live and initialised at this
        // address, so `deflateReset` finds the state block `deflateInit2_` wrote and the identity
        // check on the state's back-pointer to the stream succeeds. It allocates nothing and touches
        // neither the input nor the output window.
        unsafe { libz_rs_sys::deflateReset(core::ptr::addr_of_mut!(self.strm)) }
    }

    /// `deflateBound(strm, sourceLen)` -- `zlib.h` L768 -- as a `usize`, or `None` if a conversion
    /// does not fit.
    fn bound(&mut self, source_len: usize) -> Option<usize> {
        let source = libz_rs_sys::uLong::try_from(source_len).ok()?;
        // SAFETY: `strm` is live and initialised at this address. `deflateBound` only reads the state
        // block -- and, for a gzip stream, the `gz_header` the state holds, which is null here
        // because this suite never calls `deflateSetHeader` -- and writes nothing. It touches neither
        // window, so no pointer needs to be installed first.
        let bound =
            unsafe { libz_rs_sys::deflateBound(core::ptr::addr_of_mut!(self.strm), source) };
        usize::try_from(bound).ok()
    }

    /// `deflateBound_z(strm, sourceLen)` -- the `size_t` form, `zlib.h` L769.
    fn bound_z(&mut self, source_len: usize) -> Option<usize> {
        let source = libz_rs_sys::z_size_t::try_from(source_len).ok()?;
        // SAFETY: as for `bound`; the only difference is the width of the argument and the result.
        // No conversion on the way out: both mirrors define `z_size_t` as `usize` unconditionally, so
        // this is already the type the caller wants and `try_from` would be a no-op the lint gate
        // rejects.
        Some(unsafe { libz_rs_sys::deflateBound_z(core::ptr::addr_of_mut!(self.strm), source) })
    }

    /// `deflateEnd(strm)` -- `zlib.h` L836 -- releasing every buffer the stream owns.
    ///
    /// Idempotent: the second call is a no-op rather than a double free, which is what lets [`Drop`]
    /// delegate here unconditionally.
    ///
    /// ★ **Take `&mut self`, and never `drop(self)`.** This method exists precisely so that a caller
    /// who needs the stream ended *before* the end of its scope -- the memory pass, which has to read
    /// the allocator balance after `deflateEnd` has returned every block -- can do it without moving
    /// the guard. `drop(guard)` takes the guard **by value**, which is a move, and a moved stream has a
    /// new address: `deflate.c` L521 stored the old one inside the state block and L538's
    /// `deflateStateCheck` compares it by identity, so `deflateEnd` would answer `Z_STREAM_ERROR` and
    /// free nothing at all. That failure is silent in every respect except an allocator balance, which
    /// is exactly how it was found here.
    fn end(&mut self) -> c_int {
        if !self.live {
            return Z_OK;
        }
        self.live = false;
        // SAFETY: `strm` is live, was initialised by `deflateInit2_` on this same address, and has not
        // moved since -- this method takes `&mut self` rather than `self` so that it cannot have.
        // `deflateEnd` frees the state and its buffers through the same allocator that produced them
        // and nulls `state`, and the `live` flag above makes this run at most once per stream, so a
        // double free is impossible.
        unsafe { libz_rs_sys::deflateEnd(core::ptr::addr_of_mut!(self.strm)) }
    }
}

impl Drop for PortDeflate {
    fn drop(&mut self) {
        self.end();
    }
}

/// The reference's deflate stream, ended on every path by [`Drop`].
#[derive(Debug)]
struct OracleDeflate {
    /// The stream itself. Must not move once [`OracleDeflate::live`] is set -- and here the
    /// requirement has teeth, because `deflate.c` L538 checks `s->strm` by identity.
    strm: oracle::z_stream,
    /// Whether `c_deflateInit2_` succeeded and `c_deflateEnd` therefore owes a call.
    live: bool,
}

impl OracleDeflate {
    /// A zeroed, not-yet-initialised stream. Bind the result to a local before calling `init`.
    fn new() -> Self {
        Self {
            strm: zeroed_oracle_stream(),
            live: false,
        }
    }

    /// Install the instrumented allocator before `init`. Same order and same reason as
    /// [`PortDeflate::install_allocator`].
    ///
    /// The two implementations' `alloc_func` and `free_func` aliases resolve to the same underlying
    /// `extern "C"` signature, which is why one pair of hooks serves both sides. The `z_stream`
    /// types do not, and are never crossed.
    fn install_allocator(&mut self, opaque: *mut c_void) {
        self.strm.opaque = opaque;
        self.strm.zalloc = Some(tracked_alloc);
        self.strm.zfree = Some(tracked_free);
    }

    /// `deflateInit2_` in the reference, reached as `c_deflateInit2_`.
    fn init(&mut self, config: Config) -> c_int {
        // SAFETY: as for `PortDeflate::init`, with `stream_size` taken from the ORACLE's own
        // `z_stream` mirror rather than the facade's -- the two are distinct Rust types and each
        // library compares the number against its own `sizeof`.
        let status = unsafe {
            oracle::c_deflateInit2_(
                core::ptr::addr_of_mut!(self.strm),
                config.level,
                Z_DEFLATED,
                config.window_bits,
                config.mem_level,
                config.strategy,
                version_ptr(),
                stream_size::<oracle::z_stream>(),
            )
        };
        self.live = status == Z_OK;
        status
    }

    /// `deflateReset` in the reference, reached as `c_deflateReset`.
    fn reset(&mut self) -> c_int {
        // SAFETY: as for `PortDeflate::reset`. `strm` is live, initialised at this address, and still
        // the address the state block's back-pointer holds.
        unsafe { oracle::c_deflateReset(core::ptr::addr_of_mut!(self.strm)) }
    }

    /// `c_deflateBound(strm, sourceLen)`.
    fn bound(&mut self, source_len: usize) -> Option<usize> {
        let source = oracle::uLong::try_from(source_len).ok()?;
        // SAFETY: as for `PortDeflate::bound`, against the reference's own type and entry point.
        let bound = unsafe { oracle::c_deflateBound(core::ptr::addr_of_mut!(self.strm), source) };
        usize::try_from(bound).ok()
    }

    /// `c_deflateBound_z(strm, sourceLen)`.
    fn bound_z(&mut self, source_len: usize) -> Option<usize> {
        let source = oracle::z_size_t::try_from(source_len).ok()?;
        // SAFETY: as for `PortDeflate::bound_z`, against the reference's own type and entry point.
        // `z_size_t` is `usize` in the oracle's mirror too, so the result needs no conversion.
        Some(unsafe { oracle::c_deflateBound_z(core::ptr::addr_of_mut!(self.strm), source) })
    }

    /// `c_deflateEnd(strm)`, with the same contract and the same `&mut self` requirement as
    /// [`PortDeflate::end`] -- and here the requirement is not merely defensive, because
    /// `deflate.c` L538 is the code that performs the identity check.
    fn end(&mut self) -> c_int {
        if !self.live {
            return Z_OK;
        }
        self.live = false;
        // SAFETY: as for `PortDeflate::end`, against the reference's own allocator. `strm` is live,
        // initialised at this address, and still the address the state block's back-pointer holds.
        unsafe { oracle::c_deflateEnd(core::ptr::addr_of_mut!(self.strm)) }
    }
}

impl Drop for OracleDeflate {
    fn drop(&mut self) {
        self.end();
    }
}

/// The reference's inflate stream, used only to round-trip a compressed result back to its input.
///
/// Verification only: no timed closure ever touches one of these. Decoding the *port's* output with
/// the *reference's* decoder is the Rust→C direction of the interoperability requirement in AAP
/// §0.8.1 directive 4, so the guard on the measurement doubles as a crossing on every case; and using
/// one decoder for both sides' output means a difference in the result is a difference in the encoder
/// rather than in the check.
#[derive(Debug)]
struct OracleInflate {
    /// The stream itself. Must not move once [`OracleInflate::live`] is set: `inflate.c` L94 checks
    /// `state->strm` by identity.
    strm: oracle::z_stream,
    /// Whether `c_inflateInit2_` succeeded and `c_inflateEnd` therefore owes a call.
    live: bool,
}

impl OracleInflate {
    /// A zeroed, not-yet-initialised stream. Bind the result to a local before calling `init`.
    fn new() -> Self {
        Self {
            strm: zeroed_oracle_stream(),
            live: false,
        }
    }

    /// `inflateInit2_` in the reference, reached as `c_inflateInit2_`.
    ///
    /// `window_bits` is the encoder's own, unchanged: 15 decodes a zlib stream, −15 a raw one and 31
    /// a gzip one, exactly as it selected them on the way in.
    fn init(&mut self, window_bits: c_int) -> c_int {
        // SAFETY: as for `OracleDeflate::init` -- a live zeroed local of the oracle's own type, the
        // `'static` NUL-terminated version string, and that type's own `size_of`. `inflateInit2_`
        // writes `state` and touches neither window.
        let status = unsafe {
            oracle::c_inflateInit2_(
                core::ptr::addr_of_mut!(self.strm),
                window_bits,
                version_ptr(),
                stream_size::<oracle::z_stream>(),
            )
        };
        self.live = status == Z_OK;
        status
    }

    /// One `inflate(Z_FINISH)` call over the whole of `input` into the whole of `out`.
    fn decode(&mut self, input: &[u8], out: &mut [u8]) -> Option<Encode> {
        let out_len = out.len();
        let avail_in = oracle::uInt::try_from(input.len()).ok()?;
        let avail_out = oracle::uInt::try_from(out_len).ok()?;

        install_oracle(&mut self.strm, input, out);
        self.strm.avail_in = avail_in;
        self.strm.avail_out = avail_out;

        // SAFETY: `avail_in` bytes are readable at `next_in` and `avail_out` bytes writable at
        // `next_out`, both installed immediately above from live slices that outlive this call, and
        // both counts are those slices' own lengths converted with `try_from`. `strm` is live,
        // initialised at this address, and has not moved since `c_inflateInit2_` wrote the
        // back-pointer `inflate.c` L94 checks by identity.
        let status = unsafe { oracle::c_inflate(core::ptr::addr_of_mut!(self.strm), Z_FINISH) };

        let left = usize::try_from(self.strm.avail_out).ok()?;
        Some(Encode {
            status,
            produced: out_len.checked_sub(left)?,
        })
    }
}

impl Drop for OracleInflate {
    fn drop(&mut self) {
        if !self.live {
            return;
        }
        self.live = false;
        // SAFETY: as for `OracleDeflate::end`. `strm` is live, was initialised by `c_inflateInit2_` on
        // this same address and has not moved -- every `OracleInflate` in this file is created,
        // used and dropped inside one function body, and is never passed to `drop` by value, which
        // would move it and defeat `inflate.c` L94's identity check. `inflateEnd` frees the state and
        // the window through the allocator that produced them and nulls `state`, so this runs at most
        // once per stream.
        unsafe { oracle::c_inflateEnd(core::ptr::addr_of_mut!(self.strm)) };
    }
}

// =================================================================================================
//  The encode drivers
// =================================================================================================
//
//  Four functions: two feeding modes on each side. They are written out per side rather than behind a
//  trait because `zlib_rs_differential::oracle::z_stream` and the facade's `z_stream` are distinct
//  Rust types with identical layout -- nothing is ever transmuted or aliased between them -- and
//  because a reader auditing one `unsafe` block should not have to follow a generic to find the type
//  it applies to.
//
//  None of them allocates, reads a file, prints, or compares anything: they are exactly the work the
//  criterion rows are supposed to be timing, and nothing else. A conversion that could not be
//  represented yields `None` silently, because the verification pass outside the timed region has
//  already established that it cannot happen for the case being measured.

/// One `deflate(Z_FINISH)` call with the whole input and the whole output buffer, through the port.
///
/// The single-pass shape `deflateBound` is documented for (`zlib.h` L768-L774): one call, one block
/// sequence, no intermediate flush and therefore none of the framing a `Z_SYNC_FLUSH` costs.
fn port_single_shot(stream: &mut PortDeflate, input: &[u8], out: &mut [u8]) -> Option<Encode> {
    let out_len = out.len();
    let avail_in = libz_rs_sys::uInt::try_from(input.len()).ok()?;
    let avail_out = libz_rs_sys::uInt::try_from(out_len).ok()?;

    install_port(&mut stream.strm, input, out);
    stream.strm.avail_in = avail_in;
    stream.strm.avail_out = avail_out;

    // SAFETY: `avail_in` bytes are readable at `next_in` and `avail_out` bytes writable at
    // `next_out`, both installed immediately above from live slices that outlive this call, and both
    // counts are those slices' own lengths converted with `try_from`. `strm` is live, initialised at
    // this address, and reset by the caller when the caller intends a fresh stream.
    let status = unsafe { libz_rs_sys::deflate(core::ptr::addr_of_mut!(stream.strm), Z_FINISH) };

    let left = usize::try_from(stream.strm.avail_out).ok()?;
    Some(Encode {
        status,
        produced: out_len.checked_sub(left)?,
    })
}

/// The reference driver's compression loop, through the port.
///
/// Faithful to `contrib/testzlib/testzlib.c` L204-L213: `next_in` and `next_out` are installed once
/// because the library advances them itself, and each round offers `min(remaining, chunk)` input and
/// a chunk of output space, calls `deflate` with `Z_FINISH` when this round's input is the last of it
/// and `Z_SYNC_FLUSH` otherwise, and continues while the status is `Z_OK`.
///
/// Two guards the reference loop does not need. `avail_out` is clamped to the space that remains,
/// rather than provisioned by over-allocating an extra chunk (`testzlib.c` L186), so the loop stops
/// when the buffer is full instead of overrunning it -- and a stop there leaves the status at `Z_OK`,
/// which [`Prepared::accepts`] reports as a skip rather than publishing a number for a truncated
/// stream. And a round that consumes nothing and produces nothing ends the drive: `deflate` cannot
/// report `Z_OK` without progress, so this cannot trigger on a well-formed stream, but it makes
/// termination a property of the loop rather than a property of the library.
fn port_chunked(
    stream: &mut PortDeflate,
    input: &[u8],
    out: &mut [u8],
    chunk: usize,
) -> Option<Encode> {
    let chunk = chunk.max(1);
    let out_len = out.len();
    install_port(&mut stream.strm, input, out);

    let mut in_left = input.len();
    let mut out_left = out_len;
    let mut status = Z_OK;

    while out_left > 0 {
        let offer_in = in_left.min(chunk);
        let offer_out = out_left.min(chunk);
        // `(zcpr.avail_in == lOrigToDo) ? Z_FINISH : Z_SYNC_FLUSH` -- testzlib.c L209.
        let flush = if offer_in == in_left {
            Z_FINISH
        } else {
            Z_SYNC_FLUSH
        };
        stream.strm.avail_in = libz_rs_sys::uInt::try_from(offer_in).ok()?;
        stream.strm.avail_out = libz_rs_sys::uInt::try_from(offer_out).ok()?;

        // SAFETY: as for `port_single_shot` -- `avail_in` bytes readable at `next_in` and `avail_out`
        // bytes writable at `next_out`, both derived from the live slices installed before the loop,
        // and both counts clamped to what those slices still hold. The library advances the two
        // pointers itself, so re-offering counts without re-installing pointers is the documented way
        // to resume.
        status = unsafe { libz_rs_sys::deflate(core::ptr::addr_of_mut!(stream.strm), flush) };

        let consumed = offer_in.checked_sub(usize::try_from(stream.strm.avail_in).ok()?)?;
        let produced = offer_out.checked_sub(usize::try_from(stream.strm.avail_out).ok()?)?;
        in_left = in_left.checked_sub(consumed)?;
        out_left = out_left.checked_sub(produced)?;

        if status != Z_OK || (consumed == 0 && produced == 0) {
            break;
        }
    }

    Some(Encode {
        status,
        produced: out_len.checked_sub(out_left)?,
    })
}

/// One `deflate(Z_FINISH)` call through the reference. Same shape as [`port_single_shot`].
fn oracle_single_shot(stream: &mut OracleDeflate, input: &[u8], out: &mut [u8]) -> Option<Encode> {
    let out_len = out.len();
    let avail_in = oracle::uInt::try_from(input.len()).ok()?;
    let avail_out = oracle::uInt::try_from(out_len).ok()?;

    install_oracle(&mut stream.strm, input, out);
    stream.strm.avail_in = avail_in;
    stream.strm.avail_out = avail_out;

    // SAFETY: as for `port_single_shot`, against the oracle's own `z_stream` type and its own entry
    // point. The stream has not moved since `c_deflateInit2_` wrote the back-pointer that
    // `deflate.c` L538 checks by identity.
    let status = unsafe { oracle::c_deflate(core::ptr::addr_of_mut!(stream.strm), Z_FINISH) };

    let left = usize::try_from(stream.strm.avail_out).ok()?;
    Some(Encode {
        status,
        produced: out_len.checked_sub(left)?,
    })
}

/// The reference driver's compression loop, through the reference. Same shape as [`port_chunked`].
fn oracle_chunked(
    stream: &mut OracleDeflate,
    input: &[u8],
    out: &mut [u8],
    chunk: usize,
) -> Option<Encode> {
    let chunk = chunk.max(1);
    let out_len = out.len();
    install_oracle(&mut stream.strm, input, out);

    let mut in_left = input.len();
    let mut out_left = out_len;
    let mut status = Z_OK;

    while out_left > 0 {
        let offer_in = in_left.min(chunk);
        let offer_out = out_left.min(chunk);
        let flush = if offer_in == in_left {
            Z_FINISH
        } else {
            Z_SYNC_FLUSH
        };
        stream.strm.avail_in = oracle::uInt::try_from(offer_in).ok()?;
        stream.strm.avail_out = oracle::uInt::try_from(offer_out).ok()?;

        // SAFETY: as for `port_chunked`, against the oracle's own type and entry point.
        status = unsafe { oracle::c_deflate(core::ptr::addr_of_mut!(stream.strm), flush) };

        let consumed = offer_in.checked_sub(usize::try_from(stream.strm.avail_in).ok()?)?;
        let produced = offer_out.checked_sub(usize::try_from(stream.strm.avail_out).ok()?)?;
        in_left = in_left.checked_sub(consumed)?;
        out_left = out_left.checked_sub(produced)?;

        if status != Z_OK || (consumed == 0 && produced == 0) {
            break;
        }
    }

    Some(Encode {
        status,
        produced: out_len.checked_sub(out_left)?,
    })
}

/// Drive the port for one whole stream, in whichever feeding mode `feeding` names.
fn port_encode(
    stream: &mut PortDeflate,
    input: &[u8],
    out: &mut [u8],
    feeding: Feeding,
) -> Option<Encode> {
    match feeding {
        Feeding::SingleShot => port_single_shot(stream, input, out),
        Feeding::Chunked(chunk) => port_chunked(stream, input, out, chunk),
    }
}

/// Drive the reference for one whole stream, in whichever feeding mode `feeding` names.
fn oracle_encode(
    stream: &mut OracleDeflate,
    input: &[u8],
    out: &mut [u8],
    feeding: Feeding,
) -> Option<Encode> {
    match feeding {
        Feeding::SingleShot => oracle_single_shot(stream, input, out),
        Feeding::Chunked(chunk) => oracle_chunked(stream, input, out, chunk),
    }
}

// =================================================================================================
//  The one-shot wrappers of compress.c
// =================================================================================================

/// Which one-shot entry point one `compress_oneshot` row exercises.
///
/// Both wrap a whole stream: `compress.c`'s `compress2_z` (L31) initialises, drives and ends one
/// internally, supplying `Z_DEFLATED`, `MAX_WBITS`, `DEF_MEM_LEVEL` and `Z_DEFAULT_STRATEGY` itself.
/// So a row here has the lifecycle group's cost profile and pins a configuration no `deflateInit2_`
/// row reaches by the same route.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OneShot {
    /// `compress2` -- the `uLong` out-parameter form, `zlib.h` L1284.
    Compress2,
    /// `compress2_z` -- the `z_size_t` out-parameter form, `zlib.h` L1286.
    Compress2Z,
}

impl OneShot {
    /// The short token this entry point contributes to a benchmark id.
    const fn tag(self) -> &'static str {
        match self {
            Self::Compress2 => "compress2",
            Self::Compress2Z => "compress2_z",
        }
    }

    /// Call this entry point through the port.
    fn port(self, dest: &mut [u8], source: &[u8], level: c_int) -> Option<Encode> {
        match self {
            Self::Compress2 => port_compress2(dest, source, level),
            Self::Compress2Z => port_compress2_z(dest, source, level),
        }
    }

    /// Call this entry point through the reference.
    fn oracle(self, dest: &mut [u8], source: &[u8], level: c_int) -> Option<Encode> {
        match self {
            Self::Compress2 => oracle_compress2(dest, source, level),
            Self::Compress2Z => oracle_compress2_z(dest, source, level),
        }
    }
}

/// Both one-shot entry points, in `compress.c`'s own forwarding order.
const ONE_SHOTS: [OneShot; 2] = [OneShot::Compress2, OneShot::Compress2Z];

/// `compress2(dest, &destLen, source, sourceLen, level)` through the port.
///
/// `destLen` is an in-out parameter: it carries the capacity in and the produced length out, which is
/// why it is initialised from `dest.len()` rather than from zero.
fn port_compress2(dest: &mut [u8], source: &[u8], level: c_int) -> Option<Encode> {
    let mut dest_len = libz_rs_sys::uLongf::try_from(dest.len()).ok()?;
    let source_len = libz_rs_sys::uLong::try_from(source.len()).ok()?;

    // SAFETY: `dest_len` bytes are writable at `dest.as_mut_ptr()` and `source_len` bytes readable at
    // `source.as_ptr()`, both counts being those live slices' own lengths converted with `try_from`,
    // and both slices outlive this call. `dest_len` is a local this call may write through, and it is
    // read back only after the call returns. `compress2` retains no pointer past the return.
    let status = unsafe {
        libz_rs_sys::compress2(
            dest.as_mut_ptr(),
            core::ptr::addr_of_mut!(dest_len),
            source.as_ptr(),
            source_len,
            level,
        )
    };

    Some(Encode {
        status,
        produced: usize::try_from(dest_len).ok()?,
    })
}

/// `compress2_z(dest, &destLen, source, sourceLen, level)` through the port.
fn port_compress2_z(dest: &mut [u8], source: &[u8], level: c_int) -> Option<Encode> {
    let mut dest_len = libz_rs_sys::z_size_t::try_from(dest.len()).ok()?;
    let source_len = libz_rs_sys::z_size_t::try_from(source.len()).ok()?;

    // SAFETY: as for `port_compress2`; the only difference is the width of the out-parameter and of
    // the source length.
    let status = unsafe {
        libz_rs_sys::compress2_z(
            dest.as_mut_ptr(),
            core::ptr::addr_of_mut!(dest_len),
            source.as_ptr(),
            source_len,
            level,
        )
    };

    // `dest_len` needs no conversion: `z_size_t` is `usize` in both mirrors by definition.
    Some(Encode {
        status,
        produced: dest_len,
    })
}

/// `c_compress2(dest, &destLen, source, sourceLen, level)` through the reference.
fn oracle_compress2(dest: &mut [u8], source: &[u8], level: c_int) -> Option<Encode> {
    let mut dest_len = oracle::uLongf::try_from(dest.len()).ok()?;
    let source_len = oracle::uLong::try_from(source.len()).ok()?;

    // SAFETY: as for `port_compress2`, against the reference's own entry point and its own scalar
    // aliases.
    let status = unsafe {
        oracle::c_compress2(
            dest.as_mut_ptr(),
            core::ptr::addr_of_mut!(dest_len),
            source.as_ptr(),
            source_len,
            level,
        )
    };

    Some(Encode {
        status,
        produced: usize::try_from(dest_len).ok()?,
    })
}

/// `c_compress2_z(dest, &destLen, source, sourceLen, level)` through the reference.
fn oracle_compress2_z(dest: &mut [u8], source: &[u8], level: c_int) -> Option<Encode> {
    let mut dest_len = oracle::z_size_t::try_from(dest.len()).ok()?;
    let source_len = oracle::z_size_t::try_from(source.len()).ok()?;

    // SAFETY: as for `port_compress2_z`, against the reference's own entry point.
    let status = unsafe {
        oracle::c_compress2_z(
            dest.as_mut_ptr(),
            core::ptr::addr_of_mut!(dest_len),
            source.as_ptr(),
            source_len,
            level,
        )
    };

    // As for `port_compress2_z`: `z_size_t` is `usize`, so there is nothing to convert.
    Some(Encode {
        status,
        produced: dest_len,
    })
}

// =================================================================================================
//  The bound functions
// =================================================================================================
//
//  Not merely informational. Callers size their output buffers with what these return, so a value
//  that is too small becomes a buffer overflow in CALLER code and one that is too large breaks tests
//  asserting exact sizes (AAP §0.6.2.2). They are measured because a caller pays for them on every
//  buffer it sizes, and they are compared during setup because benchmarking with a wrongly sized
//  buffer would be worse than not benchmarking at all.

/// Which bound entry point one `deflate_bound` row exercises.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BoundEntry {
    /// `deflateBound(strm, sourceLen)` -- `zlib.h` L768. Reads the initialised state, so its answer
    /// reflects the actual `windowBits` and wrapper.
    DeflateBound,
    /// `deflateBound_z(strm, sourceLen)` -- the `size_t` form, `zlib.h` L769.
    DeflateBoundZ,
    /// `compressBound(sourceLen)` -- `zlib.h` L1307. Takes no stream, so it must assume the
    /// configuration `compress2` uses.
    CompressBound,
    /// `compressBound_z(sourceLen)` -- the `size_t` form, `zlib.h` L1308.
    CompressBoundZ,
}

/// Every bound entry point, stream-taking ones first.
const BOUND_ENTRIES: [BoundEntry; 4] = [
    BoundEntry::DeflateBound,
    BoundEntry::DeflateBoundZ,
    BoundEntry::CompressBound,
    BoundEntry::CompressBoundZ,
];

impl BoundEntry {
    /// The short token this entry point contributes to a benchmark id.
    const fn tag(self) -> &'static str {
        match self {
            Self::DeflateBound => "deflateBound",
            Self::DeflateBoundZ => "deflateBound_z",
            Self::CompressBound => "compressBound",
            Self::CompressBoundZ => "compressBound_z",
        }
    }

    /// Call this entry point through the port. `stream` is ignored by the two `compressBound` arms,
    /// which take no stream at all.
    fn port(self, stream: &mut PortDeflate, source_len: usize) -> Option<usize> {
        match self {
            Self::DeflateBound => stream.bound(source_len),
            Self::DeflateBoundZ => stream.bound_z(source_len),
            Self::CompressBound => port_compress_bound(source_len),
            Self::CompressBoundZ => port_compress_bound_z(source_len),
        }
    }

    /// Call this entry point through the reference.
    fn oracle(self, stream: &mut OracleDeflate, source_len: usize) -> Option<usize> {
        match self {
            Self::DeflateBound => stream.bound(source_len),
            Self::DeflateBoundZ => stream.bound_z(source_len),
            Self::CompressBound => oracle_compress_bound(source_len),
            Self::CompressBoundZ => oracle_compress_bound_z(source_len),
        }
    }
}

/// `compressBound(sourceLen)` through the port.
///
/// No `unsafe`: the facade declares this one `extern "C" fn` rather than `unsafe extern "C" fn`,
/// because it takes no pointer and therefore has nothing to validate.
fn port_compress_bound(source_len: usize) -> Option<usize> {
    let source = libz_rs_sys::uLong::try_from(source_len).ok()?;
    usize::try_from(libz_rs_sys::compressBound(source)).ok()
}

/// `compressBound_z(sourceLen)` through the port. Also pointer-free and therefore also safe.
fn port_compress_bound_z(source_len: usize) -> Option<usize> {
    let source = libz_rs_sys::z_size_t::try_from(source_len).ok()?;
    Some(libz_rs_sys::compressBound_z(source))
}

/// `c_compressBound(sourceLen)` through the reference.
fn oracle_compress_bound(source_len: usize) -> Option<usize> {
    let source = oracle::uLong::try_from(source_len).ok()?;
    // SAFETY: the oracle declares every symbol in one `extern "C"` block, so even a pointer-free
    // function is an unsafe call in Rust. There is no pointer, no length and no state: the argument
    // is a scalar and the result is a scalar, so the only obligation is that the symbol be the one
    // `src/oracle.rs` declares -- which `build.rs`'s `c_` renaming and its `nm` audit establish.
    let bound = unsafe { oracle::c_compressBound(source) };
    usize::try_from(bound).ok()
}

/// `c_compressBound_z(sourceLen)` through the reference.
fn oracle_compress_bound_z(source_len: usize) -> Option<usize> {
    let source = oracle::z_size_t::try_from(source_len).ok()?;
    // SAFETY: as for `oracle_compress_bound` -- a scalar in, a scalar out, no pointer involved.
    Some(unsafe { oracle::c_compressBound_z(source) })
}

// =================================================================================================
//  One prepared case
// =================================================================================================

/// Bytes of slack allowed for each `Z_SYNC_FLUSH` round, on top of `deflateBound`.
///
/// `deflateBound` bounds a **single-pass** encode (`zlib.h` L768-L774), and the chunked feeding modes
/// are not single-pass: every `Z_SYNC_FLUSH` terminates the current block and appends an empty stored
/// block, which `deflateBound` does not account for. The framing a round can add is small and
/// bounded, so the correction is per round rather than proportional:
///
/// * the block the round terminates is at worst a stored block, whose overhead over its own payload
///   is the 3-bit type code, the padding to a byte boundary, and the four-byte `LEN`/`NLEN` pair --
///   five bytes;
/// * the empty stored block a `Z_SYNC_FLUSH` appends is another five (`00 00 00 FF FF`);
/// * a byte of alignment at worst.
///
/// Eleven, so 16 with margin. The payload itself is already covered, because `deflateBound` bounds
/// the whole input at `sourceLen + sourceLen/8 + sourceLen/64 + 5` plus the wrapper and the blocks
/// partition that same input between them.
const SYNC_FLUSH_SLACK: usize = 16;

/// A fixed tail on every output buffer, over and above the per-round slack.
///
/// Absorbs the wrapper -- ten-plus bytes of gzip header and eight of trailer at the widest -- and any
/// rounding in the arithmetic above, so that a buffer is never one byte short of a `Z_STREAM_END`.
/// The cost is 64 bytes per case; the alternative is a `Z_OK` return that the verification pass
/// reports as a skip.
const OUTPUT_TAIL: usize = 64;

/// A case that is ready to measure: the bytes to compress, the configuration to compress them with,
/// and the two implementations' agreed output bound.
///
/// Everything expensive happens in [`prepare`] and [`Prepared::verify`], both of which run before any
/// timed closure exists. A timed closure only ever reads [`Prepared::original`] and writes an output
/// buffer allocated for it in advance.
#[derive(Debug)]
struct Prepared<'a> {
    /// The benchmark id parameter, and the case identity in every diagnostic and ratio line. It
    /// always begins with the fixture or member name, and it never contains `/`, because criterion
    /// turns an id into a path under `target/criterion`.
    id: String,
    /// The uncompressed bytes both implementations compress.
    original: &'a [u8],
    /// The encoder configuration, identical on both sides.
    config: Config,
    /// How the encoder is fed.
    feeding: Feeding,
    /// `deflateBound(original.len())`, which both implementations returned identically -- [`prepare`]
    /// refuses the case otherwise.
    bound: usize,
    /// `compressBound(original.len())`, likewise agreed. Sizes the one-shot group's buffer.
    compress_bound: usize,
}

/// Initialise both encoders, confirm every bound agrees, and return a case ready to measure.
///
/// `None` is not an error path to be silenced: the reason is printed, and the caller moves on to the
/// next case. The two streams opened here are ended by their guards' [`Drop`] before this function
/// returns -- they exist only so that `deflateBound` has an initialised state to read, since its
/// answer depends on `wrap`, `w_bits` and `hash_bits`.
fn prepare(id: String, original: &[u8], config: Config, feeding: Feeding) -> Option<Prepared<'_>> {
    let mut port = PortDeflate::new();
    let port_init = port.init(config);
    let mut reference = OracleDeflate::new();
    let reference_init = reference.init(config);

    if port_init != Z_OK || reference_init != Z_OK {
        eprintln!(
            "{LOG_PREFIX} SKIPPING case={id}: deflateInit2_ returned {port_init} for the port and \
             {reference_init} for the reference ({config})",
            config = config.describe(),
        );
        return None;
    }

    let bound = agreed_bound(
        &id,
        config,
        BoundEntry::DeflateBound,
        &mut port,
        &mut reference,
        original.len(),
    )?;
    let bound_z = agreed_bound(
        &id,
        config,
        BoundEntry::DeflateBoundZ,
        &mut port,
        &mut reference,
        original.len(),
    )?;
    let compress_bound = agreed_bound(
        &id,
        config,
        BoundEntry::CompressBound,
        &mut port,
        &mut reference,
        original.len(),
    )?;
    let compress_bound_z = agreed_bound(
        &id,
        config,
        BoundEntry::CompressBoundZ,
        &mut port,
        &mut reference,
        original.len(),
    )?;

    // The two widths of the same question must also answer alike, on each side. A divergence here
    // would mean one width had lost precision, which is the LLP64 hazard of AAP §0.6.3.3 showing up
    // as a number rather than as a compile error.
    if bound != bound_z || compress_bound != compress_bound_z {
        eprintln!(
            "{LOG_PREFIX} SKIPPING case={id}: the uLong and size_t bound widths disagree -- \
             deflateBound={bound} deflateBound_z={bound_z} compressBound={compress_bound} \
             compressBound_z={compress_bound_z} for {config}. AAP §0.6.2.2 requires them to be the \
             same number; the authoritative gate is \
             crates/zlib-rs-differential/tests/byte_identical.rs.",
            config = config.describe(),
        );
        return None;
    }

    Some(Prepared {
        id,
        original,
        config,
        feeding,
        bound,
        compress_bound,
    })
}

/// One bound entry point's value, or `None` with a loud diagnostic if the two sides disagree.
///
/// AAP §0.6.2.2 is the rationale and it is worth restating: callers size their output buffers with
/// these numbers, so a mismatch is a caller-side overflow risk rather than a cosmetic difference.
/// Here it is also a practical guard -- benchmarking with a wrongly sized buffer would produce a
/// number for work that was never completed. The authoritative numeric gate lives in
/// `crates/zlib-rs-differential/tests/byte_identical.rs`; this is the cheap version of it.
fn agreed_bound(
    id: &str,
    config: Config,
    entry: BoundEntry,
    port: &mut PortDeflate,
    reference: &mut OracleDeflate,
    source_len: usize,
) -> Option<usize> {
    let port_bound = entry.port(port, source_len);
    let reference_bound = entry.oracle(reference, source_len);

    match (port_bound, reference_bound) {
        (Some(mine), Some(theirs)) if mine == theirs => Some(mine),
        (Some(mine), Some(theirs)) => {
            eprintln!(
                "{LOG_PREFIX} SKIPPING case={id}: {entry} DISAGREES -- the port says {mine} and the \
                 reference says {theirs} for {source_len} bytes at {config}. AAP §0.6.2.2: callers \
                 size their output buffers with this number, so a value that is too small is a \
                 buffer overflow in caller code. Not benchmarking this case.",
                entry = entry.tag(),
                config = config.describe(),
            );
            None
        }
        _ => {
            eprintln!(
                "{LOG_PREFIX} SKIPPING case={id}: {entry} could not be represented in this target's \
                 uLong or size_t for {source_len} bytes at {config}.",
                entry = entry.tag(),
                config = config.describe(),
            );
            None
        }
    }
}

impl Prepared<'_> {
    /// The size of the buffer a streaming encode is given.
    ///
    /// `deflateBound` plus [`SYNC_FLUSH_SLACK`] for every round the feeding mode will make, plus
    /// [`OUTPUT_TAIL`]. The derivation is on [`SYNC_FLUSH_SLACK`]: the bound is single-pass and the
    /// chunked modes are not, so the framing each `Z_SYNC_FLUSH` adds has to be paid for explicitly.
    /// `saturating_*` throughout, because a benchmark answers an unrepresentable size by allocating
    /// what it can and letting the verification pass report the shortfall.
    fn output_len(&self) -> usize {
        let rounds = self.feeding.rounds(self.original.len());
        self.bound
            .saturating_add(rounds.saturating_mul(SYNC_FLUSH_SLACK))
            .saturating_add(OUTPUT_TAIL)
    }

    /// The size of the buffer a one-shot call is given: `compressBound`, exactly.
    ///
    /// No slack, because `compress2` performs a single `Z_FINISH` pass internally -- which is the
    /// shape the bound is documented for -- and because `destLen` carries the capacity in, so a
    /// caller who passes the bound is passing what the library asked for.
    fn one_shot_output_len(&self) -> usize {
        self.compress_bound
    }

    /// The buffer a round trip decodes into: the original length, or one byte when that is zero.
    ///
    /// `inflate` answers `Z_BUF_ERROR` for `avail_out == 0`, so `empty.bin` needs a byte it will never
    /// use in order to reach `Z_STREAM_END`.
    fn round_trip_len(&self) -> usize {
        self.original.len().max(1)
    }

    /// Everything a diagnostic should name, in one line.
    fn describe(&self) -> String {
        format!(
            "case={id} {config} feeding={feeding} original={original}B bound={bound}",
            id = self.id,
            config = self.config.describe(),
            feeding = self.feeding.tag(),
            original = self.original.len(),
            bound = self.bound,
        )
    }

    /// Check one encode outcome and round-trip its result, reporting rather than asserting.
    ///
    /// Returns the produced length on success. The checks, in order: the drive completed; it ended at
    /// `Z_STREAM_END` rather than at `Z_OK` with a full buffer; the produced length fits the buffer;
    /// and the produced bytes decode back to the original. That last step is the acceptance check of
    /// `contrib/testzlib/testzlib.c` L267-L270 -- the produced length equals the original length *and*
    /// the bytes compare equal -- performed with the reference's decoder so that a difference is a
    /// difference in the encoder.
    ///
    /// Deliberately not an assertion. Byte identity is gated under `cargo test` by
    /// `crates/zlib-rs-differential/tests/byte_identical.rs`, where a failure should stop the run and
    /// name the configuration. Here a mismatch means one case is not measured.
    fn accepts(&self, label: &str, outcome: Option<Encode>, out: &[u8]) -> Option<usize> {
        let Some(encode) = outcome else {
            eprintln!(
                "{LOG_PREFIX} SKIPPING [{label}]: a length could not be represented in this \
                 target's uInt/uLong while driving {}",
                self.describe()
            );
            return None;
        };

        if encode.status != Z_STREAM_END {
            eprintln!(
                "{LOG_PREFIX} SKIPPING [{label}]: expected Z_STREAM_END ({Z_STREAM_END}) and got \
                 {status} for {description}. A Z_OK here means the {capacity}-byte output buffer \
                 filled before the stream finished.",
                status = encode.status,
                description = self.describe(),
                capacity = out.len(),
            );
            return None;
        }

        let Some(produced) = out.get(..encode.produced) else {
            eprintln!(
                "{LOG_PREFIX} SKIPPING [{label}]: reported {produced} bytes into a {capacity}-byte \
                 buffer for {description}",
                produced = encode.produced,
                capacity = out.len(),
                description = self.describe(),
            );
            return None;
        };

        if !self.round_trips(label, produced) {
            return None;
        }

        Some(encode.produced)
    }

    /// Decode `compressed` with the reference and confirm it reproduces the original bytes.
    ///
    /// One decoder for both sides' output, on purpose. For the port's output this is the Rust→C
    /// crossing of AAP §0.8.1 directive 4; for the reference's it is a self-check that costs nothing
    /// and keeps the two paths symmetrical.
    fn round_trips(&self, label: &str, compressed: &[u8]) -> bool {
        let window_bits = self.config.window_bits;
        let mut out = vec![0_u8; self.round_trip_len()];

        let mut decoder = OracleInflate::new();
        let init = decoder.init(window_bits);
        if init != Z_OK {
            eprintln!(
                "{LOG_PREFIX} SKIPPING [{label}]: c_inflateInit2_(windowBits={window_bits}) \
                 returned {init} while checking {}",
                self.describe()
            );
            return false;
        }

        let Some(round) = decoder.decode(compressed, &mut out) else {
            eprintln!(
                "{LOG_PREFIX} SKIPPING [{label}]: a length could not be represented while decoding \
                 {}",
                self.describe()
            );
            return false;
        };

        if round.status != Z_STREAM_END || round.produced != self.original.len() {
            eprintln!(
                "{LOG_PREFIX} SKIPPING [{label}]: the round trip returned status {status} and \
                 {produced} bytes, expected {Z_STREAM_END} and {expected}, for {description}",
                status = round.status,
                produced = round.produced,
                expected = self.original.len(),
                description = self.describe(),
            );
            return false;
        }

        let Some(recovered) = out.get(..round.produced) else {
            return false;
        };

        if recovered != self.original {
            eprintln!(
                "{LOG_PREFIX} SKIPPING [{label}]: the round trip recovered {len} bytes that do not \
                 match the original for {description}. Correctness is gated by \
                 crates/zlib-rs-differential/tests/, not here.",
                len = round.produced,
                description = self.describe(),
            );
            return false;
        }

        true
    }

    /// Encode this case once with each implementation and confirm the two agree.
    ///
    /// Runs strictly outside every timed region, once per case, with the default allocator on both
    /// sides. A throughput number for an encoder that produced the wrong bytes is worse than no
    /// number, and this is what prevents one being published.
    ///
    /// The comparison is of compressed *lengths*, not of bytes. That is deliberate: byte identity
    /// across the whole encoder matrix is `tests/byte_identical.rs`'s job and duplicating it here
    /// would be both slower and less thorough. A length difference is the cheapest signal that the
    /// two encoders made different choices, and each side's output is separately proven to decode back
    /// to the input.
    fn verify(&self) -> bool {
        let mut out = vec![0_u8; self.output_len()];

        let mut port = PortDeflate::new();
        let port_init = port.init(self.config);
        if port_init != Z_OK {
            eprintln!(
                "{LOG_PREFIX} SKIPPING [{PORT_LABEL}]: deflateInit2_ returned {port_init} for {}",
                self.describe()
            );
            return false;
        }
        let port_encoded = port_encode(&mut port, self.original, &mut out, self.feeding);
        let Some(port_len) = self.accepts(PORT_LABEL, port_encoded, &out) else {
            return false;
        };
        // `end()` through `&mut`, never `drop(port)`: the latter would move the stream and
        // `deflateEnd` would then fail the identity check and free nothing. The guard stays bound and
        // its `Drop` becomes a no-op. Ending it here rather than at the end of the function keeps at
        // most one encoder alive at a time, which is what makes the two sides' measurements
        // independent.
        port.end();

        out.fill(0);

        let mut reference = OracleDeflate::new();
        let reference_init = reference.init(self.config);
        if reference_init != Z_OK {
            eprintln!(
                "{LOG_PREFIX} SKIPPING [{ORACLE_LABEL}]: c_deflateInit2_ returned \
                 {reference_init} for {}",
                self.describe()
            );
            return false;
        }
        let reference_encoded =
            oracle_encode(&mut reference, self.original, &mut out, self.feeding);
        let Some(reference_len) = self.accepts(ORACLE_LABEL, reference_encoded, &out) else {
            return false;
        };

        if port_len != reference_len {
            eprintln!(
                "{LOG_PREFIX} SKIPPING: the two encoders produced different lengths -- \
                 {PORT_LABEL} {port_len} bytes, {ORACLE_LABEL} {reference_len} bytes -- for \
                 {description}. Byte identity is gated by \
                 crates/zlib-rs-differential/tests/byte_identical.rs, which is where this should be \
                 diagnosed; not benchmarking this case.",
                description = self.describe(),
            );
            return false;
        }

        true
    }

    /// The one-shot counterpart of [`Prepared::verify`].
    ///
    /// `compress2` reports `Z_OK` rather than `Z_STREAM_END`: it consumes the whole input internally
    /// and returns the wrapper's verdict, not the state machine's. So the status check is inline here
    /// rather than delegated to [`Prepared::accepts`], and the round trip is invoked directly.
    fn verify_one_shot(&self, entry: OneShot) -> bool {
        let mut out = vec![0_u8; self.one_shot_output_len()];

        let Some(port_len) = self.accepts_one_shot(
            PORT_LABEL,
            entry,
            entry.port(&mut out, self.original, self.config.level),
            &out,
        ) else {
            return false;
        };

        out.fill(0);

        let Some(reference_len) = self.accepts_one_shot(
            ORACLE_LABEL,
            entry,
            entry.oracle(&mut out, self.original, self.config.level),
            &out,
        ) else {
            return false;
        };

        if port_len != reference_len {
            eprintln!(
                "{LOG_PREFIX} SKIPPING: {tag} produced different lengths -- {PORT_LABEL} \
                 {port_len} bytes, {ORACLE_LABEL} {reference_len} bytes -- for {description}",
                tag = entry.tag(),
                description = self.describe(),
            );
            return false;
        }

        true
    }

    /// Check one one-shot outcome and round-trip its result.
    fn accepts_one_shot(
        &self,
        label: &str,
        entry: OneShot,
        outcome: Option<Encode>,
        out: &[u8],
    ) -> Option<usize> {
        let Some(encode) = outcome else {
            eprintln!(
                "{LOG_PREFIX} SKIPPING [{label}]: a length could not be represented while calling \
                 {tag} for {description}",
                tag = entry.tag(),
                description = self.describe(),
            );
            return None;
        };

        if encode.status != Z_OK {
            eprintln!(
                "{LOG_PREFIX} SKIPPING [{label}]: {tag} returned {status}, expected Z_OK, for \
                 {description} into a {capacity}-byte buffer",
                tag = entry.tag(),
                status = encode.status,
                description = self.describe(),
                capacity = out.len(),
            );
            return None;
        }

        let Some(produced) = out.get(..encode.produced) else {
            eprintln!(
                "{LOG_PREFIX} SKIPPING [{label}]: {tag} reported {produced} bytes into a \
                 {capacity}-byte buffer for {description}",
                tag = entry.tag(),
                produced = encode.produced,
                capacity = out.len(),
                description = self.describe(),
            );
            return None;
        };

        if !self.round_trips(label, produced) {
            return None;
        }

        Some(encode.produced)
    }
}

// =================================================================================================
//  Throughput
// =================================================================================================

/// A byte count for [`Throughput::Bytes`], reporting rather than panicking on the impossible.
///
/// Every fixture and every Silesia member is far below `u64::MAX`, so this cannot fail on any target
/// Rust supports. It is written fallibly because `usize` is not `u64` by definition and because the
/// workspace denies `unwrap`: a benchmark has a correct answer for an unrepresentable length, and it
/// is to say so and move on.
///
/// A zero length yields `None` as well, and that is a real case rather than a defensive one:
/// `empty.bin` is a committed fixture and `Throughput::Bytes(0)` is a divisor of zero. The latency
/// groups that measure it set no throughput at all -- see [`DEGENERATE_FIXTURE_NAMES`] -- so this
/// branch only fires if a zero-length input reaches a throughput group by mistake.
fn throughput_bytes(len: usize) -> Option<Throughput> {
    if len == 0 {
        eprintln!(
            "{LOG_PREFIX} skipping a zero-byte case in a throughput group: \
             Throughput::Bytes(0) has no rate. Zero-length inputs belong in the latency group."
        );
        return None;
    }

    let Ok(bytes) = u64::try_from(len) else {
        eprintln!(
            "{LOG_PREFIX} skipping a {len}-byte case: the length does not fit a u64 and so has no \
             expressible throughput."
        );
        return None;
    };

    Some(Throughput::Bytes(bytes))
}

/// A byte count as an `f64`, for the memory ratio, or `None` if it would lose precision.
///
/// `usize as f64` is a precision-losing cast that `clippy::pedantic` denies, and rightly: above 2^53
/// the conversion is lossy. Routing through `u32` makes the conversion exact and total. The ceiling
/// that imposes is 4 `GiB`, which a single deflate stream cannot approach -- at the maximum
/// `windowBits` of 15 and `memLevel` of 9 its allocations are a 64 `KiB` window, a 64 `KiB` `prev`
/// array, a 128 `KiB` `head` array and the symbol buffers, a few hundred kilobytes in total
/// (`deflate.h` L104-L288) -- so the `None` branch is unreachable in practice and exists so that the
/// impossible is reported rather than silently mis-scaled.
fn as_f64(bytes: usize) -> Option<f64> {
    u32::try_from(bytes).ok().map(f64::from)
}

// =================================================================================================
//  Criterion budget
// =================================================================================================

/// Samples per row.
///
/// 50 rather than criterion's default 100. A default run registers about a hundred and forty rows
/// across nine groups, and 100 samples at the default five-second measurement would put a full run
/// well past twenty minutes -- long enough that it stops being run. Fifty samples still gives
/// criterion enough to resample from for a usable confidence interval, and criterion refuses fewer
/// than ten outright.
const SAMPLE_SIZE: usize = 50;

/// Warm-up per throughput row: 1 second rather than the default 3.
///
/// Compression reaches steady state quickly -- the window, the hash arrays and the symbol buffers are
/// all touched within the first iteration -- so the remaining two seconds bought precision the 10%
/// gate does not need.
const WARM_UP: Duration = Duration::from_secs(1);

/// Measurement per throughput row: 3 seconds rather than the default 5.
///
/// With the warm-up that is about four seconds per row. ★ The budget is sized for the slowest case in
/// the suite, which is level 9 on `window_boundary.bin`: `configuration_table[9]` sets `max_chain` to
/// 4096, so `longest_match` may walk four thousand hash-chain entries per position, and one iteration
/// there costs milliseconds rather than microseconds. criterion adapts the iteration count to the
/// budget, so that row takes the same wall clock as every other one; what it gets is fewer
/// iterations, and fifty samples is still enough to resample from. Pass `--sample-size`,
/// `--warm-up-time` or `--measurement-time` to trade wall clock for precision, or filter to one group
/// to spend the whole budget on one axis.
const MEASUREMENT: Duration = Duration::from_secs(3);

/// Warm-up for the sub-microsecond groups.
const WARM_UP_SHORT: Duration = Duration::from_millis(500);

/// Measurement for the sub-microsecond groups: 1 second.
///
/// [`deflate_bound`] and [`deflate_degenerate`] measure operations that cost nanoseconds to
/// hundreds of nanoseconds, and criterion drives millions of iterations in a second for those. The
/// extra two seconds the throughput budget spends would narrow an already-tight interval; spending
/// them across twenty-six rows would add a minute and a half to every run for nothing.
const MEASUREMENT_SHORT: Duration = Duration::from_secs(1);

/// Apply the throughput budget to a group.
///
/// Every throughput group is configured through this one function so that the numbers are stated once
/// and no group silently drifts onto criterion's defaults.
fn configure(group: &mut BenchmarkGroup<'_, WallTime>) {
    group.sample_size(SAMPLE_SIZE);
    group.warm_up_time(WARM_UP);
    group.measurement_time(MEASUREMENT);
}

/// Apply the shorter budget to a sub-microsecond group. See [`MEASUREMENT_SHORT`].
fn configure_short(group: &mut BenchmarkGroup<'_, WallTime>) {
    group.sample_size(SAMPLE_SIZE);
    group.warm_up_time(WARM_UP_SHORT);
    group.measurement_time(MEASUREMENT_SHORT);
}

// =================================================================================================
//  The indicative ratio pass, and the ledgers that report it
// =================================================================================================
//
//  criterion measures, and the criterion regression job of AAP §0.4.1.6 decides. But criterion's
//  harness API hands no timing back to the benchmark that produced it, so a plain `cargo bench` run
//  would print two rates per case and no ratio -- and both gates are ratios. This pass supplies the
//  throughput one; the memory one is exact and needs no timing at all.
//
//  It is deliberately small: bounded to a couple of milliseconds per round, three rounds, minimum
//  reported. The minimum rather than the mean because the shortest observed time is the least
//  contaminated by scheduling, and because the figure is a comparison rather than a distribution.
//  criterion's `target/criterion/<group>/<label>/<case>/new/estimates.json` remains the authoritative
//  measurement.

/// Nanoseconds in a second, as a float, for turning a [`Duration`] into a per-operation figure.
const NANOS_PER_SEC: f64 = 1_000_000_000.0;

/// Timed rounds per side. The minimum of the three is reported.
const CALIBRATION_ROUNDS: u32 = 3;

/// Wall-clock budget for one round, from which the repetition count is derived.
const CALIBRATION_BUDGET: Duration = Duration::from_millis(2);

/// Ceiling on repetitions per round, so that a nanosecond-scale operation cannot turn the budget
/// calculation into a long loop.
const CALIBRATION_MAX_REPS: u32 = 100_000;

/// The mean nanoseconds per call of `op`, from a bounded self-timed pass, or `None` if `op` failed.
///
/// One probe call establishes the scale, an integer division turns [`CALIBRATION_BUDGET`] into a
/// repetition count, and [`CALIBRATION_ROUNDS`] rounds of that many calls are timed. All arithmetic on
/// the count is integer arithmetic on nanoseconds, so no float is ever cast from an integer and no
/// precision is lost silently.
///
/// `op` returns `false` to mean "this did not work", which aborts the pass and yields `None`; the
/// caller reports the case as unmeasured rather than publishing a ratio for a failed operation.
fn indicative_ns<F: FnMut() -> bool>(mut op: F) -> Option<f64> {
    let probe_started = Instant::now();
    if !op() {
        return None;
    }
    let probe = probe_started.elapsed();

    let probe_ns = probe.as_nanos().max(1);
    let wanted = CALIBRATION_BUDGET.as_nanos() / probe_ns;
    let reps = u32::try_from(wanted.clamp(1, u128::from(CALIBRATION_MAX_REPS)))
        .unwrap_or(CALIBRATION_MAX_REPS);

    let mut best: Option<Duration> = None;
    for _ in 0..CALIBRATION_ROUNDS {
        let started = Instant::now();
        for _ in 0..reps {
            if !op() {
                return None;
            }
        }
        let elapsed = started.elapsed();
        best = match best {
            Some(current) if current <= elapsed => Some(current),
            _ => Some(elapsed),
        };
    }

    Some(best?.as_secs_f64() * NANOS_PER_SEC / f64::from(reps))
}

/// Whether a group's cases count against [`GATE_RATIO`] at all.
///
/// Declared once, where the group's ledger is created, rather than decided per case: whether a group
/// is *about* the gate is a property of the group. `deflate_stored_floor` measures level 0,
/// `deflate_mem_level` and `deflate_strategy` measure non-default configurations, `deflate_degenerate`
/// measures latency and `deflate_bound` measures a function that copies nothing -- none of those is
/// the quantity AAP §0.8.4 bounds, and every one of them still prints its full `RATIO` line so that
/// nothing is hidden.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GatePolicy {
    /// A case of at least [`GATE_MIN_BYTES`] counts against the gate.
    Gated,
    /// No case counts. The group is outside the gate by construction, not by size.
    Informational,
}

/// Accumulates the per-case throughput ratios of one group and prints the greppable lines CI
/// consumes.
///
/// The format is fixed and documented in the module header. Nothing here panics, exits or aborts on a
/// regression: `verdict=over` is the whole of this file's enforcement, and the pass/fail decision
/// belongs to the workflow.
#[derive(Debug)]
struct RatioLedger {
    /// The group name every line carries.
    group: &'static str,
    /// Whether this group's cases can count against the gate at all.
    policy: GatePolicy,
    /// How many cases produced a ratio at all.
    cases: u32,
    /// How many of those had a payload of at least [`GATE_MIN_BYTES`] and so count against the gate.
    gated: u32,
    /// How many *gated* cases exceeded [`GATE_RATIO`]. This is the number a workflow reads.
    over: u32,
    /// How many cases were below the floor, or in an informational group, and are therefore reported
    /// but not counted.
    informational: u32,
}

impl RatioLedger {
    /// An empty gated ledger for `group`: a case of at least [`GATE_MIN_BYTES`] counts.
    const fn new(group: &'static str) -> Self {
        Self {
            group,
            policy: GatePolicy::Gated,
            cases: 0,
            gated: 0,
            over: 0,
            informational: 0,
        }
    }

    /// An empty ledger for a group that is outside the gate by construction.
    const fn informational(group: &'static str) -> Self {
        Self {
            group,
            policy: GatePolicy::Informational,
            cases: 0,
            gated: 0,
            over: 0,
            informational: 0,
        }
    }

    /// Record one case, printing its `RATIO` line.
    ///
    /// `bytes` is the case's *uncompressed* length, which is both the quantity the throughput is
    /// expressed per and the quantity [`GATE_MIN_BYTES`] is compared against.
    ///
    /// A side that could not be timed, or a reference time of zero that no ratio can be taken
    /// against, is reported as unmeasured on its own line and does not enter any count -- so an
    /// aggregate of zero `over` never quietly means "nothing was compared".
    fn record(&mut self, case: &str, bytes: usize, port_ns: Option<f64>, oracle_ns: Option<f64>) {
        let (Some(port_ns), Some(oracle_ns)) = (port_ns, oracle_ns) else {
            eprintln!(
                "{LOG_PREFIX} no ratio for group={group} case={case}: one side could not be timed.",
                group = self.group,
            );
            return;
        };

        if oracle_ns <= 0.0 {
            eprintln!(
                "{LOG_PREFIX} no ratio for group={group} case={case}: the reference measured \
                 {oracle_ns} ns, which nothing can be divided by.",
                group = self.group,
            );
            return;
        }

        let ratio = port_ns / oracle_ns;
        let verdict = if ratio > GATE_RATIO { "over" } else { "within" };
        let counted = self.policy == GatePolicy::Gated && bytes >= GATE_MIN_BYTES;
        let gate = if counted { "counted" } else { "informational" };

        self.cases = self.cases.saturating_add(1);
        if counted {
            self.gated = self.gated.saturating_add(1);
            if ratio > GATE_RATIO {
                self.over = self.over.saturating_add(1);
            }
        } else {
            self.informational = self.informational.saturating_add(1);
        }

        eprintln!(
            "{LOG_PREFIX} {RATIO_KEY} group={group} case={case} bytes={bytes} \
             port_ns={port_ns:.1} oracle_ns={oracle_ns:.1} ratio={ratio:.3} \
             limit={GATE_RATIO:.2} gate={gate} verdict={verdict}",
            group = self.group,
        );
    }

    /// Print the group's aggregate `RATIO-SUMMARY` line.
    ///
    /// `cases` is `gated + informational`, and `over` counts only gated cases -- so a workflow that
    /// reads `over` from the `deflate_steady_state` line is reading exactly the AAP §0.8.4
    /// compression gate.
    fn finish(&self) {
        eprintln!(
            "{LOG_PREFIX} {RATIO_SUMMARY_KEY} group={group} cases={cases} gated={gated} \
             over={over} informational={informational} limit={GATE_RATIO:.2}",
            group = self.group,
            cases = self.cases,
            gated = self.gated,
            over = self.over,
            informational = self.informational,
        );
    }
}

/// Accumulates the per-case memory ratios of the memory pass and prints its greppable lines.
///
/// Structurally the throughput ledger's twin, with two differences: there is no size floor, because a
/// stream's footprint is a property of its configuration rather than of its payload and is exact at
/// any payload length; and the numbers are byte counts rather than timings, so nothing is
/// statistical.
#[derive(Debug)]
struct MemoryLedger {
    /// The group name every line carries.
    group: &'static str,
    /// How many cases produced a ratio at all.
    cases: u32,
    /// How many exceeded [`GATE_MEMORY_RATIO`]. This is the number a workflow reads.
    over: u32,
    /// How many sides reported an unclean allocator balance -- a leak, an out-of-order free or a
    /// rogue free.
    dirty: u32,
}

impl MemoryLedger {
    /// An empty ledger for `group`.
    const fn new(group: &'static str) -> Self {
        Self {
            group,
            cases: 0,
            over: 0,
            dirty: 0,
        }
    }

    /// Record one case's two high-water marks, printing its `MEMORY` line.
    fn record(&mut self, case: &str, port_bytes: usize, oracle_bytes: usize) {
        let (Some(mine), Some(theirs)) = (as_f64(port_bytes), as_f64(oracle_bytes)) else {
            eprintln!(
                "{LOG_PREFIX} no memory ratio for group={group} case={case}: a high-water mark of \
                 {port_bytes}/{oracle_bytes} bytes cannot be represented exactly.",
                group = self.group,
            );
            return;
        };

        // Zero on EITHER side is a broken measurement rather than a frugal implementation, and both
        // directions are refused rather than one. A zero reference leaves nothing to divide by; a
        // zero port would divide to 0.000 and read as a spectacular pass. Either way it would mean
        // `deflateInit2_` ignored the `zalloc` it was given, which is a defect worth naming out loud.
        if mine <= 0.0 || theirs <= 0.0 {
            eprintln!(
                "{LOG_PREFIX} no memory ratio for group={group} case={case}: the instrumented \
                 allocator recorded {port_bytes} bytes for the port and {oracle_bytes} for the \
                 reference, and a zero on either side means that side's deflateInit2_ did not \
                 allocate through the zalloc it was given.",
                group = self.group,
            );
            return;
        }

        let ratio = mine / theirs;
        let verdict = if ratio > GATE_MEMORY_RATIO {
            "over"
        } else {
            "within"
        };

        self.cases = self.cases.saturating_add(1);
        if ratio > GATE_MEMORY_RATIO {
            self.over = self.over.saturating_add(1);
        }

        eprintln!(
            "{LOG_PREFIX} {MEMORY_KEY} group={group} case={case} port_bytes={port_bytes} \
             oracle_bytes={oracle_bytes} ratio={ratio:.3} limit={GATE_MEMORY_RATIO:.2} \
             verdict={verdict}",
            group = self.group,
        );
    }

    /// Record that one side's allocator balance was not clean.
    fn record_dirty(&mut self) {
        self.dirty = self.dirty.saturating_add(1);
    }

    /// Print the group's aggregate `MEMORY-SUMMARY` line.
    fn finish(&self) {
        eprintln!(
            "{LOG_PREFIX} {MEMORY_SUMMARY_KEY} group={group} cases={cases} over={over} \
             dirty={dirty} limit={GATE_MEMORY_RATIO:.2}",
            group = self.group,
            cases = self.cases,
            over = self.over,
            dirty = self.dirty,
        );
    }
}

// =================================================================================================
//  Configuration banner
// =================================================================================================

/// The reference's own version string, or `None` if it cannot be read as UTF-8.
///
/// Reported rather than asserted, and it answers a question the init handshake cannot. That handshake
/// compares only the major digit (`deflate.c` L394), so it would pass against a reference reporting
/// any `1.x`; this line prints the reference's *whole* string beside the `zlib.h` L44 constant
/// [`version_ptr`] passes, which is how a reader confirms the two agree character for character rather
/// than merely in their first byte.
fn oracle_version() -> Option<&'static str> {
    // SAFETY: `zlibVersion` returns a pointer to `ZLIB_VERSION`, a string literal with static storage
    // duration compiled into the oracle archive, so the pointee outlives the process and is never
    // written through. It is documented never to return null, and the check below does not rely on
    // that.
    let raw = unsafe { oracle::c_zlibVersion() };
    if raw.is_null() {
        return None;
    }

    // SAFETY: `raw` is non-null, as just checked, and addresses that same NUL-terminated `'static`
    // literal, so the string it describes is valid for reads for the whole program and the `'static`
    // lifetime this borrow claims is honest.
    let text = unsafe { CStr::from_ptr(raw) };
    text.to_str().ok()
}

/// Print the compiled configuration, the version handshake and both gates once, to stderr.
///
/// Every group calls this; [`Once`] makes it happen exactly one time however the run is filtered.
/// stderr rather than stdout because criterion's machine-readable output goes to stdout and must not
/// be polluted -- which is also why the `RATIO` and `MEMORY` lines go to stderr.
///
/// The point is that no row in the output is ambiguous. `simd` is resolved at compile time and
/// `deflate` runs an Adler-32 or a CRC-32 over everything it consumes, so the checksum backend is on
/// the encoder's hot path and a rate on its own does not say which backend produced it.
fn report_configuration() {
    static BANNER: Once = Once::new();

    BANNER.call_once(|| {
        eprintln!(
            "{LOG_PREFIX} measuring compression throughput and per-stream memory: the Rust port \
             against the in-process C oracle."
        );
        eprintln!(
            "{LOG_PREFIX}   port rows are labelled {PORT_LABEL:?}, reference rows {ORACLE_LABEL:?}"
        );
        eprintln!(
            "{LOG_PREFIX}   the AAP §0.8.4 throughput gate is ratio <= {GATE_RATIO:.2} at levels \
             {GATE_LEVELS:?} and is read from the {GROUP_STEADY_STATE} group; {GROUP_LIFECYCLE} is a \
             separate signal that includes deflateInit2_ and deflateEnd."
        );
        eprintln!(
            "{LOG_PREFIX}   the AAP §0.8.4 memory gate is ratio <= {GATE_MEMORY_RATIO:.2} and is \
             read from the {GROUP_MEMORY} group, which registers no criterion row: it runs an \
             allocation-tracked encode outside every timed region, because the counter's \
             bookkeeping and its 0xa5 fill would otherwise contaminate a rate."
        );
        eprintln!(
            "{LOG_PREFIX}   per-case lines are `{RATIO_KEY} group=... case=... bytes=... \
             port_ns=... oracle_ns=... ratio=... limit=... gate=counted|informational \
             verdict=within|over` and `{MEMORY_KEY} group=... case=... port_bytes=... \
             oracle_bytes=... ratio=... limit=... verdict=within|over`, with one \
             `{RATIO_SUMMARY_KEY}` or `{MEMORY_SUMMARY_KEY}` line per group and one \
             `{BALANCE_KEY}` line per side per memory case. Reported, never enforced -- the \
             workflow decides."
        );
        eprintln!(
            "{LOG_PREFIX}   only payloads of at least {GATE_MIN_BYTES} bytes are gate=counted: \
             below one page an encode is fixed cost rather than throughput, so those rows carry \
             their ratio but do not enter the `over` tally. The informational groups \
             ({GROUP_STORED_FLOOR}, {GROUP_MEM_LEVEL}, {GROUP_STRATEGY}, {GROUP_DEGENERATE}, \
             {GROUP_BOUND}) are outside the gate by group name."
        );
        eprintln!(
            "{LOG_PREFIX}   ★ AAP §0.7.1(c): this suite may not change what the encoder emits. No \
             configuration_table value is tuned, no non-default compile-time knob (FASTEST, \
             LIT_MEM, UNALIGNED_OK, FORCE_STATIC, FORCE_STORED) is enabled, and an encoder-level \
             speed-up is prohibited however large -- admissible optimization is output-neutral only."
        );

        match oracle_version() {
            Some(version) => eprintln!(
                "{LOG_PREFIX}   version handshake: both deflateInit2_ calls are passed \
                 {expected:?} (zlib.h L44) and the reference reports {version:?}",
                expected = ZLIB_VERSION.to_str().unwrap_or("<not UTF-8>"),
            ),
            None => eprintln!(
                "{LOG_PREFIX}   version handshake: the reference's zlibVersion() could not be read \
                 as UTF-8; the deflateInit2_ calls are still passed the zlib.h L44 constant."
            ),
        }

        eprintln!(
            "{LOG_PREFIX}   cargo feature \"simd\": {}",
            if cfg!(feature = "simd") {
                "ENABLED -- vectorization-friendly Adler-32 and CRC-32 backends compiled in, and \
                 deflate runs a checksum over everything it consumes"
            } else {
                "disabled (the default) -- scalar checksum backends only"
            }
        );

        // `ZLIB_RS_SIMD` is a build-time consistency check in crates/libz-rs-sys/build.rs, not a
        // selector: cargo resolves features before it runs a build script, so a build script cannot
        // enable one. Reporting it here makes a disagreement between what was asked for and what was
        // compiled visible in the measurement output rather than only in a build log.
        match option_env!("ZLIB_RS_SIMD") {
            Some(value) => eprintln!(
                "{LOG_PREFIX}   ZLIB_RS_SIMD={value:?} at compile time (a consistency check against \
                 the feature above, never a second selector)"
            ),
            None => eprintln!("{LOG_PREFIX}   ZLIB_RS_SIMD unset at compile time"),
        }

        eprintln!(
            "{LOG_PREFIX}   feeding: single-shot Z_FINISH, and chunked Z_FINISH-on-the-last-round \
             at {CHUNK_DEFAULT}/{CHUNK_MEDIUM}/{CHUNK_SMALL} bytes \
             (contrib/testzlib/testzlib.c L147-L148, L204-L213)"
        );
        eprintln!(
            "{LOG_PREFIX}   budget per row: {SAMPLE_SIZE} samples, {WARM_UP:?} warm-up, \
             {MEASUREMENT:?} measurement; {WARM_UP_SHORT:?}/{MEASUREMENT_SHORT:?} for the \
             sub-microsecond groups"
        );
    });
}

// =================================================================================================
//  The measurement shapes
// =================================================================================================

/// Whether a row is annotated with a throughput, or measured as bare latency.
///
/// The distinction is not cosmetic. A criterion group's throughput setting is *sticky* across rows,
/// so a zero-length row mixed into a throughput group would silently inherit the previous row's
/// divisor -- which is why [`deflate_degenerate`] is a separate group and why this enum exists rather
/// than a conditional inside one function.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Report {
    /// Annotate with `Throughput::Bytes(original.len())`, so criterion reports input bytes/second.
    Rate,
    /// Annotate with nothing, so criterion reports time per call. For payloads with no meaningful
    /// rate.
    Latency,
}

/// Register the steady-state pair for one case: `deflateReset` plus an encode, per iteration.
///
/// The stream is initialised once here, outside every timed closure, and `deflateReset` returns it to
/// the start of a stream inside the closure. `deflateReset` keeps the window, the hash arrays and the
/// pending buffer, so what is timed is the encode plus the cheap half of the lifecycle and not the
/// allocator -- which is why this is the shape the AAP §0.8.4 throughput gate is read from.
///
/// The reset is inside the timed region on purpose: it is what makes each iteration compress the same
/// input from the beginning rather than continue a finished stream, so it is part of the work, not
/// setup. It costs a few dozen field assignments and one `_tr_init`.
///
/// Both streams use each library's own internal allocator -- `zalloc`, `zfree` and `opaque` are left
/// null. The instrumented allocator belongs to [`measure_memory`] and never appears in a timed
/// region.
fn measure_steady_state(
    group: &mut BenchmarkGroup<'_, WallTime>,
    ledger: &mut RatioLedger,
    case: &Prepared<'_>,
    report: Report,
) {
    let throughput = match report {
        Report::Rate => match throughput_bytes(case.original.len()) {
            Some(throughput) => Some(throughput),
            None => return,
        },
        Report::Latency => None,
    };
    if !case.verify() {
        return;
    }

    let mut out = vec![0_u8; case.output_len()];

    // Bound to locals and initialised in place: a guard must not move once it is live, because the
    // state block holds a pointer back to the stream and `deflate.c` L538 checks it by identity.
    let mut port = PortDeflate::new();
    let port_init = port.init(case.config);
    let mut reference = OracleDeflate::new();
    let reference_init = reference.init(case.config);
    if port_init != Z_OK || reference_init != Z_OK {
        eprintln!(
            "{LOG_PREFIX} SKIPPING {}: deflateInit2_ returned {port_init} for the port and \
             {reference_init} for the reference after the verification pass had succeeded.",
            case.describe()
        );
        return;
    }

    let port_ns = indicative_ns(|| {
        port.reset() == Z_OK
            && port_encode(&mut port, case.original, &mut out, case.feeding).is_some()
    });
    let oracle_ns = indicative_ns(|| {
        reference.reset() == Z_OK
            && oracle_encode(&mut reference, case.original, &mut out, case.feeding).is_some()
    });
    ledger.record(&case.id, case.original.len(), port_ns, oracle_ns);

    if let Some(throughput) = throughput {
        group.throughput(throughput);
    }
    group.bench_function(BenchmarkId::new(PORT_LABEL, &case.id), |b| {
        b.iter(|| {
            let reset = port.reset();
            let encoded = port_encode(
                &mut port,
                black_box(case.original),
                black_box(out.as_mut_slice()),
                case.feeding,
            );
            black_box((reset, encoded))
        });
    });
    group.bench_function(BenchmarkId::new(ORACLE_LABEL, &case.id), |b| {
        b.iter(|| {
            let reset = reference.reset();
            let encoded = oracle_encode(
                &mut reference,
                black_box(case.original),
                black_box(out.as_mut_slice()),
                case.feeding,
            );
            black_box((reset, encoded))
        });
    });
}

/// Register the full-lifecycle pair for one case: `deflateInit2_`, an encode and `deflateEnd`.
///
/// One iteration is a whole stream from nothing to nothing, `deflateEnd` included -- the guard's
/// [`Drop`] runs at the end of each iteration and is therefore inside the timed region, which is the
/// entire point of this shape. Read against [`measure_steady_state`], the difference between the two
/// groups *is* the per-stream setup cost, and for a deflate stream that cost is substantial: the
/// window, the `prev` and `head` arrays and the symbol buffers are all allocated and freed per
/// iteration.
fn measure_lifecycle(
    group: &mut BenchmarkGroup<'_, WallTime>,
    ledger: &mut RatioLedger,
    case: &Prepared<'_>,
) {
    let Some(throughput) = throughput_bytes(case.original.len()) else {
        return;
    };
    if !case.verify() {
        return;
    }

    let mut out = vec![0_u8; case.output_len()];

    let port_ns = indicative_ns(|| {
        let mut port = PortDeflate::new();
        port.init(case.config) == Z_OK
            && port_encode(&mut port, case.original, &mut out, case.feeding).is_some()
    });
    let oracle_ns = indicative_ns(|| {
        let mut reference = OracleDeflate::new();
        reference.init(case.config) == Z_OK
            && oracle_encode(&mut reference, case.original, &mut out, case.feeding).is_some()
    });
    ledger.record(&case.id, case.original.len(), port_ns, oracle_ns);

    group.throughput(throughput);
    group.bench_function(BenchmarkId::new(PORT_LABEL, &case.id), |b| {
        b.iter(|| {
            let mut port = PortDeflate::new();
            let init = port.init(case.config);
            let encoded = port_encode(
                &mut port,
                black_box(case.original),
                black_box(out.as_mut_slice()),
                case.feeding,
            );
            black_box((init, encoded))
        });
    });
    group.bench_function(BenchmarkId::new(ORACLE_LABEL, &case.id), |b| {
        b.iter(|| {
            let mut reference = OracleDeflate::new();
            let init = reference.init(case.config);
            let encoded = oracle_encode(
                &mut reference,
                black_box(case.original),
                black_box(out.as_mut_slice()),
                case.feeding,
            );
            black_box((init, encoded))
        });
    });
}

/// Register the one-shot pair for one case and one entry point.
///
/// No guard, because there is no caller-visible stream: `compress.c` allocates, drives and frees one
/// internally per call, so the lifecycle is inside the entry point and cannot be separated from the
/// encode. That is exactly why this group exists beside the other two rather than inside them.
fn measure_one_shot(
    group: &mut BenchmarkGroup<'_, WallTime>,
    ledger: &mut RatioLedger,
    case: &Prepared<'_>,
    entry: OneShot,
) {
    let Some(throughput) = throughput_bytes(case.original.len()) else {
        return;
    };
    if !case.verify_one_shot(entry) {
        return;
    }

    let level = case.config.level;
    let mut out = vec![0_u8; case.one_shot_output_len()];

    let port_ns = indicative_ns(|| entry.port(&mut out, case.original, level).is_some());
    let oracle_ns = indicative_ns(|| entry.oracle(&mut out, case.original, level).is_some());
    ledger.record(&case.id, case.original.len(), port_ns, oracle_ns);

    group.throughput(throughput);
    group.bench_function(BenchmarkId::new(PORT_LABEL, &case.id), |b| {
        b.iter(|| {
            black_box(entry.port(
                black_box(out.as_mut_slice()),
                black_box(case.original),
                level,
            ))
        });
    });
    group.bench_function(BenchmarkId::new(ORACLE_LABEL, &case.id), |b| {
        b.iter(|| {
            black_box(entry.oracle(
                black_box(out.as_mut_slice()),
                black_box(case.original),
                level,
            ))
        });
    });
}

/// Register the bound pair for one case and one entry point.
///
/// No `Throughput`: these functions return a number and copy nothing, so bytes per second would be a
/// fiction. Their agreement has already been established by [`prepare`], which refuses a case whose
/// two sides disagree, so what is left to measure is the per-call cost a caller pays every time it
/// sizes a buffer.
///
/// The two `deflateBound` arms need an initialised stream, because their answer depends on `wrap`,
/// `w_bits` and `hash_bits`; the two `compressBound` arms take no stream at all. Both guards are
/// created for every entry so that the shape is uniform and so that the stream-free arms are measured
/// under the same conditions as the others.
fn measure_bound(
    group: &mut BenchmarkGroup<'_, WallTime>,
    ledger: &mut RatioLedger,
    case: &Prepared<'_>,
    entry: BoundEntry,
) {
    let source_len = case.original.len();

    let mut port = PortDeflate::new();
    let port_init = port.init(case.config);
    let mut reference = OracleDeflate::new();
    let reference_init = reference.init(case.config);
    if port_init != Z_OK || reference_init != Z_OK {
        eprintln!(
            "{LOG_PREFIX} SKIPPING {}: deflateInit2_ returned {port_init} for the port and \
             {reference_init} for the reference while preparing the {tag} row.",
            case.describe(),
            tag = entry.tag(),
        );
        return;
    }

    let port_ns = indicative_ns(|| entry.port(&mut port, source_len).is_some());
    let oracle_ns = indicative_ns(|| entry.oracle(&mut reference, source_len).is_some());
    ledger.record(&case.id, source_len, port_ns, oracle_ns);

    group.bench_function(BenchmarkId::new(PORT_LABEL, &case.id), |b| {
        b.iter(|| black_box(entry.port(&mut port, black_box(source_len))));
    });
    group.bench_function(BenchmarkId::new(ORACLE_LABEL, &case.id), |b| {
        b.iter(|| black_box(entry.oracle(&mut reference, black_box(source_len))));
    });
}

/// Measure one case's per-stream memory on both sides, outside every timed region.
///
/// ★ This function registers **no criterion row**, and that is the design rather than an omission.
/// The instrumented allocator maintains a registry and fills every block with `0xa5`
/// (`test/infcover.c` L87), both of which cost time; letting that bookkeeping into a criterion closure
/// would publish a rate for the harness rather than for the encoder. So the tracked encode happens
/// exactly once per configuration, here, and the throughput groups run with the default allocator.
///
/// The sequence per side is `test/infcover.c`'s: create the counter, install `opaque`/`zalloc`/`zfree`
/// **before** `deflateInit2_` (L158-L173), drive one encode, end the stream, then read `highwater`
/// (L192-L197) and check the balance (L200-L240). Ending the stream before reading the balance is what
/// makes the balance a leak check: every block `deflateInit2_` took must have come back through
/// `deflateEnd`.
fn measure_memory(ledger: &mut MemoryLedger, case: &Prepared<'_>) {
    let Some((port_bytes, port_clean)) = tracked_port_encode(case) else {
        return;
    };
    let Some((oracle_bytes, oracle_clean)) = tracked_oracle_encode(case) else {
        return;
    };

    if !port_clean {
        ledger.record_dirty();
    }
    if !oracle_clean {
        ledger.record_dirty();
    }

    ledger.record(&case.id, port_bytes, oracle_bytes);
}

/// One allocation-tracked encode through the port: its high-water mark, and whether the balance was
/// clean.
///
/// The counter is declared before the stream so that Rust's reverse declaration order drops the
/// stream first: `deflateEnd` must run, and must return every block through [`tracked_free`], while
/// the counter is still alive.
fn tracked_port_encode(case: &Prepared<'_>) -> Option<(usize, bool)> {
    let mut tracker = TrackedAllocator::new();
    let mut port = PortDeflate::new();
    port.install_allocator(tracker.opaque());

    let init = port.init(case.config);
    if init != Z_OK {
        eprintln!(
            "{LOG_PREFIX} SKIPPING [{PORT_LABEL}] memory: deflateInit2_ with the instrumented \
             allocator returned {init} for {}",
            case.describe()
        );
        return None;
    }

    let mut out = vec![0_u8; case.output_len()];
    let encoded = port_encode(&mut port, case.original, &mut out, case.feeding);
    if !encode_completed(PORT_LABEL, case, encoded) {
        return None;
    }

    // ★ Explicit, ordered, and `&mut` -- all three matter. `deflateEnd` has to run BEFORE the balance
    // is read, or every stream would look like a leak; and it has to run without moving the guard,
    // because `drop(port)` would take it by value and `deflate.c` L538's identity check would then
    // reject the relocated stream, free nothing, and report `frees=0` for a stream that was never
    // actually leaked. See [`PortDeflate::end`].
    let end = port.end();
    let clean = tracker.report_balance(PORT_LABEL, &case.id) && end == Z_OK;
    if end != Z_OK {
        eprintln!(
            "{LOG_PREFIX} [{PORT_LABEL}] memory: deflateEnd returned {end} for {}, so the balance \
             above describes a stream that was not cleanly torn down.",
            case.describe()
        );
    }

    Some((tracker.highwater(), clean))
}

/// One allocation-tracked encode through the reference. Same shape as [`tracked_port_encode`].
fn tracked_oracle_encode(case: &Prepared<'_>) -> Option<(usize, bool)> {
    let mut tracker = TrackedAllocator::new();
    let mut reference = OracleDeflate::new();
    reference.install_allocator(tracker.opaque());

    let init = reference.init(case.config);
    if init != Z_OK {
        eprintln!(
            "{LOG_PREFIX} SKIPPING [{ORACLE_LABEL}] memory: c_deflateInit2_ with the instrumented \
             allocator returned {init} for {}",
            case.describe()
        );
        return None;
    }

    let mut out = vec![0_u8; case.output_len()];
    let encoded = oracle_encode(&mut reference, case.original, &mut out, case.feeding);
    if !encode_completed(ORACLE_LABEL, case, encoded) {
        return None;
    }

    // As for the port: ended in place through `&mut`, before the balance is read.
    let end = reference.end();
    let clean = tracker.report_balance(ORACLE_LABEL, &case.id) && end == Z_OK;
    if end != Z_OK {
        eprintln!(
            "{LOG_PREFIX} [{ORACLE_LABEL}] memory: c_deflateEnd returned {end} for {}, so the \
             balance above describes a stream that was not cleanly torn down.",
            case.describe()
        );
    }

    Some((tracker.highwater(), clean))
}

/// Whether one tracked encode reached `Z_STREAM_END`, reporting rather than asserting.
///
/// Lighter than [`Prepared::accepts`] on purpose: the round trip and the length comparison have
/// already run in [`Prepared::verify`] with the default allocator, and repeating them here would say
/// nothing new about the footprint. What does matter is that the stream *completed*, because a stream
/// that stopped early may not have allocated everything it would have needed.
fn encode_completed(label: &str, case: &Prepared<'_>, outcome: Option<Encode>) -> bool {
    match outcome {
        Some(encode) if encode.status == Z_STREAM_END => true,
        Some(encode) => {
            eprintln!(
                "{LOG_PREFIX} SKIPPING [{label}] memory: the tracked encode returned {status} \
                 rather than Z_STREAM_END for {description}, so its high-water mark would not \
                 reflect a complete stream.",
                status = encode.status,
                description = case.describe(),
            );
            false
        }
        None => {
            eprintln!(
                "{LOG_PREFIX} SKIPPING [{label}] memory: a length could not be represented while \
                 driving the tracked encode for {}",
                case.describe()
            );
            false
        }
    }
}

// =================================================================================================
//  The groups
// =================================================================================================

/// The group the AAP §0.8.4 throughput gate is read from.
const GROUP_STEADY_STATE: &str = "deflate_steady_state";

/// The group that includes the per-stream setup cost.
const GROUP_LIFECYCLE: &str = "deflate_lifecycle";

/// The group that varies the container format.
const GROUP_CONTAINER: &str = "deflate_container";

/// The group that varies how the encoder is fed.
const GROUP_FEEDING: &str = "deflate_feeding";

/// The informational group that varies `memLevel`.
const GROUP_MEM_LEVEL: &str = "deflate_mem_level";

/// The informational group that varies the strategy.
const GROUP_STRATEGY: &str = "deflate_strategy";

/// The group that measures the `compress.c` wrappers.
const GROUP_ONE_SHOT: &str = "compress_oneshot";

/// The informational level-0 floor.
const GROUP_STORED_FLOOR: &str = "deflate_stored_floor";

/// The informational group that measures the four bound entry points.
const GROUP_BOUND: &str = "deflate_bound";

/// The latency group for the degenerate and very short fixtures.
const GROUP_DEGENERATE: &str = "deflate_degenerate";

/// The group the AAP §0.8.4 memory gate is read from. Registers no criterion row.
const GROUP_MEMORY: &str = "deflate_memory";

/// The opt-in tier-2 group.
const GROUP_SILESIA: &str = "deflate_silesia";

/// Steady-state compression throughput, per fixture and per gated level.
///
/// ★ This is the group the ≤10% throughput gate of AAP §0.8.4 is read from. The container is zlib,
/// the `memLevel` is the default 8, the strategy is `Z_DEFAULT_STRATEGY` and the feeding is the
/// reference driver's 32768-byte chunking, so the axis is exactly the one the gate names: levels 1, 6
/// and 9 over every committed fixture whose encode profile differs.
fn deflate_steady_state(c: &mut Criterion) {
    report_configuration();

    let mut group = c.benchmark_group(GROUP_STEADY_STATE);
    configure(&mut group);
    let mut ledger = RatioLedger::new(GROUP_STEADY_STATE);

    for fixture in selected(&FIXTURE_NAMES) {
        for level in GATE_LEVELS {
            let id = format!("{name}-L{level}", name = fixture.name);
            let Some(case) = prepare(id, &fixture.bytes, Config::at_level(level), DEFAULT_FEEDING)
            else {
                continue;
            };
            measure_steady_state(&mut group, &mut ledger, &case, Report::Rate);
        }
    }

    group.finish();
    ledger.finish();
}

/// Whole-stream compression: `deflateInit2_`, encode, `deflateEnd`, per iteration.
///
/// Ordered smallest payload first, because that is the axis that matters here: at 14 bytes almost the
/// whole measurement is the allocator, at 65 `KiB` almost none of it is. Held at level 6 and zlib so
/// that the only thing varying is the payload size.
fn deflate_lifecycle(c: &mut Criterion) {
    report_configuration();

    let mut group = c.benchmark_group(GROUP_LIFECYCLE);
    configure(&mut group);
    let mut ledger = RatioLedger::new(GROUP_LIFECYCLE);

    for fixture in selected(&LIFECYCLE_FIXTURE_NAMES) {
        let id = format!("{name}-L{DEFAULT_LEVEL}", name = fixture.name);
        let Some(case) = prepare(
            id,
            &fixture.bytes,
            Config::at_level(DEFAULT_LEVEL),
            DEFAULT_FEEDING,
        ) else {
            continue;
        };
        measure_lifecycle(&mut group, &mut ledger, &case);
    }

    group.finish();
    ledger.finish();
}

/// Steady-state throughput across the three container formats.
///
/// zlib, raw DEFLATE and gzip, at level 6 and the default chunking. Each writes a different wrapper
/// and, more to the point for a rate, a different checksum: an Adler-32, none at all, or a CRC-32.
/// The two fixtures are the ones large enough for that per-byte difference to be visible rather than
/// lost in the fixed cost of the header.
fn deflate_container(c: &mut Criterion) {
    report_configuration();

    let mut group = c.benchmark_group(GROUP_CONTAINER);
    configure(&mut group);
    let mut ledger = RatioLedger::new(GROUP_CONTAINER);

    for fixture in selected(&AXIS_FIXTURE_NAMES) {
        for container in CONTAINERS {
            let id = format!("{name}-{tag}", name = fixture.name, tag = container.tag());
            let Some(case) = prepare(
                id,
                &fixture.bytes,
                Config::in_container(container),
                DEFAULT_FEEDING,
            ) else {
                continue;
            };
            measure_steady_state(&mut group, &mut ledger, &case, Report::Rate);
        }
    }

    group.finish();
    ledger.finish();
}

/// Steady-state throughput across the four feeding modes.
///
/// Single-shot `Z_FINISH`, then the reference driver's loop at 32768, 1024 and 16 bytes. AAP §0.6.4.2
/// records that chunk boundaries interact with flush handling and pending-buffer state -- and for an
/// encoder they do so visibly, because every `Z_SYNC_FLUSH` terminates a block and appends an empty
/// stored one, so the 16-byte row emits thousands of blocks where the single-shot row emits a handful.
/// AAP §0.3.2.4's `-Cllvm-args=-enable-dfa-jump-thread` lever was measured on small chunked input,
/// which makes the 16-byte row the one it should move.
fn deflate_feeding(c: &mut Criterion) {
    report_configuration();

    let mut group = c.benchmark_group(GROUP_FEEDING);
    configure(&mut group);
    let mut ledger = RatioLedger::new(GROUP_FEEDING);

    for fixture in selected(&AXIS_FIXTURE_NAMES) {
        for feeding in FEEDINGS {
            let id = format!("{name}-{tag}", name = fixture.name, tag = feeding.tag());
            let Some(case) = prepare(id, &fixture.bytes, Config::at_level(DEFAULT_LEVEL), feeding)
            else {
                continue;
            };
            measure_steady_state(&mut group, &mut ledger, &case, Report::Rate);
        }
    }

    group.finish();
    ledger.finish();
}

/// Steady-state throughput at `memLevel` 1 and 9. Informational.
///
/// The gate group already covers the default 8, so this group is the two ends of the range against
/// it. `memLevel` sets `hash_bits` and therefore the hash table's size, so 1 means far more
/// collisions and longer chains for `longest_match` (`deflate.c` L1389) to walk at the same level --
/// which is why the effect is visible on `repetitive.bin` in particular.
fn deflate_mem_level(c: &mut Criterion) {
    report_configuration();

    let mut group = c.benchmark_group(GROUP_MEM_LEVEL);
    configure(&mut group);
    let mut ledger = RatioLedger::informational(GROUP_MEM_LEVEL);

    for fixture in selected(&AXIS_FIXTURE_NAMES) {
        for mem_level in AXIS_MEM_LEVELS {
            let id = format!("{name}-mem{mem_level}", name = fixture.name);
            let config = Config::at_level(DEFAULT_LEVEL).with_mem_level(mem_level);
            let Some(case) = prepare(id, &fixture.bytes, config, DEFAULT_FEEDING) else {
                continue;
            };
            measure_steady_state(&mut group, &mut ledger, &case, Report::Rate);
        }
    }

    group.finish();
    ledger.finish();
}

/// Steady-state throughput across the four non-default strategies. Informational.
///
/// `Z_FILTERED`, `Z_HUFFMAN_ONLY`, `Z_RLE` and `Z_FIXED`. Two of them do not reach the match finder
/// at all -- `Z_RLE` runs `deflate_rle` (`deflate.c` L2084) and `Z_HUFFMAN_ONLY` runs `deflate_huff`
/// (L2155) -- so this group is where those two functions are measured, and it is outside the gate
/// because AAP §0.8.4 states the gate for the default configuration.
fn deflate_strategy(c: &mut Criterion) {
    report_configuration();

    let mut group = c.benchmark_group(GROUP_STRATEGY);
    configure(&mut group);
    let mut ledger = RatioLedger::informational(GROUP_STRATEGY);

    for fixture in selected(&AXIS_FIXTURE_NAMES) {
        for strategy in AXIS_STRATEGIES {
            let id = format!(
                "{name}-{tag}",
                name = fixture.name,
                tag = strategy_name(strategy)
            );
            let config = Config::at_level(DEFAULT_LEVEL).with_strategy(strategy);
            let Some(case) = prepare(id, &fixture.bytes, config, DEFAULT_FEEDING) else {
                continue;
            };
            measure_steady_state(&mut group, &mut ledger, &case, Report::Rate);
        }
    }

    group.finish();
    ledger.finish();
}

/// The one-shot wrappers of `compress.c`, per fixture and per entry point.
///
/// zlib only, and that is a property of the entry points rather than a choice: `compress2_z`
/// (`compress.c` L31) reaches the state machine through `deflateInit` with `MAX_WBITS`, so there is no
/// argument by which a caller could ask for a raw or a gzip stream. Level 6 throughout, since the
/// level axis is already covered where a stream can be configured. The `Config` is the zlib one
/// deliberately, so that the round-trip check in [`Prepared::round_trips`] decodes with the
/// `windowBits` these wrappers actually used.
fn compress_oneshot(c: &mut Criterion) {
    report_configuration();

    let mut group = c.benchmark_group(GROUP_ONE_SHOT);
    configure(&mut group);
    let mut ledger = RatioLedger::new(GROUP_ONE_SHOT);

    for fixture in selected(&FIXTURE_NAMES) {
        for entry in ONE_SHOTS {
            let id = format!("{name}-{tag}", name = fixture.name, tag = entry.tag());
            let Some(case) = prepare(
                id,
                &fixture.bytes,
                Config::at_level(DEFAULT_LEVEL),
                DEFAULT_FEEDING,
            ) else {
                continue;
            };
            measure_one_shot(&mut group, &mut ledger, &case, entry);
        }
    }

    group.finish();
    ledger.finish();
}

/// Level 0 -- `deflate_stored` (`deflate.c` L1668). Informational, and outside the gate.
///
/// A floor rather than a measurement of the encoder: level 0 performs no match search, so the row is
/// dominated by the copy into the window and the checksum over it. It is worth having precisely
/// because it separates those two costs from the match finder in every other row, and it is outside
/// the gate because AAP §0.8.4 names levels 1, 6 and 9.
fn deflate_stored_floor(c: &mut Criterion) {
    report_configuration();

    let mut group = c.benchmark_group(GROUP_STORED_FLOOR);
    configure(&mut group);
    let mut ledger = RatioLedger::informational(GROUP_STORED_FLOOR);

    for fixture in selected(&AXIS_FIXTURE_NAMES) {
        let id = format!("{name}-L{STORED_LEVEL}", name = fixture.name);
        let Some(case) = prepare(
            id,
            &fixture.bytes,
            Config::at_level(STORED_LEVEL),
            DEFAULT_FEEDING,
        ) else {
            continue;
        };
        measure_steady_state(&mut group, &mut ledger, &case, Report::Rate);
    }

    group.finish();
    ledger.finish();
}

/// The four bound entry points, per call. Informational, and with no `Throughput`.
///
/// These are the numbers callers size their output buffers with (AAP §0.6.2.2), so their *agreement*
/// matters far more than their speed -- and that agreement is already enforced for every case in this
/// file by [`prepare`], which refuses a case whose two sides disagree. What this group adds is the
/// per-call cost, which a caller pays on every buffer it sizes.
fn deflate_bound(c: &mut Criterion) {
    report_configuration();

    let Some(fixture) = memory_fixture() else {
        return;
    };

    let mut group = c.benchmark_group(GROUP_BOUND);
    configure_short(&mut group);
    let mut ledger = RatioLedger::informational(GROUP_BOUND);

    for entry in BOUND_ENTRIES {
        let id = format!("{name}-{tag}", name = fixture.name, tag = entry.tag());
        let Some(case) = prepare(
            id,
            &fixture.bytes,
            Config::at_level(DEFAULT_LEVEL),
            DEFAULT_FEEDING,
        ) else {
            continue;
        };
        measure_bound(&mut group, &mut ledger, &case, entry);
    }

    group.finish();
    ledger.finish();
}

/// The degenerate and very short fixtures, measured as latency. Informational.
///
/// ★ No `Throughput` anywhere in this group, and that is load-bearing rather than tidy: `empty.bin`
/// is zero bytes, `Throughput::Bytes(0)` is a divisor of zero, and a criterion group's throughput
/// setting is sticky across rows -- so a zero-length row inside a throughput group would silently
/// inherit the previous row's divisor. Separating the latency cases is what makes both kinds of number
/// honest.
///
/// What each fixture is for is recorded on [`DEGENERATE_FIXTURE_NAMES`]. Held at level 6, zlib and the
/// default chunking, since at these lengths a single `deflate` call does all the work whatever the
/// chunk size is.
fn deflate_degenerate(c: &mut Criterion) {
    report_configuration();

    let mut group = c.benchmark_group(GROUP_DEGENERATE);
    configure_short(&mut group);
    let mut ledger = RatioLedger::informational(GROUP_DEGENERATE);

    for fixture in selected(&DEGENERATE_FIXTURE_NAMES) {
        let id = format!(
            "{name}-{len}B",
            name = fixture.name,
            len = fixture.bytes.len()
        );
        let Some(case) = prepare(
            id,
            &fixture.bytes,
            Config::at_level(DEFAULT_LEVEL),
            DEFAULT_FEEDING,
        ) else {
            continue;
        };
        measure_steady_state(&mut group, &mut ledger, &case, Report::Latency);
    }

    group.finish();
    ledger.finish();
}

/// The AAP §0.8.4 memory gate: per-stream high-water marks across levels 1/6/9 and `memLevel` 1/8/9.
///
/// Registers no criterion row. It runs nine allocation-tracked encodes per side, entirely outside any
/// timed region, and prints one `MEMORY` line per configuration, one `ALLOC-BALANCE` line per side per
/// configuration, and one `MEMORY-SUMMARY` line.
///
/// The axes are the ones the footprint actually depends on. `windowBits` is held at 15 because the gate
/// is about the configuration the library ships with and because a smaller window would shrink both
/// sides identically; the level and `memLevel` vary because they are what size the state:
/// `deflate.h` L104-L288 allocates the window from `w_size`, the `prev` array from `w_size`, the `head`
/// array from `hash_size` and the symbol buffers from `lit_bufsize`, and `LIT_BUFS` (L225-L229) scales
/// the last of those. Nine configurations is the whole product, which is cheap because each is one
/// encode rather than a timed sweep.
///
/// One fixture, and it must be the one past the window: a stream that never fills its window never
/// exercises `fill_window`/`slide_hash` and its high-water mark would understate a fully driven
/// encoder. See [`MEMORY_FIXTURE_NAME`].
fn deflate_memory(_c: &mut Criterion) {
    report_configuration();

    let Some(fixture) = memory_fixture() else {
        return;
    };

    let mut ledger = MemoryLedger::new(GROUP_MEMORY);

    for level in GATE_LEVELS {
        for mem_level in MEMORY_MEM_LEVELS {
            let id = format!("{name}-L{level}-mem{mem_level}", name = fixture.name);
            let config = Config::at_level(level).with_mem_level(mem_level);
            let Some(case) = prepare(id, &fixture.bytes, config, DEFAULT_FEEDING) else {
                continue;
            };
            if !case.verify() {
                continue;
            }
            measure_memory(&mut ledger, &case);
        }
    }

    ledger.finish();
}

/// Steady-state throughput over the opt-in tier-2 corpus, or one note and nothing else.
///
/// This is the corpus AAP §0.8.4 names for the throughput gate, and it is measured in the gate
/// configuration only -- levels 1, 6 and 9, zlib, `memLevel` 8, 32768-byte chunking -- because the
/// other axes are already covered on tier 1, where a case costs milliseconds rather than seconds.
/// Skipped entirely, with a single note from [`silesia_members`], when the corpus is not present.
///
/// Each member is read, prepared, measured and dropped inside one loop iteration, so the peak memory
/// is one member plus one output buffer rather than the whole two-hundred-megabyte corpus at once.
fn deflate_silesia(c: &mut Criterion) {
    report_configuration();

    let members = silesia_members();
    if members.is_empty() {
        return;
    }

    let mut group = c.benchmark_group(GROUP_SILESIA);
    configure(&mut group);
    let mut ledger = RatioLedger::new(GROUP_SILESIA);

    for path in members {
        let Some(name) = path.file_name().and_then(OsStr::to_str) else {
            eprintln!(
                "{LOG_PREFIX} skipping a Silesia member: {} has no name this platform can render \
                 as UTF-8, and a benchmark id has to be printable.",
                path.display()
            );
            continue;
        };

        let bytes = match fs::read(path) {
            Ok(bytes) if bytes.is_empty() => {
                eprintln!(
                    "{LOG_PREFIX} skipping Silesia member {name}: it is empty, and a zero-byte case \
                     has no rate to report."
                );
                continue;
            }
            Ok(bytes) => bytes,
            Err(error) => {
                eprintln!(
                    "{LOG_PREFIX} skipping Silesia member {name}: cannot read {}: {error}",
                    path.display()
                );
                continue;
            }
        };

        for level in GATE_LEVELS {
            let id = format!("{name}-L{level}");
            let Some(case) = prepare(id, &bytes, Config::at_level(level), DEFAULT_FEEDING) else {
                continue;
            };
            measure_steady_state(&mut group, &mut ledger, &case, Report::Rate);
        }
    }

    group.finish();
    ledger.finish();
}

// =================================================================================================
//  Harness
// =================================================================================================
//
//  `harness = false` in the `[[bench]]` entry means libtest supplies no `main`, so `criterion_main!`
//  does. There is no `#[bench]` anywhere and no libtest is linked.
//
//  Ordered the way a reader wants them: the two gate groups first, because those are the numbers AAP
//  §0.8.4 is about -- throughput, then the lifecycle group it is only interpretable against, then the
//  memory gate; then the two structural axis groups; then the informational groups; then the one-shot
//  wrappers and the bound and latency groups; and the opt-in tier-2 group last, because on most
//  machines it prints one line and returns.

criterion_group!(
    deflate_benches,
    deflate_steady_state,
    deflate_lifecycle,
    deflate_memory,
    deflate_container,
    deflate_feeding,
    deflate_mem_level,
    deflate_strategy,
    deflate_stored_floor,
    compress_oneshot,
    deflate_bound,
    deflate_degenerate,
    deflate_silesia,
);
criterion_main!(deflate_benches);
