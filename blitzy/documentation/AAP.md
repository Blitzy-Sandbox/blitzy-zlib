# Technical Specification

# 0. Agent Action Plan

## 0.1 Intent Clarification

### 0.1.1 Core Refactoring Objective

Based on the prompt, the Blitzy platform understands that the refactoring objective is to perform a **complete language-level rewrite** of the zlib 1.3.2.1-motley C compression library into production-ready, idiomatic Rust. This is a **tech stack migration** — every line of C source code in the repository is to be replaced with Rust equivalents that leverage Rust's ownership model, type system, and memory safety guarantees to eliminate the manual memory management patterns pervasive throughout the original C implementation.

- **Refactoring type:** Tech stack migration (C → Rust)
- **Target repository:** Same repository — the C sources serve as the authoritative specification for the Rust rewrite, producing a new Rust project in-place
- **Source library version:** zlib 1.3.2.1-motley (`VERNUM 0x1321`), comprising 23,107 total lines of C across 15 implementation files and 11 headers

The refactoring goals, with enhanced clarity, are:

- **Replace all manual memory management with Rust ownership semantics** — The C codebase uses `zalloc`/`zfree` hooks (via `ZALLOC`/`ZFREE` macros in `zutil.h`) for all heap allocations, with manual lifecycle tracking across `deflateInit2` → `deflate` → `deflateEnd` and `inflateInit2` → `inflate` → `inflateEnd` call sequences. In Rust, these become owned `Box<T>`, `Vec<u8>`, and `Drop` trait implementations that tie resource lifetime to scope.
- **Maintain full DEFLATE format compatibility** — The Rust implementation must produce and consume byte-identical streams compliant with RFC 1950 (zlib), RFC 1951 (DEFLATE), and RFC 1952 (gzip). Compressed output from the Rust library must be decompressible by reference C zlib and vice versa.
- **Provide a C-compatible FFI interface** — A `#[no_mangle] extern "C"` layer must expose all 105 public symbols listed in `win32/zlib.def` and versioned across 14 milestones in `zlib.map`, enabling the Rust library to function as a drop-in replacement for `libz.so` / `zlib1.dll`.
- **Achieve zero `unsafe` blocks in core compression logic** — The deflate engine, inflate engine, Huffman coding, checksum computation, and gzip file I/O must all be implemented in safe Rust. `unsafe` code is confined exclusively to the FFI boundary layer for raw pointer interop.
- **Pass the official zlib test vectors** — The test suite ported from `test/example.c` (13+ test functions) and `test/infcover.c` (exhaustive inflate coverage harness) must pass, plus compatibility testing against reference zlib output.

### 0.1.2 Technical Interpretation

This refactoring translates to the following technical transformation strategy:

- **C state structures → Rust ownership types:** The `deflate_state` (~50 fields including sliding window, hash chains, Huffman trees, pending buffer) becomes a Rust `DeflateState` struct owning all its buffers via `Vec<u8>` and `Vec<u16>`. The `inflate_state` (~30 fields including mode state machine, sliding window, code tables) becomes a Rust `InflateState` with mode encoded as a Rust `enum`. The `gz_state` (file descriptor, buffers, embedded z_stream) becomes a `GzState` holding `File` handles with `Drop`-based cleanup.
- **C state machines → Rust `enum`-based dispatch:** The deflate engine's 8-state progression (INIT_STATE=42 → FINISH_STATE=666) and the inflate engine's 30+ mode enum (HEAD=16180 → SYNC) become Rust `enum` types with exhaustive `match` expressions, eliminating invalid state transitions at compile time.
- **C pointer arithmetic → Rust slices and iterators:** All `next_in`/`next_out`/`avail_in`/`avail_out` buffer management through raw pointer advancement becomes Rust slice-based I/O (`&[u8]` / `&mut [u8]`) with bounds-checking.
- **C hash chains → Rust `Vec<u16>` with index-based access:** The `head[]`/`prev[]` hash chain arrays in `deflate.h` that use `Pos`/`IPos` indices translate to Rust `Vec<u16>` with safe indexing, eliminating buffer overrun risks inherent in the C `NIL` sentinel pattern.
- **C macro-heavy code → Rust traits and generics:** The `DO1`/`DO2`/`DO4`/`DO8`/`DO16` unrolled macros in `adler32.c`, the `CRC32_WORD` braided macros in `crc32.c`, and the `_tr_tally_lit`/`_tr_tally_dist` inline macros in `deflate.h` become idiomatic Rust functions, potentially leveraging `#[inline]` hints for performance parity.
- **C `#ifdef` conditional compilation → Cargo features:** Platform-conditional code paths (`DYNAMIC_CRC_TABLE`, `NO_DIVIDE`, `Z_SOLO`, `GUNZIP`, `NO_GZIP`) become Cargo feature flags, and type detection from `zconf.h` becomes standard Rust types (`u8`, `u16`, `u32`, `u64`, `usize`).
- **C symbol versioning → Rust crate versioning:** The 14-milestone `zlib.map` version script becomes Rust semver versioning, with the FFI cdylib optionally emitting a compatible version script via `build.rs`.
- **Architecture layers preserved:** The six-layer architecture (Public API, Deflate Engine, Inflate Engine, Callback Decompressor, Checksum Engines, Gzip File I/O) maps directly to Rust module hierarchy within a Cargo workspace.

## 0.2 Source Analysis

### 0.2.1 Comprehensive Source File Discovery

The zlib 1.3.2.1-motley repository contains 33 root-level files and 9 subdirectories. All files are marked UNCHANGED, representing the canonical C source that serves as the specification for the Rust rewrite. Source discovery was performed using exhaustive repository inspection across all directories.

**Current Structure Mapping:**

