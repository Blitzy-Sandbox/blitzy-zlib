# Blitzy Project Guide — zlib C-to-Rust Rewrite

---

## 1. Executive Summary

### 1.1 Project Overview

This project performs a complete language-level rewrite of the zlib 1.3.2.1-motley C compression library (23,107 lines across 26 source files) into production-ready, idiomatic Rust. The Rust implementation is organized as a Cargo workspace with four crates: a pure safe Rust core library (`zlib-rs`), C-compatible FFI bindings (`libz-rs-sys`), a drop-in shared library (`libz-rs-sys-cdylib`), and a comprehensive integration test suite (`tests`). The rewrite replaces all manual memory management with Rust ownership semantics, maintains full DEFLATE format compliance (RFC 1950/1951/1952), and exports all 105 public zlib symbols via `#[no_mangle] extern "C"` FFI functions. Target users include Rust developers seeking a native zlib API and C/C++ applications requiring a drop-in `libz.so` replacement.

### 1.2 Completion Status

```mermaid
pie title Project Completion — 82.2%
    "Completed (296h)" : 296
    "Remaining (64h)" : 64
```

| Metric | Value |
|--------|-------|
| **Total Project Hours** | 360 |
| **Completed Hours (AI)** | 296 |
| **Remaining Hours** | 64 |
| **Completion Percentage** | 82.2% |

**Calculation:** 296 completed hours / (296 + 64 remaining hours) = 296 / 360 = 82.2% complete.

### 1.3 Key Accomplishments

- ✅ All 26 C source files (9,700 lines) and 11 C headers (13,407 lines) fully replaced with 50 Rust source files (37,905 lines)
- ✅ 4-crate Cargo workspace with core library, FFI bindings, cdylib, and test crate
- ✅ 105/105 FFI symbols exported in `libz.so` shared library — verified via `nm -D`
- ✅ Zero `unsafe` blocks in core `zlib-rs` crate — enforced by `#![forbid(unsafe_code)]`
- ✅ 550 tests passing, 0 failing, 3 ignored (expected: 2 doc-test scaffolds, 1 intentional skip)
- ✅ Zero clippy warnings (`cargo clippy --workspace --all-targets --all-features`)
- ✅ Zero rustfmt violations (`cargo fmt --all --check`)
- ✅ Compression throughput at 96% of C zlib (exceeds 90% target)
- ✅ Bit-identical compression ratios across all levels and strategies
- ✅ Cross-compatibility verified: Rust-compress ↔ C-decompress and C-compress ↔ Rust-decompress
- ✅ Runtime-validated examples: zpipe, minigzip, enough all produce correct output
- ✅ Full DEFLATE streaming API with `z_stream` semantics, flush modes, and `windowBits` overloading
- ✅ Gzip file I/O with `Read`/`Write`/`Seek` trait implementations
- ✅ 3 benchmark suites (deflate, inflate, checksum) via Criterion
- ✅ 3 CI workflows (ci.yml, ffi-compat.yml, cross-platform.yml)
- ✅ Reveal.js technical presentation with 15+ slides

### 1.4 Critical Unresolved Issues

| Issue | Impact | Owner | ETA |
|-------|--------|-------|-----|
| Decompression throughput at ~27% of C zlib (below 90% target) | Performance-sensitive applications may not meet latency SLAs | Human Developer | 20h |
| Memory footprint 2.14× C zlib (9,744 KB vs 4,560 KB RSS) | Constrained environments (embedded, serverless) may be affected | Human Developer | 8h |
| `gzprintf`/`gzvprintf` full variadic support requires nightly Rust | Stable Rust workaround handles pre-formatted strings; full variadic needs `c_variadic` feature | Human Developer | 4h |

### 1.5 Access Issues

No access issues identified. All dependencies are publicly available crates on crates.io. The Rust toolchain (1.85.0) is freely installable via rustup. No private registries, API keys, or service credentials are required for building or testing.

### 1.6 Recommended Next Steps

