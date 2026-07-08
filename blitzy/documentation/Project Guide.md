# Blitzy Project Guide — `zlib-rs`

> **Project:** Tech-stack migration of zlib `1.3.2.1-motley` from ANSI C to a memory-safe Rust crate
> **Branch:** `blitzy-f493b7a8-0fa4-43f6-aeca-1f196c4a1ee3` · **HEAD:** `9cef39a` · **Toolchain:** rustc/cargo `1.96.1`
> **Brand legend:** ■ Completed / AI Work = Dark Blue `#5B39F3` · □ Remaining = White `#FFFFFF`

---

## 1. Executive Summary

### 1.1 Project Overview

This project migrates the zlib compression library (`1.3.2.1-motley`, `ZLIB_VERNUM 0x1321`) from ~23,107 lines of ANSI C into `zlib-rs`, an idiomatic, memory-safe Rust crate (Rust 2024 edition, MSRV 1.85.0). It targets systems developers and any downstream C consumer of zlib. The crate preserves byte-identical DEFLATE (RFC 1951), zlib (RFC 1950), and gzip (RFC 1952) wire formats and exposes the exact zlib C ABI via a `#[no_mangle] extern "C"` FFI boundary emitting `cdylib` and `staticlib` artifacts — enabling drop-in substitution without recompiling consumers, while replacing all manual memory management with Rust ownership. Business impact: eliminates a class of memory-safety defects in a ubiquitous dependency without sacrificing compatibility or performance.

### 1.2 Completion Status

```mermaid
%%{init: {'theme':'base','themeVariables':{'pie1':'#5B39F3','pie2':'#FFFFFF','pieStrokeColor':'#333333','pieOuterStrokeColor':'#333333','pieSectionTextColor':'#333333','pieStrokeWidth':'2px','pieTitleTextSize':'16px'}}}%%
pie showData title AAP-Scoped Completion — 89.9% Complete
    "Completed Work (470h)" : 470
    "Remaining Work (53h)" : 53
```

_Legend: ■ Completed = `#5B39F3` (dark blue) · □ Remaining = `#FFFFFF` (white)._

| Metric | Hours |
|---|---:|
| **Total Hours** | **523** |
| Completed Hours (AI + Manual) | 470 |
| Remaining Hours | 53 |
| **Percent Complete** | **89.9%** |

> Completion is computed on AAP-scoped work only: `470 / (470 + 53) × 100 = 89.9%`. The completed hours are 100% autonomous (Blitzy agents); the remaining 53h is path-to-production hardening requiring human involvement.

### 1.3 Key Accomplishments

- [x] **Full C→Rust source rewrite** — 40 `src/*.rs` modules (~32,354 LOC) across deflate, inflate, checksum, gz, util, public-API, and FFI layers (49 total `.rs` deliverables + 5 fuzz targets).
- [x] **Verified C-ABI drop-in** — `#[repr(C)]` `z_stream`/`gz_header` + `extern "C"` shims; static **and** dynamic linking pass 17/17 runtime checks; `zlibVersion()` returns `1.3.2.1-motley`.
- [x] **Byte-exact behavior** — `crc32("123456789")=0xcbf43926`, `adler32=0x091e01de`, exact `compressBound` formula, `ENOUGH=1444` (AAP §0.6.6 correction applied).
- [x] **Full feature parity** — 10 levels, 5 strategies, 7 flush modes, preset dictionaries, `inflateBack`, zlib/raw/gzip/auto-detect `windowBits`.
- [x] **100% test pass, 0 ignored** — 487 tests default (399 unit + 68 integration + 20 doctests); 363 in `no-default-features`; suites ported from official C drivers.
- [x] **Core-compression safety constraint honored** — `src/deflate/**` contains **zero** `unsafe`; `unsafe` confined to `ffi/**` (+ nominal inflate fast path) with `// SAFETY:` justifications.
- [x] **All 3 AAP-flagged risks resolved** — `no_std` builds and links (previously 129 errors); 5 cargo-fuzz targets (~1.13M execs, 0 crashes); performance validated (~85% compression / 107–127% decompression vs C).
- [x] **Clean quality gates** — clippy `-D warnings` clean; `cargo fmt --check` clean; all 3 crate-types emit.

### 1.4 Critical Unresolved Issues