```
Current:
zlib-1.3.2.1/
├── adler32.c              (164 lines — Adler-32 checksum engine)
├── compress.c             (99 lines — one-call compress utility)
├── crc32.c                (983 lines — CRC-32 checksum engine, multi-path)
├── crc32.h                (9446 lines — generated CRC lookup tables)
├── deflate.c              (2185 lines — DEFLATE compression engine)
├── deflate.h              (383 lines — deflate internal state definitions)
├── gzclose.c              (23 lines — gzip close dispatcher)
├── gzguts.h               (216 lines — gzip internal state/types)
├── gzlib.c                (609 lines — gzip common: open, reset, error)
├── gzread.c               (668 lines — gzip read pipeline)
├── gzwrite.c              (700 lines — gzip write pipeline)
├── infback.c              (579 lines — callback-based decompression)
├── inffast.c              (321 lines — fast-path inflate decoder)
├── inffast.h              (11 lines — inffast prototype)
├── inffixed.h             (94 lines — pre-computed fixed Huffman tables)
├── inflate.c              (1413 lines — DEFLATE decompression state machine)
├── inflate.h              (126 lines — inflate internal state definitions)
├── inftrees.c             (424 lines — Huffman table construction)
├── inftrees.h             (64 lines — code struct, ENOUGH constants)
├── trees.c                (1119 lines — Huffman tree builder/encoder)
├── trees.h                (128 lines — static tree data, dist/length codes)
├── uncompr.c              (101 lines — one-call uncompress utility)
├── zconf.h                (551 lines — platform configuration/detection)
├── zlib.h                 (2057 lines — public API, z_stream, all declarations)
├── zutil.c                (312 lines — internal utilities, default allocators)
├── zutil.h                (331 lines — internal macros, types, OS detection)
├── CMakeLists.txt         (311 lines — primary build system)
├── Makefile.in            (—  autotools build template)
├── configure              (—  autotools configure script)
├── zlib.pc.in             (—  pkg-config template)
├── zlib.map               (—  GNU symbol versioning script)
├── zlib.3                 (—  man page)
├── ChangeLog              (—  change history)
├── FAQ                    (—  frequently asked questions)
├── INDEX                  (—  file index)
├── LICENSE                (—  zlib license)
├── README                 (—  project readme)
├── test/
│   ├── CMakeLists.txt     (test build configuration)
│   ├── example.c          (canonical regression test — 13+ test functions)
│   ├── infcover.c         (inflate coverage harness with memory tracking)
│   └── minigzip.c         (gzip/gunzip utility for integration testing)
├── doc/
│   ├── algorithm.txt      (DEFLATE algorithm description)
│   ├── rfc1950.txt        (zlib format specification)
│   ├── rfc1951.txt        (DEFLATE specification)
│   ├── rfc1952.txt        (gzip format specification)
│   └── txtvsbin.txt       (text vs binary detection)
├── examples/
│   ├── zpipe.c            (pipe compression example)
│   ├── zran.c/zran.h      (random access index for gzip)
│   ├── enough.c           (ENOUGH constant calculation)
│   ├── fitblk.c           (fixed-size block compression)
│   ├── gun.c              (gzip decompressor)
│   ├── gzappend.c         (append to gzip file)
│   ├── gzjoin.c           (join gzip files)
│   ├── gzlog.c/gzlog.h    (gzip log file operations)
│   ├── gznorm.c           (normalize gzip file)
│   └── zlib_how.html      (usage tutorial)
├── contrib/               (18 subdirectories — extensions, bindings)
│   ├── minizip/           (ZIP archive support)
│   ├── blast/             (PKWare decompressor)
│   ├── puff/              (standalone inflate)
│   ├── infback9/          (Deflate64 support)
│   ├── ada/               (Ada binding)
│   ├── pascal/            (Pascal binding)
│   ├── delphi/            (Delphi binding)
│   ├── dotzlib/           (.NET binding)
│   ├── iostream/          (C++ iostream binding)
│   ├── iostream2/         (C++ iostream v2)
│   ├── iostream3/         (C++ iostream v3)
│   ├── crc32vx/           (S390X vector CRC)
│   ├── gcc_gvmat64/       (AMD64 longest_match asm)
│   ├── nuget/             (NuGet packaging)
│   ├── testzlib/          (zlib test utility)
│   ├── vstudio/           (Visual Studio project)
│   └── zlib1-dll/         (Windows DLL project)
├── win32/
│   ├── zlib.def           (105 exported DLL symbols)
│   ├── zlib1.rc           (Windows resource file)
│   └── *.txt              (Windows documentation)
├── os400/                 (IBM i/OS 400 support)
├── amiga/                 (Amiga Makefiles)
├── watcom/                (OpenWatcom DOS Makefiles)
└── .github/workflows/     (7 CI/CD workflow files)
```

### 0.2.2 Complete Source File Inventory

**Core Implementation Files (15 C source files — 9,700 lines):**

| File | Lines | Component | Purpose |
|------|-------|-----------|---------|
| `deflate.c` | 2,185 | Deflate Engine | LZ77 sliding-window matching, 5 compression strategies, 8-state machine |
| `inflate.c` | 1,413 | Inflate Engine | 30+ mode state machine, format auto-detection, error recovery |
| `trees.c` | 1,119 | Huffman Coder | Tree building, static/dynamic/fixed encoding, block flushing |
| `crc32.c` | 983 | Checksum | CRC-32 with braided, hardware (ARM), and byte-wise paths |
| `gzwrite.c` | 700 | Gzip File I/O | Buffered gzip write pipeline, formatted output |
| `gzread.c` | 668 | Gzip File I/O | Auto-detect read pipeline (LOOK/COPY/GZIP modes) |
| `gzlib.c` | 609 | Gzip File I/O | Open, close, error handling, seek abstraction |
| `infback.c` | 579 | Callback Decompressor | I/O callback-driven raw deflate decompression |
| `inftrees.c` | 424 | Table Builder | Huffman code table construction (CODES/LENS/DISTS) |
| `inffast.c` | 321 | Fast Decoder | Performance-critical inner decode loop |
| `zutil.c` | 312 | Utilities | Error messages, compile flags, default allocators |
| `adler32.c` | 164 | Checksum | Adler-32 with NMAX-optimized unrolled loop |
| `uncompr.c` | 101 | Utility | One-call decompression wrapper |
| `compress.c` | 99 | Utility | One-call compression wrapper |
| `gzclose.c` | 23 | Gzip File I/O | Close dispatcher (gzclose_r/gzclose_w) |

**Core Header Files (11 headers — 13,407 lines):**

| File | Lines | Scope | Purpose |
|------|-------|-------|---------|
| `crc32.h` | 9,446 | Generated | Pre-computed CRC lookup tables (braided, word-level) |
| `zlib.h` | 2,057 | Public API | z_stream, gz_header, all function declarations, constants |
| `zconf.h` | 551 | Platform | Type detection, visibility, Z_PREFIX, large file support |
| `deflate.h` | 383 | Internal | deflate_state struct (~50 fields), state constants, tree types |
| `zutil.h` | 331 | Internal | Macros, OS detection, ZALLOC/ZFREE, debug tracing |
| `gzguts.h` | 216 | Internal | gz_state struct, LSEEK abstraction, buffer constants |
| `trees.h` | 128 | Internal | Static Huffman tree data, distance/length code tables |
| `inflate.h` | 126 | Internal | inflate_state struct, 30+ mode enum, inflate_mode |
| `inffixed.h` | 94 | Generated | Fixed Huffman tables (lenfix[512], distfix[32]) |
| `inftrees.h` | 64 | Internal | code struct, ENOUGH=1444, codetype enum |
| `inffast.h` | 11 | Internal | inflate_fast() prototype |

**Test Files (4 files):**

| File | Purpose |
|------|---------|
| `test/example.c` | Canonical regression: compress, gzip I/O, deflate/inflate, flush/sync, dictionary |
| `test/infcover.c` | Exhaustive inflate coverage with memory tracking and forced-path hex fixtures |
| `test/minigzip.c` | Feature-complete gzip/gunzip utility with mmap acceleration |
| `test/CMakeLists.txt` | Test build configuration (example, infcover, minigzip targets) |

**Build & Configuration Files:**

| File | Purpose |
|------|---------|
| `CMakeLists.txt` | Primary CMake build (3.12–3.31), 4 build options, 2 library targets |
| `Makefile.in` | Autotools fallback build template |
| `configure` | Autotools configuration script |
| `zlib.pc.in` | pkg-config metadata template |
| `zlib.map` | GNU symbol versioning (14 version milestones, 105+ symbols) |
| `win32/zlib.def` | Windows DLL export definition (105 symbols) |

**Documentation Files:**

| File | Purpose |
|------|---------|
| `doc/algorithm.txt` | DEFLATE algorithm description |
| `doc/rfc1950.txt` | zlib format specification |
| `doc/rfc1951.txt` | DEFLATE compressed data format |
| `doc/rfc1952.txt` | gzip file format specification |
| `zlib.3` | Unix man page |

## 0.3 Scope Boundaries

### 0.3.1 Exhaustively In Scope

**Source Transformations (C → Rust rewrite):**