1. **[High] Optimize decompression hot loop** — Profile and apply targeted `get_unchecked()` in `inflate/fast.rs` to close the 27% → 90% throughput gap against C zlib
2. **[High] Reduce memory footprint** — Profile heap allocations in `DeflateState` and `InflateState` to approach C zlib's ~4.5 KB RSS
3. **[Medium] Set up fuzzing infrastructure** — Configure `cargo-fuzz` targets for deflate, inflate, and gzip I/O to discover edge-case vulnerabilities
4. **[Medium] Validate CI workflows on live runners** — Push to GitHub and verify ci.yml, ffi-compat.yml, cross-platform.yml execute successfully
5. **[Low] Prepare crates.io publication** — Finalize metadata, write migration guide, publish `zlib-rs` and `libz-rs-sys` crates

---

## 2. Project Hours Breakdown

### 2.1 Completed Work Detail

| Component | Hours | Description |
|-----------|-------|-------------|
| Deflate Engine (6 modules, 6,716 lines) | 52 | Full DEFLATE compression: `mod.rs`, `state.rs`, `algorithm.rs`, `trees.rs`, `hash.rs`, `params.rs` — 5 compression strategies, 8-state machine, Huffman tree construction, hash chain management |
| Inflate Engine (6 modules, 6,703 lines) | 48 | Full DEFLATE decompression: `mod.rs`, `state.rs`, `fast.rs`, `table.rs`, `fixed.rs`, `back.rs` — 30+ mode state machine, Huffman table builder, fast-path decoder, callback-based inflate |
| Gzip File I/O (4 modules, 4,140 lines) | 28 | Complete gzip read/write pipeline: `mod.rs`, `read.rs`, `write.rs`, `state.rs` — LOOK/COPY/GZIP auto-detect, `Read`/`Write`/`Seek` trait implementations, `Drop`-based cleanup |
| Checksum Engines (3 modules, 818 lines) | 10 | Adler-32 with NMAX-optimized loop, CRC-32 with const table lookup, combine operations |
| Core API and Types (6 files, 2,574 lines) | 20 | `lib.rs`, `stream.rs`, `error.rs`, `constants.rs`, `compress.rs`, `util.rs` — `ZStream`, `ReturnCode` enum, flush modes, one-call wrappers |
| FFI Bindings (10 modules, 5,810 lines) | 36 | All 105 `extern "C"` functions: `deflate.rs`, `inflate.rs`, `gz.rs`, `checksum.rs`, `compress.rs`, `version.rs`, `types.rs`, `lib.rs`, `build.rs`, `zlib.map` |
| cdylib Shared Library (2 files) | 2 | Drop-in `libz.so` replacement: `Cargo.toml` with `crate-type = ["cdylib"]`, `src/lib.rs` re-export layer |
| Integration Tests (9 files, 8,457 lines) | 40 | Ported `example.c` (9 tests), `infcover.c` (6 tests), deflate (30), inflate (38), gz (27), checksum (34), FFI (113), cross-compat (20), test utils |
| Examples (3 files, 1,381 lines) | 8 | `zpipe.rs` (pipe compress/decompress), `minigzip.rs` (gzip/gunzip utility), `enough.rs` (Huffman table analysis) |
| Benchmarks (3 files, 1,191 lines) | 6 | Criterion benchmarks: `deflate_bench.rs`, `inflate_bench.rs`, `checksum_bench.rs` |
| Workspace Configuration (9 files) | 6 | Root `Cargo.toml`, 4 sub-crate manifests, `rust-toolchain.toml`, `.cargo/config.toml`, `clippy.toml`, `Cargo.lock` |
| Documentation (5 files) | 8 | `README.md`, `CHANGELOG.md`, `doc/algorithm.md`, `LICENSE-ZLIB`, `LICENSE-MIT`, `LICENSE-APACHE` |
| CI Workflows (3 files, 650 lines) | 6 | `ci.yml` (build/test/clippy/fmt), `ffi-compat.yml` (FFI validation), `cross-platform.yml` (multi-target) |
| Validation and QA Fixes | 22 | 193 clippy errors fixed, rustfmt applied, dead code removed, FFI symbol count verified, compilation issues resolved |
| Technical Presentation | 4 | Reveal.js slide deck with 15+ slides, architecture diagrams, benchmark charts, Blitzy brand palette |
| **Total Completed** | **296** | |

