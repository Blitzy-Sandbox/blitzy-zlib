# zlib-rs

**A complete Rust rewrite of zlib 1.3.2.1-motley**

[![crates.io](https://img.shields.io/crates/v/zlib-rs.svg)](https://crates.io/crates/zlib-rs)
[![docs.rs](https://docs.rs/zlib-rs/badge.svg)](https://docs.rs/zlib-rs)
[![License](https://img.shields.io/crates/l/zlib-rs.svg)](LICENSE-ZLIB)

Production-ready, idiomatic Rust implementation of the zlib compression library,
providing full DEFLATE format compatibility with
[RFC 1950](https://datatracker.ietf.org/doc/html/rfc1950) (zlib),
[RFC 1951](https://datatracker.ietf.org/doc/html/rfc1951) (DEFLATE), and
[RFC 1952](https://datatracker.ietf.org/doc/html/rfc1952) (gzip).

This library is a faithful, ground-up rewrite of the original C zlib library
(version 1.3.2.1-motley, `VERNUM 0x1321`) authored by Jean-loup Gailly and
Mark Adler. It replaces all manual memory management with Rust ownership
semantics and eliminates undefined behavior by design.

---

## Feature Highlights

- **Pure safe Rust core** — Zero `unsafe` blocks in the `zlib-rs` core crate.
  All compression, decompression, checksum, and gzip I/O logic is implemented
  in safe Rust, leveraging ownership, lifetimes, and the type system to
  eliminate memory safety issues at compile time.

- **C-compatible FFI** — Drop-in replacement for `libz.so` / `zlib1.dll` via
  the `libz-rs-sys` and `libz-rs-sys-cdylib` crates, exposing all 96 public
  symbols from the original zlib C API through `#[no_mangle] extern "C"`
  functions.

- **Full format compatibility** — Bit-identical compressed output for
  deterministic algorithm paths. Compressed data produced by this library is
  decompressible by reference C zlib and vice versa. Interoperable with any
  standard zlib, gzip, or raw DEFLATE implementation.

- **Rust-native API** — Idiomatic Rust types (`ZStream`, `DeflateState`,
  `InflateState`) with `Result<T, ZlibError>` return types, slice-based I/O
  (`&[u8]` / `&mut [u8]`), and `Read` / `Write` / `Seek` trait
  implementations for gzip file I/O.

- **Comprehensive test suite** — Ported from the official zlib test harnesses
  (`test/example.c` and `test/infcover.c`), plus cross-compatibility
  validation ensuring Rust-compressed data round-trips through reference C
  zlib and vice versa.

---

## Workspace Structure

The project is organized as a Cargo workspace with four crates that mirror the
architectural separation of the original C library:

```
zlib-rs/               Core Rust library (pure safe Rust, no external runtime deps)
  src/
    lib.rs             Crate root and public API re-exports
    stream.rs          ZStream — Rust-native stream interface
    error.rs           ReturnCode enum, ZlibError type
    constants.rs       Flush modes, strategies, compression levels
    compress.rs        One-call compress() / uncompress() helpers
    util.rs            Error messages, compile flags, internal helpers
    deflate/           DEFLATE compression engine
      mod.rs           deflate(), deflate_init2(), deflate_end(), etc.
      state.rs         DeflateState — owns window, hash chains, pending buffer
      algorithm.rs     stored, fast, slow, huff, rle compression strategies
      trees.rs         Huffman tree building and block encoding
      hash.rs          Hash chain management, longest_match
      params.rs        CompressionConfig, level-to-parameter mapping
    inflate/           DEFLATE decompression engine
      mod.rs           inflate(), inflate_init2(), inflate_end(), etc.
      state.rs         InflateState — mode enum, window, code tables
      fast.rs          inflate_fast() hot loop
      table.rs         Huffman table construction
      fixed.rs         Pre-computed fixed Huffman tables
      back.rs          inflateBack() callback-based decompression
    gz/                Gzip file I/O
      mod.rs           gzopen, gzclose, gzbuffer, error handling
      read.rs          GzReader — LOOK/COPY/GZIP auto-detect pipeline
      write.rs         GzWriter — buffered compression pipeline
      state.rs         GzState — mode, file handle, buffers
    checksum/          Checksum engines
      mod.rs           Checksum module exports
      adler32.rs       Adler-32 with NMAX-optimized loop
      crc32.rs         CRC-32 with table-based computation

libz-rs-sys/          C-compatible FFI bindings (#[no_mangle] extern "C")
  src/
    lib.rs             Re-exports all 96 extern "C" symbols
    types.rs           #[repr(C)] z_stream, gz_header, alloc_func
    deflate.rs         deflateInit_, deflate, deflateEnd, etc.
    inflate.rs         inflateInit_, inflate, inflateEnd, etc.
    gz.rs              gzopen, gzread, gzwrite, etc.
    checksum.rs        adler32, crc32, combine functions
    compress.rs        compress, compress2, uncompress, etc.
    version.rs         zlibVersion, zlibCompileFlags
  build.rs             Version script and pkg-config generation
  zlib.map             GNU symbol versioning (14 version milestones)

libz-rs-sys-cdylib/   Shared library output (drop-in libz.so replacement)
  src/
    lib.rs             Re-exports all FFI symbols as cdylib

tests/                 Integration test suite
  tests/
    example_compat.rs  Port of test/example.c (9 test functions)
    infcover_compat.rs Port of test/infcover.c (coverage harness)
    deflate_tests.rs   Deflate-specific edge cases
    inflate_tests.rs   Inflate-specific edge cases
    gz_tests.rs        Gzip file I/O round-trip tests
    checksum_tests.rs  Adler-32 and CRC-32 correctness tests
    ffi_compat_tests.rs  FFI symbol presence validation
    cross_compat_tests.rs  Rust ↔ C cross-compatibility
```

---

## Getting Started

### Prerequisites

- [Rust](https://www.rust-lang.org/tools/install) 1.85.0 or later (edition 2024)
- The toolchain is pinned via `rust-toolchain.toml`; `rustup` will
  automatically download the correct version.

### Building

```bash
# Build all workspace crates in release mode
cargo build --release

# Build only the core Rust library
cargo build --release -p zlib-rs

# Build the C-compatible shared library (libz.so / zlib1.dll)
cargo build --release -p libz-rs-sys-cdylib

# Run the full test suite
cargo test --workspace

# Run benchmarks (deflate, inflate, checksum)
cargo bench -p zlib-rs

# Check code formatting
cargo fmt --check --all

# Run lints (strict mode: deny clippy::all + clippy::pedantic)
cargo clippy --workspace -- -D warnings
```

### Using as a Rust Dependency

Add `zlib-rs` to your `Cargo.toml`:

```toml
[dependencies]
zlib-rs = "0.1.0"
```

Then use it in your Rust code:

```rust
use zlib_rs::compress::{compress, uncompress};

// One-call compression
let input = b"Hello, zlib-rs!";
let compressed = compress(input, 6)?;

// One-call decompression
let decompressed = uncompress(&compressed, input.len())?;
assert_eq!(decompressed, input);
```

### Using as a C Library Replacement

Build the shared library and use it as a drop-in replacement for `libz.so` or
`zlib1.dll`:

```bash
# Build the shared library
cargo build --release -p libz-rs-sys-cdylib

# The output is in target/release/:
#   Linux:   libz.so
#   macOS:   libz.dylib
#   Windows: zlib1.dll

# Link your C/C++ application against the produced library
# instead of the system zlib
```

---

## API Overview

### Streaming Compression

```
deflate_init2()  →  deflate()  →  deflate_end()
```

Initialize a compression stream with configurable window size, memory level,
and strategy. Feed input data in chunks via `deflate()` and finalize with
`Z_FINISH` flush mode.

### Streaming Decompression

```
inflate_init2()  →  inflate()  →  inflate_end()
```

Initialize a decompression stream with automatic format detection (zlib, gzip,
or raw DEFLATE via `windowBits` overloading). Feed compressed data in chunks
and retrieve decompressed output.

### One-Call Helpers

- `compress(input, level)` — Compress an entire buffer in a single call
- `uncompress(input, output_len)` — Decompress an entire buffer in a single call

### Gzip File I/O

- `GzReader` — Implements `std::io::Read` for transparent gzip decompression
  with automatic format detection (LOOK/COPY/GZIP modes)
- `GzWriter` — Implements `std::io::Write` for buffered gzip compression

### Checksums

- `adler32(checksum, data)` — Adler-32 checksum with NMAX-optimized block processing
- `crc32(checksum, data)` — CRC-32 checksum with const table-based computation
- `adler32_combine()` / `crc32_combine()` — Combine checksums from independent data segments

### Callback-Based Decompression

- `inflate_back()` — I/O callback-driven raw DEFLATE decompression for
  applications that manage their own I/O buffering

---

## Compatibility

| Property | Value |
|---|---|
| **Rust version (MSRV)** | 1.85.0 |
| **Rust edition** | 2024 |
| **Original C library** | zlib 1.3.2.1-motley (`VERNUM 0x1321`) |
| **Original authors** | Jean-loup Gailly and Mark Adler |
| **Format compliance** | RFC 1950 (zlib), RFC 1951 (DEFLATE), RFC 1952 (gzip) |
| **FFI symbols exported** | 96 |
| **Symbol versioning** | 14 milestones (`ZLIB_1.2.0` through `ZLIB_1.3.2`) |

### Format Selection via `windowBits`

The `windowBits` parameter controls both the window size and the container
format, preserving the original zlib behavior:

| `windowBits` Range | Format | Description |
|---|---|---|
| 8..=15 | zlib | zlib wrapper (RFC 1950) |
| -8..=-15 | raw | Raw DEFLATE (RFC 1951), no header/trailer |
| 24..=31 (16 + 8..=15) | gzip | gzip wrapper (RFC 1952) |
| 40..=47 (32 + 8..=15) | auto | Auto-detect zlib or gzip on decompression |

### Performance

The release profile is tuned for performance parity with C zlib:

- **Fat LTO** — Full link-time optimization across all crates
- **Single codegen unit** — Enables maximum LLVM cross-function optimization
- **`opt-level = 3`** — Maximum optimization level, matching C zlib's `-O3`
- **LLVM DFA jump thread** — Optional optimization for state machine dispatch

Target: compression and decompression throughput within 90% of C zlib at
equivalent optimization levels.

---

## License

This project is triple-licensed:

- **[zlib License](LICENSE-ZLIB)** — The original zlib license by Jean-loup
  Gailly and Mark Adler applies to the algorithmic design and is preserved
  from the original C library.
- **[MIT License](LICENSE-MIT)** — Applies to the Rust implementation code.
- **[Apache License 2.0](LICENSE-APACHE)** — Applies to the Rust
  implementation code as an alternative to MIT.

You may use this library under the terms of any of these licenses at your
option. The zlib license is included to honor the original work; the
MIT/Apache-2.0 dual license follows the standard Rust ecosystem convention.

See [LICENSE-ZLIB](LICENSE-ZLIB), [LICENSE-MIT](LICENSE-MIT), and
[LICENSE-APACHE](LICENSE-APACHE) for full license texts.

---

## Contributing

Contributions are welcome! Please follow these guidelines:

1. **Fork and branch** — Create a feature branch from `main`.
2. **Format your code** — Run `cargo fmt --all` before committing.
3. **Pass all lints** — Run `cargo clippy --workspace -- -D warnings` with
   zero warnings. The project enforces `deny(clippy::all, clippy::pedantic)`.
4. **Pass all tests** — Run `cargo test --workspace` and ensure all tests pass.
5. **No `unsafe` in core** — The `zlib-rs` core crate must remain free of
   `unsafe` blocks. All `unsafe` code belongs exclusively in the
   `libz-rs-sys` FFI boundary crate.
6. **No `unwrap()` or `expect()` in library code** — Use `Result<T, E>` and
   the `?` operator for all fallible operations. Panics are permitted only in
   test code.
7. **Document public items** — The `zlib-rs` core crate enforces
   `deny(missing_docs)` on all public types, functions, and modules.

### CI Workflows

The following GitHub Actions workflows validate every pull request:

- **[ci.yml](.github/workflows/ci.yml)** — Build, test, clippy, and format
  checks across the full workspace.
- **[ffi-compat.yml](.github/workflows/ffi-compat.yml)** — FFI compatibility
  validation ensuring all 96 symbols are correctly exported.
- **[cross-platform.yml](.github/workflows/cross-platform.yml)** —
  Cross-compilation matrix for supported targets.

---

## Acknowledgments

This library is a Rust rewrite of [zlib](https://zlib.net/), the venerable
compression library created by **Jean-loup Gailly** and **Mark Adler**. The
original C implementation has served as the foundation of data compression
across virtually every computing platform since 1995.

The data format specifications that this library implements are documented in:

- [RFC 1950](https://datatracker.ietf.org/doc/html/rfc1950) — ZLIB Compressed Data Format Specification
- [RFC 1951](https://datatracker.ietf.org/doc/html/rfc1951) — DEFLATE Compressed Data Format Specification
- [RFC 1952](https://datatracker.ietf.org/doc/html/rfc1952) — GZIP File Format Specification