- `adler32.c` — Adler-32 checksum engine → Rust module
- `compress.c` — One-call compress wrapper → Rust module
- `crc32.c` — CRC-32 checksum engine → Rust module
- `crc32.h` — CRC lookup tables → Rust const arrays or generated code
- `deflate.c` — DEFLATE compression engine → Rust deflate modules
- `deflate.h` — Deflate state definitions → Rust structs and enums
- `gzclose.c` — Gzip close dispatcher → Rust gz module
- `gzguts.h` — Gzip internal types → Rust gz state types
- `gzlib.c` — Gzip common operations → Rust gz module
- `gzread.c` — Gzip read pipeline → Rust gz::read module
- `gzwrite.c` — Gzip write pipeline → Rust gz::write module
- `infback.c` — Callback-based inflate → Rust inflate::back module
- `inffast.c` — Fast-path decoder → Rust inflate::fast module
- `inffast.h` — Fast decoder prototype → Rust module interface
- `inffixed.h` — Fixed Huffman tables → Rust const arrays
- `inflate.c` — Inflate state machine → Rust inflate module
- `inflate.h` — Inflate state definitions → Rust enums and structs
- `inftrees.c` — Huffman table builder → Rust inflate::table module
- `inftrees.h` — Code struct, constants → Rust types
- `trees.c` — Huffman tree builder → Rust deflate::trees module
- `trees.h` — Static tree data → Rust const data
- `uncompr.c` — One-call decompress wrapper → Rust module
- `zconf.h` — Platform configuration → Cargo features and Rust types
- `zlib.h` — Public API definitions → Rust public API + FFI types
- `zutil.c` — Internal utilities → Rust util module
- `zutil.h` — Internal macros/types → Rust util module

**FFI Interface Layer (new Rust code mapping to C API):**

- `win32/zlib.def` — All 105 exported symbols → `#[no_mangle] extern "C"` functions in FFI crate
- `zlib.map` — Symbol versioning → `build.rs`-generated version script for cdylib

**Test Suite (ported to Rust):**

- `test/example.c` — Regression tests → Rust integration tests
- `test/infcover.c` — Coverage harness → Rust coverage tests
- `test/minigzip.c` — Integration utility → Rust example binary
- `test/CMakeLists.txt` — Test build → Cargo test configuration

**Build System (replaced by Cargo):**

- `CMakeLists.txt` → `Cargo.toml` workspace manifest
- `Makefile.in` → Cargo build commands
- `zlib.pc.in` → `build.rs` pkg-config generation for cdylib
- `configure` → Cargo feature detection (standard Rust types)

**Documentation (updated for Rust):**

- `README` → `README.md` (updated for Rust project with Cargo instructions)
- `doc/algorithm.txt` → Reference documentation preserved
- `doc/rfc1950.txt` → Reference documentation preserved
- `doc/rfc1951.txt` → Reference documentation preserved
- `doc/rfc1952.txt` → Reference documentation preserved
- `LICENSE` → Dual-licensed (original zlib license + MIT/Apache-2.0)

**Examples (ported to Rust):**

- `examples/zpipe.c` → `examples/zpipe.rs`
- `examples/enough.c` → `examples/enough.rs`

**Configuration & Metadata:**

- `Cargo.toml` — Workspace root manifest (CREATE)
- `rust-toolchain.toml` — Toolchain pinning (CREATE)
- `.cargo/config.toml` — Build configuration (CREATE)
- `clippy.toml` — Lint configuration (CREATE)

### 0.3.2 Explicitly Out of Scope

Per the user's explicit instructions, the following are out of scope:

- **bzip2, lzma, or other compression formats** — Only DEFLATE (RFC 1951) is implemented; no BZ2, LZMA, Zstandard, Brotli, or other algorithms
- **New compression algorithms** — The Rust rewrite faithfully implements the existing DEFLATE algorithm only; no novel compression research
- **GUI or tooling beyond the library itself** — No graphical interfaces, IDE plugins, or tooling external to the library and its test/example binaries

Additionally, based on the nature of the rewrite, the following are also out of scope:

- **Contributed extensions** (`contrib/`) — The 18 contributed subdirectories (minizip, blast, puff, infback9, Ada/Pascal/Delphi/.NET bindings, iostream, crc32vx, gcc_gvmat64, nuget, testzlib, vstudio, zlib1-dll) are contributed extensions not part of core zlib and are excluded from the Rust rewrite
- **Platform-specific build files** — `amiga/`, `watcom/`, `os400/`, `win32/Makefile.msc` build configurations for legacy platforms are replaced by Cargo's cross-compilation
- **Assembly optimizations** — `contrib/gcc_gvmat64/gvmat64.S` (AMD64 longest_match) and `contrib/crc32vx/` (S390X vector CRC) assembly files are not ported; Rust compiler auto-vectorization and LLVM optimizations provide the equivalent
- **CI workflow files** (`.github/workflows/*.yml`) — New CI workflows will be created for Rust but the existing C-focused workflows are not ported

## 0.4 Target Design

### 0.4.1 Refactored Structure Planning

The Rust rewrite is organized as a **Cargo workspace** with four crates that mirror the architectural separation in the original C library: a safe core library, a C-compatible FFI bindings crate, a cdylib crate for producing the drop-in replacement shared library, and a test crate for cross-compatibility validation.

**Target Architecture:**

