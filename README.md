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
  with `std::io::{Read, Write, BufRead, Seek}` integration.
- **C-compatible FFI drop-in** for `libz` — exact symbol names, `#[repr(C)]` `z_stream`
  layout, and integer return codes, with a generated C header.
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

For a bare-metal / `no_std` build, disable the default features and select `no-std`:

```toml
[dependencies]
zlib-rs = { version = "1.3.2", default-features = false, features = ["no-std"] }
```

## Usage

### One-shot compression and decompression

The `compress` and `uncompress` helpers cover the common case where the whole input is
available in memory.

```rust,no_run
use zlib_rs::{compress, uncompress};

let data = b"Hello, zlib-rs! This is a compression test.";

// One-call compression at level 6 (the default level).
let compressed = compress(data, 6).expect("compression failed");

// One-call decompression. The second argument is a size hint for the
// output buffer (here, the known original length).
let restored = uncompress(&compressed, data.len()).expect("decompression failed");

assert_eq!(&data[..], restored.as_slice());
```

### Checksums

Adler-32 and CRC-32 are exposed directly. As in C zlib, the running Adler-32 value is
seeded with `1` and the running CRC-32 value is seeded with `0`; passing the previous
result back in lets you checksum a stream incrementally.

```rust,no_run
use zlib_rs::{adler32, crc32};

// Adler-32 (used by the zlib/RFC 1950 wrapper) — seeded with 1.
let a = adler32(1, b"123456789");
assert_eq!(a, 0x091e_01de);

// CRC-32 (IEEE; used by the gzip/RFC 1952 wrapper) — seeded with 0.
let c = crc32(0, b"123456789");
println!("crc32 = {c:#010x}");
```

### Streaming with `Read` / `Write`

The gzip layer implements the standard `std::io::Read` and `std::io::Write` traits, so
gzip streams compose with the rest of the I/O ecosystem. The snippet below is
illustrative of the streaming surface:

```rust,ignore
use std::io::{Read, Write};
use zlib_rs::gz::GzFile;

// Write a gzip-compressed file.
let mut writer = GzFile::create("greeting.txt.gz", 6)?;
writer.write_all(b"streamed through zlib-rs")?;
writer.finish()?;

// Read it back.
let mut reader = GzFile::open("greeting.txt.gz")?;
let mut text = String::new();
reader.read_to_string(&mut text)?;
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

The build script (`build.rs`) does two things at compile time:

1. **Regenerates the CRC-32 lookup tables** from the IEEE polynomial — mirroring the way
   upstream generates its `crc32.h` — instead of vendoring a large pre-computed table.
2. **Invokes [cbindgen](https://github.com/mozilla/cbindgen)** (configured by
   `cbindgen.toml`) to emit a C header, `zlib_rs.h` (include guard `ZLIB_RS_H`), into the
   build's `OUT_DIR`. Because the header is generated from the `#[no_mangle] extern "C"`
   functions and `#[repr(C)]` types in `src/ffi.rs`, the published C ABI can never
   silently drift from the Rust source — it is the machine-checked equivalent of the
   hand-written upstream `zlib.h`.

Because the exported symbols (`deflate`, `inflate`, `crc32`, `adler32`, `compress2`,
`uncompress`, …) match the canonical zlib C API exactly, a C program can include the
generated header and link `libzlib_rs.so` / `libzlib_rs.a` in place of `libz`.

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

`no-std` and `std` are mutually exclusive; the crate enforces this with a `compile_error!`
guard. To build for a `no_std` target, turn off the defaults and opt in:

```sh
cargo build --no-default-features --features no-std
```

## Testing

The full suite — unit, integration, and documentation tests — runs with:

```sh
cargo test
```

The integration suites mirror the upstream C test programs and add property-based and
oracle-based checks:

```sh
cargo test --test regression        # ports of test/example.c
cargo test --test inflate_coverage   # ports of test/infcover.c
cargo test --test round_trip         # quickcheck property round-trips
cargo test --test interop            # byte-identical vs. C zlib (flate2 oracle)
cargo test --test gzip_compat        # gzip file-format compatibility
cargo test --test checksum           # Adler-32 / CRC-32 known-answer tests
```

`tests/interop.rs` uses [`flate2`](https://crates.io/crates/flate2) — configured to link
**canonical C zlib** — as a reference oracle for byte-for-byte validation. `flate2` is a
**development-only** dependency: it is never linked into the shipped artifact, so the
released crate carries zero C dependency.

> **Known caveat:** `cargo test --no-default-features` currently fails to compile some
> test modules (they reference `alloc` types without importing them when `std` is off).
> The **library itself** compiles cleanly under every feature configuration, including
> `--no-default-features` and `--no-default-features --features no-std`; only the test
> harness is affected.

## Benchmarks

Performance is tracked with [Criterion](https://crates.io/crates/criterion) benchmarks:

```sh
cargo bench                          # run all benchmark groups
cargo bench --bench deflate_bench    # compression throughput
cargo bench --bench inflate_bench    # decompression throughput
cargo bench --bench checksum_bench   # Adler-32 / CRC-32 throughput
```

The benchmarks gate the project against these targets, relative to C zlib:

| Operation                | Target relative to C zlib       |
|--------------------------|---------------------------------|
| Compression throughput   | ≥ 80% of C zlib                 |
| Decompression throughput | ≥ C zlib (parity or better)     |
| CRC-32 (SIMD path)       | ≥ 3× the scalar implementation  |
| Memory footprint         | ≤ C zlib                        |

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
├── ffi.rs            #[no_mangle] extern "C" C-ABI shim
├── deflate/          DEFLATE engine (stored/fast/slow/huff/rle + trees)
├── inflate/          INFLATE engine (state machine, fast loop, tables, callback)
├── checksum/         Adler-32 and CRC-32
├── gz/               gzip file I/O (open/read/write/close)
└── util/             compress/uncompress wrappers and version helpers
```

## Minimum supported Rust version

`zlib-rs` targets the **Rust 2024 edition** and requires **Rust 1.85.0 or newer** (the
release that stabilized edition 2024). The MSRV is declared as `rust-version = "1.85.0"`
in `Cargo.toml`.

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

