# zlib-rs

**A pure Rust implementation of the zlib compression library.**

[![Crates.io](https://img.shields.io/crates/v/zlib-rs.svg)](https://crates.io/crates/zlib-rs)
[![Documentation](https://docs.rs/zlib-rs/badge.svg)](https://docs.rs/zlib-rs)
[![License: Zlib](https://img.shields.io/badge/license-Zlib-blue.svg)](LICENSE)

Byte-level DEFLATE compression and decompression fully compatible with
[C zlib](https://zlib.net/) (version 1.3.2.1-motley). This crate implements the
complete zlib public API surface as an independent, zero-C-dependency Rust library
conforming to:

- [RFC 1950](https://datatracker.ietf.org/doc/html/rfc1950) — zlib compressed data format
- [RFC 1951](https://datatracker.ietf.org/doc/html/rfc1951) — DEFLATE compressed data format
- [RFC 1952](https://datatracker.ietf.org/doc/html/rfc1952) — gzip file format

Any data compressed by C zlib decompresses identically through `zlib-rs`, and vice
versa — the two implementations are fully interoperable at the wire-format level.

## Features

### Core Capabilities

| Capability | Description |
|-----------|-------------|
| **DEFLATE Compression** | Levels 0–9 with 5 strategies: Default, Filtered, Huffman-Only, RLE, and Fixed |
| **DEFLATE Decompression** | Streaming decompression with automatic format detection (zlib, gzip, raw DEFLATE) |
| **Checksums** | Adler-32 and CRC-32 computation with combine operations for parallel processing |
| **Gzip File I/O** | `stdio`-like interface: `gz_open`, `gz_read`, `gz_write`, `gz_close`, `gz_seek`, and more |
| **One-Call Utilities** | `compress`, `compress2`, `uncompress`, `uncompress2` for simple buffer-to-buffer operations |
| **Preset Dictionaries** | Full preset dictionary support for both compression and decompression |

### Safety and Quality

- **Pure Rust** — zero C dependencies, no `cc` build step, no C linker requirement
- **Memory safe** — greater than 98% safe Rust code by line count; `unsafe` is restricted
  to performance-critical inner loops (e.g., `inflate_fast`) with documented safety
  invariants
- **Rust 2024 edition** — built with `edition = "2024"`, minimum supported Rust version
  (MSRV) **1.85.0**
- **`no_std` ready** — core compression, decompression, and checksum modules compile
  without the standard library when the `std` feature is disabled

## Quick Start

### Installation

Add `zlib-rs` to your `Cargo.toml`:

```toml
[dependencies]
zlib-rs = "1.3.2"
```

### One-Call Compression and Decompression

The simplest way to compress and decompress data in a single call:

```rust
use zlib_rs::util::compress::{compress, compress_bound};
use zlib_rs::util::uncompress::uncompress;

// Compress
let input = b"Hello, zlib-rs! This is a test of one-call compression.";
let bound = compress_bound(input.len());
let mut compressed = vec![0u8; bound];
let compressed_len = compress(&mut compressed, input).unwrap();
compressed.truncate(compressed_len);

// Decompress
let mut decompressed = vec![0u8; input.len()];
let decompressed_len = uncompress(&mut decompressed, &compressed).unwrap();
decompressed.truncate(decompressed_len);

assert_eq!(&decompressed, input);
```

### Streaming Compression (Deflate)

For incremental or large-data compression, use the streaming `ZStream` API:

```rust
use zlib_rs::stream::ZStream;
use zlib_rs::deflate;
use zlib_rs::constants::{Z_DEFAULT_COMPRESSION, Z_FINISH};

// Initialize a deflate stream
let mut stream = ZStream::new();
deflate::deflate_init(&mut stream, Z_DEFAULT_COMPRESSION).unwrap();

// Provide input and output buffers
let input = b"Streaming compression with zlib-rs is straightforward.";
let mut output = vec![0u8; 256];

stream.set_input(input);
stream.set_output(&mut output);

// Compress in one pass (use Z_FINISH for single-buffer input)
let status = deflate::deflate(&mut stream, Z_FINISH).unwrap();

// Finalize
let compressed_size = stream.total_out as usize;
deflate::deflate_end(&mut stream).unwrap();

output.truncate(compressed_size);
```

### Streaming Decompression (Inflate)

```rust
use zlib_rs::stream::ZStream;
use zlib_rs::inflate;
use zlib_rs::constants::Z_FINISH;

// Initialize an inflate stream
let mut stream = ZStream::new();
inflate::inflate_init(&mut stream).unwrap();

// Provide compressed input and output buffer
stream.set_input(&compressed_data);
let mut output = vec![0u8; expected_size];
stream.set_output(&mut output);

// Decompress
let status = inflate::inflate(&mut stream, Z_FINISH).unwrap();

let decompressed_size = stream.total_out as usize;
inflate::inflate_end(&mut stream).unwrap();

output.truncate(decompressed_size);
```

### Checksums

Compute Adler-32 and CRC-32 checksums:

```rust
use zlib_rs::checksum::adler32::adler32;
use zlib_rs::checksum::crc32::crc32;

let data = b"The quick brown fox jumps over the lazy dog";

// Adler-32
let a32 = adler32(1, data);  // Initial value of 1
println!("Adler-32: {:#010x}", a32);

// CRC-32
let c32 = crc32(0, data);    // Initial value of 0
println!("CRC-32:   {:#010x}", c32);
```

### Gzip File I/O

Read and write `.gz` files with a `stdio`-like interface:

```rust
use std::path::Path;
use zlib_rs::gz;

// Write compressed data to a gzip file
let mut gzfile = gz::gz_open(Path::new("output.gz"), "wb").unwrap();
gz::gz_write(&mut gzfile, b"Hello from gzip file I/O!").unwrap();
gz::gz_close(&mut gzfile).unwrap();

// Read it back
let mut gzfile = gz::gz_open(Path::new("output.gz"), "rb").unwrap();
let mut buf = vec![0u8; 256];
let n = gz::gz_read(&mut gzfile, &mut buf).unwrap();
buf.truncate(n);
gz::gz_close(&mut gzfile).unwrap();

assert_eq!(&buf, b"Hello from gzip file I/O!");
```

## Feature Flags

`zlib-rs` uses Cargo feature flags to control optional functionality. All features
listed as **default** are enabled when you add the crate without specifying features.

| Feature | Default | Description |
|---------|---------|-------------|
| `std` | ✅ | Enable `std::io`-based gzip file I/O and standard library integration |
| `gzip` | ✅ | Enable gzip format support in deflate and inflate operations |
| `gz-io` | ✅ | Enable gzip file I/O operations (`gz_open`, `gz_read`, `gz_write`, etc.). Implies `std` and `gzip` |
| `no-std` | ❌ | Bare-metal mode: disables `std`, provides only compression, decompression, and checksums |
| `simd` | ✅ | SIMD-accelerated CRC-32 via the [`crc32fast`](https://crates.io/crates/crc32fast) crate |

### Disabling Default Features

To use `zlib-rs` in a `no_std` environment, disable all default features and enable
only what you need:

```toml
[dependencies]
zlib-rs = { version = "1.3.2", default-features = false, features = ["no-std"] }
```

For standard builds without gzip file I/O:

```toml
[dependencies]
zlib-rs = { version = "1.3.2", default-features = false, features = ["std", "gzip", "simd"] }
```

## API Overview

### Public Modules

| Module | Description |
|--------|-------------|
| `zlib_rs::stream` | `ZStream` struct and `StreamState` enum — the central exchange object for streaming operations |
| `zlib_rs::deflate` | DEFLATE compression: `deflate_init`, `deflate_init2`, `deflate`, `deflate_end`, `deflate_reset`, `deflate_params`, `deflate_tune`, `deflate_bound`, `deflate_pending`, `deflate_prime`, `deflate_set_header`, `deflate_set_dictionary`, `deflate_get_dictionary`, `deflate_copy` |
| `zlib_rs::inflate` | DEFLATE decompression: `inflate_init`, `inflate_init2`, `inflate`, `inflate_end`, `inflate_reset`, `inflate_reset2`, `inflate_sync`, `inflate_copy`, `inflate_prime`, `inflate_mark`, `inflate_get_header`, `inflate_set_dictionary`, `inflate_get_dictionary`, `inflate_back` |
| `zlib_rs::checksum` | `adler32`, `adler32_z`, `adler32_combine`, `crc32`, `crc32_z`, `crc32_combine`, `crc32_combine_gen`, `crc32_combine_op` |
| `zlib_rs::gz` | Gzip file I/O: `gz_open`, `gz_read`, `gz_write`, `gz_close`, `gz_seek`, `gz_tell`, `gz_eof`, `gz_direct`, `gz_error_msg`, `gz_clearerr`, and more |
| `zlib_rs::util` | One-call utilities: `compress`, `compress2`, `compress_bound`, `uncompress`, `uncompress2`, `zlib_version`, `compile_flags` |
| `zlib_rs::error` | `ZlibError` enum (failure cases), `ReturnCode` enum (success variants), and `Result` type alias |
| `zlib_rs::constants` | Flush modes, compression levels, strategies, data types, and limit constants |
| `zlib_rs::gz_header` | `GzHeader` struct for gzip metadata fields |

### The Streaming API Pattern

`zlib-rs` preserves the classic zlib streaming paradigm. Callers control buffer
management by setting input and output buffers on a `ZStream` object, then calling
`deflate()` or `inflate()` repeatedly:

```
┌──────────┐     set_input(&[u8])     ┌──────────┐
│  Caller  │ ──────────────────────── │ ZStream  │
│          │     set_output(&mut)     │          │
│          │ ──────────────────────── │          │
│          │                          │          │
│          │   deflate() / inflate()  │          │
│          │ ◄──────────────────────► │  State   │
│          │                          │          │
│          │    total_in / total_out  │          │
│          │ ◄─────────────────────── │          │
└──────────┘                          └──────────┘
```

The caller feeds data incrementally in arbitrarily sized chunks. The library
processes as much as possible per call and signals its status via `ReturnCode`.

### `windowBits` Overloading

The `windowBits` parameter in `deflate_init2` and `inflate_init2` controls both the
window size and the format:

| Range | Format | Description |
|-------|--------|-------------|
| 8–15 | zlib | Standard zlib wrapping (RFC 1950) with Adler-32 checksum |
| −8 to −15 | Raw DEFLATE | No header or trailer (RFC 1951 only) |
| 24–31 (8–15 + 16) | gzip | Gzip wrapping (RFC 1952) with CRC-32 checksum |
| 40–47 (8–15 + 32) | Auto-detect | Inflate auto-detects zlib or gzip format from the header |

### Error Handling

All fallible operations return `Result<ReturnCode, ZlibError>`:

```rust
use zlib_rs::error::{ReturnCode, ZlibError};

// ReturnCode covers success and special-but-normal events:
//   ReturnCode::Ok          — operation completed normally
//   ReturnCode::StreamEnd   — end of compressed stream reached
//   ReturnCode::NeedDict    — a preset dictionary is required

// ZlibError covers failures:
//   ZlibError::Errno        — file I/O error (gzip operations)
//   ZlibError::StreamError  — invalid parameter or inconsistent stream state
//   ZlibError::DataError    — corrupted or invalid compressed data
//   ZlibError::MemError     — insufficient memory
//   ZlibError::BufError     — output buffer too small / input exhausted
//   ZlibError::VersionError — library version mismatch
```

## Compatibility

### Wire-Format Interoperability

`zlib-rs` is wire-format compatible with C zlib. This means:

- Data compressed by C zlib decompresses correctly through `zlib-rs`
- Data compressed by `zlib-rs` decompresses correctly through C zlib
- Checksums (Adler-32, CRC-32) are computed identically
- Header and trailer bytes match the same RFC-defined formats

### Flush Modes

All 7 flush modes are supported with identical semantics to C zlib:

| Constant | Value | Behavior |
|----------|-------|----------|
| `FlushMode::NoFlush` | 0 | Normal operation, buffer as much as possible |
| `FlushMode::PartialFlush` | 1 | Obsolete; behaves like `SyncFlush` |
| `FlushMode::SyncFlush` | 2 | Flush to a byte boundary, emit a stored block sync marker |
| `FlushMode::FullFlush` | 3 | Like `SyncFlush` but resets compression state for random access |
| `FlushMode::Finish` | 4 | Signal end of input; finalize the compressed stream |
| `FlushMode::Block` | 5 | Emit the current block but do not align to a byte boundary |
| `FlushMode::Trees` | 6 | Like `Block` but also return after emitting Huffman tree headers |

### Compression Levels

All 10 compression levels are supported:

| Level | Constant | Behavior |
|-------|----------|----------|
| 0 | `Z_NO_COMPRESSION` | No compression — data is stored verbatim in DEFLATE blocks |
| 1 | `Z_BEST_SPEED` | Fastest compression using greedy matching |
| 2–3 | — | Greedy matching with progressively longer chains |
| 4–8 | — | Lazy matching with increasing search depth |
| 9 | `Z_BEST_COMPRESSION` | Maximum compression with deepest hash chain search |
| −1 | `Z_DEFAULT_COMPRESSION` | Library default (equivalent to level 6) |

### Compression Strategies

All 5 compression strategies are supported:

| Constant | Value | Description |
|----------|-------|-------------|
| `Z_DEFAULT_STRATEGY` | 0 | Balanced LZ77 + Huffman coding (suitable for most data) |
| `Z_FILTERED` | 1 | Tuned for data produced by a filter or predictor |
| `Z_HUFFMAN_ONLY` | 2 | Huffman coding only, no LZ77 string matching |
| `Z_RLE` | 3 | Run-length encoding — matches only at distance 1 |
| `Z_FIXED` | 4 | Use fixed (pre-defined) Huffman codes instead of dynamic |

## Building and Testing

```bash
# Build the library
cargo build

# Build in release mode with optimizations
cargo build --release

# Run the full test suite
cargo test

# Run with strict lint checking
cargo clippy -- -D warnings

# Format check
cargo fmt -- --check

# Run benchmarks (requires nightly or stable with criterion)
cargo bench
```

## License

This project is licensed under the **zlib license** — see the [LICENSE](LICENSE) file
for details.

```
Copyright (C) 1995-2026 Jean-loup Gailly and Mark Adler

This software is provided 'as-is', without any express or implied
warranty. In no event will the authors be held liable for any damages
arising from the use of this software.

Permission is granted to anyone to use this software for any purpose,
including commercial applications, and to alter it and redistribute it
freely, subject to the following restrictions:

1. The origin of this software must not be misrepresented; you must not
   claim that you wrote the original software. If you use this software
   in a product, an acknowledgment in the product documentation would be
   appreciated but is not required.
2. Altered source versions must be plainly marked as such, and must not be
   misrepresented as being the original software.
3. This notice may not be removed or altered from any source distribution.
```

## Credits

`zlib-rs` is a Rust rewrite of the [zlib](https://zlib.net/) compression library,
originally created by **Jean-loup Gailly** and **Mark Adler**. The original C
implementation is available at <https://github.com/madler/zlib>.

This Rust implementation aims to preserve complete wire-format compatibility while
leveraging Rust's ownership system, borrow checker, and type system to eliminate
entire categories of memory safety vulnerabilities.
