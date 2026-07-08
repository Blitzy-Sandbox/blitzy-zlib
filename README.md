# zlib-rs

> A memory-safe, idiomatic Rust rewrite of **zlib 1.3.2.1** with byte-identical
> DEFLATE output and a C-compatible FFI drop-in layer.

`zlib-rs` is a from-scratch Rust port of the venerable [zlib](https://zlib.net/)
compression library. It implements the DEFLATE ([RFC 1951](https://datatracker.ietf.org/doc/html/rfc1951)),
zlib ([RFC 1950](https://datatracker.ietf.org/doc/html/rfc1950)), and gzip
([RFC 1952](https://datatracker.ietf.org/doc/html/rfc1952)) formats, produces
streams that are **byte-for-byte compatible** with the reference C
implementation, and ships as both an idiomatic Rust crate and a drop-in
replacement for the C `libz` shared/static library.

---

## Overview

zlib is one of the most widely deployed pieces of software in the world. This
crate reimplements it in safe Rust so that the same wire format can be produced
and consumed without the manual memory management of the original C code.

- **Full wire-format compatibility.** For a given input, compression level, and
  strategy, the compressed output is byte-for-byte identical to reference zlib.
  This is validated **by default** (no C toolchain required) in
  `tests/interop.rs`, which checks zlib-rs output against 300 vectors baked from
  genuine C zlib 1.3.2.1-motley across levels, strategies, and
  zlib/raw/gzip/`windowBits` framings. Decompression accepts any valid zlib,
  raw-DEFLATE, or gzip stream, including those produced by other
  implementations.
- **Memory safety by construction.** Manual `zcalloc`/`zcfree` allocation is
  replaced by Rust ownership, borrowing, and `Drop`-based cleanup. The
  compression core (`src/deflate/`) contains **zero `unsafe`**.
- **`unsafe` isolated to the boundary.** What little `unsafe` exists is confined
  to the FFI layer (`src/ffi/`) and — at most — the inflate fast-path decode
  loop (`src/inflate/fast.rs`), and every occurrence carries a `// SAFETY:`
  justification.
- **Zero C dependency in the shipped artifact.** The library links no C code:
  its only runtime dependencies (`crc32fast`, `cfg-if`) are pure Rust, so there
  is no C toolchain in the build.
- **Two APIs, one implementation.** Use the ergonomic Rust API for new code, or
  drop the emitted `cdylib`/`staticlib` in place of `libz` for existing C
  consumers.

## Status

**Experimental — active migration.** The crate is being brought up as a faithful
Rust reimplementation and its API surface may still shift while parity work
continues. It tracks upstream `zlib 1.3.2.1-motley` (`ZLIB_VERNUM 0x1321`).

Compression/decompression correctness and byte-identical format compatibility
are the primary acceptance criteria and are validated by default. Byte-identity,
the `no_std` test suite, and `cargo-fuzz` targets are all now in place;
performance has been measured and is reported in the [Roadmap](#roadmap).

## Highlights

- DEFLATE encoder with every match-finding strategy (`stored`, `fast`, `slow`,
  `rle`, `huffman-only`) and all ten compression levels.
- Inflate decoder with the raw-callback `inflateBack` path and streaming
  `inflateSync` error recovery.
- zlib (RFC 1950) and gzip (RFC 1952) wrappers around raw DEFLATE (RFC 1951),
  with the full `windowBits` overloading contract (zlib / raw / gzip /
  auto-detect).
- Adler-32 and CRC-32 checksums, including the `adler32_combine` /
  `crc32_combine` operations; the CRC-32 hot path is SIMD-accelerated via
  [`crc32fast`](https://crates.io/crates/crc32fast).
- All seven flush modes, preset-dictionary support, and the exact
  `compressBound` sizing formula.
- A C ABI (`#[no_mangle] extern "C"`) exposing the exact zlib symbol table with
  `#[repr(C)]` mirrors of `z_stream` and `gz_header`.

## Installation

Add the crate with Cargo:

```sh
cargo add zlib-rs
```

or add it to your `Cargo.toml` directly:

```toml
[dependencies]
zlib-rs = "1.3.2"
```

The crate name on crates.io is `zlib-rs`; the importable module path is
`zlib_rs` (Cargo replaces `-` with `_`):

```rust,ignore
use zlib_rs::{compress, uncompress};
```

**Minimum Supported Rust Version (MSRV): `1.85.0`.** The crate targets the Rust
**2024 edition**, which was stabilized in Rust 1.85.0. Any newer stable toolchain
also works.

## Usage — idiomatic Rust API

The public API is re-exported from the crate root (`zlib_rs::…`). The examples
below are illustrative; they mirror the crate's intended surface while the
migration stabilizes.

### One-shot compression

`compress` and `uncompress` operate on caller-provided buffers and return the
number of bytes written, mirroring the classic zlib one-call helpers. Size the
destination buffer with `compress_bound`.

```rust,ignore
use zlib_rs::{compress, uncompress, compress_bound};

let source = b"the quick brown fox jumps over the lazy dog";

// Upper bound on the compressed size, then compress into an owned buffer.
let mut packed = vec![0u8; compress_bound(source.len())];
let n = compress(&mut packed, source).expect("compress failed");
packed.truncate(n);

// Round-trip back to the original bytes.
let mut restored = vec![0u8; source.len()];
let m = uncompress(&mut restored, &packed).expect("uncompress failed");
restored.truncate(m);

assert_eq!(restored, source);
```

Choose an explicit compression level (0–9, or `Z_DEFAULT_COMPRESSION`) with
`compress2`:

```rust,ignore
use zlib_rs::{compress2, compress_bound, Z_BEST_COMPRESSION};

let source = b"payload to squeeze as hard as possible";
let mut packed = vec![0u8; compress_bound(source.len())];
let n = compress2(&mut packed, source, Z_BEST_COMPRESSION).expect("compress2 failed");
packed.truncate(n);
```

### Streaming deflate / inflate

For incremental work, build a configuration and drive a `ZStream`. The
`DeflateConfig` builder replaces the multi-argument `deflateInit2_` call, and
cleanup is automatic via `Drop` (no explicit `deflateEnd` required, though it is
available for parity).

```rust,ignore
use zlib_rs::{ZStream, DeflateConfig, FlushMode, deflate, deflate_end};

// Configure: level 6, default method / window / memory / strategy.
let mut strm = ZStream::new();
DeflateConfig::new()
    .level(6)
    .init(&mut strm)
    .expect("deflateInit failed");

// Attach input/output buffers to `strm`, then pump until `FlushMode::Finish`
// reports the stream is complete:
//
//     loop {
//         let outcome = deflate(&mut strm, FlushMode::Finish)?;
//         // drain produced output, refill input …
//         if outcome.stream_end() { break; }
//     }

deflate_end(&mut strm).ok();
```

The inflate side mirrors this with `inflate_init` / `inflate` / `inflate_end`,
and `inflate_init2` accepts a `windowBits` value to select zlib, raw, gzip, or
auto-detect framing. All seven flush modes (`Z_NO_FLUSH` … `Z_TREES`) are
honored.

### Checksums

Adler-32 and CRC-32 follow the zlib seeding convention: start Adler-32 from `1`
and CRC-32 from `0`, then fold in successive slices.

```rust,ignore
use zlib_rs::{adler32, crc32};

let data = b"the quick brown fox";

// Running Adler-32 (seed 1) and CRC-32 (seed 0).
let a = adler32(1, data);
let c = crc32(0, data);

// Chunked updates produce the same result as a single call.
let (head, tail) = data.split_at(9);
let a_chunked = adler32(adler32(1, head), tail);
assert_eq!(a, a_chunked);
let _ = c;
```

Use `adler32_combine` / `crc32_combine` to merge the checksums of two
concatenated segments without rescanning the data.

### gzip file I/O

The gzip layer offers a `.gz`-aware file API layered over `std::fs` and
`std::io`, closely following zlib's `gz*` functions. It is available with the
default `gz-io` feature.

```rust,ignore
use zlib_rs::gz::{gzopen, gzwrite, gzclose};

// Write a gzip-compressed file, then close to flush the trailer.
let file = gzopen("hello.txt.gz", "wb").expect("gzopen failed");
gzwrite(file, b"hello, gzip\n").expect("gzwrite failed");
gzclose(file).expect("gzclose failed");
```

## Usage — C drop-in (FFI)

For existing C/C++ consumers, `zlib-rs` emits `cdylib` and `staticlib`
artifacts that expose the **exact zlib C API**. The FFI layer in `src/ffi/`
provides `#[no_mangle] extern "C"` shims for every public zlib prototype and
`#[repr(C)]` mirror structs for `z_stream` and `gz_header`, reproducing the C
field order and type widths so the emitted object can substitute for `libz`
without recompiling downstream code.

> **Variadic `gzprintf`/`gzvprintf`.** Both symbols are **always exported** so
> the artifact presents the complete zlib symbol table. Genuine C-variadic
> formatting depends on the unstable `c_variadic` language feature (nightly
> only), which would break the crate's stable build and its MSRV contract
> (AAP §0.7.2) and its zero-C-dependency guarantee (AAP §0.5.2). The FFI layer
> therefore ships the **documented no-`vsnprintf` zlib build variant**: the two
> shims return `Z_STREAM_ERROR` rather than formatting — exactly as a zlib built
> without a secure `*printf` does — and `zlibCompileFlags` reports this by
> setting bit 27. Every other public prototype is fully implemented, and the
> idiomatic Rust `gzprintf` (which takes `core::fmt::Arguments` instead of a
> C `va_list`) formats fully.

Build the shared and static objects:

```sh
cargo build --release
# target/release/libzlib_rs.so   (cdylib — the drop-in libz replacement)
# target/release/libzlib_rs.a    (staticlib — for static linking)
```

Because the exported symbol table matches zlib's, the shared object can be
injected in place of the system library — for example via `LD_PRELOAD` — so a
C program that calls `deflate`, `inflate`, `crc32`, `gzopen`, and friends runs
against the Rust implementation unchanged:

```sh
LD_PRELOAD=./target/release/libzlib_rs.so ./your_existing_c_program
```

Return codes (`Z_OK = 0` … `Z_VERSION_ERROR = -6`), buffer bounds, and streaming
semantics are surfaced at the boundary exactly as C callers expect, even though
the internal implementation uses Rust `Result` types.

## Feature flags

Feature flags map the C preprocessor conditionals (`GZIP`, `NO_GZCOMPRESS`,
`Z_SOLO`, …) onto Cargo features. The names and defaults below are the
authoritative contract and are kept in sync with `Cargo.toml`.

| Feature          | Default | Description |
|------------------|:-------:|-------------|
| `std`            | ✅ | Standard-library build (I/O, allocation, formatting). |
| `gzip`           | ✅ | gzip framing within the deflate/inflate engines (maps C `#ifdef GZIP`). |
| `gz-io`          | ✅ | gzip **file** I/O layer — the `gz*` functions (maps C `#ifndef NO_GZCOMPRESS`); implies `std` + `gzip`. |
| `no-std`         |    | Core-only, bare-metal build with no gz file I/O (maps C `Z_SOLO`). |
| `simd`           | ✅ | SIMD-accelerated CRC-32 via [`crc32fast`](https://crates.io/crates/crc32fast). |
| `inflate_strict` |    | Stricter inflate distance validation (maps the C `INFLATE_STRICT` switch). Off by default so the default build stays byte-exact with reference zlib; enable only to reject out-of-window distances early. |

The default feature set is `std`, `gzip`, `gz-io`, and `simd`. The non-default
feature `inflate_strict` is an optional opt-in beyond that core set; it is not
required for a complete drop-in ABI. To build a core-only, no-standard-library
configuration, disable the defaults:

```sh
cargo build --no-default-features
```

To pick individual features explicitly, combine `--no-default-features` with
`--features`:

```sh
cargo build --no-default-features --features "std,gzip,simd"
```

## Building, testing, and benchmarking

The crate builds with a stock Cargo toolchain — **no CMake, `./configure`, or
`make` required**.

```sh
# Build (debug / optimized)
cargo build
cargo build --release

# Run the full test suite (unit, integration, and doc tests)
cargo test

# Lint with Clippy, treating warnings as errors
cargo clippy --all-targets -- -D warnings

# Verify formatting
cargo fmt -- --check

# Run the Criterion benchmarks
cargo bench
```

Offline builds work from a warmed Cargo cache. The development dependencies
([`criterion`](https://crates.io/crates/criterion),
[`flate2`](https://crates.io/crates/flate2),
[`quickcheck`](https://crates.io/crates/quickcheck), and
[`rand`](https://crates.io/crates/rand)) are used only for benchmarking and
compatibility testing; `flate2` uses its pure-Rust `miniz_oxide` backend so the
test graph needs no C toolchain either.

## Project layout

The crate mirrors the six functional layers of the C baseline as Rust modules,
adds a public-API-types layer, and isolates the C ABI in a dedicated FFI
boundary:

```text
src/
├── lib.rs            crate root, module declarations, public re-exports
├── error.rs          ZlibError enum + ReturnCode
├── constants.rs      flush modes, levels, strategies, windowBits contract
├── stream.rs         idiomatic ZStream<A: Allocator>
├── gz_header.rs      GzHeader
├── deflate/          compression engine (zero unsafe): mod, state, trees,
│                     strategy, fast, slow, stored, huff, rle
├── inflate/          decompression engine: mod, state, fast, tables, fixed, back
├── checksum/         adler32, crc32 (SIMD hot path), combine ops
├── gz/               gzip file I/O: mod, state, open, read, write, close
├── util/             one-call wrappers, version reporting
└── ffi/              extern "C" drop-in boundary: mod, types, deflate,
                      inflate, gz, util
```

## Compatibility and RFCs

The data formats implemented by this crate are described by the following
Requests for Comments, whose text is also vendored under `doc/` for reference:

- **[RFC 1950](https://datatracker.ietf.org/doc/html/rfc1950)** — ZLIB Compressed
  Data Format (`doc/rfc1950.txt`).
- **[RFC 1951](https://datatracker.ietf.org/doc/html/rfc1951)** — DEFLATE
  Compressed Data Format (`doc/rfc1951.txt`).
- **[RFC 1952](https://datatracker.ietf.org/doc/html/rfc1952)** — GZIP File
  Format (`doc/rfc1952.txt`).

Byte-identical output against reference zlib is validated **by default** by an
interoperability test suite (`tests/interop.rs`) that checks zlib-rs output
against 300 vectors baked from genuine C zlib 1.3.2.1-motley, decodes
reference-produced streams, and round-trips arbitrary inputs — all without a C
toolchain. The zlib home page — with the canonical specifications and FAQ — is
<https://zlib.net/>.

## Memory safety and design

Every C idiom is mapped to a safe-Rust equivalent: integer-tagged state machines
become exhaustively-matched `enum`s, function-pointer dispatch tables become enum
strategy dispatch, opaque `internal_state *` pointers become owned `Box`ed state,
and manual `zcalloc`/`zcfree` allocation becomes ownership plus `Drop`. This
removes entire classes of defects — use-after-free, double-free, buffer overruns,
and unhandled state transitions — without changing a single output byte.

The design deliberately keeps the compression core free of `unsafe` and pushes
the unavoidable raw-pointer work (opaque handle round-tripping, slice
construction from C pointers, C-string handling) to the `src/ffi/` boundary,
where it is auditable and each block is annotated with a `// SAFETY:` comment.

## Roadmap

Parity work is complete for the core codecs and validated by default. The gates
that were previously tracked as open items are now closed:

- **Byte-identity (done).** Validated by default in `tests/interop.rs` against
  300 vectors from genuine C zlib 1.3.2.1-motley — no opt-in feature or C
  toolchain required.
- **`no_std` test coverage (done).** The full test suite compiles and passes
  under `cargo test --no-default-features` (and `--features no-std`), and CI
  runs it as a blocking gate.
- **Fuzzing (done).** A detached `cargo-fuzz` crate under `fuzz/` ships targets
  for inflate, deflate round-trip, gzip parsing, checksums, and the FFI
  boundary; `.github/workflows/fuzz.yml` builds every target and runs each for a
  bounded budget.

Remaining, workload-dependent work:

- **Performance.** Measured on a single Linux host against C zlib 1.3.2.1-motley
  (8 MiB inputs, best-of-5 peak throughput, release build), with byte-identical
  output confirmed in every case:
  - Compression ≈ 77–90% of C (≈ 80% and above at levels 6–9 on compressible
    data; ≈ 77% at level 1 / incompressible input).
  - Decompression ≈ 78–115% of C (matching or exceeding C on incompressible and
    semi-structured data; ≈ 78% on highly compressible text, where C's
    `inffast` has an edge).
  - CRC-32 ≈ 1.6× C throughput via the SIMD-accelerated `crc32fast` hot path.

  These figures vary by workload and hardware; closing the remaining
  compression gap toward the ≥ 80% goal across *all* inputs is ongoing.

## Contributing

Issues and pull requests are welcome. Before submitting, please ensure
`cargo test`, `cargo clippy --all-targets -- -D warnings`, and
`cargo fmt -- --check` all pass. New `unsafe` outside the FFI boundary is not
accepted; any `unsafe` that is genuinely required must carry a `// SAFETY:`
justification.

## License

`zlib-rs` is distributed under the **Zlib** license — the same permissive license
as upstream zlib. See the [`LICENSE`](LICENSE) file for the full text.

```text
(C) 1995-2026 Jean-loup Gailly and Mark Adler

This software is provided 'as-is', without any express or implied warranty. In
no event will the authors be held liable for any damages arising from the use of
this software.

Permission is granted to anyone to use this software for any purpose, including
commercial applications, and to alter it and redistribute it freely, subject to
the following restrictions:

1. The origin of this software must not be misrepresented; you must not claim
   that you wrote the original software. If you use this software in a product,
   an acknowledgment in the product documentation would be appreciated but is
   not required.
2. Altered source versions must be plainly marked as such, and must not be
   misrepresented as being the original software.
3. This notice may not be removed or altered from any source distribution.
```

## Acknowledgments

The DEFLATE format used by zlib was defined by **Phil Katz**. The DEFLATE and
zlib specifications were written by **L. Peter Deutsch**. The original zlib
library was written by **Jean-loup Gailly** (compression) and **Mark Adler**
(decompression); this crate is a Rust reimplementation of their work and would
not exist without it. Thanks also to the many contributors who reported problems
and suggested improvements to zlib over the decades.
