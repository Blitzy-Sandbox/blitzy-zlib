# zlib-rs

**A memory-safe Rust rewrite of zlib 1.3.2.1 — byte-compatible with C zlib and exposing a drop-in C ABI.**

[![License: Zlib](https://img.shields.io/badge/license-Zlib-blue.svg)](./LICENSE)
[![Rust edition](https://img.shields.io/badge/edition-2024-orange.svg)](https://doc.rust-lang.org/edition-guide/rust-2024/index.html)
[![MSRV](https://img.shields.io/badge/rustc-1.85%2B-blue.svg)](#minimum-supported-rust-version)

`zlib-rs` is a general-purpose, lossless data-compression library. It is a complete,
idiomatic Rust port of the canonical [zlib](https://zlib.net/) C library (upstream
`1.3.2.1-motley`) that replaces every byte of manual memory management with Rust
ownership semantics — eliminating buffer overflows, use-after-free, and double-free by
construction — without changing the wire format or the public contract.

The data formats produced and consumed by the library are defined by the IETF RFCs:
[RFC 1950](https://datatracker.ietf.org/doc/html/rfc1950) (zlib format),
[RFC 1951](https://datatracker.ietf.org/doc/html/rfc1951) (DEFLATE format), and
[RFC 1952](https://datatracker.ietf.org/doc/html/rfc1952) (gzip format). For the same
input, compression level, strategy, and window configuration, `zlib-rs` produces
**byte-identical** compressed output to C zlib, and it decodes any stream that canonical
zlib can produce. Two surfaces are offered side by side:

- an **idiomatic Rust API** that returns `Result`/`Option` and integrates with
  `std::io::{Read, Write}`; and
- a thin, `#[no_mangle] extern "C"` **FFI shim** whose exported symbol names and
  signatures match the zlib C API exactly, so the compiled `cdylib`/`staticlib` can be
  used as a binary-level drop-in replacement for `libz`.

> The crate is published as `zlib-rs` and imported in Rust as `zlib_rs` (Cargo maps the
> hyphenated package name to the underscored crate path).

## What's included

`zlib-rs` is a complete library. The full feature set is delivered and exercised by the
test, doctest, and benchmark suites:

- **Compression and decompression** — the full DEFLATE encoder (stored / fast / slow /
  Huffman-only / RLE drivers selected per level) and the full INFLATE decoder, for the
  zlib, raw, gzip, and auto-detect framings, driven through the idiomatic
  [`Deflate`](https://docs.rs/zlib-rs) and `Inflate` types — including preset dictionaries,
  `inflateSync` recovery, and the gzip header sink.
- **One-shot helpers** — `compress` / `compress2` / `compress_bound` / `uncompress` /
  `uncompress2`, the whole-buffer convenience layer, exposed at the crate root.
- **Checksums** — Adler-32 and CRC-32 with the `*_z` and `*_combine` / `*_combine64`
  variants and SIMD-accelerated CRC-32 (`zlib_rs::{adler32, crc32, …}`).
- **gzip file I/O** — the C-faithful `gzopen`/`gzread`/`gzwrite`/`gzclose` family plus the
  idiomatic `GzReader` (`impl Read` + `BufRead` + `Seek`) and `GzWriter` (`impl Write`)
  adapters.
- **C-ABI drop-in** — the `#[no_mangle] extern "C"` FFI shim in `src/ffi.rs`, gated by the
  `capi` feature, which exports the canonical zlib symbols and (with cbindgen) a matching
  C header so the `cdylib`/`staticlib` can replace `libz` at the binary level.
- **Foundation types** — all `Z_*` constants, the `FlushMode` / `Strategy` / `DataType`
  enums, `ZlibError` / `ReturnCode`, `ZStream` / `Allocator`, `GzHeader`, and the version
  helpers (`zlib_rs::util::*`).
- **Tests and benchmarks** — the integration suites under `tests/` (regression,
  inflate-coverage, round-trip, interop, gzip-compat, checksum) and the Criterion
  benchmarks under `benches/` that enforce the throughput gates.

## Features

- **DEFLATE compression and decompression** with byte-for-byte parity to C zlib.
- **Three stream framings** selected by the `windowBits` overloading convention:
  `8..=15` (zlib/RFC 1950), `-8..=-15` (raw DEFLATE/RFC 1951), `24..=31` (gzip/RFC 1952),
  and `40..=47` (automatic zlib-or-gzip detection on inflate).
- **Ten compression levels** — `0` (`Z_NO_COMPRESSION`) through `9`
  (`Z_BEST_COMPRESSION`), plus `Z_DEFAULT_COMPRESSION` (`-1`, which resolves to level 6).
- **Five strategies** — `Z_DEFAULT_STRATEGY`, `Z_FILTERED`, `Z_HUFFMAN_ONLY`, `Z_RLE`,
  and `Z_FIXED`.
- **Seven flush modes** — `Z_NO_FLUSH`, `Z_PARTIAL_FLUSH`, `Z_SYNC_FLUSH`,
  `Z_FULL_FLUSH`, `Z_FINISH`, `Z_BLOCK`, and `Z_TREES`.
- **Checksums** — Adler-32 (modulo 65521) and CRC-32 (IEEE reflected polynomial),
  including the `*_combine` variants; CRC-32 is SIMD-accelerated via `crc32fast`.
- **Preset dictionaries** and the `Z_NEED_DICT` handshake, plus `inflateSync` error
  recovery.
- **gzip file I/O** — a stdio-like interface (`gzopen`/`gzread`/`gzwrite`/`gzclose`)
  with `std::io::{Read, Write, BufRead, Seek}` integration via `GzReader` / `GzWriter`.
- **C-compatible FFI drop-in** for `libz` — exact symbol names, `#[repr(C)]` `z_stream`
  layout, and integer return codes, with a generated C header (the `capi` feature).
- **Memory-safe by construction** — the entire LZ77/Huffman core is safe Rust (>98% safe
  by line); `unsafe` is confined to the FFI boundary and the bounded inflate fast loop.
- **`no_std` capable** — compression, decompression, and checksums work without `std`
  (the `Z_SOLO` analogue) via the `no-std` feature.

## Installation

Add the crate to your `Cargo.toml`:

```toml
[dependencies]
zlib-rs = "1.3.2"
```

For a bare-metal / `no_std` build, disable the default features and select `no-std`
(see [Cargo features](#cargo-features) for the `rlib`-only build invocation):

```toml
[dependencies]
zlib-rs = { version = "1.3.2", default-features = false, features = ["no-std"] }
```

## Usage

### One-shot compression and decompression

The one-shot helpers cover the common case where the whole input is available in memory.
`compress` uses the default level; `compress2` takes an explicit level (`0..=9`, or `-1`
for the default). `uncompress` writes into a caller-sized destination buffer and returns
the number of bytes produced.

```rust
use zlib_rs::{compress, compress2, uncompress};

let data = b"Hello, zlib-rs! This is a compression test.";

// One-call compression at the default level, and again at an explicit level 6.
let compressed = compress(data).expect("compression failed");
let compressed_l6 = compress2(data, 6).expect("compression failed");

// One-call decompression into a caller-provided buffer (here sized to the known
// original length); `uncompress` returns the number of bytes written.
let mut restored = vec![0u8; data.len()];
let written = uncompress(&mut restored, &compressed).expect("decompression failed");

assert_eq!(&restored[..written], &data[..]);
assert_eq!(compressed_l6.is_empty(), false);
```

### Decompression with the streaming `Inflate` engine

The INFLATE engine is also available through the idiomatic `Inflate` decompressor. It
decodes the zlib, raw-DEFLATE, gzip, and auto-detect framings (selected via
`Inflate::with_window_bits`); `Inflate::new` selects the zlib format with the default
window. The example below decompresses a zlib stream produced by canonical C zlib:

```rust,no_run
use zlib_rs::{Inflate, Z_FINISH, Z_STREAM_END};

// A zlib (RFC 1950) stream emitted by C zlib at level 6 for the 39-byte message below.
let stream: [u8; 44] = [
    0x78, 0x9c, 0xab, 0xca, 0xc9, 0x4c, 0xd2, 0x2d, 0x2a, 0x56, 0x48, 0x49,
    0x4d, 0xce, 0xcf, 0x2d, 0x28, 0x4a, 0x2d, 0x2e, 0x4e, 0x2d, 0x56, 0x28,
    0x4a, 0x4d, 0xcc, 0x51, 0xa8, 0x02, 0xca, 0x28, 0x14, 0x97, 0x00, 0xd9,
    0xb9, 0xc5, 0x7a, 0x00, 0x2b, 0x67, 0x0e, 0xd3,
];

let mut inflate = Inflate::new().expect("inflate init");
let mut out = [0u8; 64];
let (code, _consumed, produced) = inflate.inflate(&stream, &mut out, Z_FINISH);

assert_eq!(code, Z_STREAM_END);
assert_eq!(&out[..produced], b"zlib-rs decompresses real zlib streams.");
```

### Checksums

Adler-32 and CRC-32 are exposed directly. As in C zlib, the running Adler-32 value is
seeded with `1` and the running CRC-32 value is seeded with `0`; passing the previous
result back in lets you checksum a stream incrementally.

```rust
use zlib_rs::{adler32, crc32};

// Adler-32 (used by the zlib/RFC 1950 wrapper) — seeded with 1.
let a = adler32(1, b"123456789");
assert_eq!(a, 0x091e_01de);

// CRC-32 (IEEE; used by the gzip/RFC 1952 wrapper) — seeded with 0.
let c = crc32(0, b"123456789");
assert_eq!(c, 0xcbf4_3926);
```

### Streaming gzip files with `Read` / `Write`

The gzip file-I/O layer (`zlib_rs::gz`, the `gz-io` feature) provides the idiomatic
`GzWriter` and `GzReader` adapters, which implement `std::io::Write` and
`std::io::Read` so gzip files compose with the rest of the I/O ecosystem. Each adapter
borrows an open `gzopen` handle; the handle is finalized with `gzclose` once the adapter
is dropped.

```rust,no_run
use std::io::{Read, Write};
use zlib_rs::{gzopen, gzclose, GzReader, GzWriter, Z_OK};

// Write a gzip-compressed file via the idiomatic `Write` adapter.
let mut handle = gzopen(std::path::Path::new("greeting.txt.gz"), "wb")
    .expect("gzopen for writing");
{
    let mut writer = GzWriter::new(&mut handle);
    writer.write_all(b"streamed through zlib-rs").expect("write");
    writer.flush().expect("flush");
} // `writer` is dropped here, releasing its borrow on `handle`.
assert_eq!(gzclose(Some(handle)), Z_OK);

// Read it back via the idiomatic `Read` adapter.
let mut handle = gzopen(std::path::Path::new("greeting.txt.gz"), "rb")
    .expect("gzopen for reading");
let mut text = String::new();
{
    let mut reader = GzReader::new(&mut handle);
    reader.read_to_string(&mut text).expect("read");
}
assert_eq!(gzclose(Some(handle)), Z_OK);
assert_eq!(text, "streamed through zlib-rs");
```

## Compatibility and guarantees

`zlib-rs` is a behavior-preserving rewrite: the externally observable byte stream and the
C ABI are invariant. Specifically, it preserves:

- **Byte-identical compressed output** to C zlib for the same input, level, strategy, and
  `windowBits` — the LZ77 match decisions, Huffman code construction, and block
  boundaries all match the upstream `1.3.2.1` configuration tables.
- **RFC conformance** to RFC 1950 (zlib), RFC 1951 (DEFLATE), and RFC 1952 (gzip).
- **`windowBits` overloading** — `8..=15` (zlib), `-8..=-15` (raw), `24..=31` (gzip),
  and `40..=47` (auto-detect on inflate).
- **All ten levels and five strategies**, and **all seven flush modes**.
- **Preset dictionaries** / `Z_NEED_DICT` and `inflateSync` recovery, so streams round-trip
  with canonical zlib in both directions.
- The **`compressBound` formula**:
  `sourceLen + (sourceLen >> 12) + (sourceLen >> 14) + (sourceLen >> 25) + 13`.
- **Memory bounds** comparable to C zlib — roughly 256 KiB per deflate stream at the
  default `windowBits = 15` / `memLevel = 8`, and about 7 KiB of inflate state plus a
  window of at most 32 KiB.

## Building

```sh
# Debug build.
cargo build

# Optimized release build (LTO enabled).
cargo build --release
```

### Library artifacts

The crate declares `crate-type = ["lib", "cdylib", "staticlib"]`, so a single build
produces all three artifacts:

| Artifact    | Output (Linux)       | Consumer                                   |
|-------------|----------------------|--------------------------------------------|
| `lib`       | `libzlib_rs.rlib`    | Rust crates depending on `zlib_rs`          |
| `cdylib`    | `libzlib_rs.so`      | C/C++ programs linking dynamically          |
| `staticlib` | `libzlib_rs.a`       | C/C++ programs linking statically           |

(On macOS the `cdylib` is `libzlib_rs.dylib`; on Windows it is `zlib_rs.dll`.)

### Using zlib-rs as a C drop-in for `libz`

The build script (`build.rs`) **regenerates the CRC-32 lookup tables** from the IEEE
polynomial on every build — mirroring the way upstream generates its `crc32.h`, instead
of vendoring a large pre-computed table.

The C drop-in is delivered as the `#[no_mangle] extern "C"` FFI shim in `src/ffi.rs`,
gated by the `capi` feature. Build the C-linkable artifacts with:

```sh
cargo build --release --features capi
```

When `capi` is enabled, the exported symbols (`deflate`, `inflate`, `crc32`, `adler32`,
`compress2`, `uncompress`, `gzopen`, `gzprintf`, `gzclose`, `zlibVersion`, …) match the
canonical zlib C API exactly — so a C program can link `libzlib_rs.so` / `libzlib_rs.a`
in place of `libz`. To also emit the matching C header, `build.rs` invokes
[cbindgen](https://github.com/mozilla/cbindgen) (configured by `cbindgen.toml`) when the
`ZLIB_RS_GENERATE_HEADER` environment variable is set, producing `zlib-rs.h` (include
guard `ZLIB_RS_H`). Because the header is generated from the `#[no_mangle] extern "C"`
functions and `#[repr(C)]` types in `src/ffi.rs`, the published C ABI can never silently
drift from the Rust source — it is the machine-checked equivalent of the hand-written
upstream `zlib.h`. With `capi` off (the default), header emission is skipped and the
crate builds as a pure-Rust library.

> The `capi` feature is **off** during `cargo test` because the canonical symbol names
> (`deflate`, `inflate`, …) would otherwise collide at link time with the C zlib that the
> `flate2` dev-dependency bundles for the interop oracle. The shipped pure-Rust library
> therefore carries **zero** C dependency; the only C in the `capi` bridge is this crate's
> own small variadic shim (`csrc/gzprintf.c`) for `gzprintf`/`gzvprintf`, which builds on
> the same Rust 1.85 MSRV plus a C compiler.

## Cargo features

These Cargo features replace the C preprocessor conditionals of the original build. The
default set (`std`, `gzip`, `gz-io`, `simd`) reproduces a standard C zlib build.

| Feature  | Default | Behavior                                                                          |
|----------|---------|-----------------------------------------------------------------------------------|
| `std`    | yes     | Heap allocation and `std::io` integration (the default C build).                  |
| `gzip`   | yes     | gzip wrapper-format support in deflate/inflate (the `GZIP` define).               |
| `gz-io`  | yes     | gzip `FILE`-style I/O layer; implies `std` + `gzip` (`NO_GZCOMPRESS`/`NO_GZIP`).   |
| `no-std` | no      | Bare-metal `Z_SOLO`-style build; **mutually exclusive** with `std`.               |
| `simd`   | yes     | SIMD-accelerated CRC-32 via the optional `crc32fast` dependency.                   |
| `capi`   | no      | Enables the `#[no_mangle] extern "C"` FFI exports (`src/ffi.rs`) and the cbindgen C-header emission in `build.rs`. Builds on the same Rust 1.85 MSRV. |

`no-std` and `std` are mutually exclusive; the crate enforces this with a `compile_error!`
guard. Because the `[lib]` target also emits `cdylib` / `staticlib` — final link artifacts
that require a global allocator and a `#[panic_handler]` which a `#![no_std]` library must
not impose — a plain `cargo build --no-default-features --features no-std` cannot link
them. The supported `no-std` (`Z_SOLO`) artifact is therefore the single Rust `rlib`,
built (and linted) by overriding the crate-type for that one invocation:

```sh
cargo rustc --lib --crate-type rlib --no-default-features --features no-std
```

This is exactly the invocation used by the CI `no-std` cells and the `Cargo.toml` `[lib]`
note.

## Testing

The full suite — unit tests, integration tests, and `///` documentation tests — runs with:

```sh
cargo test
```

The integration suites under `tests/` mirror the upstream C test programs and add
property-based and oracle-based checks:

```sh
cargo test --test regression         # ports of test/example.c
cargo test --test inflate_coverage   # ports of test/infcover.c
cargo test --test round_trip         # quickcheck property round-trips (zlib/raw/gzip)
cargo test --test interop            # byte-identical vs. C zlib oracle (flate2)
cargo test --test gzip_compat        # gzip file-format + GzReader/GzWriter compatibility
cargo test --test checksum           # Adler-32 / CRC-32 known answers
```

`tests/interop.rs` uses [`flate2`](https://crates.io/crates/flate2) — configured to link
**canonical C zlib** — as a reference oracle for byte-for-byte validation. `flate2` is a
**development-only** dependency: it is never linked into the shipped artifact, so the
released crate carries zero C dependency.

> **`no_std` testing note:** the test harness itself uses `std`, so the test suite runs
> under the default (hosted) configuration. The library compiles under
> `--no-default-features --features no-std` when built as an `rlib`
> (`cargo rustc --lib --crate-type rlib --no-default-features --features no-std`); the
> `cdylib` / `staticlib` artifacts require `std` for their allocator and panic handler, so
> they are not part of the `no_std` build.

## Benchmarks

Performance is tracked with [Criterion](https://crates.io/crates/criterion) benchmarks:

```sh
cargo bench                          # run all benchmark groups
cargo bench --bench deflate_bench    # compression throughput (vs. C zlib oracle)
cargo bench --bench inflate_bench    # decompression throughput (vs. C zlib oracle)
cargo bench --bench checksum_bench   # CRC-32 SIMD vs. scalar, and Adler-32
```

These targets gate the project relative to C zlib; the CI `perf` job enforces all four
and **fails the build** if any gate is missed:

| Operation                | Target relative to C zlib       | Measured (reference host) |
|--------------------------|---------------------------------|---------------------------|
| Compression throughput   | ≥ 80% of C zlib                 | ≥ 0.86× (≥ 1.2× at low levels) |
| Decompression throughput | ≥ C zlib (parity or better)     | ≥ 1.02× (parity or better) |
| CRC-32 (SIMD path)       | ≥ 3× the scalar implementation  | ≈ 55× the scalar baseline |
| Memory footprint         | ≤ C zlib                        | ≤ C zlib (deflate at parity, 268096 B; inflate 7152 B, −8 B) |

> Throughput ratios are hardware- and load-dependent; the figures above were measured on
> the reference CI host and comfortably clear the gates. The CI `perf` job enforces all
> four gates on every run: it parses the Criterion `estimates.json` output for the three
> throughput gates, and — since Criterion measures time rather than memory — runs the
> deterministic `tests/memory_footprint.rs` allocation test (a counting global allocator
> that measures the bytes the public `Deflate` / `Inflate` engines retain at the default
> `level=6` / `windowBits=15` / `memLevel=8` configuration) for the memory-footprint gate.

## Development workflow

Lint and format checks are part of the quality gate and must pass clean:

```sh
cargo clippy --all-targets -- -D warnings   # zero warnings
cargo fmt -- --check                        # verify formatting
cargo fmt                                   # auto-format
```

## Crate layout

```text
src/
├── lib.rs            crate root, public re-exports, feature gates
├── error.rs          ZlibError + return codes
├── constants.rs      all Z_* constants
├── stream.rs         ZStream (the streaming state container)
├── gz_header.rs      gzip header metadata
├── ffi.rs            #[no_mangle] extern "C" C-ABI shim (the `capi` feature)
├── deflate/          DEFLATE engine (stored/fast/slow/huff/rle drivers, trees)
├── inflate/          INFLATE engine (state machine, fast loop, tables, callback)
├── checksum/         Adler-32 and CRC-32
├── gz/               gzip file I/O (open/read/write/close + GzReader/GzWriter)
└── util/             version helpers and one-shot compress/uncompress wrappers
tests/                integration suites (regression, inflate_coverage, round_trip,
                      interop, gzip_compat, checksum)
benches/              Criterion harnesses (deflate_bench, inflate_bench, checksum_bench)
csrc/                 gzprintf.c — the small variadic C shim for the `capi` bridge
```

## Minimum supported Rust version

`zlib-rs` targets the **Rust 2024 edition** and requires **Rust 1.85.0 or newer** (the
release that stabilized edition 2024). The MSRV is declared as `rust-version = "1.85.0"`
in `Cargo.toml` and applies to **all** features, including the optional `capi` C-ABI
drop-in (whose variadic `gzprintf`/`gzvprintf` symbols are provided by a small C shim
rather than by any newer-toolchain Rust feature).

## License

`zlib-rs` is distributed under the permissive [zlib license](./LICENSE), the same license
as upstream zlib. See the [`LICENSE`](./LICENSE) file for the full text.

## Acknowledgements

The DEFLATE format was defined by Phil Katz; the DEFLATE and zlib specifications were
written by L. Peter Deutsch. The original zlib library was written by **Jean-loup Gailly**
and **Mark Adler**, whose C implementation is the reference baseline that this crate
ports and validates against. CRC-32 SIMD acceleration is provided by the
[`crc32fast`](https://crates.io/crates/crc32fast) crate.

## References

- zlib home page: <https://zlib.net/>
- RFC 1950 — ZLIB Compressed Data Format: <https://datatracker.ietf.org/doc/html/rfc1950>
- RFC 1951 — DEFLATE Compressed Data Format: <https://datatracker.ietf.org/doc/html/rfc1951>
- RFC 1952 — GZIP File Format: <https://datatracker.ietf.org/doc/html/rfc1952>