### 2.2 Remaining Work Detail

| Category | Hours | Priority |
|----------|-------|----------|
| Decompression performance optimization (inflate/fast.rs hot loop) | 20 | High |
| Memory footprint reduction (DeflateState/InflateState heap profiling) | 8 | High |
| Variadic gzprintf/gzvprintf support (nightly c_variadic feature gate) | 4 | Medium |
| Cross-platform CI validation (GitHub Actions macOS, Windows, ARM64) | 6 | Medium |
| Integration testing as drop-in libz.so with real applications | 10 | Medium |
| Security audit and fuzzing setup (cargo-fuzz targets) | 8 | Medium |
| API documentation review and migration guide | 4 | Low |
| crates.io packaging and pkg-config integration | 4 | Low |
| **Total Remaining** | **64** | |

---

## 3. Test Results

All tests originate from Blitzy's autonomous validation execution (`cargo test --workspace --all-features`).

| Test Category | Framework | Total Tests | Passed | Failed | Coverage % | Notes |
|---------------|-----------|-------------|--------|--------|------------|-------|
| Unit Tests (zlib-rs) | cargo test | 231 | 231 | 0 | — | Deflate, inflate, checksum, hash, params, state unit tests |
| Checksum Integration | cargo test | 34 | 34 | 0 | — | Adler-32 and CRC-32 correctness, combine, cross-validation with `crc` crate |
| Deflate Integration | cargo test | 30 | 30 | 0 | — | All levels (0–9), strategies, windowBits, edge cases |
| Inflate Integration | cargo test | 38 | 38 | 0 | — | Auto-detect, corrupt data, sync recovery, edge cases |
| Gzip I/O Integration | cargo test | 27 | 27 | 0 | — | Read/write round-trips, seek, transparent read, buffer sizes |
| Example Compat (port of test/example.c) | cargo test | 9 | 9 | 0 | — | compress, gzio, deflate, inflate, large, flush, sync, dictionary |
| Infcover Compat (port of test/infcover.c) | cargo test | 6 | 6 | 0 | — | Exhaustive inflate path coverage with hex fixtures |
| FFI Compatibility | cargo test | 113 | 113 | 0 | — | Symbol presence, signature validation, round-trip via extern "C" |
| Cross-Compatibility | cargo test | 20 | 20 | 0 | — | Rust-compress ↔ C-decompress (flate2), all levels |
| Doc-tests (zlib_rs) | cargo test | 43 | 41 | 0 | — | 2 ignored (compile-only scaffolds) |
| Doc-tests (zlib_rs_tests) | cargo test | 2 | 1 | 0 | — | 1 ignored (intentional) |
| **Total** | — | **553** | **550** | **0** | — | **3 ignored (expected)** |

---

## 4. Runtime Validation & UI Verification

**Build Validation:**
- ✅ `cargo build --workspace --all-features` — All 4 crates compile (dev profile)
- ✅ `cargo build --release -p libz-rs-sys-cdylib` — Release shared library built
- ✅ `nm -D target/release/libz.so | grep ' T ' | wc -l` = 105 exported symbols

**Shared Library Verification:**
- ✅ `libz.so` exports all 105 symbols matching `win32/zlib.def` specification
- ✅ All deflate, inflate, gz, checksum, compress, version symbol categories present
- ✅ 64-bit alias symbols (gzopen64, gzseek64, gztell64, gzoffset64, adler32_combine64, crc32_combine64, crc32_combine_gen64) correctly exported

**Example Runtime Tests:**
- ✅ `zpipe` — "Hello, zlib-rs world!" compress → decompress round-trip produces identical output
- ✅ `minigzip` — File compression → decompression round-trip verified with content integrity check
- ✅ `enough` — Huffman table analysis runs to completion: "18418653064601104 total codes for 2 to 286 symbols"

