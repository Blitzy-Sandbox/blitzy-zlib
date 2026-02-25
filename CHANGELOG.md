# Changelog

All notable changes to the **zlib-rs** project will be documented in this file.

This is the changelog for the Rust rewrite of the
[zlib 1.3.2.1-motley](https://github.com/ArtifactedAI/zlib-rs) compression
library (C, `VERNUM 0x1321`), originally authored by Jean-loup Gailly and
Mark Adler. The Rust implementation lives in the same repository and provides a
drop-in replacement for the original C `libz`.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] — Initial Rust Rewrite

Complete rewrite of zlib 1.3.2.1-motley from C to idiomatic, production-ready
Rust. This release establishes byte-level compatibility with the original C
library for all supported compression formats.

### Added

#### Core Library (`zlib-rs` crate)

- DEFLATE compression engine with five strategies: stored, fast, slow
  (default), Huffman-only, and RLE. Ported from `deflate.c` (2 185 lines of C).
- DEFLATE decompression engine with a 30+-mode state machine supporting
  automatic format detection (zlib, gzip, raw DEFLATE). Ported from `inflate.c`
  (1 413 lines of C).
- Huffman tree construction and block encoding for dynamic, static, and fixed
  codes. Ported from `trees.c` (1 119 lines of C).
- Fast-path inner decode loop for performance-critical inflate operations.
  Ported from `inffast.c` (321 lines of C).
- Huffman code table construction (`inflate_table`). Ported from `inftrees.c`
  (424 lines of C).
- Pre-computed fixed Huffman tables (`LENFIX` / `DISTFIX`) as compile-time
  `const` arrays. Ported from `inffixed.h`.
- Callback-based decompression (`inflate_back`) using Rust closures. Ported
  from `infback.c` (579 lines of C).
- Adler-32 checksum engine with `NMAX`-optimized unrolled loop and
  `adler32_combine` support. Ported from `adler32.c` (164 lines of C).
- CRC-32 checksum engine with const table-based computation and
  `crc32_combine` / `crc32_combine_op` support. Ported from `crc32.c`
  (983 lines of C) and `crc32.h` (9 446 lines of generated tables).
- Gzip file I/O with `Read` / `Write` / `Seek` trait implementations, LOOK /
  COPY / GZIP auto-detect read pipeline, and buffered write pipeline. Ported
  from `gzlib.c`, `gzread.c`, `gzwrite.c`, and `gzclose.c`.
- One-call `compress` / `uncompress` convenience wrappers. Ported from
  `compress.c` and `uncompr.c`.
- `ZStream` Rust-native streaming interface with owned `Vec<u8>` buffers
  replacing raw `next_in` / `next_out` pointer arithmetic.
- `ReturnCode` enum mapping all zlib status codes (`Z_OK` through
  `Z_VERSION_ERROR`) with diagnostic message support.
- Full set of flush modes (`Z_NO_FLUSH` through `Z_TREES`) with identical
  output behavior to C zlib, including sync flush point markers
  (`00 00 FF FF`).
- `DeflateState` owning all compression buffers (`Vec<u8>` sliding window,
  `Vec<u16>` hash chains, `Vec<u8>` pending buffer, Huffman trees) with
  automatic `Drop`-based cleanup.
- `InflateState` with mode encoded as a Rust `enum` for exhaustive `match`
  dispatch, owned sliding window, bit accumulator, and code tables.
- `GzState` holding `std::fs::File` handles with `Drop`-based resource
  cleanup.
- Hash chain management using safe `Vec<u16>` indexing, eliminating buffer
  overrun risks from the C `NIL` sentinel pattern.
- `CompressionConfig` table mapping compression levels (0–9) to
  `good_length` / `max_lazy` / `nice_length` / `max_chain` parameters.
- `windowBits` overloading preserved: positive = zlib, negative = raw,
  +16 = gzip, +32 = auto-detect.
- Cargo feature flags replacing C `#ifdef` conditional compilation:
  `gz-io` (gzip file I/O), `gzip` (gzip format support).

#### FFI Bindings (`libz-rs-sys` crate)

- All 105 public symbols from `win32/zlib.def` exposed as
  `#[no_mangle] extern "C"` functions.
- `#[repr(C)]` type definitions for `z_stream`, `gz_header`, `alloc_func`,
  and `free_func` ensuring binary layout compatibility with C zlib.
- GNU symbol versioning across 14 milestones (`ZLIB_1.2.0` through
  `ZLIB_1.3.2`) via generated `zlib.map`.
- Build script (`build.rs`) generating `zlib.pc` pkg-config metadata and
  linker version script for the shared library.
- Custom allocator support: `zalloc` / `zfree` / `opaque` hooks in
  `z_stream` are functional through the FFI layer.
- `export-symbols` Cargo feature controlling whether C symbols are exported.