```
Target:
zlib-rs/
├── Cargo.toml                         (workspace root manifest)
├── rust-toolchain.toml                (Rust 1.85.0, edition 2024)
├── .cargo/
│   └── config.toml                    (build optimization flags)
├── clippy.toml                        (lint configuration)
├── README.md                          (project documentation)
├── LICENSE-ZLIB                       (original zlib license)
├── LICENSE-MIT                        (MIT license)
├── LICENSE-APACHE                     (Apache-2.0 license)
├── CHANGELOG.md                       (release history)
├── doc/
│   ├── algorithm.md                   (DEFLATE algorithm — converted from .txt)
│   ├── rfc1950.txt                    (zlib format spec — reference)
│   ├── rfc1951.txt                    (DEFLATE spec — reference)
│   └── rfc1952.txt                    (gzip format spec — reference)
│
├── zlib-rs/                           (core Rust library — pure safe Rust)
│   ├── Cargo.toml                     (lib crate, no external deps)
│   ├── src/
│   │   ├── lib.rs                     (crate root, public API re-exports)
│   │   ├── stream.rs                  (ZStream — Rust-native stream interface)
│   │   ├── error.rs                   (ReturnCode enum, ZlibError type)
│   │   ├── constants.rs               (flush modes, strategies, limits)
│   │   ├── compress.rs                (one-call compress/uncompress)
│   │   ├── deflate/
│   │   │   ├── mod.rs                 (deflate(), deflateInit2, deflateEnd)
│   │   │   ├── state.rs               (DeflateState — owns window, hash, pending)
│   │   │   ├── algorithm.rs           (stored, fast, slow, huff, rle strategies)
│   │   │   ├── trees.rs               (Huffman tree building and block encoding)
│   │   │   ├── hash.rs                (hash chain management, insert_string)
│   │   │   └── params.rs              (CompressionConfig table, level mapping)
│   │   ├── inflate/
│   │   │   ├── mod.rs                 (inflate(), inflateInit2, inflateEnd)
│   │   │   ├── state.rs               (InflateState — mode enum, window, tables)
│   │   │   ├── fast.rs                (inflate_fast hot loop)
│   │   │   ├── table.rs               (Huffman table construction)
│   │   │   ├── fixed.rs               (pre-computed fixed Huffman tables)
│   │   │   └── back.rs                (inflateBack callback decompression)
│   │   ├── gz/
│   │   │   ├── mod.rs                 (gzopen, gzclose, gzbuffer, error handling)
│   │   │   ├── read.rs                (GzReader — LOOK/COPY/GZIP auto-detect)
│   │   │   ├── write.rs               (GzWriter — buffered compression)
│   │   │   └── state.rs               (GzState — mode, file handle, buffers)
│   │   ├── checksum/
│   │   │   ├── mod.rs                 (checksum module exports)
│   │   │   ├── adler32.rs             (Adler-32 with NMAX-optimized loop)
│   │   │   └── crc32.rs               (CRC-32 with table-based computation)
│   │   └── util.rs                    (error messages, compile flags, helpers)
│   └── benches/
│       ├── deflate_bench.rs           (compression benchmarks)
│       ├── inflate_bench.rs           (decompression benchmarks)
│       └── checksum_bench.rs          (checksum benchmarks)
│
├── libz-rs-sys/                       (C-compatible FFI bindings crate)
│   ├── Cargo.toml                     (depends on zlib-rs core)
│   ├── build.rs                       (version script generation, pkg-config)
│   ├── src/
│   │   ├── lib.rs                     (crate root, re-exports all extern "C" fns)
│   │   ├── types.rs                   (#[repr(C)] z_stream, gz_header, alloc_func)
│   │   ├── deflate.rs                 (deflateInit_, deflate, deflateEnd, etc.)
│   │   ├── inflate.rs                 (inflateInit_, inflate, inflateEnd, etc.)
│   │   ├── gz.rs                      (gzopen, gzread, gzwrite, etc.)
│   │   ├── checksum.rs                (adler32, crc32, combine functions)
│   │   ├── compress.rs                (compress, compress2, uncompress, etc.)
│   │   └── version.rs                 (zlibVersion, zlibCompileFlags)
│   └── zlib.map                       (GNU version script — generated or static)
│
├── libz-rs-sys-cdylib/                (shared library output crate)
│   ├── Cargo.toml                     (cdylib crate type, depends on libz-rs-sys)
│   └── src/
│       └── lib.rs                     (re-exports all FFI symbols)
│
├── tests/                             (integration test crate)
│   ├── Cargo.toml                     (test dependencies)
│   ├── src/
│   │   └── lib.rs                     (test utilities)
│   ├── tests/
│   │   ├── example_compat.rs          (port of test/example.c — 13+ tests)
│   │   ├── infcover_compat.rs         (port of test/infcover.c — coverage)
│   │   ├── deflate_tests.rs           (deflate-specific edge cases)
│   │   ├── inflate_tests.rs           (inflate-specific edge cases)
│   │   ├── gz_tests.rs                (gzip file I/O tests)
│   │   ├── checksum_tests.rs          (Adler-32 and CRC-32 tests)
│   │   ├── ffi_compat_tests.rs        (FFI layer smoke tests)
│   │   └── cross_compat_tests.rs      (Rust compress ↔ C decompress)
│   └── fixtures/
│       └── test_vectors/              (official zlib test data)
│
├── examples/
│   ├── zpipe.rs                       (port of examples/zpipe.c)
│   ├── minigzip.rs                    (port of test/minigzip.c)
│   └── enough.rs                      (port of examples/enough.c)
│
└── .github/
    └── workflows/
        ├── ci.yml                     (Rust CI — build, test, clippy, fmt)
        ├── ffi-compat.yml             (FFI compatibility validation)
        └── cross-platform.yml         (cross-compilation matrix)
```

### 0.4.2 Web Search Research Conducted

Web research was conducted to inform the target architecture with current best practices:

- **zlib-rs (Trifecta Tech Foundation)** — An existing Rust implementation of zlib, at version 0.6.0 with 12K SLoC and 36M+ downloads. Uses a workspace structure with `zlib-rs` (core), `libz-rs-sys` (FFI), and `libz-rs-sys-cdylib` (shared lib) crates. Key learnings: uses `extern "C"` (not `extern "C-unwind"`) at FFI boundaries to abort on panic rather than triggering undefined behavior; `gzprintf`/`gzvprintf` require nightly Rust due to c-variadic function definitions being unstable; `export-symbols` Cargo feature controls whether C symbols are exported.
- **flate2-rs (rust-lang)** — The Rust ecosystem's primary DEFLATE/gzip/zlib binding crate. Supports multiple backends (miniz_oxide, zlib-rs, zlib-ng, cloudflare_zlib). Demonstrates the `Read`/`Write`/`BufRead` trait-based API pattern for idiomatic Rust compression. Current MSRV tracks current + previous stable Rust.
- **Rust FFI best practices (Rustonomicon)** — Documents `#[repr(C)]` for struct layout compatibility, `CString`/`CStr` for null-terminated strings, opaque type patterns for forward-declared structs, and the requirement to use `panic=abort` in cdylib crates to prevent undefined behavior from unwinding across FFI boundaries.
- **Rust stable version** — Current stable Rust is 1.93.1 (released January 2026); the 2024 Edition is now stable and available as of Rust 1.85.0. Rust 1.85.0 will be used as the MSRV (Minimum Supported Rust Version) for access to the 2024 edition.

### 0.4.3 Design Pattern Applications

The following design patterns guide the C → Rust transformation:

- **Newtype pattern for type safety** — C's `Pos` (`unsigned short`) and `IPos` (`unsigned int`) become distinct Rust newtypes (`struct Pos(u16)`, `struct IPos(u32)`) preventing accidental misuse between hash chain indices and window positions
- **Builder pattern for initialization** — The multi-parameter `deflateInit2(level, method, windowBits, memLevel, strategy)` becomes a `DeflateConfig::builder()` chain with typed setters and validation
- **State machine via enum + match** — The inflate engine's 30+ C integer modes become a Rust `enum InflateMode { Head, Flags, Time, Os, ... }` with exhaustive match, making invalid state transitions a compile-time error
- **RAII for resource management** — `deflateEnd`/`inflateEnd`/`gzclose` cleanup becomes automatic via `Drop` trait implementations on `DeflateState`, `InflateState`, and `GzState`
- **Trait-based I/O abstraction** — The `gz*` stdio-like API maps to Rust's `Read`/`Write`/`Seek` traits, and the inflateBack callback pattern maps to closures `FnMut(&mut [u8]) -> io::Result<usize>`
- **Const generics for tables** — The fixed Huffman tables (`lenfix[512]`, `distfix[32]`) and CRC tables become `const` arrays computed at compile time, eliminating the `DYNAMIC_CRC_TABLE` runtime initialization pattern
- **Feature flags for conditional compilation** — `Z_SOLO` becomes `#[cfg(not(feature = "gz-io"))]`, `NO_GZIP`/`GUNZIP` become `#[cfg(feature = "gzip")]`, enabling no_std-compatible minimal builds

### 0.4.4 User Interface Design

Not applicable — the user explicitly stated that GUI and tooling beyond the library itself are out of scope. The library exposes two interfaces:

- **Rust-native API** — Idiomatic Rust types (`ZStream`, `DeflateState`, `InflateState`) with `Result<T, ZlibError>` return types, slice-based I/O, and trait implementations
- **C-compatible FFI API** — Exact signature match to zlib's 105 exported symbols via `#[no_mangle] extern "C"` functions accepting raw pointers and returning integer status codes

## 0.5 Transformation Mapping

### 0.5.1 File-by-File Transformation Plan

Every target file is mapped to one or more source files. The transformation mode indicates the relationship: **CREATE** means a new file derived from one or more C sources; **REFERENCE** means the source is used as a pattern or specification guide.