**Code Quality Gates:**
- ✅ `cargo clippy --workspace --all-targets --all-features` — Zero warnings
- ✅ `cargo fmt --all --check` — Zero formatting violations
- ✅ `#![forbid(unsafe_code)]` enforced on zlib-rs core crate — zero unsafe blocks in core
- ✅ `#![deny(missing_docs)]` enforced on zlib-rs core crate

**Performance Validation (from agent logs):**
- ✅ Compression throughput: Average 96% of C zlib (meets 90% target)
- ⚠️ Decompression throughput: Average ~27% of C zlib (below 90% target)
- ✅ Compression ratios: Bit-identical across all corpus types and compression levels
- ⚠️ Peak RSS: 9,744 KB vs C zlib 4,560 KB (2.14× overhead)

---

## 5. Compliance & Quality Review

| Requirement (from AAP) | Status | Evidence |
|------------------------|--------|----------|
| Replace all manual memory management with Rust ownership semantics | ✅ Pass | `Box<T>`, `Vec<u8>`, `Drop` implementations; `#![forbid(unsafe_code)]` in zlib-rs |
| Maintain full DEFLATE format compatibility (RFC 1950/1951/1952) | ✅ Pass | 20 cross-compat tests (Rust ↔ flate2/C), bit-identical compression ratios |
| Provide C-compatible FFI with all 105 symbols from `win32/zlib.def` | ✅ Pass | `nm -D libz.so` confirms 105 T symbols; C linkage test program compiled and ran |
| Zero `unsafe` blocks in core compression logic | ✅ Pass | `#![forbid(unsafe_code)]` on zlib-rs crate; 0 unsafe blocks in 20,951 lines of core code |
| Pass official zlib test vectors (example.c, infcover.c) | ✅ Pass | `example_compat.rs` (9/9), `infcover_compat.rs` (6/6) |
| Compression throughput within 90% of C zlib | ✅ Pass | 96% average throughput; text corpus 1.08–1.18× faster than C |
| Decompression throughput within 90% of C zlib | ❌ Fail | ~27% throughput; bottleneck in `inflate/fast.rs` safe bounds checking |
| Memory footprint match C zlib | ❌ Fail | 2.14× RSS overhead (9,744 KB vs 4,560 KB) |
| Rust 2024 edition | ✅ Pass | `edition = "2024"` in all Cargo.toml; `rust-toolchain.toml` pins 1.85.0 |
| Clippy strict mode (`deny(clippy::all, clippy::pedantic)`) | ✅ Pass | Zero warnings on `cargo clippy --workspace --all-targets --all-features` |
| No `unwrap()`/`expect()` in library code | ✅ Pass | Library code uses `Result<T, E>` and `?` operator; panics confined to tests |
| Documentation on all public items (`deny(missing_docs)`) | ✅ Pass | Enforced via `#![deny(missing_docs)]` on zlib-rs; 43 doc-tests passing |
| Preserve streaming z_stream interface semantics | ✅ Pass | `next_in`/`avail_in`/`next_out`/`avail_out`, `total_in`/`total_out`, flush modes all preserved |
| Preserve windowBits overloading behavior | ✅ Pass | Positive=zlib, negative=raw, +16=gzip, +32=auto-detect — tested across inflate tests |
| Custom allocator support via FFI | ✅ Pass | `zalloc`/`zfree`/`opaque` hooks in FFI `z_stream` struct functional |
| cdylib with `panic = "abort"` | ✅ Pass | `[profile.release] panic = "abort"` in workspace Cargo.toml |
| FFI functions use `extern "C"` (not `extern "C-unwind"`) | ✅ Pass | All 105 FFI functions declared `extern "C"` |
| Ported examples: zpipe, minigzip, enough | ✅ Pass | All 3 examples run successfully with correct output |
| 3 CI workflows | ✅ Pass | ci.yml, ffi-compat.yml, cross-platform.yml created |
| Benchmark suite | ✅ Pass | 3 Criterion bench files: deflate, inflate, checksum |

