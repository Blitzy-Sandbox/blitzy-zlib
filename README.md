# zlib-rs

**A memory-safe Rust rewrite of zlib 1.3.2.1 — byte-compatible with C zlib and exposing a drop-in C ABI.**

![crate version](https://img.shields.io/badge/version-1.3.2-5B39F3)
![edition](https://img.shields.io/badge/edition-2024-orange)
![MSRV](https://img.shields.io/badge/MSRV-1.85.0-blue)
![license](https://img.shields.io/badge/license-Zlib-green)
![safety](https://img.shields.io/badge/unsafe-%3C2%25%20by%20line-brightgreen)

`zlib-rs` is a general-purpose, lossless data-compression library. It is a
behavior-preserving port of [zlib](https://zlib.net/) `1.3.2.1` from C to safe
Rust: the compression algorithms, wire formats, and public contract are frozen,
while manual memory management (`ZALLOC`/`ZFREE`, raw pointers, `goto`-driven
state machines) is replaced with Rust ownership, borrowed slices, and exhaustive
`enum` state machines. All code is thread-safe (see the upstream
[zlib FAQ](https://zlib.net/zlib_faq.html) for caveats). The data formats are
defined by the IETF RFCs:

- **RFC 1950** — the zlib wrapper format
  ([rfc1950](https://datatracker.ietf.org/doc/html/rfc1950))
- **RFC 1951** — the raw DEFLATE compressed-data format
  ([rfc1951](https://datatracker.ietf.org/doc/html/rfc1951))
- **RFC 1952** — the gzip file format
  ([rfc1952](https://datatracker.ietf.org/doc/html/rfc1952))

The crate tracks upstream zlib `1.3.2.1-motley` (version number `0x1321`).
For the **same input, compression level, strategy, and window configuration**,
`zlib-rs` produces **byte-identical** compressed output to canonical C zlib, and
it can inflate any stream that C zlib can produce. This makes it safe to mix and
match `zlib-rs` and C zlib on either side of a compressed stream.

> **Crate name vs. import path:** the published crate is named `zlib-rs`
> (with a hyphen), but the Rust import path uses an underscore — write
> `use zlib_rs::...;` in your code.

> **Project status:** this crate is feature-complete. It ships the foundation
> types, the Adler-32 / CRC-32 checksum engine, the complete INFLATE
> (decompression) and DEFLATE (compression) engines, the one-shot
> `compress` / `compress2` / `uncompress` helpers, the gzip **file** I/O layer,
> and the C-ABI FFI drop-in (`src/ffi.rs`, behind the non-default `capi`
> feature). The full Cargo + CI workflow is in place: `cargo build`, the
> `--no-default-features` / `no-std` library builds, `cargo build --features
> capi`, `cargo clippy`, and `cargo fmt`, together with the integration test
> suite (`tests/`) and the criterion benchmarks (`benches/`), all run and pass.

---

## Contents

- [Features](#features)
- [Installation](#installation)
- [Quick start](#quick-start)
- [Building from source](#building-from-source)
- [Using `zlib-rs` as a C `libz` drop-in](#using-zlib-rs-as-a-c-libz-drop-in)
- [Cargo feature flags](#cargo-feature-flags)
- [Testing, benchmarks & development workflow](#testing-benchmarks--development-workflow)
- [Compatibility & conformance](#compatibility--conformance)
- [Memory safety](#memory-safety)
- [License & credits](#license--credits)

---

## Features

- **Full DEFLATE engine** — compression and decompression conforming to
  RFC 1951, with output that is byte-identical to C zlib.
- **Three stream framings**, selected through the `windowBits` overloading
  convention:
  - `8..=15` — zlib wrapper (RFC 1950)
  - `-8..=-15` — raw DEFLATE, no wrapper (RFC 1951)
  - `24..=31` — gzip wrapper (RFC 1952)
  - `40..=47` — automatic header detection on inflate (zlib or gzip)
- **Ten compression levels** — `0` (`Z_NO_COMPRESSION`) through `9`
  (`Z_BEST_COMPRESSION`), plus `Z_DEFAULT_COMPRESSION` (`-1`, which resolves to
  level `6`).
- **Five strategies** — `Z_DEFAULT_STRATEGY`, `Z_FILTERED`, `Z_HUFFMAN_ONLY`,
  `Z_RLE`, and `Z_FIXED`.
- **Seven flush modes** — `Z_NO_FLUSH`, `Z_PARTIAL_FLUSH`, `Z_SYNC_FLUSH`,
  `Z_FULL_FLUSH`, `Z_FINISH`, `Z_BLOCK`, and `Z_TREES`.
- **Checksums** — Adler-32 (modulo 65521) and CRC-32 (IEEE reflected
  polynomial), including the resumable `*_combine` variants. The CRC-32 path is
  SIMD-accelerated via [`crc32fast`](https://crates.io/crates/crc32fast).
- **Preset dictionaries** and the `Z_NEED_DICT` handshake, plus `inflateSync`
  error recovery.
- **Gzip file I/O** — an `stdio`-like interface (`gzopen`/`gzread`/`gzwrite`/…)
  for reading and writing `.gz` files.
- **Idiomatic Rust API** — `Result`/`Option` returns instead of integer codes,
  and `std::io::{Read, Write}` integration for streaming.
- **C-compatible FFI** — `#[no_mangle] extern "C"` exports whose names and
  signatures match the zlib C API exactly, so the compiled `cdylib`/`staticlib`
  is a binary-level drop-in replacement for `libz`.
- **Memory-safe by construction** — over 98% safe Rust by line count, with
  `unsafe` confined to the FFI boundary and one performance-critical inflate
  inner loop; every `unsafe` block carries a `// SAFETY:` comment.
- **`no_std` capable** — the core compression, decompression, and checksum
  engines build without the standard library (mirroring zlib's `Z_SOLO` mode).

---

## Installation

Add the crate to your `Cargo.toml`:

```toml
[dependencies]
zlib-rs = "1.3.2"
```

To build the core compression engine without the standard library (no heap-backed
file I/O, mirroring zlib's `Z_SOLO` build), disable the default features and
enable `no-std`:

```toml
[dependencies]
zlib-rs = { version = "1.3.2", default-features = false, features = ["no-std"] }
```

The minimum supported Rust version is **1.85.0** (the crate uses edition 2024).

> **MSRV exception for the `capi` feature.** The optional, non-default `capi`
> C-ABI drop-in build requires **Rust ≥ 1.88**, because its variadic `gzprintf`
> export is implemented with a stable naked function (`#[unsafe(naked)]`,
> stabilized in 1.88). The core crate and all default builds and tests remain on
> MSRV 1.85.0; only `--features capi` raises the floor.

---

## Quick start

### One-shot compression and decompression

The simplest entry points compress or decompress an entire in-memory buffer in a
single call. `compress2` takes the data and a level (`0`–`9`, or `-1` for the
default), while the one-argument `compress` uses the default level. `uncompress`
decodes the stream into a caller-provided output buffer sized to the known
original length and returns the number of bytes written.

```rust
use zlib_rs::{compress2, uncompress};

fn main() {
    // One-call compression at level 6 (the default level). The one-argument
    // `compress(data)` is equivalent at the default level.
    let data = b"Hello, zlib-rs! This is a compression test.";
    let compressed = compress2(data, 6).expect("compression failed");

    // One-call decompression into a caller-provided buffer sized to the known
    // original length; `uncompress` returns the number of bytes written.
    let mut decompressed = vec![0u8; data.len()];
    let n = uncompress(&mut decompressed, &compressed).expect("decompression failed");

    assert_eq!(data.as_slice(), &decompressed[..n]);
}
```

### Checksums

Adler-32 and CRC-32 follow the C zlib calling convention: seed Adler-32 with `1`
and CRC-32 with `0`, then thread the running value through subsequent calls to
checksum data incrementally.

```rust
use zlib_rs::{adler32, crc32};

fn main() {
    let data = b"The quick brown fox jumps over the lazy dog";

    // Adler-32 is seeded with 1; CRC-32 (IEEE) is seeded with 0.
    let adler = adler32(1, data);
    let crc = crc32(0, data);
    assert_ne!(adler, 0);
    assert_ne!(crc, 0);

    // Both checksums are resumable: feeding the data in two chunks and threading
    // the running value yields the same result as a single call.
    let mid = data.len() / 2;
    let adler_streamed = adler32(adler32(1, &data[..mid]), &data[mid..]);
    assert_eq!(adler, adler_streamed);
}
```

### Streaming with `Read` / `Write`

For data that does not fit in memory, the gzip layer (enabled by the default
`gz-io` feature) integrates with the standard `std::io` traits. The `gzopen`
family returns a `GzFile` handle (an owned `Box<GzState>`) that mirrors C zlib's
`gzFile` and finalizes the stream deterministically on `Drop` (replacing the
explicit `gzclose` call). A read handle implements `Read`, `BufRead`, and
`Seek`; for writing, wrap the handle in a `GzWriter`, which implements `Write`:

```no_run
use std::io::{Read, Write};
use std::path::Path;
use zlib_rs::gz::{gzopen, GzFile, GzWriter};

// Compress while writing: open for gzip writing and wrap the handle in a
// `GzWriter` so any data written is deflated into a gzip member.
let handle = gzopen(Path::new("example.txt.gz"), "wb").expect("open for gzip writing");
let mut out = GzWriter::from(handle);
out.write_all(b"streamed payload that is compressed on the fly ...").unwrap();
out.finish(); // finalize the gzip stream (or just drop `out`)

// Decompress while reading: `gzopen` returns an `Option<GzFile>`; the handle
// inflates on demand through its `Read` implementation.
let mut input: GzFile = gzopen(Path::new("example.txt.gz"), "rb").expect("open for gzip reading");
let mut text = String::new();
input.read_to_string(&mut text).unwrap();
```

---

## Building from source

`zlib-rs` is a standard Cargo crate — there is no `./configure`, `make`, or
CMake step. Build it with:

```sh
# Debug build
cargo build

# Optimized release build (LTO + single codegen unit, used by the perf gates)
cargo build --release
```

The `[lib] crate-type = ["lib", "cdylib", "staticlib"]` declaration means a
release build produces three artifacts:

- the Rust `rlib` (for Rust consumers via `use zlib_rs::...`),
- a C-linkable shared object (`libzlib_rs.so` / `.dylib` / `.dll`), and
- a C-linkable static archive (`libzlib_rs.a` / `.lib`).

A build script, `build.rs`, runs automatically and performs two jobs:

1. it regenerates the CRC-32 lookup tables at build time (mirroring the way the
   upstream `crc32.h` tables are generated), so no large table file is checked
   in; and
2. once the C-ABI shim (`src/ffi.rs`) is present, it invokes
   [`cbindgen`](https://crates.io/crates/cbindgen) to emit a C header
   (`include/zlib-rs.h`, guarded by `ZLIB_RS_H`) describing the exported C ABI.
   Until that module exists this step is skipped automatically (it emits a
   build warning and continues), so the library build is unaffected.

---

## Using `zlib-rs` as a C `libz` drop-in

Because the FFI shim exports `#[no_mangle] pub extern "C"` functions named
identically to the zlib C API (`deflate`, `inflate`, `deflateInit_`,
`inflateInit_`, `crc32`, `adler32`, `compress2`, `uncompress`, the `gz*`
family, and the rest of the symbol set), the generated `cdylib`/`staticlib`
can replace the system `libz` at the binary level.

> **Status:** `cargo build --release` produces the `cdylib`/`staticlib` today,
> but the exported C symbols come from the FFI shim (`src/ffi.rs`), which is
> gated behind the non-default `capi` feature. The linkage below — and the
> generated `include/zlib-rs.h` header — therefore applies once the crate is
> built with that feature enabled (`cargo build --release --features capi`).

```sh
cargo build --release --features capi
# Link a C program against the generated artifacts and header:
cc my_program.c -L target/release -I include -lzlib_rs -o my_program
```

The generated `include/zlib-rs.h` is the regenerated equivalent of the upstream
hand-written `zlib.h`; it operates over `#[repr(C)]` `z_stream` / `gz_header`
structures and the `Z_*` integer return codes, so existing C code that targets
zlib compiles against it unchanged. The `zalloc`/`zfree`/`opaque` allocator
hooks are preserved, so C callers that supply custom allocators continue to
work.

---

## Cargo feature flags

Cargo features replace zlib's C preprocessor conditionals (`GZIP`,
`NO_GZCOMPRESS`, `Z_SOLO`) and its SIMD selection. They take the place of the
old CMake build options.

| Feature  | Default | Replaces (C)              | Behavior |
|----------|:-------:|---------------------------|----------|
| `std`    | yes     | the default C build       | Heap allocation and `std::io` integration. |
| `gzip`   | yes     | `#ifdef GZIP`             | gzip wrapper-format support in deflate/inflate (RFC 1952). |
| `gz-io`  | yes     | `#ifndef NO_GZCOMPRESS`   | gzip **file** I/O (`gzopen`/`gzread`/`gzwrite`/…). Implies `std` + `gzip`. |
| `no-std` | no      | `Z_SOLO`                  | Bare-metal core (compress/decompress/checksums only). **Mutually exclusive with `std`.** |
| `simd`   | yes     | hardware CRC paths        | SIMD-accelerated CRC-32 via the optional `crc32fast` dependency. |
| `capi`   | no      | the C-ABI drop-in build   | Exports the `#[no_mangle] extern "C"` zlib-API symbols (`src/ffi.rs`) for the `cdylib`/`staticlib` and triggers the `cbindgen` C-header generation. |

Build the bare-metal core with no standard library:

```sh
cargo build --no-default-features --features no-std
```

Because Cargo features are additive, `std` and `no-std` cannot be hard-enforced
as mutually exclusive by Cargo itself; the crate carries a `compile_error!`
guard that fires if both are enabled simultaneously.

---

## Testing, benchmarks & development workflow

### Tests

Run the in-crate unit tests and documentation tests:

```sh
cargo test          # runs the in-crate unit tests + doc tests
```

The integration suite (in `tests/*.rs`) ports the C test programs and adds
property-based and oracle tests. Each target runs via:

```sh
cargo test --test regression         # ported from test/example.c
cargo test --test inflate_coverage   # ported from test/infcover.c
cargo test --test round_trip         # quickcheck property round-trips
cargo test --test interop            # byte-identical cross-check vs C zlib
cargo test --test gzip_compat        # gzip file-format compatibility
cargo test --test checksum           # Adler-32 / CRC-32 known-answer tests
```

> **Dev-only oracle:** `tests/interop.rs` uses
> [`flate2`](https://crates.io/crates/flate2) with its `zlib` backend (which
> links canonical C zlib) purely as a reference oracle to verify byte-for-byte
> output equality. `flate2` is a **dev-dependency only** — it is never linked
> into the shipped library, which carries **zero C dependency**.

> **`--no-default-features` builds and tests:** `cargo test
> --no-default-features` both compiles and passes. A bare
> `--no-default-features` build (neither `std` nor `no-std` selected) is a
> reduced but still **`std`-linked** profile, so the libtest harness links
> normally; the genuine bare-metal artifact is built with
> `--no-default-features --features no-std`. The **library itself** compiles
> cleanly under every feature configuration.

### Benchmarks

Three [`criterion`](https://crates.io/crates/criterion) benchmark harnesses
(in `benches/*.rs`) validate the performance gates. They run via:

```sh
cargo bench                          # run all benchmarks
cargo bench --bench deflate_bench    # compression throughput
cargo bench --bench inflate_bench    # decompression throughput
cargo bench --bench checksum_bench   # Adler-32 / CRC-32 throughput
```

They validate the project's performance gates relative to C zlib:

| Operation                | Target relative to C zlib |
|--------------------------|---------------------------|
| Compression throughput   | ≥ 80% of C zlib           |
| Decompression throughput | ≥ C zlib (parity or better) |
| CRC-32 (SIMD path)       | ≥ 3× the scalar implementation |
| Memory footprint         | ≤ C zlib                  |

### Linting & formatting

```sh
cargo clippy --all-targets -- -D warnings   # zero-warning lint gate
cargo fmt -- --check                        # formatting gate
```

---

## Compatibility & conformance

`zlib-rs` is a behavior-preserving rewrite, so it honors zlib's compatibility
promises exactly:

- **Byte-identical compressed output** to C zlib for the same input, level,
  strategy, and `windowBits` — identical LZ77 match decisions, Huffman codes,
  and block boundaries.
- **RFC conformance** to RFC 1950 (zlib), RFC 1951 (DEFLATE), and RFC 1952
  (gzip).
- **Bidirectional stream compatibility** — it inflates any stream C zlib can
  produce, across all window sizes, preset dictionaries (`Z_NEED_DICT`), and
  `inflateSync` recovery.
- **`compressBound`** uses the same formula as upstream:
  `sourceLen + (sourceLen >> 12) + (sourceLen >> 14) + (sourceLen >> 25) + 13`.
- **Memory bounds** match upstream: roughly 256 KB per deflate stream at the
  default `windowBits = 15` / `memLevel = 8`, and about 7 KB of inflate state
  plus a window of at most 32 KB.

---

## Memory safety

The LZ77/Huffman compression and decompression core is **100% safe Rust**.
`unsafe` is confined to exactly two zones, both small and audited:

- the C-ABI shim (`src/ffi.rs`), which dereferences raw `*mut z_stream`
  pointers and marshals C strings; and
- the `inflate_fast` inner loop, where bounded window/output copies elide
  bounds checks for performance.

The result is over 98% safe Rust by line count, with a `// SAFETY:` comment on
every `unsafe` block. Buffers are owned `Vec<u8>` / `Box<[_]>` values and stream
state lives in `Option<Box<…>>`, so teardown is handled by `Drop` — double-free
and use-after-free are unrepresentable in the safe core.

---

## License & credits

`zlib-rs` is distributed under the **Zlib** license, the same permissive license
as upstream zlib. See the [`LICENSE`](LICENSE) file for the full text.

The original zlib library was written by **Jean-loup Gailly** and **Mark
Adler**; the DEFLATE format was defined by Phil Katz, and the zlib and DEFLATE
specifications were written by L. Peter Deutsch. This crate is a Rust port that
preserves their algorithms and on-the-wire formats verbatim.

### Further reading

- zlib home page — <https://zlib.net/>
- zlib FAQ — <https://zlib.net/zlib_faq.html>
- RFC 1950 (zlib) — <https://datatracker.ietf.org/doc/html/rfc1950>
- RFC 1951 (DEFLATE) — <https://datatracker.ietf.org/doc/html/rfc1951>
- RFC 1952 (gzip) — <https://datatracker.ietf.org/doc/html/rfc1952>