**Core Library Crate (`zlib-rs/`)**

| Target File | Transformation | Source File(s) | Key Changes |
|-------------|---------------|----------------|-------------|
| `zlib-rs/Cargo.toml` | CREATE | `CMakeLists.txt` | Cargo manifest with no external deps, feature flags for gz-io, gzip |
| `zlib-rs/src/lib.rs` | CREATE | `zlib.h` | Crate root: re-export public types, version constants, module declarations |
| `zlib-rs/src/stream.rs` | CREATE | `zlib.h` (lines 90–120) | `ZStream` struct with `Vec<u8>` owned buffers replacing `next_in`/`next_out` raw pointers |
| `zlib-rs/src/error.rs` | CREATE | `zlib.h` (lines 181–189), `zutil.c` | `ReturnCode` enum (Ok, StreamEnd, NeedDict, DataError, etc.), `z_errmsg` equivalents |
| `zlib-rs/src/constants.rs` | CREATE | `zlib.h` (lines 172–232), `zutil.h` | Flush modes, strategies, compression levels, MAX_MATCH, MIN_MATCH, block types |
| `zlib-rs/src/compress.rs` | CREATE | `compress.c`, `uncompr.c` | One-call `compress()`/`uncompress()` wrapping streaming API, size_t-safe |
| `zlib-rs/src/util.rs` | CREATE | `zutil.c`, `zutil.h` | Error message arrays, compile flags, helper functions |
| `zlib-rs/src/deflate/mod.rs` | CREATE | `deflate.c` | `deflate()`, `deflate_init2()`, `deflate_end()`, `deflate_reset()`, `deflate_params()`, `deflate_copy()`, `deflate_set_dictionary()`, `deflate_bound()` |
| `zlib-rs/src/deflate/state.rs` | CREATE | `deflate.h` | `DeflateState` struct owning window (`Vec<u8>`), hash chains (`Vec<u16>`), pending buffer (`Vec<u8>`), Huffman trees |
| `zlib-rs/src/deflate/algorithm.rs` | CREATE | `deflate.c` (lines 1600–2185) | Five compression functions: `deflate_stored`, `deflate_fast`, `deflate_slow`, `deflate_huff`, `deflate_rle` |
| `zlib-rs/src/deflate/trees.rs` | CREATE | `trees.c`, `trees.h` | Huffman tree construction, `_tr_init`, `_tr_tally`, `_tr_flush_block`, static tree data |
| `zlib-rs/src/deflate/hash.rs` | CREATE | `deflate.c` (hash chain sections) | Hash chain insert, longest_match, UPDATE_HASH equivalent using safe indexing |
| `zlib-rs/src/deflate/params.rs` | CREATE | `deflate.c` (lines 112–124) | `CompressionConfig` struct, `CONFIGURATION_TABLE` mapping levels to good_length/max_lazy/nice_length/max_chain |
| `zlib-rs/src/inflate/mod.rs` | CREATE | `inflate.c` | `inflate()`, `inflate_init2()`, `inflate_end()`, `inflate_reset()`, `inflate_sync()`, `inflate_copy()` |
| `zlib-rs/src/inflate/state.rs` | CREATE | `inflate.h` | `InflateState` with `InflateMode` enum (30+ variants), owned window, bit accumulator, code tables |
| `zlib-rs/src/inflate/fast.rs` | CREATE | `inffast.c`, `inffast.h` | `inflate_fast()` hot loop with safe slice operations replacing pointer arithmetic |
| `zlib-rs/src/inflate/table.rs` | CREATE | `inftrees.c`, `inftrees.h` | `inflate_table()` Huffman table builder, `Code` struct, `ENOUGH` constant |
| `zlib-rs/src/inflate/fixed.rs` | CREATE | `inffixed.h` | `const LENFIX: [Code; 512]` and `const DISTFIX: [Code; 32]` pre-computed tables |
| `zlib-rs/src/inflate/back.rs` | CREATE | `infback.c` | `inflate_back()` callback-based decompression using Rust closures |
| `zlib-rs/src/gz/mod.rs` | CREATE | `gzlib.c`, `gzclose.c` | `gz_open()`, `gz_close()`, `gz_buffer()`, `gz_error()`, platform seek abstraction |
| `zlib-rs/src/gz/read.rs` | CREATE | `gzread.c` | `GzReader` with LOOK/COPY/GZIP auto-detect pipeline, `Read` trait impl |
| `zlib-rs/src/gz/write.rs` | CREATE | `gzwrite.c` | `GzWriter` with buffered compression, `Write` trait impl, `gzprintf` equivalent |
| `zlib-rs/src/gz/state.rs` | CREATE | `gzguts.h` | `GzState` struct owning `File`, buffers, embedded `ZStream`, mode/how enums |
| `zlib-rs/src/checksum/mod.rs` | CREATE | `adler32.c`, `crc32.c` | Checksum module exports |
| `zlib-rs/src/checksum/adler32.rs` | CREATE | `adler32.c` | Adler-32 with NMAX=5552 block optimization, `adler32_combine()` |
| `zlib-rs/src/checksum/crc32.rs` | CREATE | `crc32.c`, `crc32.h` | CRC-32 with const table lookup, `crc32_combine()`, `crc32_combine_op()` |

**FFI Bindings Crate (`libz-rs-sys/`)**

| Target File | Transformation | Source File(s) | Key Changes |
|-------------|---------------|----------------|-------------|
| `libz-rs-sys/Cargo.toml` | CREATE | `CMakeLists.txt` | FFI crate with `libc` dependency, depends on `zlib-rs` |
| `libz-rs-sys/build.rs` | CREATE | `zlib.pc.in`, `zlib.map` | Generate version script, pkg-config file, link configuration |
| `libz-rs-sys/src/lib.rs` | CREATE | `win32/zlib.def` | Re-export all 105 `extern "C"` symbols |
| `libz-rs-sys/src/types.rs` | CREATE | `zlib.h` (lines 90–120), `zconf.h` | `#[repr(C)]` z_stream, gz_header, alloc_func/free_func typedefs |
| `libz-rs-sys/src/deflate.rs` | CREATE | `zlib.h` (deflate API section) | `deflateInit_`, `deflateInit2_`, `deflate`, `deflateEnd`, `deflateSetDictionary`, `deflateCopy`, `deflateReset`, `deflateParams`, `deflateTune`, `deflateBound`, `deflatePending`, `deflateUsed`, `deflatePrime`, `deflateSetHeader` |
| `libz-rs-sys/src/inflate.rs` | CREATE | `zlib.h` (inflate API section) | `inflateInit_`, `inflateInit2_`, `inflate`, `inflateEnd`, `inflateSetDictionary`, `inflateSync`, `inflateCopy`, `inflateReset`, `inflateReset2`, `inflatePrime`, `inflateMark`, `inflateGetHeader`, `inflateBack`, `inflateBackInit_`, `inflateBackEnd` |
| `libz-rs-sys/src/gz.rs` | CREATE | `zlib.h` (gz API section) | All 28 `gz*` functions: `gzopen`, `gzdopen`, `gzbuffer`, `gzsetparams`, `gzread`, `gzfread`, `gzwrite`, `gzfwrite`, `gzprintf`, `gzputs`, `gzgets`, `gzputc`, `gzgetc`, `gzungetc`, `gzflush`, `gzseek`, `gzrewind`, `gztell`, `gzoffset`, `gzeof`, `gzdirect`, `gzclose`, `gzclose_r`, `gzclose_w`, `gzerror`, `gzclearerr`, plus 64-bit variants |
| `libz-rs-sys/src/checksum.rs` | CREATE | `zlib.h` (checksum section) | `adler32`, `adler32_z`, `adler32_combine`, `crc32`, `crc32_z`, `crc32_combine`, `crc32_combine_gen`, `crc32_combine_op` |
| `libz-rs-sys/src/compress.rs` | CREATE | `zlib.h` (utility section) | `compress`, `compress2`, `compress_z`, `compress2_z`, `compressBound`, `compressBound_z`, `uncompress`, `uncompress2`, `uncompress_z`, `uncompress2_z` |
| `libz-rs-sys/src/version.rs` | CREATE | `zlib.h` (version section) | `zlibVersion`, `zlibCompileFlags`, `zError`, `inflateSyncPoint`, `get_crc_table`, `inflateUndermine`, `inflateValidate`, `inflateCodesUsed`, `inflateResetKeep`, `deflateResetKeep` |
| `libz-rs-sys/zlib.map` | CREATE | `zlib.map` | GNU symbol versioning for 14 version milestones |