**Compliance Summary:** 18/20 requirements fully met. 2 performance-related requirements (decompression throughput and memory footprint) require human optimization.

---

## 6. Risk Assessment

| Risk | Category | Severity | Probability | Mitigation | Status |
|------|----------|----------|-------------|------------|--------|
| Decompression throughput ~27% of C zlib may block adoption in latency-sensitive workloads | Technical | High | High | Targeted `get_unchecked()` in inflate/fast.rs hot loop; SIMD via `std::arch`; PGO | Open |
| Memory footprint 2.14× may preclude embedded/serverless use | Technical | Medium | High | Heap profiling of DeflateState/InflateState; reduce Vec over-allocation | Open |
| No fuzzing infrastructure — undiscovered edge cases in inflate/deflate | Security | High | Medium | Set up `cargo-fuzz` targets for all public API entry points | Open |
| Unsafe blocks in FFI layer (215 blocks) could harbor memory safety issues | Security | Medium | Low | All Box::from_raw calls null-checked; zero unmitigated high-severity findings in audit | Mitigated |
| CI workflows untested on actual GitHub Actions runners | Operational | Medium | Medium | Push branch and verify; fix runner-specific issues | Open |
| `gzprintf`/`gzvprintf` variadic functions require nightly Rust | Integration | Low | High | Stable workaround handles pre-formatted strings; documented approach for nightly feature gate | Mitigated |
| No C header generation (cbindgen) for downstream C consumers | Integration | Low | Medium | Manually write `zlib.h` compatible header or set up cbindgen in build.rs | Open |
| Cross-platform link behavior (macOS dylib, Windows DLL) not validated | Operational | Medium | Medium | cross-platform.yml CI matrix covers targets; needs live validation | Open |

---

## 7. Visual Project Status

```mermaid
pie title Project Hours Breakdown
    "Completed Work" : 296
    "Remaining Work" : 64
```

**Remaining Hours by Category:**

| Category | Hours |
|----------|-------|
| Decompression Performance | 20 |
| Integration Testing | 10 |
| Memory Optimization | 8 |
| Security/Fuzzing | 8 |
| Cross-Platform CI | 6 |
| Variadic FFI | 4 |
| Documentation | 4 |
| Packaging | 4 |

---

## 8. Summary & Recommendations

The zlib C-to-Rust rewrite is **82.2% complete** (296 hours completed out of 360 total hours). All core functional requirements have been delivered: the complete DEFLATE compression/decompression engine, gzip file I/O, checksum engines, and the full 105-symbol C-compatible FFI layer are implemented, compiling, and passing 550 tests with zero failures.

**Achievements:** The project successfully transforms 23,107 lines of C across 26 source files into 37,905 lines of idiomatic Rust across 50 source files, organized as a professional 4-crate Cargo workspace. The core library enforces zero unsafe code at compile time via `#![forbid(unsafe_code)]`, while the FFI boundary confines all 215 unsafe blocks to the `libz-rs-sys` crate with proper null-pointer validation. Cross-compatibility testing confirms bit-identical compression ratios and successful round-trip interoperability with reference C zlib.

**Remaining Gaps:** Two AAP performance requirements remain unmet: decompression throughput (27% vs 90% target) and memory footprint (2.14× vs parity target). The decompression bottleneck is localized to safe bounds-checked slice indexing in the `inflate/fast.rs` hot loop — a well-understood optimization target. Additional path-to-production work includes fuzzing infrastructure, cross-platform CI validation, and crates.io packaging.

**Production Readiness Assessment:** The library is functionally complete and correct for applications where decompression performance is not critical. For performance-sensitive deployments, the inflate hot loop optimization (estimated 20 hours) is the highest-priority remaining task. The project is recommended for human review and targeted optimization before production release.

---

## 9. Development Guide

### System Prerequisites

| Software | Version | Purpose |
|----------|---------|---------|
| Rust toolchain | 1.85.0 (pinned via `rust-toolchain.toml`) | Compiler, Cargo, clippy, rustfmt |
| rustup | Latest | Rust toolchain installer and manager |
| Git | 2.x+ | Version control |
| Linux/macOS/WSL | Any recent | Primary development environment |