#### Shared Library (`libz-rs-sys-cdylib` crate)

- `cdylib` crate producing a drop-in replacement for `libz.so` /
  `zlib1.dll`.
- Release profile with `panic = "abort"` to prevent undefined behavior from
  stack unwinding across FFI boundaries.
- Fat LTO, single codegen unit, and `opt-level = 3` for performance parity
  with C zlib.

#### Test Suite (`tests` crate)

- Ported regression tests from `test/example.c` (13+ test functions):
  `test_compress`, `test_gzio`, `test_deflate`, `test_inflate`,
  `test_large_deflate`, `test_large_inflate`, `test_flush`, `test_sync`,
  `test_dict_deflate`, `test_dict_inflate`.
- Ported inflate coverage harness from `test/infcover.c` with memory
  tracking and hex fixture decoding.
- Deflate-specific edge case tests (all levels, strategies, `windowBits`
  values).
- Inflate edge case tests (auto-detect, corrupt data, sync recovery).
- Gzip file I/O round-trip tests (seek, transparent read).
- Adler-32 and CRC-32 correctness and `combine` operation tests.
- FFI symbol presence and signature validation tests.
- Cross-compatibility tests: Rust-compress ↔ C-decompress and
  C-compress ↔ Rust-decompress round-trips.

#### Examples

- `zpipe.rs` — Pipe compression / decompression utility (port of
  `examples/zpipe.c`).
- `minigzip.rs` — Feature-complete gzip / gunzip utility (port of
  `test/minigzip.c`).
- `enough.rs` — `ENOUGH` constant calculation tool (port of
  `examples/enough.c`).

#### Benchmarks

- Deflate compression benchmarks (`zlib-rs/benches/deflate_bench.rs`).
- Inflate decompression benchmarks (`zlib-rs/benches/inflate_bench.rs`).
- Checksum computation benchmarks (`zlib-rs/benches/checksum_bench.rs`).

#### Build & Tooling

- Cargo workspace with four crates replacing the CMake / autotools build
  system.
- `rust-toolchain.toml` pinning Rust 1.85.0 (MSRV), edition 2024.
- `.cargo/config.toml` with release profile optimizations and optional LLVM
  DFA jump thread flag.
- `clippy.toml` enforcing strict lint mode
  (`deny(clippy::all, clippy::pedantic)`).
- `deny(missing_docs)` on the `zlib-rs` core crate.
- Triple-licensed: zlib (original) + MIT + Apache-2.0 for the Rust code.

#### Documentation

- `README.md` with Cargo build, test, and usage instructions.
- `doc/algorithm.md` — Markdown conversion of the DEFLATE algorithm
  description.
- RFC reference documents preserved: RFC 1950 (zlib), RFC 1951 (DEFLATE),
  RFC 1952 (gzip).

### Design Decisions

- **Zero `unsafe` in core logic.** All `unsafe` code is confined to the
  `libz-rs-sys` FFI boundary crate. The `zlib-rs` core crate uses only safe
  Rust.
- **`extern "C"` (not `extern "C-unwind"`)** at FFI boundaries to abort on
  panic rather than triggering undefined behavior through stack unwinding
  into C code.
- **No `unwrap()` or `expect()` in library code.** All fallible operations
  use `Result<T, E>` or the `?` operator; panics are confined to test code.
- **Const evaluation over runtime initialization.** CRC tables and fixed
  Huffman tables are compile-time `const` arrays, eliminating the
  `DYNAMIC_CRC_TABLE` runtime initialization pattern from C.
- **Standard Rust types replace platform detection.** The `zconf.h` type
  detection logic (`Z_U4`, `z_crc_t`, `z_word_t`, `z_size_t`, `z_off_t`)
  is replaced by standard Rust types: `u32`, `u32`, `u64`, `usize`, `i64`.

## Heritage

This project is a Rust rewrite of the **zlib** general-purpose compression
library, one of the most widely deployed software libraries in existence.

| | |
|---|---|
| **Original library** | zlib |
| **Original version** | 1.3.2.1-motley (`VERNUM 0x1321`) |
| **Original authors** | Jean-loup Gailly (`jloup@gzip.org`) and Mark Adler (`madler@alumni.caltech.edu`) |
| **Original copyright** | © 1995–2026 Jean-loup Gailly and Mark Adler |
| **Original license** | zlib license (see `LICENSE-ZLIB`) |
| **Specifications** | RFC 1950 (zlib format), RFC 1951 (DEFLATE format), RFC 1952 (gzip format) |

The original C source code served as the authoritative specification for
every algorithm, data structure, and behavioral contract in this Rust
implementation.

[Unreleased]: https://github.com/ArtifactedAI/zlib-rs/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/ArtifactedAI/zlib-rs/releases/tag/v0.1.0