| Issue | Impact | Owner | ETA |
|---|---|---|---|
| _None blocking._ All AAP-specified deliverables are complete, compile cleanly, and pass 100% of tests. | No release-blocking defects identified. | — | — |
| Formal human code review & sign-off pending | Governance gate before production release (not a build/functional blocker) | Rust maintainer / reviewer | ~16h (see §2.2 / task list) |

### 1.5 Access Issues

| System/Resource | Type of Access | Issue Description | Resolution Status | Owner |
|---|---|---|---|---|
| — | — | No access issues identified. All build, test, lint, fuzz-build, and C drop-in link/run steps were reproduced offline from the warmed dependency cache. | N/A | — |

### 1.6 Recommended Next Steps

1. **[High]** Conduct formal human code review & sign-off of the ~40k-LOC migration, focusing on the `unsafe` FFI boundary and `// SAFETY:` justifications (~16h).
2. **[High]** Run a security & supply-chain audit — `cargo audit` + `cargo deny` on the ~105-package graph plus manual `unsafe` review (~8h).
3. **[Medium]** Expand CI to a cross-platform matrix (Windows, macOS, Linux aarch64, a big-endian target) (~6h).
4. **[Medium]** Establish crates.io packaging & release governance (`cargo publish --dry-run`, versioning, CHANGELOG) (~3h).
5. **[Medium]** Extend the byte-identity conformance matrix across all 10 levels × 5 strategies vs reference C zlib (~4h).

---

## 2. Project Hours Breakdown

### 2.1 Completed Work Detail

| Component | Hours | Description |
|---|---:|---|
| Cargo manifest + `build.rs` | 16 | `Cargo.toml` (edition 2024, crate-type lib/cdylib/staticlib, features) ← `CMakeLists.txt`; `build.rs` generates CRC-32 tables ← `crc32.h` |
| CI/CD migration + README + docs | 12 | `ci.yml` + `fuzz.yml` (6 legacy C workflows removed); `README.md`, `docs/index.md`, `mkdocs.yml`, `LICENSE` |
| Public API & types layer | 42 | `lib.rs`, `error.rs` (`ZlibError`/`ReturnCode`), `constants.rs` (levels/strategies/flush/`windowBits`), `stream.rs` (`ZStream<A>`), `gz_header.rs` |
| DEFLATE compression engine | 92 | 9 modules (mod/state/trees/strategy/fast/slow/stored/huff/rle); all 5 strategies + 10 levels; **zero-unsafe core** |
| INFLATE decompression engine | 74 | 6 modules (mod/state/fast/tables/fixed/back); 30+ mode state machine incl. `inflateBack` raw-callback path |
| Checksum layer | 19 | Adler-32 (`DO16` unrolling + combine) and CRC-32 (SIMD via `crc32fast` + combine/combine_op) |
| Gzip file-I/O layer | 50 | 6 modules (mod/state/open/read/write/close) over `std::fs`/`std::io` |
| One-call wrappers & version | 14 | `util`: `compress`/`compress2`/`compressBound`, `uncompress`/`uncompress2`, `zlibVersion`/`zlibCompileFlags`/`zError` |
| C-ABI FFI drop-in boundary | 62 | 7 modules; `#[repr(C)]` `z_stream`/`gz_header` mirrors; `extern "C"` shims; full public symbol surface exported |
| Test suite (ported C drivers) | 44 | 6 files: regression (example.c), inflate_coverage (infcover.c), gzip_compat (minigzip.c), round_trip, interop (flate2 byte-identity), checksum — 487 tests |
| Criterion benchmarks | 8 | deflate/inflate/checksum throughput harnesses |
| cargo-fuzz targets | 9 | 5 targets (checksum, deflate_roundtrip, ffi_roundtrip, gzip, inflate) + harness/lockfile |
| Executive summary presentation | 6 | Self-contained reveal.js deck (`blitzy-deck/executive-summary.html`) |
| Autonomous validation, QA & review cycles | 22 | 33 Blitzy commits; checkpoint reviews (CP1–CP8), QA findings, doctest promotion, FFI soundness fixes |
| **Total Completed** | **470** | Matches §1.2 Completed Hours |

### 2.2 Remaining Work Detail