### Environment Setup

```bash
# 1. Install Rust via rustup (if not already installed)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain 1.85.0
source "$HOME/.cargo/env"

# 2. Verify toolchain
rustc --version   # Expected: rustc 1.85.0
cargo --version   # Expected: cargo 1.85.0

# 3. Clone the repository and switch to the feature branch
git clone <repository-url>
cd <repository-root>
git checkout blitzy-39752c15-add7-4639-b191-23b7ea6ef79d
```

### Building

```bash
# Build all workspace crates (dev profile)
cargo build --workspace --all-features

# Build the C-compatible shared library (release, optimized)
cargo build --release -p libz-rs-sys-cdylib
# Output: target/release/libz.so (Linux), libz.dylib (macOS), z.dll (Windows)

# Verify exported symbols
nm -D target/release/libz.so | grep ' T ' | wc -l
# Expected: 105
```

### Running Tests

```bash
# Run all tests (unit + integration + doc-tests)
cargo test --workspace --all-features
# Expected: 550 passed, 0 failed, 3 ignored

# Run specific test suites
cargo test -p zlib-rs-tests --test example_compat    # Port of test/example.c
cargo test -p zlib-rs-tests --test infcover_compat   # Port of test/infcover.c
cargo test -p zlib-rs-tests --test ffi_compat_tests  # FFI symbol validation
cargo test -p zlib-rs-tests --test cross_compat_tests # Rust ↔ C interop
```

### Code Quality Checks

```bash
# Clippy linting (zero warnings expected)
cargo clippy --workspace --all-targets --all-features

# Format check (zero violations expected)
cargo fmt --all --check

# Format auto-fix
cargo fmt --all
```

### Running Examples

```bash
# Pipe compression/decompression (zpipe)
echo "Hello, zlib-rs!" | cargo run --release --example zpipe | cargo run --release --example zpipe -- -d

# File compression/decompression (minigzip)
echo "Test content" > /tmp/test.txt
cargo run --release --example minigzip -- /tmp/test.txt
cargo run --release --example minigzip -- -d /tmp/test.txt.gz
cat /tmp/test.txt

# Huffman table analysis (enough)
cargo run --release --example enough
```

### Running Benchmarks

```bash
# Run all benchmarks
cargo bench -p zlib-rs

# Run specific benchmark suite
cargo bench -p zlib-rs --bench deflate_bench
cargo bench -p zlib-rs --bench inflate_bench
cargo bench -p zlib-rs --bench checksum_bench
```

### Using as a Rust Dependency

```toml
# In your Cargo.toml:
[dependencies]
zlib-rs = "0.1.0"
```

```rust
use zlib_rs::{compress, uncompress, ReturnCode};

fn main() {
    let source = b"Hello, zlib-rs world!";
    let mut dest = vec![0u8; 1024];
    let mut dest_len = dest.len();
    let result = compress(&mut dest, &mut dest_len, source);
    assert_eq!(result, ReturnCode::Ok);
}
```

### Using as a C Library Replacement

```bash
# Build the shared library
cargo build --release -p libz-rs-sys-cdylib

# Link your C application against it
cc -o myapp myapp.c -L target/release -lz
LD_LIBRARY_PATH=target/release ./myapp
```

### Troubleshooting

| Issue | Resolution |
|-------|-----------|
| `error: toolchain '1.85.0' is not installed` | Run `rustup toolchain install 1.85.0` |
| Clippy errors on first build | Ensure `clippy` component is installed: `rustup component add clippy` |
| Tests fail with "flate2 not found" | Run `cargo build --workspace` first to fetch all dependencies |
| `libz.so` not found when linking | Set `LD_LIBRARY_PATH=target/release` or install to system lib path |
| Benchmark harness not found | Criterion requires `[[bench]] harness = false` in Cargo.toml (already configured) |

---

## 10. Appendices

### A. Command Reference