**Shared Library Crate (`libz-rs-sys-cdylib/`)**

| Target File | Transformation | Source File(s) | Key Changes |
|-------------|---------------|----------------|-------------|
| `libz-rs-sys-cdylib/Cargo.toml` | CREATE | `CMakeLists.txt` (shared lib target) | cdylib crate type, `panic=abort`, depends on `libz-rs-sys` |
| `libz-rs-sys-cdylib/src/lib.rs` | CREATE | `win32/zlib.def` | Re-export all symbols for dynamic library |

**Test Crate (`tests/`)**

| Target File | Transformation | Source File(s) | Key Changes |
|-------------|---------------|----------------|-------------|
| `tests/Cargo.toml` | CREATE | `test/CMakeLists.txt` | Test crate with dev-dependencies on `zlib-rs` and `libz-rs-sys` |
| `tests/tests/example_compat.rs` | CREATE | `test/example.c` | Port 13+ test functions: `test_compress`, `test_gzio`, `test_deflate`, `test_inflate`, `test_large_deflate`, `test_large_inflate`, `test_flush`, `test_sync`, `test_dict_deflate`, `test_dict_inflate` |
| `tests/tests/infcover_compat.rs` | CREATE | `test/infcover.c` | Port coverage harness with memory tracking, hex fixture decoding |
| `tests/tests/deflate_tests.rs` | CREATE | `deflate.c` | Deflate-specific edge case tests (all levels, strategies, windowBits values) |
| `tests/tests/inflate_tests.rs` | CREATE | `inflate.c` | Inflate edge cases (auto-detect, corrupt data, sync recovery) |
| `tests/tests/gz_tests.rs` | CREATE | `gzlib.c`, `gzread.c`, `gzwrite.c` | Gzip file I/O round-trip tests, seek, transparent read |
| `tests/tests/checksum_tests.rs` | CREATE | `adler32.c`, `crc32.c` | Checksum correctness and combine operation tests |
| `tests/tests/ffi_compat_tests.rs` | CREATE | `win32/zlib.def` | FFI symbol presence and signature validation |
| `tests/tests/cross_compat_tests.rs` | CREATE | `test/example.c` | Rust-compress + C-decompress and C-compress + Rust-decompress round-trips |

**Workspace-Level Files**

| Target File | Transformation | Source File(s) | Key Changes |
|-------------|---------------|----------------|-------------|
| `Cargo.toml` | CREATE | `CMakeLists.txt` | Workspace manifest listing all 4 crates |
| `rust-toolchain.toml` | CREATE | — | Pin Rust 1.85.0, edition 2024 |
| `.cargo/config.toml` | CREATE | — | Release profile optimizations, LLVM flags |
| `clippy.toml` | CREATE | — | Lint strictness configuration |
| `README.md` | CREATE | `README` | Project documentation with Cargo build/test instructions |
| `LICENSE-ZLIB` | CREATE | `LICENSE` | Original zlib license preserved |
| `LICENSE-MIT` | CREATE | — | MIT license for Rust code |
| `LICENSE-APACHE` | CREATE | — | Apache-2.0 license for Rust code |
| `CHANGELOG.md` | CREATE | `ChangeLog` | Rust-specific changelog |
| `doc/algorithm.md` | CREATE | `doc/algorithm.txt` | Markdown conversion of algorithm description |
| `examples/zpipe.rs` | CREATE | `examples/zpipe.c` | Pipe compression/decompression example |
| `examples/minigzip.rs` | CREATE | `test/minigzip.c` | Gzip/gunzip utility ported to Rust |
| `examples/enough.rs` | CREATE | `examples/enough.c` | ENOUGH constant calculation tool |

### 0.5.2 Cross-File Dependencies

The Rust rewrite replaces C `#include` directives with Rust `use` module imports. The following import transformation rules apply across the entire codebase:

**Internal module imports (within `zlib-rs` core crate):**

- FROM: `#include "zutil.h"` / `#include "zlib.h"`
- TO: `use crate::constants::*;` + `use crate::error::ReturnCode;` + `use crate::stream::ZStream;`

- FROM: `#include "deflate.h"` (in `deflate.c`, `trees.c`)
- TO: `use crate::deflate::state::DeflateState;` + `use crate::deflate::params::CompressionConfig;`

- FROM: `#include "inflate.h"` / `#include "inftrees.h"` (in `inflate.c`, `inffast.c`, `infback.c`)
- TO: `use crate::inflate::state::{InflateState, InflateMode};` + `use crate::inflate::table::Code;`

- FROM: `#include "gzguts.h"` (in `gzlib.c`, `gzread.c`, `gzwrite.c`, `gzclose.c`)
- TO: `use crate::gz::state::GzState;` + `use std::fs::File;` + `use std::io::{Read, Write, Seek};`

**FFI crate imports (within `libz-rs-sys`):**

- FROM: (no C equivalent — these are new)
- TO: `use zlib_rs::{deflate, inflate, gz, checksum, compress};` + `use crate::types::z_stream;`

**Configuration changes:**

- FROM: `#ifdef Z_SOLO` / `#ifdef NO_GZIP` / `#ifdef GUNZIP`
- TO: `#[cfg(feature = "gz-io")]` / `#[cfg(feature = "gzip")]` / `#[cfg(not(feature = "no-gzip"))]`

- FROM: `#ifdef DYNAMIC_CRC_TABLE` (runtime table initialization)
- TO: `const CRC_TABLE: [[u32; 256]; N] = { ... };` (compile-time const evaluation)

- FROM: `Z_U4` / `z_crc_t` / `z_word_t` / `z_size_t` / `z_off_t` type detection in `zconf.h`
- TO: Standard Rust types: `u32`, `u32`, `u64`, `usize`, `i64` — no configuration needed

### 0.5.3 Wildcard Patterns

The following trailing wildcard patterns summarize file groups:

- `zlib-rs/src/deflate/*.rs` — All deflate engine modules (6 files)
- `zlib-rs/src/inflate/*.rs` — All inflate engine modules (6 files)
- `zlib-rs/src/gz/*.rs` — All gzip file I/O modules (4 files)
- `zlib-rs/src/checksum/*.rs` — All checksum modules (3 files)
- `libz-rs-sys/src/*.rs` — All FFI binding modules (8 files)
- `tests/tests/*.rs` — All integration test files (8 files)
- `examples/*.rs` — All example binaries (3 files)
- `doc/*.md` — All converted documentation (1 file)
- `doc/*.txt` — All reference RFC documents (3 files)