| Category | Hours | Priority |
|---|---:|---|
| Human code review & sign-off of the ~40k-LOC safety-critical migration | 16 | High |
| Security & supply-chain audit (`cargo audit`/`cargo deny` + manual `unsafe`/FFI review) | 8 | High |
| Cross-platform CI matrix (Windows/macOS/ARM/big-endian) | 6 | Medium |
| crates.io packaging & release governance | 3 | Medium |
| Broader byte-identity conformance matrix (all levels × strategies vs C) | 4 | Medium |
| Real-hardware `no_std` embedded-target validation | 5 | Medium |
| Sustained/scheduled fuzzing campaign (CI-integrated) setup | 3 | Medium |
| Worst-case incompressible-input deflate perf tuning | 6 | Low |
| Optional `cdylib` symbol-versioning parity from `zlib.map` | 2 | Low |
| **Total Remaining** | **53** | High 24h · Medium 21h · Low 8h |

### 2.3 Reconciliation

`Completed (470h) + Remaining (53h) = Total (523h)` → **89.9% complete**. The remaining 53h is exclusively path-to-production hardening; no AAP feature work remains.

---

## 3. Test Results

_All results below originate from Blitzy's autonomous validation logs and were independently reproduced this session._

**Default configuration (`cargo test --offline`):**

| Test Category | Framework | Total Tests | Passed | Failed | Coverage % | Notes |
|---|---|---:|---:|---:|---|---|
| Unit (library) | Rust `#[test]` | 399 | 399 | 0 | Parity-based* | Per-module tests across deflate/inflate/checksum/gz/util/ffi |
| Integration | Rust (`tests/`) | 68 | 68 | 0 | Parity-based* | checksum 18 · gzip_compat 6 · inflate_coverage 7 · interop 14 · regression 11 · round_trip 12 |
| Doctests | rustdoc | 20 | 20 | 0 | — | Executed API examples (6 promoted from `ignore` during validation) |
| **Total (default)** | — | **487** | **487** | **0** | — | **0 ignored** |

**Additional configurations & harnesses:**

| Test Category | Framework | Total | Passed | Failed | Notes |
|---|---|---:|---:|---:|---|
| `--no-default-features` (no_std/Z_SOLO) | Rust | 363 | 363 | 0 | gz-io/gzip/simd tests correctly compiled out |
| `--all-features` | Rust | 487 | 487 | 0 | Full feature surface |
| Fuzzing (smoke) | cargo-fuzz / libFuzzer | 5 targets | 5 | 0 | ~1.13M executions, **0 crashes** (checksum 369k, inflate 303k, gzip 353k, deflate 56k, ffi 52k) |
| Benchmarks | criterion | 3 | 3 | 0 | Compile + execute (crc32 ~19 GiB/s, adler32 ~3 GiB/s) |

\* Coverage is validated by **behavioral parity** (byte-identical output vs reference C zlib / flate2 and ported official C test drivers) rather than a line-coverage percentage, which was not emitted by the autonomous test logs.

---

## 4. Runtime Validation & UI Verification

_zlib-rs is a headless systems library — there is no product UI. "UI verification" is interpreted as runtime/ABI verification._

- ✅ **Operational — C drop-in (static)**: `libzlib_rs.a` linked into a C program via `zlib.h`; compress→uncompress round-trip, checksums, `compressBound` parity — 17/17 checks pass.
- ✅ **Operational — C drop-in (dynamic)**: `libzlib_rs.so` linked + run with `LD_LIBRARY_PATH`; identical results, proving `#[repr(C)] z_stream` layout matches C.
- ✅ **Operational — checksums**: `crc32("123456789")=0xcbf43926`, `adler32("123456789")=0x091e01de`.
- ✅ **Operational — gzip framing**: `deflateInit2` (wbits=31) emits `1f 8b`; `inflateInit2` auto-detect works.
- ✅ **Operational — FFI symbol surface**: `cdylib` exports the full public zlib API (95 matching symbols via `nm -D`; validator confirmed all 54 documented public entry points from `zlib.map`, zero internal symbols leaked).
- ✅ **Operational — artifacts**: `libzlib_rs.rlib` (2.3M), `libzlib_rs.so` (594K), `libzlib_rs.a` (22M) all emit under `--release`.
- ✅ **Operational — fuzz stability**: 5 targets, ~1.13M executions, 0 crashes.
- ✅ **Operational — executive deck**: self-contained reveal.js HTML renders independently.