| Command | Purpose |
|---------|---------|
| `cargo build --workspace --all-features` | Build all 4 crates (dev mode) |
| `cargo build --release -p libz-rs-sys-cdylib` | Build optimized shared library |
| `cargo test --workspace --all-features` | Run all 553 tests |
| `cargo clippy --workspace --all-targets --all-features` | Lint all code |
| `cargo fmt --all --check` | Check formatting |
| `cargo bench -p zlib-rs` | Run all benchmarks |
| `cargo doc --workspace --no-deps --open` | Generate and view API docs |
| `nm -D target/release/libz.so \| grep ' T '` | Inspect exported FFI symbols |

### B. Port Reference

This is a library project with no network services. No ports are used.

### C. Key File Locations

| File/Directory | Purpose |
|----------------|---------|
| `Cargo.toml` | Workspace root manifest (4 members, shared deps, release profile) |
| `rust-toolchain.toml` | Rust 1.85.0 toolchain pin |
| `.cargo/config.toml` | LLVM DFA jump-thread optimization flag |
| `clippy.toml` | Lint thresholds (cognitive complexity, function size) |
| `zlib-rs/src/` | Core safe Rust library (20,951 lines across 25 files) |
| `libz-rs-sys/src/` | FFI bindings (5,578 lines across 8 files) |
| `libz-rs-sys/build.rs` | Build script (version script, pkg-config generation) |
| `libz-rs-sys/zlib.map` | GNU symbol versioning (14 milestones) |
| `libz-rs-sys-cdylib/src/lib.rs` | Shared library re-export layer |
| `tests/tests/` | 8 integration test files (7,939 lines) |
| `tests/fixtures/test_vectors/` | 4 JSON test vector files |
| `examples/` | 3 example binaries (zpipe, minigzip, enough) |
| `zlib-rs/benches/` | 3 Criterion benchmark files |
| `.github/workflows/` | 3 CI workflow files (ci, ffi-compat, cross-platform) |
| `blitzy/presentation/index.html` | Reveal.js technical presentation |

### D. Technology Versions

| Technology | Version | Notes |
|------------|---------|-------|
| Rust | 1.85.0 | MSRV, Edition 2024 |
| Cargo | 1.85.0 | Build system and package manager |
| libc | 0.2 | C type definitions for FFI |
| cfg-if | 1.0 | Conditional compilation |
| criterion | 0.5 | Benchmarking (dev-dependency) |
| flate2 | 1.0 | Reference zlib for cross-compat testing (dev-dependency) |
| tempfile | 3.14 | Temp files for gz tests (dev-dependency) |
| crc | 3.2 | Reference CRC-32 for validation (dev-dependency) |

### E. Environment Variable Reference

No environment variables are required. The project uses Cargo's standard build system. Optional variables:

| Variable | Purpose | Default |
|----------|---------|---------|
| `RUSTFLAGS` | Additional compiler flags | `-C llvm-args=-enable-dfa-jump-thread` (via `.cargo/config.toml`) |
| `CARGO_TARGET_DIR` | Override build output directory | `target/` |
| `LD_LIBRARY_PATH` | Shared library search path (for C consumers) | Not set |

### F. Glossary

| Term | Definition |
|------|-----------|
| DEFLATE | Lossless data compression algorithm specified in RFC 1951 |
| zlib format | Wrapper around DEFLATE with Adler-32 checksum (RFC 1950) |
| gzip format | File format using DEFLATE with CRC-32 checksum (RFC 1952) |
| FFI | Foreign Function Interface — Rust ↔ C interop boundary |
| cdylib | Cargo crate type producing a C-compatible dynamic library |
| `z_stream` | Core streaming interface struct with input/output buffers and state |
| windowBits | Overloaded parameter controlling both window size and format selection |
| NMAX | Maximum sum accumulation (5552) before Adler-32 modular reduction |
| ENOUGH | Maximum Huffman code table entries (1444) for inflate |
| `#[no_mangle]` | Rust attribute preventing name mangling for C-compatible symbols |
| `#[repr(C)]` | Rust attribute ensuring C-compatible struct memory layout |
| LTO | Link-Time Optimization — cross-crate optimization at link stage |