### 0.5.4 One-Phase Execution

The entire Rust rewrite is executed by Blitzy in **one phase**. All 60+ target files across the 4 workspace crates are created in a single pass. There is no phased rollout, no incremental migration, and no temporary dual-language state. The C source files serve exclusively as the specification reference during code generation.

## 0.6 Dependency Inventory

### 0.6.1 Key Private and Public Packages

The Rust rewrite deliberately minimizes external dependencies in the core library to mirror the C zlib philosophy of zero external runtime dependencies. The FFI crate requires `libc` for C-type interoperability, and test/bench crates use standard Rust development tooling.

| Registry | Package | Version | Crate | Purpose |
|----------|---------|---------|-------|---------|
| crates.io | `libc` | 0.2 | `libz-rs-sys` | C type definitions (`c_int`, `c_uint`, `c_ulong`, `c_char`, `c_void`) for FFI function signatures |
| crates.io | `cfg-if` | 1.0 | `zlib-rs` | Conditional compilation macro for platform-specific code paths |
| crates.io | `criterion` | 0.5 | `zlib-rs` (dev) | Benchmark framework for performance regression testing |
| rust-lang | `std` | (bundled) | `zlib-rs` | Standard library (`std::io`, `std::fs`, `std::fmt`) for gzip file I/O and formatting |
| crates.io | `crc` | 3.2 | `tests` (dev) | Reference CRC-32 implementation for cross-validation |
| crates.io | `flate2` | 1.0 | `tests` (dev) | Reference zlib implementation for cross-compatibility testing |
| crates.io | `tempfile` | 3.14 | `tests` (dev) | Temporary file creation for gzip I/O tests |

**Toolchain Dependencies:**

| Tool | Version | Purpose |
|------|---------|---------|
| `rustc` | 1.85.0 (MSRV) | Rust compiler — 2024 edition support |
| `cargo` | 1.85.0+ | Build system and package manager |
| `clippy` | (bundled) | Lint enforcement — `#![deny(clippy::all, clippy::pedantic)]` |
| `rustfmt` | (bundled) | Code formatting — standard Rust style |
| `cargo-c` | 0.10+ | Build C-compatible shared/static libraries from cdylib crate |
| `cbindgen` | 0.27+ | Optional: auto-generate C header from Rust FFI functions |

### 0.6.2 Dependency Updates

**Import Refactoring**

The C-to-Rust migration eliminates all C `#include` directives. No legacy C imports remain. The following Rust import patterns apply across all target files:

- `zlib-rs/src/**/*.rs` — Internal crate imports use `use crate::{module}` paths
- `libz-rs-sys/src/**/*.rs` — Cross-crate imports use `use zlib_rs::{module}` paths
- `tests/tests/**/*.rs` — Test imports use `use zlib_rs::{...}` and `use libz_rs_sys::{...}`

**External Reference Updates:**

| File Pattern | Update Type | Description |
|-------------|-------------|-------------|
| `Cargo.toml` (workspace root) | CREATE | Workspace members, shared dependencies, profiles |
| `zlib-rs/Cargo.toml` | CREATE | Core crate manifest — `[lib]` section, features (gz-io, gzip, no-std) |
| `libz-rs-sys/Cargo.toml` | CREATE | FFI crate — `[dependencies]` libc, `[features]` export-symbols |
| `libz-rs-sys-cdylib/Cargo.toml` | CREATE | cdylib crate — `[lib] crate-type = ["cdylib"]`, `[profile.release] panic = "abort"` |
| `tests/Cargo.toml` | CREATE | Test crate — `[dev-dependencies]` on core and FFI crates, flate2, tempfile |
| `rust-toolchain.toml` | CREATE | `[toolchain] channel = "1.85.0"` |
| `.cargo/config.toml` | CREATE | `[profile.release] lto = true, codegen-units = 1` and optional LLVM DFA jump thread flag |
| `libz-rs-sys/build.rs` | CREATE | Build script generating `zlib.pc`, linker version script, and `#[cfg]` for platform detection |

### 0.6.3 Cargo Workspace Configuration

The workspace root `Cargo.toml` unifies all crates:

```toml
[workspace]
members = ["zlib-rs", "libz-rs-sys", "libz-rs-sys-cdylib", "tests"]
resolver = "2"
```

The release profile applies aggressive optimizations for performance parity with C:

```toml
[profile.release]
lto = "fat"
codegen-units = 1
opt-level = 3
```

## 0.7 Refactoring Rules

### 0.7.1 User-Specified Rules

The following rules are explicitly mandated by the user and are non-negotiable constraints on the Rust rewrite:

- **Binary compatibility with zlib-produced streams** — Output from the Rust library must be decompressible by reference C zlib (`inflate()`), and the Rust library must decompress any valid zlib/gzip/raw DEFLATE stream produced by reference C zlib. The compressed byte representation for identical inputs, compression levels, and strategies must be bit-identical where the algorithm is deterministic.
- **FFI layer must match the zlib C API signature exactly** — Every one of the 105 symbols exported in `win32/zlib.def` must have an exactly matching `extern "C"` function in the FFI crate. Parameter types, return types, and calling conventions must be identical. The FFI functions must accept `*mut z_stream` where the C API accepts `z_streamp`, and return `c_int` where the C API returns `int`.
- **Zero `unsafe` blocks in core compression logic** — The `zlib-rs` core crate containing the deflate engine, inflate engine, Huffman coding, checksum computation, gzip file I/O, and utility functions must contain zero `unsafe` blocks. All `unsafe` code is confined to the `libz-rs-sys` FFI boundary crate where raw pointer interop is unavoidable.
- **Must pass the official zlib test vectors** — The test suite must validate against the exact outputs expected by `test/example.c` (13+ test functions covering compress, gzip I/O, deflate/inflate, flush/sync, dictionary) and `test/infcover.c` (exhaustive inflate path coverage). Byte-for-byte output comparison ensures format compliance.

### 0.7.2 Special Instructions and Constraints

**Implicit constraints derived from the architecture:**

- **Preserve all public API contracts** — Every function documented in `zlib.h` lines 1–2057 must have a Rust equivalent. The FFI layer must expose all 105 symbols across 14 version milestones (from `ZLIB_1.2.0` through `ZLIB_1.3.2`).
- **Maintain the streaming `z_stream` interface semantics** — The `next_in`/`avail_in`/`next_out`/`avail_out` buffer protocol, the `total_in`/`total_out` running counters, and the `adler`/`data_type` feedback fields must behave identically in the Rust implementation.
- **Preserve `windowBits` overloading behavior** — The overloaded `windowBits` parameter that selects container format (positive=zlib, negative=raw, +16=gzip, +32=auto-detect) while simultaneously controlling window size (bits 0–3) must produce identical format selection in Rust.
- **Preserve flush mode semantics** — All seven flush modes (`Z_NO_FLUSH` through `Z_TREES`) must produce identical output behavior, including sync flush point markers (`00 00 FF FF`) that enable `inflateSync()` recovery.
- **Preserve error handling contracts** — Every return code (`Z_OK`, `Z_STREAM_END`, `Z_NEED_DICT`, `Z_ERRNO`, `Z_STREAM_ERROR`, `Z_DATA_ERROR`, `Z_MEM_ERROR`, `Z_BUF_ERROR`, `Z_VERSION_ERROR`) must be returned under identical conditions, and `strm->msg` must be set to diagnostic strings matching the C library's `z_errmsg[]` array.
- **Custom allocator support via FFI** — The `zalloc`/`zfree`/`opaque` hooks in `z_stream` must be functional through the FFI layer, allowing C callers to supply custom memory allocators just as they do with C zlib.