---

## 5. Compliance & Quality Review

| AAP Deliverable / Benchmark | Status | Progress | Notes / Fixes Applied |
|---|---|---|---|
| Full C→Rust source rewrite (all layers) | ✅ Pass | 100% | 40 `src/*.rs`; zero placeholders/`todo!()`/`unimplemented!()` |
| DEFLATE compress + decompress + `inflateBack` | ✅ Pass | 100% | All 5 strategies; back-inflate raw-callback ported |
| zlib / gzip / raw / auto-detect framing | ✅ Pass | 100% | `windowBits` overloading preserved (8–15, −8..−15, 24–31, 40–47) |
| C-ABI FFI drop-in (`extern "C"`, `#[repr(C)]`) | ✅ Pass | 100% | cdylib + staticlib; verified static & dynamic linking |
| 10 levels + 5 strategies + 7 flush modes + preset dict | ✅ Pass | 100% | Config table → enum dispatch |
| Adler-32 / CRC-32 (+combine), SIMD CRC | ✅ Pass | 100% | Bit-exact vectors verified |
| Byte-identical wire format | ✅ Pass | 100% | interop vs flate2; broader all-levels×strategies sweep is a hardening item (§2.2) |
| Zero `unsafe` in core compression (`src/deflate/**`) | ✅ Pass | 100% | Constraint honored; several modules `#![deny(unsafe_code)]` |
| `unsafe` isolated + `// SAFETY:` documented | ✅ Pass | 100% | Confined to `ffi/**`; 267 `// SAFETY:` comments |
| Test suite ported from official C drivers | ✅ Pass | 100% | example.c, infcover.c, minigzip.c |
| Rust 2024 / MSRV 1.85.0 | ✅ Pass | 100% | Builds + `check --all-targets` on MSRV |
| `no_std` via feature flag | ✅ Pass | 100% | AAP 129-error risk **RESOLVED**; builds & links, 363 tests pass |
| Zero C dependency in shipped artifact | ✅ Pass | 100% | `crc32fast`/`cfg-if` pure Rust; `flate2` dev-only via `miniz_oxide` |
| clippy clean / `fmt` clean | ✅ Pass | 100% | `-D warnings` clean |
| cargo-fuzz targets | ✅ Pass | 100% | AAP "no fuzz targets" risk **RESOLVED** (5 targets) |
| Performance goals (≥80% compression, ≥C decompression) | ✅ Pass | 100% | ~85% compression; 107–127% decompression |
| Executive reveal.js deck | ✅ Pass | 100% | `blitzy-deck/executive-summary.html` |
| Formal human review / security audit / cross-platform CI | ⚠ Pending | 0% | Path-to-production items (§2.2) |

**Fixes applied during autonomous validation:** promoted 6 illustrative doctests to executed examples (commit `90526d6`); tracked `fuzz/Cargo.lock` for reproducible fuzz builds (commit `9cef39a`).

---

## 6. Risk Assessment

| Risk | Category | Severity | Probability | Mitigation | Status |
|---|---|---|---|---|---|
| Worst-case incompressible-input compression below C throughput | Technical | Low | Medium | Tune `longest_match`/hash-chain hot path (≥80% goal already met on level sweep) | Open (tracked) |
| Byte-identity not exhaustively swept across all levels × strategies vs C | Technical | Medium | Low | Expand interop conformance matrix (identity already asserted on tested cases) | Open |
| `no_std` validated only in host harness, not real embedded hardware | Technical | Low | Low | Add embedded-target CI (thumbv7em, etc.) | Open |
| `unsafe` at FFI boundary — UB if C caller passes invalid pointers | Security | Medium | Low | 267 `// SAFETY:` justifications, null/stateless guards tested, `fuzz_ffi` 52k execs 0 crashes | Mitigated (audit pending) |
| Supply chain — ~105 transitive deps not formally audited | Security | Low | Low | Add `cargo audit`/`cargo deny` to CI | Open |
| Allocator-hook path (caller `zalloc`/`zfree`) misuse | Security | Low | Low | Routed through hook; OOM path tested | Mitigated |
| CI validates only Linux/x86_64 | Operational | Medium | Low | Cross-platform CI matrix | Open |
| No crates.io release/publish pipeline | Operational | Low | Medium | Release governance + `cargo publish --dry-run` | Open |
| Sustained fuzzing not scheduled (smoke only) | Operational | Low | Low | Scheduled/CI-integrated fuzz job | Partially mitigated |
| `cdylib` lacks symbol-versioning (`@ZLIB_1.x`) | Integration | Low | Low | Apply version script at cdylib link (optional per AAP §0.6.5) | Open (optional) |
| `#[repr(C)]` `z_stream` layout parity verified on x86_64 Linux only | Integration | Medium | Low | 17/17 runtime drop-in checks pass (static+dynamic); validate on more ABIs | Mitigated |
| gz file-I/O relies on `std::fs`; `Z_SOLO`/`no_std` consumers must avoid `gz-io` | Integration | Low | Low | Feature-gated; compiles out cleanly | Mitigated |

