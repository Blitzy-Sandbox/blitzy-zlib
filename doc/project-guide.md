# Project Guide: zlib-rs — C-to-Rust Migration of zlib Compression Library

## 1. Executive Summary

**Project:** Same-repository technology-stack migration of the zlib compression library from ANSI C to idiomatic, memory-safe Rust  
**Status:** **Experimental — active migration** — functionally complete and validated; the primary remaining gate is formal human code review / sign-off  
**Completion (effort-estimate snapshot):** 240 hours completed out of 282 total hours = **85.1% complete** (an original effort-estimate snapshot retained for planning history — see §4.1)

The zlib-rs crate implements the complete zlib public API surface as an independent, zero-C-dependency Rust library conforming to RFC 1950 (zlib format), RFC 1951 (DEFLATE), and RFC 1952 (gzip format). This is a **same-repository** migration: the full Rust crate (`src/` — 40 `.rs` modules — plus `tests/`, `benches/`, `fuzz/`, `build.rs`, `Cargo.toml`/`Cargo.lock`, CI, and docs) was **added alongside** the upstream zlib C baseline, which is **retained in-repo as the behavioral/ABI reference oracle** (`Cargo.toml`'s `exclude` list keeps the C files out of the *published* crate only — it never affects `cargo build`/`test`/`bench`). The crate compiles cleanly (debug + release), passes the full default-configuration test suite (487 total: 487 passed, 0 ignored, 0 failed), has zero clippy warnings, and is fully formatted.

**Key Achievements:**
- 32,561 lines of Rust code across 40 `.rs` files, reimplementing the ~23,107-line C baseline that is retained in-repo as the reference oracle
- Complete DEFLATE compression engine with all 5 strategies (stored, fast, slow, huff, rle)
- Complete DEFLATE decompression engine with 30+ mode state machine
- Adler-32 and CRC-32 checksum engines with combine operations
- Gzip file I/O with stdio-like interface
- 487 tests passing (399 unit + 68 integration + 20 doc tests)
- Cargo-based CI/CD pipeline (`.github/workflows/ci.yml` + `fuzz.yml`) added for the Rust crate
- Pure Rust — zero C dependencies in the shipped artifact

**Previously-Tracked Open Items (now resolved):**
- `--no-default-features` test compilation is RESOLVED. The test suite now compiles and passes under both `--no-default-features` and `--no-default-features --features no-std` (363 passed, 0 ignored, 0 failed); the library itself compiles fine under all feature configurations.
- cargo-fuzz targets are RESOLVED. Five targets exist under `fuzz/fuzz_targets/` (`fuzz_deflate_roundtrip`, `fuzz_inflate`, `fuzz_checksum`, `fuzz_gzip`, `fuzz_ffi_roundtrip`) with a `fuzz/Cargo.toml`, driven by the `fuzz.yml` workflow.
- Performance vs C zlib has been measured (per the ≥80%-of-C compression performance goal, AAP §0.6). Consistent with `README.md`: compression is ≈ **77–90%** of C (≈ 80% and above at levels 6–9 on compressible data; ≈ 77% at level 1 / incompressible input), decompression is ≈ **78–115%** of C (matching or exceeding C on incompressible and semi-structured data; ≈ 78% on highly compressible text where C's `inffast` has an edge), and CRC-32 runs at ≈ **1.6×** C via the SIMD-accelerated `crc32fast` hot path. Closing the remaining compression gap toward the ≥80% goal across *all* inputs is ongoing.

**Recommended Next Steps:** the core codecs, byte-identity, `no_std`, and fuzzing gates are closed; the remaining work is hardening, performance, publication, and — the primary gate — human sign-off.
1. **Formal human code review / sign-off** of the ~40k-line migration — the primary remaining gate before any production release — focusing on the `src/ffi/` `unsafe` boundary and its `// SAFETY:` justifications.
2. Continue performance tuning of the deflate hot paths (hash-chain traversal, `longest_match` in `deflate_slow`) to close the remaining compression gap toward the ≥80%-of-C goal on worst-case incompressible input.
3. Expand real-hardware `no_std` integration testing on an embedded target (the host-harness `no_std` configuration already passes as a blocking CI gate).
4. Extend the `cargo-fuzz` corpus and integrate scheduled fuzzing runs in CI (the five targets already build and run via `.github/workflows/fuzz.yml`).
5. Prepare crates.io publication: confirm the packaged file list via `cargo package --list`, then `cargo publish --dry-run`.

---

## 2. Validation Results Summary

### 2.1 Final Validator Accomplishments

The Final Validator agent completed 1 commit (730a3ef) fixing 4 files:

| File | Fix Applied |
|------|------------|
| `src/inflate/back.rs` | Converted 3 `ignore` doc tests to runnable tests (InflateBackInput, InflateBackOutput, inflate_back_init) |
| `src/inflate/mod.rs` | Converted 1 `ignore` doc test to runnable (inflate_init) |
| `src/inflate/state.rs` | Added 14 unit tests covering parse_window_bits and InflateState construction |
| `src/deflate/mod.rs` | Applied `cargo fmt` formatting fix (function signature line wrapping) |

### 2.2 Gate Results

| Gate | Status | Details |
|------|--------|---------|
| **Dependencies** | ✅ PASS | 89 Cargo packages resolved (6 direct, 83 transitive); crc32fast 1.5.0, cfg-if 1.0.4, criterion 0.5.1, flate2 1.1.9, quickcheck 1.1.0, rand 0.9.4 |
| **Compilation** | ✅ PASS | `cargo build` (debug) — 0 errors, 0 warnings; `cargo build --release` — success; `cargo bench --no-run` — 3 benchmark binaries compile |
| **Linting** | ✅ PASS | `cargo clippy --all-targets -- -D warnings` — 0 lints |
| **Formatting** | ✅ PASS | `cargo fmt -- --check` — all code formatted |
| **Tests** | ✅ PASS | 487 passed, 0 failed, 0 ignored (487 total) |
| **Runtime** | ✅ PASS | Benchmarks compile and execute; all integration tests exercise real compression/decompression round-trips |

### 2.3 Test Results Breakdown

| Test Suite | Tests Passed | Description |
|-----------|-------------|-------------|
| Unit tests (lib) | 399 | Inline module tests across all source files |
| tests/checksum.rs | 18 | Adler-32 and CRC-32 known-answer and combine tests |
| tests/round_trip.rs | 12 | Property-based compression/decompression round-trip tests |
| tests/interop.rs | 14 | Byte-identity vs 300 baked C-zlib oracle vectors + bidirectional `flate2` (miniz_oxide) compat |
| tests/gzip_compat.rs | 6 | Gzip file I/O validation |
| tests/regression.rs | 11 | Port of C test/example.c regression driver |
| tests/inflate_coverage.rs | 7 | Port of C test/infcover.c inflate coverage |
| Doc tests | 20 (20 passed, 0 ignored) | All public API doc examples verified |
| **Total** | **487 passed, 0 failed, 0 ignored (487 total)** | |

### 2.4 Build Status (no_std) — Resolved

**`cargo test --no-default-features` now compiles and passes.** The earlier failure (129 compilation errors from test modules using `Vec`, `format!`, and `String` without `alloc` imports when the `std` feature was disabled) has been resolved: the affected test modules now import from `alloc` (or are gated appropriately). Both `cargo test --no-default-features` and `cargo test --no-default-features --features no-std` compile and pass (363 passed, 0 ignored, 0 failed). The library compiles correctly under all feature configurations, and the CI pipeline (`ci.yml`) steps for these configurations pass.

---

## 3. Visual Representation

### 3.1 Hours Breakdown

```mermaid
pie title Project Hours Breakdown
    "Completed Work" : 240
    "Remaining Work" : 42
```

**Calculation:** 240 hours completed / (240 + 42) total hours = **85.1% complete**

### 3.2 Completed Hours by Component

```mermaid
pie title Completed Work Distribution (240 hours)
    "Deflate Engine" : 60
    "Inflate Engine" : 45
    "Gzip File I/O" : 32
    "Test Suite" : 28
    "Public API Types" : 20
    "Quality/Bug Fixes" : 16
    "Checksum Engines" : 12
    "Architecture/Config" : 8
    "Benchmarks" : 6
    "Utilities" : 6
    "Documentation" : 6
    "CI/CD" : 1
```

---

## 4. Detailed Completion Analysis

### 4.1 Completed Hours Calculation

> Note: The **Hours** column is the original effort-estimate snapshot (sums to 240h and is retained for planning history). The **Lines** column has been refreshed to current measured values, so the two columns reflect different points in the project timeline. The `src/` component rows sum exactly to 40 files / 32,561 lines; Test and Benchmark rows are separate (non-`src/`). See Section 8.4 for the authoritative code-metrics summary.

| Component | Files | Lines | Hours | Rationale |
|-----------|-------|-------|-------|-----------|
| Deflate Engine | 9 files (src/deflate/) | 6,990 | 60h | 5 compression strategies, state machine, hash tables, Huffman trees — most complex module |
| Inflate Engine | 6 files (src/inflate/) | 6,570 | 45h | 30+ mode state machine, fast-path decode loop, callback API, Huffman table builder |
| Gzip File I/O | 6 files (src/gz/) | 5,317 | 32h | stdio-like interface: open/read/write/close/seek with LOOK/COPY/GZIP pipeline |
| Test Suite | 6 files (tests/) | 4,142 | 28h | 6 integration test files porting C test/example.c, infcover.c + new property tests |
| FFI Boundary | 7 files (src/ffi/) | 7,972 | — | `#[unsafe(no_mangle)] extern "C"` drop-in shims + `#[repr(C)]` mirrors (effort folded into Public API Types and Quality rows) |
| Public API Types | 5 files (lib.rs, error.rs, constants.rs, stream.rs, gz_header.rs) | 3,462 | 20h | Foundational types, error handling, streaming interface, version constants |
| Quality & Debugging | — | — | 16h | ~30 Blitzy Agent commits: formatting fixes, clippy compliance, safety comments, bug fixes |
| Checksum Engines | 3 files (src/checksum/) | 874 | 12h | Adler-32 with combine, CRC-32 with combine/gen/op, build.rs table generation |
| Architecture/Config | Cargo.toml, build.rs, .gitignore | 1,703 | 8h | Package manifest, CRC table generation, feature flags, profile config |
| Benchmarks | 3 files (benches/) | 294 | 6h | Criterion-based deflate/inflate/checksum throughput benchmarks |
| Utilities | 4 files (src/util/) | 1,376 | 6h | compress/uncompress wrappers, version/compile_flags |
| Documentation | README.md | 383 | 6h | Comprehensive crate docs with usage examples, feature flags, API overview |
| CI/CD | ci.yml, fuzz.yml | ~135 | 1h | Rust CI pipeline, cargo-fuzz workflow |
| **Total (src/ only)** | **40 .rs** | **32,561** | **240h** | Hours total spans all components; line total is `src/` only |

### 4.2 Remaining Hours Calculation

> Note: This is a historical effort-estimate snapshot. Several items listed below have since been completed — no_std test compilation (rows "Fix no_std test compilation" and "CI pipeline no_std test fixes") and "Fuzz target creation" are RESOLVED (see Sections 1, 2.4, and 5). The hour figures are retained as the original planning estimate.

| Task | Hours | Priority | Confidence |
|------|-------|----------|------------|
| Fix no_std test compilation | 3h | High | High |
| CI pipeline no_std test fixes | 1h | High | High |
| Performance benchmarking vs C zlib | 8h | High | Medium |
| Byte-identical compression verification | 6h | High | Medium |
| Fuzz target creation | 3h | Medium | High |
| Unsafe code security audit | 3h | Medium | Medium |
| no_std integration testing | 3h | Medium | Medium |
| Production edge-case hardening | 4h | Medium | Medium |
| Documentation refinement | 2h | Low | High |
| crates.io publication preparation | 2h | Low | High |
| Enterprise buffer (compliance/uncertainty 1.10×1.10) | 7h | — | — |
| **Total Remaining** | **42h** | | |

---

## 5. Detailed Task Table for Human Developers

| # | Task | Description | Action Steps | Hours | Priority | Severity |
|---|------|-------------|-------------|-------|----------|----------|
| 1 | (RESOLVED) no_std test compilation | Previously failed with 129 compilation errors under `--no-default-features` (test code used `Vec`, `format!`, `String` without `alloc` imports). Now resolved: test modules import from `alloc` (or are gated), and both `cargo test --no-default-features` and `--no-default-features --features no-std` compile and pass (363 passed, 0 ignored, 0 failed). | Complete — no further action required. | 0h (done) | — | Resolved |
| 2 | (VERIFY) CI no_std gates remain green | ci.yml runs `cargo test --locked --no-default-features` and `--no-default-features --features no-std` as gating steps; both currently pass (363 passed, 0 failed, 0 ignored) following task #1. This row is verification-only — there is no outstanding failure. | 1. Periodically confirm the locked no-std CI steps still pass 2. Ensure the feature matrix continues to exercise `--no-default-features` and `--features no-std` 3. Keep the `cargo build --no-default-features` library-only check in CI | 1h | Low | Low |
| 3 | Performance tuning vs C zlib | The ≥80%-of-C compression performance goal (AAP §0.6 / §0.7.1) is already met at levels 6–9 (≈80–90% of C); worst-case level 1 / incompressible input (≈77%) sits just below the goal. Decompression is ≈78–115% of C and CRC-32 ≈1.6× C via the SIMD `crc32fast` hot path — see §1 and `README.md`. | 1. Re-run `cargo bench` for current throughput numbers 2. Profile the deflate hot paths with `perf`/`flamegraph` (hash-chain traversal, `longest_match` in `deflate_slow`) 3. Close the remaining compression gap on incompressible input 4. Document results | 8h | High | Medium |
| 4 | (RESOLVED) Byte-identical compression verification | AAP §0.6.2 requires byte-identical output for the same input/level/strategy/framing. Validated **by default** (no C toolchain) in `tests/interop.rs` against 300 vectors baked from genuine C zlib 1.3.2.1-motley, spanning all levels, strategies, and zlib/raw/gzip/`windowBits` framings; the suite also round-trips against `flate2` (pure-Rust `miniz_oxide` backend) in both directions. | Complete — optional follow-up: extend the vector corpus as new edge cases surface. | 0h (done) | — | Resolved |
| 5 | (RESOLVED) cargo-fuzz targets | Five fuzz targets now exist under `fuzz/fuzz_targets/` (`fuzz_deflate_roundtrip`, `fuzz_inflate`, `fuzz_checksum`, `fuzz_gzip`, `fuzz_ffi_roundtrip`) with a `fuzz/Cargo.toml`, driven by the `fuzz.yml` workflow. | Optional follow-up: extend the fuzzing corpus and schedule recurring runs in CI. | 0h (done) | Low | Resolved |
| 6 | Unsafe code security audit | All `unsafe` is confined to **7 files**: 432 `unsafe` blocks + 33 `unsafe fn`s across the six `src/ffi/` modules that perform raw-pointer work (`deflate`, `inflate`, `gz`, `util`, `types`, `alloc`; `ffi/mod.rs` is a pure safe facade) plus the `no_std` `#[global_allocator]`/`#[panic_handler]` in `src/lib.rs`. The safe core (`deflate`/`inflate`/`checksum`/`gz`/`util`) is `unsafe`-free (`src/inflate/fast.rs` included), and `src/stream.rs` is `#![deny(unsafe_code)]` (it only declares the `unsafe extern "C"` allocator-hook function-pointer types). The security gate passed and clippy `undocumented_unsafe_blocks` is clean — all 283 sites carry a `// SAFETY:` comment. | 1. Periodically re-review the `unsafe` sites at the FFI boundary 2. Verify each `// SAFETY:` comment accurately describes the invariant 3. Add property tests for boundary conditions around the unsafe code 4. Replace `unsafe` with safe alternatives where possible without performance impact | 3h | Medium | Medium |
| 7 | no_std integration testing | Verify the crate works correctly in an actual no_std context beyond just compilation | 1. Create a `#![no_std]` binary test crate that depends on `zlib-rs` with `default-features = false` 2. Test compression/decompression/checksums in no_std mode 3. Verify allocator integration works correctly 4. Test on an embedded target if available | 3h | Medium | Low |
| 8 | Production edge-case hardening | Validate error recovery, boundary conditions, and memory allocation bounds | 1. Test `inflate_sync` error recovery on corrupted data streams 2. Verify `compress_bound` formula matches C zlib exactly for edge cases (0 bytes, MAX input) 3. Validate memory allocation bounds match C zlib (~256KB deflate, ~7KB inflate + window) 4. Test all 7 flush modes with minimal buffer sizes 5. Test preset dictionary edge cases | 4h | Medium | Medium |
| 9 | Documentation refinement | Polish doc comments, add missing examples, verify all links | 1. Verify all `///` doc examples compile and run 2. Add advanced usage examples (streaming, dictionary, windowBits overloading) 3. Review README.md for accuracy against final implementation 4. Ensure docs.rs rendering is correct | 2h | Low | Low |
| 10 | crates.io publication preparation | Prepare for crate publication. `Cargo.toml`'s `exclude` list already keeps the retained C sources/headers (`*.c`/`*.h`/`*.map`/`*.in`), the platform directories, and the legacy C build files out of the packaged `.crate`. | 1. Confirm the packaged file list with `cargo package --list` (only crate files should appear) 2. Tighten the `exclude` list only if any non-crate file still leaks through 3. Run `cargo publish --dry-run` 4. Verify metadata (description, license, categories, keywords) | 2h | Low | Low |
| 11 | Enterprise buffer (compliance + uncertainty) | Reserve hours for unforeseen issues, compliance requirements, and integration surprises | Applied as 1.10×1.10 multiplier on subtotal of tasks 1-10 (35h × 1.21 ≈ 42h total) | 7h | — | — |
| | **Total Remaining Hours** | | | **42h** | | |

---

## 6. Development Guide

### 6.1 System Prerequisites

| Requirement | Version | Purpose |
|------------|---------|---------|
| Rust toolchain | 1.85.0+ | MSRV for edition = "2024" |
| Cargo | 1.85.0+ | Build system (bundled with Rust) |
| Git | 2.x+ | Version control |
| OS | Linux, macOS, or Windows | All platforms supported |

### 6.2 Environment Setup

```bash
# 1. Install Rust toolchain (if not already installed)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source "$HOME/.cargo/env"

# 2. Verify toolchain version
rustc --version    # Expected: rustc 1.85.0 or later
cargo --version    # Expected: cargo 1.85.0 or later

# 3. Clone repository and switch to the migration branch
git clone <repository-url>
cd <repository-directory>
git checkout <migration-branch>
```

### 6.3 Dependency Installation

```bash
# Dependencies are managed by Cargo and installed automatically on first build.
# To pre-fetch all dependencies:
cargo fetch

# Expected: downloads 89 packages (6 direct deps + 83 transitive)
# Runtime deps: crc32fast 1.5.0, cfg-if 1.0.4
# Dev deps: criterion 0.5.1, flate2 1.1.9, quickcheck 1.1.0, rand 0.9.4
```

### 6.4 Build Commands

```bash
# Debug build (fast compilation, unoptimized)
cargo build
# Expected output: Finished `dev` profile [unoptimized + debuginfo]

# Release build (optimized with LTO)
cargo build --release
# Expected output: Finished `release` profile [optimized]

# Compile benchmarks (without running)
cargo bench --no-run
# Expected: 3 benchmark binaries compiled (deflate_bench, inflate_bench, checksum_bench)
```

### 6.5 Running Tests

```bash
# Run all tests (unit + integration + doc tests)
cargo test
# Expected: 487 passed, 0 failed, 0 ignored (487 total)

# Run only unit tests
cargo test --lib
# Expected: 399 passed

# Run only integration tests
cargo test --tests
# Expected: 399 lib + 68 integration = 467 passed

# Run only doc tests
cargo test --doc
# Expected: 20 passed, 0 ignored (20 total)

# Run specific test suite
cargo test --test regression       # Port of C test/example.c
cargo test --test checksum         # Adler-32 and CRC-32 tests
cargo test --test round_trip       # Property-based round-trip tests
cargo test --test interop          # Cross-validation vs C zlib
cargo test --test gzip_compat      # Gzip format compatibility
cargo test --test inflate_coverage # Inflate state machine coverage
```

### 6.6 Linting and Formatting

```bash
# Run Clippy linter (should produce 0 warnings)
cargo clippy --all-targets -- -D warnings

# Check formatting (should produce no output = all formatted)
cargo fmt -- --check

# Auto-format code (if needed)
cargo fmt
```

### 6.7 Running Benchmarks

```bash
# Run all benchmarks (compression, decompression, checksums)
cargo bench

# Run specific benchmark group
cargo bench --bench deflate_bench
cargo bench --bench inflate_bench
cargo bench --bench checksum_bench
```

### 6.8 Fuzzing

The crate is fuzzed with [`cargo-fuzz`](https://crates.io/crates/cargo-fuzz) / libFuzzer. The `fuzz/` directory is a **detached** package — it carries its own `[workspace]` table, so the root `cargo build`/`test`/`clippy`/`fmt` never touch it — and is driven by [`libfuzzer-sys 0.4`](https://crates.io/crates/libfuzzer-sys). It ships **five** targets under `fuzz/fuzz_targets/`:

| Target | Exercises |
|--------|-----------|
| `fuzz_inflate` | Decompression of arbitrary / malformed input (no panics, no UB) |
| `fuzz_deflate_roundtrip` | compress → decompress round-trip identity across levels / strategies |
| `fuzz_gzip` | gzip (RFC 1952) framing parse / round-trip |
| `fuzz_checksum` | Adler-32 / CRC-32 chunked-vs-single and `*_combine` parity |
| `fuzz_ffi_roundtrip` | The C-ABI boundary (`src/ffi/`) with raw pointers |

libFuzzer requires a **nightly** toolchain (for the `-Z` sanitizer / coverage instrumentation):

```bash
# Install the tooling (once)
rustup toolchain install nightly
cargo install cargo-fuzz --locked

# Build every target
cd fuzz && cargo +nightly fuzz build

# Run a single target (bounded run)
cargo +nightly fuzz run fuzz_inflate -- -max_total_time=60
```

`.github/workflows/fuzz.yml` builds all five targets and runs each for a bounded time budget on a nightly toolchain (on `workflow_dispatch` and on a schedule), failing the pipeline on any crash and uploading the reproducer from `fuzz/artifacts`.

### 6.9 Verification Steps

After building and testing, verify the following:

1. **Compilation:** `cargo build` and `cargo build --release` both succeed with 0 errors, 0 warnings
2. **Tests:** `cargo test` reports 487 passed, 0 failed, 0 ignored (487 total)
3. **Clippy:** `cargo clippy --all-targets -- -D warnings` reports 0 lints
4. **Formatting:** `cargo fmt -- --check` produces no output
5. **Benchmarks:** `cargo bench --no-run` compiles all 3 benchmark binaries

### 6.10 Example Usage

```rust
use zlib_rs::{compress2, compress_bound, uncompress, Z_BEST_COMPRESSION};

fn main() {
    // One-call compression into a caller-provided buffer.
    // `compress2` writes into `dest` and returns the number of bytes
    // produced; the trailing argument selects the compression level.
    let data = b"Hello, zlib-rs! This is a compression test.";
    let mut packed = vec![0u8; compress_bound(data.len())];
    let produced = compress2(&mut packed, data, Z_BEST_COMPRESSION)
        .expect("compression failed");
    packed.truncate(produced);

    // One-call decompression into a caller-provided buffer sized to the
    // known original length; `uncompress` returns the number of bytes written.
    let mut restored = vec![0u8; data.len()];
    let written = uncompress(&mut restored, &packed)
        .expect("decompression failed");
    restored.truncate(written);

    assert_eq!(data.as_slice(), &restored[..]);
    println!(
        "Round-trip successful: {} bytes -> {} bytes -> {} bytes",
        data.len(),
        packed.len(),
        restored.len()
    );
}
```

### 6.11 Troubleshooting

| Issue | Cause | Resolution |
|-------|-------|------------|
| `cargo test --no-default-features` fails | (RESOLVED) previously missing `alloc` imports in test code | Fixed — test modules import from `alloc`; the suite compiles and passes under `--no-default-features` and `--no-default-features --features no-std` |
| `cargo fuzz build` fails | (RESOLVED) previously no fuzz targets existed | Fixed — five targets exist under `fuzz/fuzz_targets/` with a `fuzz/Cargo.toml` |
| Benchmark results below C zlib on incompressible input | Worst-case level 1 / incompressible input only | Tracked, non-failing (see §1 and `README.md`): compression ≈ 77–90% of C (≈ 80%+ at levels 6–9; ≈ 77% at level 1 / incompressible), decompression ≈ 78–115% of C, CRC-32 ≈ 1.6× C. Optionally profile/optimize the deflate hot paths |

---

## 7. Risk Assessment

### 7.1 Technical Risks

| Risk | Severity | Likelihood | Impact | Mitigation |
|------|----------|------------|--------|------------|
| Compression output not byte-identical to C zlib | Resolved | — | Interoperability with existing zlib-compressed data | RESOLVED — byte-identity is validated **by default** in `tests/interop.rs` against 300 C-zlib oracle vectors across all levels/strategies/framings (AAP §0.6.2); no C toolchain required |
| Performance below the ≥80%-of-C compression goal on incompressible input | Low | Medium | Falls short of the AAP §0.6 / §0.7.1 performance goal only on worst-case (level 1 / incompressible) input | Task #3: profile and tune the deflate hot paths (hash-chain traversal, `longest_match`); levels 6–9 already meet the goal |
| Unsafe code contains unsound invariants | High | Low | Memory-safety violations defeating the purpose of the Rust rewrite | Task #6: audit the 432 `unsafe` blocks / 33 `unsafe fn`s at the `src/ffi/` boundary; all carry `// SAFETY:` and clippy `undocumented_unsafe_blocks` is clean; add property tests for boundary conditions |
| no_std test failures block CI | Resolved | — | Previously failed the no-default-features test configurations | RESOLVED — alloc imports fixed; `--no-default-features` and `--no-default-features --features no-std` compile and pass |

### 7.2 Security Risks

| Risk | Severity | Likelihood | Impact | Mitigation |
|------|----------|------------|--------|------------|
| No fuzz testing coverage | Resolved | — | Undiscovered crashes or panics on malformed input | RESOLVED — five cargo-fuzz targets exist (inflate, deflate round-trip, checksum, gzip, FFI round-trip); extend corpus and schedule CI runs |
| FFI boundary `unsafe` — C caller passes an invalid pointer | Medium | Low | Undefined behavior if a C caller violates the documented contract | The `src/ffi/` shims null-check and validate handles; every block carries `// SAFETY:`; `fuzz_ffi_roundtrip` exercises the boundary. (Note: `src/inflate/fast.rs` is safe Rust with bounds-checked slice access — it contains no `unsafe`.) |
| Integer overflow in checksum combine | Low | Low | Incorrect checksum values | Covered by the known-answer and `*_combine` parity tests (18 in `tests/checksum.rs` plus module unit tests) |

### 7.3 Operational Risks

| Risk | Severity | Likelihood | Impact | Mitigation |
|------|----------|------------|--------|------------|
| Retained reference/C files included in the published crate | Low | Low | Unnecessary bloat in the packaged `.crate` | Mitigated — `Cargo.toml`'s `exclude` already covers the C sources/headers, platform dirs, and legacy C build files; verify with `cargo package --list` (Task #10) |
| No published crate on crates.io | Low | Medium | Users cannot `cargo add zlib-rs` | Task #10: Validate and publish |
| Missing changelog/release notes | Low | Medium | Users unaware of capabilities and limitations | Add CHANGELOG.md before publication |

### 7.4 Integration Risks

| Risk | Severity | Likelihood | Impact | Mitigation |
|------|----------|------------|--------|------------|
| Incompatible with flate2 as backend | Medium | Low | Cannot serve as drop-in replacement for miniz_oxide | Test integration as flate2 backend; may require additional wrapper crate |
| windowBits edge cases not fully tested | Medium | Low | Format auto-detection failures | Task #8: Test all windowBits combinations (-15 to 47) |
| Preset dictionary interop with C zlib | Medium | Low | Dictionary-compressed streams may not interoperate | Covered by regression tests; add cross-implementation dictionary tests |

---

## 8. Git Repository Analysis

### 8.1 Commit Summary

This is a **same-repository, additive** migration: the Rust crate was committed **alongside** the upstream zlib C baseline, which is **retained in the tree as the behavioral/ABI reference oracle**. The C sources were **not** mass-deleted — earlier "net reduction" framing was inaccurate.

- **Migration commits:** authored by the Blitzy Agent (`agent@blitzy.com`), atop the inherited upstream zlib history.
- **Added (the Rust crate):** `src/` (40 `.rs` modules, ~32,561 lines), `tests/` (6 integration suites, 4,142 lines), `benches/` (3 Criterion suites, 294 lines), `fuzz/` (5 libFuzzer targets + `fuzz/Cargo.toml`), `build.rs` (361 lines), `Cargo.toml`/`Cargo.lock`, the CI workflows (`.github/workflows/ci.yml`, `fuzz.yml`), and the documentation set (`README.md`, `doc/`, `mkdocs.yml`, `catalog-info.yaml`).
- **Retained as reference (unchanged in the working tree):** the root C sources/headers (15 `*.c` + 11 `*.h`), `zlib.h` + `zlib.map` (the API/ABI contract), the legacy C build system (`CMakeLists.txt`, `Makefile.in`, `configure`, `treebuild.xml`, …), the platform directories (`amiga/`, `msdos/`, `os400/`, `qnx/`, `watcom/`, `win32/`), `contrib/`, `examples/`, and `test/`.
- **Published-crate scope:** `Cargo.toml`'s `exclude` list keeps the retained C files, platform dirs, and legacy build files **out of the packaged `.crate` only** — it has no effect on `cargo build`/`test`/`bench` in the workspace.

### 8.2 Files Created

| Category | Count | Key Files |
|----------|-------|-----------|
| Rust source (src/) | 40 | lib.rs, error.rs, constants.rs, stream.rs, gz_header.rs + deflate/inflate/checksum/gz/util/ffi modules |
| Integration tests | 6 | regression.rs, inflate_coverage.rs, round_trip.rs, interop.rs, gzip_compat.rs, checksum.rs |
| Benchmarks | 3 | deflate_bench.rs, inflate_bench.rs, checksum_bench.rs |
| Configuration | 4 | Cargo.toml, Cargo.lock, build.rs, .github/workflows/ci.yml |
| Documentation | 1 | README.md |

### 8.3 C Baseline Retained as Reference (Not Deleted)

> **Correction:** an earlier revision of this guide listed these files as "deleted." That was inaccurate. The upstream zlib C code is **retained in the repository as the migration's behavioral/ABI reference oracle** — it was **not** removed. `Cargo.toml`'s `exclude` list keeps these files out of the *published* `.crate` only; they remain fully present in the working tree for `cargo build`/`test`/`bench` and for byte-for-byte behavioral comparison against the Rust port.

| Retained C artifact | Count / detail | Role in the migration |
|---------------------|----------------|-----------------------|
| C source files (`*.c`) | 15 — deflate.c, inflate.c, trees.c, crc32.c, adler32.c, compress.c, uncompr.c, gzlib.c, gzread.c, gzwrite.c, gzclose.c, infback.c, inffast.c, inftrees.c, zutil.c | Behavioral oracle for the Rust port |
| C header files (`*.h`) | 11 — zlib.h, zconf.h, deflate.h, inflate.h, gzguts.h, zutil.h, trees.h, crc32.h, inffixed.h, inftrees.h, inffast.h | API surface + constant/table reference |
| ABI contract | `zlib.h` (prototypes) + `zlib.map` (exported-symbol version script) | The contract the FFI layer mirrors |
| Legacy C build system | CMakeLists.txt, Makefile.in, configure, make_vms.com, treebuild.xml | Reference only; superseded by Cargo |
| Platform directories | amiga/, msdos/, os400/, qnx/, watcom/, win32/ | Out of scope for migration; retained, not migrated |
| Contributed / example / test trees | contrib/, examples/, test/ | Reference; the C test drivers are reimplemented under `tests/` |

Every entry above appears in `Cargo.toml`'s `exclude` array, so `cargo package`/`publish` ships only the Rust crate — none of these files are removed from the working tree.

### 8.4 Rust Code Metrics

| Metric | Value |
|--------|-------|
| Total Rust files | 50 (40 src + 6 tests + 3 benches + 1 build.rs) |
| Total Rust lines | 37,358 |
| Source lines (src/) | 32,561 |
| Test lines (tests/) | 4,142 |
| Benchmark lines (benches/) | 294 |
| Build script lines (build.rs) | 361 |
| Unsafe blocks / fns | 432 `unsafe` blocks + 33 `unsafe fn`s, confined to **7 files** — the six raw-pointer `src/ffi/` modules (`deflate`, `inflate`, `gz`, `util`, `types`, `alloc`; `ffi/mod.rs` is a pure safe facade) + the `no_std` `#[global_allocator]`/`#[panic_handler]` in `src/lib.rs`. Safe core (`deflate`/`inflate`/`checksum`/`gz`/`util`) has zero `unsafe`; `src/stream.rs` is `#![deny(unsafe_code)]` |
| FFI exports | **98** `#[unsafe(no_mangle)] extern "C"` shim definitions (deflate 17, inflate 22, gz 34, util 25) compiling to **96 unique exported symbols** — `gzdopen` and `inflateGetHeader` each provide two `cfg`-gated variants of which exactly one is built per configuration. (A naive `grep -r no_mangle src/ffi` reports 106 because it also matches 8 mentions of the attribute inside doc-comments.) Separately, the `zlib.map` version script lists **54** `global:` symbols: the 47 canonical zlib symbols through the `ZLIB_1.2.12` node, plus `deflateUsed` (`ZLIB_1.3.1.2`) and six `_z*` aliases (`ZLIB_1.3.2`). |
| SAFETY comments | 283 (0 undocumented unsafe — clippy `undocumented_unsafe_blocks` clean) |
| Unit tests | 399 |
| Integration tests | 68 |
| Doc tests | 20 (20 passed, 0 ignored) |

> **Reproducing these metrics.** Every count above is measured directly from the source tree and can be regenerated with:
>
> - Source / test / bench line totals: `find src -name '*.rs' | xargs wc -l | tail -1` (likewise for `tests` and `benches`); build script: `wc -l < build.rs`.
> - `// SAFETY:` comments: `grep -rc 'SAFETY:' src | awk -F: '{s+=$2} END{print s}'`.
> - `unsafe` blocks: `grep -rn 'unsafe {' src | wc -l`.
> - FFI shim definitions: `grep -rnE '^\s*#\[\s*unsafe\(\s*no_mangle\s*\)' src/ffi | wc -l` (counts code attributes only, excluding doc-comment mentions of the attribute).
>
> The per-file line counts in §9.1 sum to their module subtotals, which in turn sum to the `src/` grand total of 32,561 lines (deflate 6,990 + inflate 6,570 + checksum 874 + gz 5,317 + util 1,376 + ffi 7,972 + the five root files 3,462).

---

## 9. Module Architecture

### 9.1 Source Module Breakdown

The Rust crate lives under `src/`. The upstream zlib C files at the repository root (`*.c`/`*.h`, `zlib.h`, `zlib.map`) are **retained alongside it as the behavioral/ABI reference oracle** — they are excluded from the published crate but remain in the working tree for `cargo build`/`test`/`bench` and for byte-for-byte comparison (see §8.1 / §8.3).

```
src/                                  (40 files, 32,561 lines)
├── lib.rs              (520 lines)  — Crate root, public re-exports, version constants
├── error.rs            (448 lines)  — ZlibError enum, ReturnCode enum, Result type alias
├── constants.rs        (729 lines)  — Flush modes, compression levels, strategies, limits
├── stream.rs          (1,317 lines) — ZStream struct, buffer management
├── gz_header.rs        (448 lines)  — GzHeader struct for gzip metadata
├── deflate/                          (9 files, 6,990 lines)
│   ├── mod.rs        (1,401 lines) — Public deflate API, main deflate state machine
│   ├── state.rs      (1,847 lines) — DeflateState struct (~80 fields)
│   ├── trees.rs      (1,736 lines) — Huffman tree construction
│   ├── strategy.rs     (471 lines) — CompressionConfig, strategy dispatch
│   ├── fast.rs         (265 lines) — Greedy matching (levels 1-3)
│   ├── slow.rs         (302 lines) — Lazy matching (levels 4-9)
│   ├── stored.rs       (313 lines) — Level 0 pass-through
│   ├── huff.rs         (196 lines) — Huffman-only (no LZ77)
│   └── rle.rs          (459 lines) — Run-length encoding
├── inflate/                          (6 files, 6,570 lines)
│   ├── mod.rs        (2,401 lines) — Public inflate API, 30+ mode state machine
│   ├── state.rs        (957 lines) — InflateState, InflateMode enum
│   ├── fast.rs         (804 lines) — Fast-path bulk decode loop
│   ├── tables.rs       (860 lines) — Huffman table builder
│   ├── fixed.rs        (334 lines) — Pre-built fixed Huffman tables
│   └── back.rs       (1,214 lines) — Callback-based decompression
├── checksum/                         (3 files, 874 lines)
│   ├── mod.rs           (13 lines) — Public checksum re-exports
│   ├── adler32.rs      (458 lines) — Adler-32 with combine
│   └── crc32.rs        (403 lines) — CRC-32 with combine/gen/op
├── gz/                               (6 files, 5,317 lines)
│   ├── mod.rs          (203 lines) — Public gzip I/O re-exports
│   ├── state.rs        (574 lines) — GzState struct
│   ├── open.rs       (1,511 lines) — gz_open, gz_seek, gz_tell, etc.
│   ├── read.rs       (1,371 lines) — gz_read, gz_fread, gz_getc, etc.
│   ├── write.rs      (1,245 lines) — gz_write, gz_fwrite, gz_putc, etc.
│   └── close.rs        (413 lines) — gz_close dispatcher
├── util/                             (4 files, 1,376 lines)
│   ├── mod.rs          (157 lines) — Public utility re-exports
│   ├── compress.rs     (414 lines) — compress, compress2, compress_bound
│   ├── uncompress.rs   (479 lines) — uncompress, uncompress2
│   └── version.rs      (326 lines) — ZLIB_VERSION, compile_flags, error_message
└── ffi/                              (7 files, 7,972 lines)
    ├── mod.rs          (265 lines) — FFI module root, symbol re-exports
    ├── types.rs      (1,734 lines) — #[repr(C)] z_stream + gz_header mirrors
    ├── deflate.rs    (1,335 lines) — extern "C" deflate* shims
    ├── inflate.rs    (1,992 lines) — extern "C" inflate* / inflateBack* shims
    ├── gz.rs         (1,388 lines) — extern "C" gz* shims
    ├── util.rs       (1,080 lines) — extern "C" one-call, checksum, version shims
    └── alloc.rs        (178 lines) — Caller zalloc/zfree allocator-hook bridge
```

### 9.2 Feature Flags

Feature flags map the C preprocessor conditionals (`GZIP`, `NO_GZCOMPRESS`, `Z_SOLO`, `INFLATE_STRICT`) onto Cargo features. The names and defaults below are the authoritative contract and are kept in sync with `Cargo.toml` and the `README.md` feature table.

| Feature | Default | Purpose |
|---------|---------|---------|
| `std` | yes | Standard-library build — I/O, allocation, formatting, and the `catch_unwind` FFI panic guards |
| `gzip` | yes | gzip framing within the deflate/inflate engines (maps C `#ifdef GZIP`) |
| `gz-io` | yes | gzip **file** I/O layer — the `gz*` functions (maps C `#ifndef NO_GZCOMPRESS`); **implies `std` + `gzip`** |
| `no-std` | no | Core-only, bare-metal build with no gz file I/O (maps C `Z_SOLO`); built via `--no-default-features` |
| `simd` | yes | SIMD-accelerated CRC-32 via [`crc32fast`](https://crates.io/crates/crc32fast) (`= dep:crc32fast`); scalar fallback when off |
| `inflate_strict` | no | Stricter inflate distance validation (maps C `INFLATE_STRICT`). Off by default so the default build stays byte-exact with reference zlib; enable only to reject out-of-window distances early |

The default feature set is `["std", "gzip", "gz-io", "simd"]`. `inflate_strict` is an optional opt-in beyond that core set and is not required for a complete drop-in ABI.

---

## 10. Related Documentation

- [Home](index.md) — project overview and documentation entry point.
- [Technical Specifications](technical-specifications.md) — the detailed technical reference for the migration.
- `README.md` (repository root) — the single source of truth for feature flags, project layout, build/test commands, and performance figures; this guide is kept in sync with it.
- `Cargo.toml` (repository root) — authoritative for dependency versions, feature names/defaults, crate type, edition/MSRV, and build profiles.