**Performance constraints:**

- **Compression/decompression throughput within 90% of C zlib** — The Rust implementation should achieve performance parity using Rust compiler optimizations (LTO, codegen-units=1, opt-level=3) and the optional `enable-dfa-jump-thread` LLVM flag.
- **Memory footprint must match C zlib** — The deflate engine at default settings (windowBits=15, memLevel=8) must consume approximately 256 KB, and the inflate engine window must not exceed 32 KB.

**Coding standards:**

- **Rust 2024 edition** — All crates use `edition = "2024"` for access to the latest language features
- **Clippy strict mode** — `#![deny(clippy::all, clippy::pedantic)]` enforced across all crates, with documented exceptions via `#[allow(...)]` where technically justified
- **No `unwrap()` or `expect()` in library code** — All fallible operations return `Result<T, E>` or use the `?` operator; panics are confined to test code
- **Documentation on all public items** — `#![deny(missing_docs)]` enforced on the `zlib-rs` core crate

### 0.7.3 FFI Boundary Safety Rules

The FFI layer (`libz-rs-sys`) is the only crate permitted to contain `unsafe` code. The following rules govern its usage:

- Every `extern "C"` function must validate all pointer arguments for null before dereferencing
- Slice reconstruction from raw pointers must use `std::slice::from_raw_parts` with length validation
- The `z_stream` `#[repr(C)]` struct must be layout-compatible with the C definition verified via `static_assert!(std::mem::size_of::<z_stream>() == ...)` 
- All FFI functions use `extern "C"` (not `extern "C-unwind"`) to abort on panic at the FFI boundary rather than triggering undefined behavior through stack unwinding into C code
- The cdylib crate sets `panic = "abort"` in its release profile as an additional safety measure

## 0.8 References

### 0.8.1 Codebase Files and Folders Searched

The following files and folders were retrieved and analyzed during the preparation of this Agent Action Plan:

**Root-Level Files Retrieved:**

| File | Purpose of Retrieval |
|------|---------------------|
| `zlib.h` (lines 1–100, 100–350, 350–700, 700–1100, 1800–2057) | Public API definitions: z_stream struct, gz_header, constants, all 105+ function declarations, checksum APIs, version macros |
| `zutil.h` (full, 331 lines) | Internal utility header: ZLIB_INTERNAL visibility, type aliases, block constants, OS detection, ZALLOC/ZFREE macros, debug macros |
| `deflate.h` (full, 383 lines) | Deflate internal state: deflate_state ~50 fields, 8 state constants, ct_data/tree_desc types, compression function prototypes |
| `inflate.h` (full, 126 lines) | Inflate internal state: inflate_mode 30+ state enum, inflate_state struct ~30 fields, code tables |
| `gzguts.h` (full, 216 lines) | Gzip internal state: gz_state struct, LSEEK platform abstraction, GZ_READ/GZ_WRITE modes, GZBUFSIZE |
| `inftrees.h` (full, 64 lines) | Inflate table types: code struct (op/bits/val), ENOUGH constants, codetype enum |
| `inffast.h` (full, 11 lines) | Fast decoder prototype |
| `inffixed.h` (full, 94 lines) | Pre-computed fixed Huffman tables: lenfix[512], distfix[32] |
| `trees.h` (full, 128 lines) | Static Huffman tree data: static_ltree, static_dtree, dist/length code tables |
| `win32/zlib.def` (full) | DLL export list: 105 exported symbols organized by category |
| `zlib.map` (full) | GNU symbol versioning: 14 version milestones from ZLIB_1.2.0 through ZLIB_1.3.2 |

**Directory Structures Explored:**

| Folder | Children Discovered |
|--------|-------------------|
| `` (root) | 33 files + 9 subdirectories |
| `test/` | CMakeLists.txt, example.c, infcover.c, minigzip.c |
| `contrib/` | 18 subdirectories (blast, minizip, nuget, pascal, puff, testzlib, vstudio, zlib1-dll, ada, dotzlib, crc32vx, delphi, gcc_gvmat64, infback9, iostream, iostream2, iostream3) |
| `doc/` | algorithm.txt, rfc1950.txt, rfc1951.txt, rfc1952.txt, txtvsbin.txt |
| `examples/` | 12 files (zpipe.c, zran.c/h, enough.c, fitblk.c, gun.c, gzappend.c, gzjoin.c, gzlog.c/h, gznorm.c, zlib_how.html) |
| `win32/` | README-WIN32.txt, zlib1.rc, VisualC.txt, zlib.def, DLL_FAQ.txt |

**Test File Analysis:**

| File | Key Findings |
|------|-------------|
| `test/example.c` (head, grep) | 13+ test functions: test_compress, test_gzio, test_deflate, test_inflate, test_large_deflate, test_large_inflate, test_flush, test_sync, test_dict_deflate, test_dict_inflate |
| `test/infcover.c` (head) | Inflate coverage harness with custom memory tracking (mem_item/mem_zone), hex fixture decoding, forced path coverage |
| `test/minigzip.c` (head) | Feature-complete gzip/gunzip with POSIX compliance |

**Line Count Analysis:**

| Category | Total Lines |
|----------|------------|
| All C implementation files (15 .c) | 9,700 |
| All C header files (11 .h) | 13,407 |
| Total C source code | 23,107 |

### 0.8.2 Technical Specification Sections Retrieved

| Section | Key Information Extracted |
|---------|------------------------|
| 1.1 Executive Summary | Project overview, core problem domain (data integrity, robustness, streaming, thread safety), stakeholders, value proposition |
| 2.1 Feature Catalog | 14 features across 6 categories (F-001 through F-014), feature dependencies, technical context for each |
| 3.1 Programming Languages | C89 baseline through C2x, language bindings (Ada, Pascal, Delphi, .NET, C++, RPG/ILE), assembly optimizations |
| 5.1 High-Level Architecture | 6-layer architecture, 12 components, data flow, windowBits format selection, component dependency map |
| 5.2 Component Details | Detailed internals of all 7 major components: Public API, Deflate Engine, Inflate Engine, Checksum Engines, Gzip File I/O, Utilities |
| 6.1 Core Services Architecture | Confirms zlib is a stateless in-process function library with no service infrastructure |
| 8.2 Build System Infrastructure | CMake 3.12–3.31 configuration, 15 C source files, 2 library targets, test infrastructure |

### 0.8.3 Web Research Conducted

| Search Query | Key Findings |
|-------------|-------------|
| "Rust zlib implementation FFI compatible crate 2025" | zlib-rs v0.6.0 by Trifecta Tech Foundation — 12K SLoC, 36M+ downloads, workspace structure with core/FFI/cdylib crates, uses `extern "C"` not `extern "C-unwind"`, `gzprintf`/`gzvprintf` need nightly |
| "Rust DEFLATE compression library idiomatic" | flate2-rs supports multiple backends (miniz_oxide, zlib-rs, zlib-ng); deflate crate is pure Rust with no unsafe; libflate provides DEFLATE/zlib/gzip |
| "Rust stable version latest 2026" | Rust 1.93.1 stable (Jan 2026), 2024 Edition stable since Rust 1.85.0, Rust 1.85.0 selected as MSRV |

### 0.8.4 Attachments

No attachments were provided for this project. No Figma URLs were specified.