---

## 7. Visual Project Status

```mermaid
%%{init: {'theme':'base','themeVariables':{'pie1':'#5B39F3','pie2':'#FFFFFF','pieStrokeColor':'#333333','pieOuterStrokeColor':'#333333','pieSectionTextColor':'#333333','pieStrokeWidth':'2px','pieTitleTextSize':'16px'}}}%%
pie showData title Project Hours Breakdown (Total 523h)
    "Completed Work" : 470
    "Remaining Work" : 53
```

_Legend: ■ Completed = `#5B39F3` (dark blue) · □ Remaining = `#FFFFFF` (white)._

**Remaining hours by priority (from §2.2):**

```mermaid
%%{init: {'theme':'base','themeVariables':{'pie1':'#5B39F3','pie2':'#7A6DEC','pie3':'#D9D9D9','pieSectionTextColor':'#333333','pieStrokeColor':'#333333'}}}%%
pie showData title Remaining Work by Priority (53h)
    "High" : 24
    "Medium" : 21
    "Low" : 8
```

> **Integrity:** "Remaining Work" = **53h**, identical to §1.2 (Remaining Hours) and the §2.2 Hours-column total. "Completed Work" = **470h** = §1.2 Completed Hours = §2.1 total.

---

## 8. Summary & Recommendations

**Achievements.** The zlib→Rust migration is functionally complete and validated. Every AAP-specified deliverable — the full source rewrite, DEFLATE/inflate engines (including `inflateBack`), all framing modes, the C-ABI FFI drop-in, all levels/strategies/flush modes, checksums, the ported official test suite, benches, fuzz targets, and the executive deck — is implemented, compiles with zero warnings, and passes 100% of tests (487 default / 363 no-default, 0 ignored). The verified C drop-in (static + dynamic) with byte-exact checksums and `zlibVersion()=1.3.2.1-motley` demonstrates true API/ABI equivalence. All three AAP-flagged risks (`no_std`, fuzzing, performance) are resolved.

**Remaining gaps.** The outstanding **53h** is exclusively path-to-production hardening requiring human judgment: formal code review & sign-off, a security/supply-chain audit, cross-platform CI, crates.io release governance, a broader conformance sweep, embedded validation, scheduled fuzzing, perf tuning, and optional symbol-versioning. None block the build or core functionality.

**Critical path to production.** (1) Human code review + security audit (High, 24h) → (2) cross-platform CI + conformance matrix (Medium) → (3) crates.io release governance (Medium) → (4) optional hardening (Low).

**Production readiness.** The crate is **engineering-complete and production-ready pending human sign-off**. Per PA1 methodology, the project is **89.9% complete** (470 of 523 AAP-scoped hours). Success metrics met: zero-warning build across all configurations, 100% test pass rate, verified drop-in, and performance goals achieved. Recommendation: proceed to human review and the security audit as the immediate next actions.

---

## 9. Development Guide

_All commands below were executed and verified this session (rustc/cargo 1.96.1). Run from the repository root._

### 9.1 System Prerequisites

- **Rust toolchain** `1.85.0`+ (verified `1.96.1`), edition 2024. Install via [rustup](https://rustup.rs).
- **Optional (C drop-in only):** a C compiler (verified `gcc` 15.2.0). The repository ships `zlib.h` at the root for drop-in linking.
- **Optional (fuzzing):** nightly toolchain + `cargo-fuzz`.
- No C toolchain is required to build the Rust crate itself (dependencies are pure Rust).

```bash
rustc --version    # rustc 1.96.1
cargo --version    # cargo 1.96.1
```

### 9.2 Environment Setup

No environment variables are required for building or testing. Dependencies resolve from crates.io (or a warmed cache with `--offline`).

```bash
# Optional: pin to MSRV for a compatibility check
rustup toolchain install 1.85.0
```

### 9.3 Dependency Installation & Build

```bash
# Debug build (all default features: std, gzip, gz-io, simd)
cargo build

# Optimized release build — emits libzlib_rs.{rlib,so,a}
cargo build --release
ls -lh target/release/libzlib_rs.rlib target/release/libzlib_rs.so target/release/libzlib_rs.a
```

Expected: `Finished ... release [optimized]` and three artifacts (~2.3M rlib, ~594K .so, ~22M .a).

### 9.4 Test & Quality Gates

```bash
cargo test                              # 487 pass (399 unit + 68 integration + 20 doctests), 0 ignored
cargo test --no-default-features        # 363 pass (no_std / Z_SOLO)
cargo test --all-features               # 487 pass
cargo clippy --all-targets -- -D warnings   # clean, exit 0
cargo fmt --all -- --check                  # clean, exit 0
cargo bench --no-run                        # compiles 3 criterion benches
```

### 9.5 Feature Flags

| Feature | Maps to C | Effect |
|---|---|---|
| `std` (default) | — | Standard-library build |
| `gzip` (default) | `#ifdef GZIP` | gzip framing within deflate/inflate |
| `gz-io` (default) | `#ifndef NO_GZCOMPRESS` | gz file-I/O layer (implies `std`+`gzip`) |
| `simd` (default) | — | `crc32fast` SIMD acceleration |
| `no-std` | `Z_SOLO` | Core-only, no gz file I/O |

```bash
# no_std / Z_SOLO core-only build (do NOT enable gz-io)
cargo build --no-default-features --features no-std
```

### 9.6 Verification — C Drop-In Example

Create `dropin_check.c`:

```c
#include <string.h>
#include <stdio.h>
#include "zlib.h"
int main(void) {
    const char *msg = "The quick brown fox jumps over the lazy dog.";
    uLong slen = (uLong)strlen(msg) + 1;
    Bytef comp[512]; uLongf clen = compressBound(slen);
    compress(comp, &clen, (const Bytef*)msg, slen);
    Bytef out[512]; uLongf olen = sizeof(out);
    uncompress(out, &olen, comp, clen);
    printf("version=%s roundtrip=%s\n", zlibVersion(), strcmp(msg,(char*)out)==0?"OK":"FAIL");
    uLong c = crc32(0L, Z_NULL, 0); c = crc32(c,(const Bytef*)"123456789",9);
    printf("crc32=0x%08lx (expect 0xcbf43926)\n", c);
    return 0;
}
```

```bash
# Static link
cc dropin_check.c -I. -Ltarget/release -l:libzlib_rs.a -lpthread -ldl -lm -o dropin_static
./dropin_static

# Dynamic link
cc dropin_check.c -I. -Ltarget/release -lzlib_rs -o dropin_dyn
LD_LIBRARY_PATH=target/release ./dropin_dyn
```

Expected output (both): `version=1.3.2.1-motley roundtrip=OK` and `crc32=0xcbf43926`.

### 9.7 Fuzzing (nightly)

```bash
cargo +nightly fuzz build                        # builds all 5 targets
cargo +nightly fuzz run fuzz_inflate -- -runs=100000
```

### 9.8 Troubleshooting

- **Cold dependency cache** — drop `--offline` to fetch from crates.io.
- **`no_std` build errors** — ensure you did **not** enable `gz-io` (it requires `std::fs`); use `--no-default-features --features no-std`.
- **Dynamic-link "cannot open shared object"** — set `LD_LIBRARY_PATH=target/release` at runtime.
- **Fuzz build fails on stable** — `cargo-fuzz` requires the nightly toolchain.
- **`externally-managed-environment`** — this is a system-Python/pip message and is unrelated to Cargo; it can be ignored for this project.

---

## 10. Appendices

### A. Command Reference

| Command | Purpose |
|---|---|
| `cargo build --release` | Optimized build; emits rlib/cdylib/staticlib |
| `cargo test` | Full default test suite (487) |
| `cargo test --no-default-features` | no_std/Z_SOLO suite (363) |
| `cargo clippy --all-targets -- -D warnings` | Lint gate |
| `cargo fmt --all -- --check` | Format gate |
| `cargo bench --no-run` | Compile benches |
| `cargo +nightly fuzz build` | Build 5 fuzz targets |
| `cargo metadata --offline` | Resolve ~105-package graph |

### B. Port Reference

Not applicable — `zlib-rs` is a headless library with no network services or listening ports.

### C. Key File Locations

| Path | Description |
|---|---|
| `Cargo.toml` | Manifest (edition 2024, crate-types, features) |
| `build.rs` | Generates CRC-32 tables at build time |
| `src/lib.rs` | Crate root, module declarations, public re-exports |
| `src/deflate/**` | Compression engine (zero-unsafe core, 9 modules) |
| `src/inflate/**` | Decompression engine (6 modules incl. `back.rs`) |
| `src/checksum/**` | Adler-32 / CRC-32 |
| `src/gz/**` | gzip file-I/O (6 modules) |
| `src/util/**` | One-call wrappers + version |
| `src/ffi/**` | C-ABI boundary (7 modules; `types.rs` = `#[repr(C)]` mirrors) |
| `tests/**` | 6 integration suites (ported from C drivers) |
| `benches/**` | 3 criterion benches |
| `fuzz/fuzz_targets/**` | 5 cargo-fuzz targets |
| `zlib.h` | C header retained for drop-in linking |
| `blitzy-deck/executive-summary.html` | reveal.js executive deck |
| `.github/workflows/{ci,fuzz}.yml` | CI pipelines |

### D. Technology Versions

| Component | Version |
|---|---|
| Rust / Cargo | 1.96.1 (MSRV 1.85.0, edition 2024) |
| `crc32fast` (runtime) | 1.5.0 |
| `cfg-if` (runtime) | 1.0.4 |
| `criterion` (dev) | 0.5.1 |
| `flate2` (dev, miniz_oxide backend) | 1.1.9 |
| `quickcheck` (dev) | 1.1.0 |
| `rand` (dev) | 0.9.4 |
| Resolved graph | ~105 packages |
| C compiler (drop-in only) | gcc 15.2.0 |

### E. Environment Variable Reference

| Variable | When | Purpose |
|---|---|---|
| `LD_LIBRARY_PATH=target/release` | Runtime (dynamic link) | Locate `libzlib_rs.so` |
| `RUSTFLAGS` | Build (optional) | e.g., `-C target-cpu=native` for SIMD tuning |
| `CARGO_NET_OFFLINE=true` (or `--offline`) | Build/test | Use warmed cache without network |

### F. Developer Tools Guide

- **clippy** — `cargo clippy --all-targets -- -D warnings` (lint gate; currently clean).
- **rustfmt** — `cargo fmt --all -- --check` (format gate; currently clean).
- **criterion** — statistical benchmarks under `benches/`; run `cargo bench`.
- **cargo-fuzz** (nightly) — coverage-guided fuzzing under `fuzz/`.
- **nm / objdump** — inspect exported FFI symbols: `nm -D target/release/libzlib_rs.so`.
- **Recommended additions (see §2.2):** `cargo audit`, `cargo deny` for the security audit.

### G. Glossary

| Term | Definition |
|---|---|
| DEFLATE | Lossless compression format (RFC 1951) underlying zlib and gzip |
| zlib wrapper | RFC 1950 framing around DEFLATE (2-byte header + Adler-32) |
| gzip wrapper | RFC 1952 framing around DEFLATE (magic `1f 8b` + CRC-32) |
| `windowBits` | Parameter overloaded to select zlib/raw/gzip/auto-detect framing |
| FFI | Foreign Function Interface — the `extern "C"` boundary exposing the zlib C ABI |
| `#[repr(C)]` | Attribute forcing C-compatible struct layout (e.g., `z_stream`) |
| cdylib / staticlib | Dynamic (`.so`) / static (`.a`) C-linkable artifacts |
| `inflateBack` | Raw-callback decompression entry point |
| MSRV | Minimum Supported Rust Version (1.85.0) |
| `no_std` / Z_SOLO | Core-only build without the standard library / gz file I/O |
| AAP | Agent Action Plan — the authoritative project scope specification |
