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

## Project status

> **Checkpoint preview.** `zlib-rs` is delivered incrementally. This README describes the
> **complete, final-target** library; the lists below record what is actually available to
> build and call **at the current checkpoint** versus what is still forthcoming. Sections
> that document not-yet-available APIs, commands, or the C drop-in are marked
> **(forthcoming)** inline.

**Available now**

- **Checksums** — Adler-32 and CRC-32 with the `*_z` and `*_combine` / `*_combine64`
  variants and SIMD-accelerated CRC-32, exposed at the crate root
  (`zlib_rs::{adler32, crc32, …}`).
- **Decompression engine** — the full INFLATE state machine for the zlib, raw, gzip, and
  auto-detect framings, driven through the idiomatic `Inflate` decompressor
  (`zlib_rs::Inflate`), including preset dictionaries, `inflateSync` recovery, and the
  gzip-header sink.
- **Foundation types** — all `Z_*` constants, the `FlushMode` / `Strategy` / `DataType`
  enums, `ZlibError` / `ReturnCode`, `ZStream` / `Allocator`, `GzHeader`, and the version
  helpers (`zlib_rs::util::*`).
- **Build scaffolding** — the Cargo manifest, build script (CRC-table generation and
  optional cbindgen header emission), feature matrix, and CI.

**Forthcoming**

- The DEFLATE **compression** engine and the one-shot `compress` / `uncompress` helpers.
- The gzip **file-I/O** layer (`zlib_rs::gz`) and its `Read` / `Write` integration.
- The `#[no_mangle] extern "C"` **FFI shim** (`src/ffi.rs`) and the generated C header that
  together make the crate a binary drop-in for `libz`.
- The **integration test** suites (`tests/*.rs`) and the Criterion **benchmarks**
  (`benches/*.rs`).

## Features

> The list below describes the **complete, final-target** `zlib-rs` feature set. See
> [Project status](#project-status) for what is available to build and call at the current
> checkpoint.

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

### Decompression

The INFLATE engine is available now through the idiomatic `Inflate` decompressor. It
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

### One-shot compression and decompression *(forthcoming)*

The one-shot `compress` / `uncompress` helpers — covering the common case where the whole
input is available in memory — arrive with the DEFLATE compression engine in a later
checkpoint. The final-target API will read:

```rust,ignore
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

### Streaming with `Read` / `Write` *(forthcoming)*

The gzip file-I/O layer (`zlib_rs::gz`) — which will implement the standard
`std::io::Read` and `std::io::Write` traits so gzip streams compose with the rest of the
I/O ecosystem — arrives in a later checkpoint together with the compression engine. The
snippet below is illustrative of the planned streaming surface:

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

### Using zlib-rs as a C drop-in for `libz` *(forthcoming)*

The build script (`build.rs`) is in place now and **regenerates the CRC-32 lookup tables**
from the IEEE polynomial on every build — mirroring the way upstream generates its
`crc32.h`, instead of vendoring a large pre-computed table.

The C drop-in itself arrives in a later checkpoint with the `#[no_mangle] extern "C"` FFI
shim (`src/ffi.rs`). When that shim is present and the `capi` feature is enabled,
`build.rs` invokes [cbindgen](https://github.com/mozilla/cbindgen) (configured by
`cbindgen.toml`) to emit a C header, `zlib_rs.h` (include guard `ZLIB_RS_H`), into the
build's `OUT_DIR`. Because the header is generated from the `#[no_mangle] extern "C"`
functions and `#[repr(C)]` types in `src/ffi.rs`, the published C ABI can never silently
drift from the Rust source — it is the machine-checked equivalent of the hand-written
upstream `zlib.h`. Until that shim lands, header emission is skipped with a
`cargo:warning` and the crate builds as a pure-Rust library.

Once the shim is in place, the exported symbols (`deflate`, `inflate`, `crc32`, `adler32`,
`compress2`, `uncompress`, …) will match the canonical zlib C API exactly, so a C program
can include the generated header and link `libzlib_rs.so` / `libzlib_rs.a` in place of
`libz`.

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
| `capi`   | no      | Gates the `#[no_mangle] extern "C"` FFI exports (`src/ffi.rs`, forthcoming) and the cbindgen C-header emission in `build.rs`. |

`no-std` and `std` are mutually exclusive; the crate enforces this with a `compile_error!`
guard. Because the `[lib]` target also emits `cdylib` / `staticlib` — final link artifacts
that require a global allocator and a `#[panic_handler]` which a `#![no_std]` library must
not impose — a plain `cargo build` cannot link them without `std`. Build (and lint) the
`no_std` configuration as an `rlib` instead:

```sh
cargo rustc --lib --crate-type rlib --no-default-features --features no-std
```

## Testing

At this checkpoint the unit tests and documentation tests run with:

```sh
cargo test
```

This exercises the checksum engine, the INFLATE engine, and the foundation types, plus
every `///` doctest in the public API.

The dedicated integration suites *(forthcoming)* will mirror the upstream C test programs
and add property-based and oracle-based checks; they land together with the compression
engine and gzip layer they validate:

```sh
cargo test --test regression         # ports of test/example.c          (forthcoming)
cargo test --test inflate_coverage   # ports of test/infcover.c         (forthcoming)
cargo test --test round_trip         # quickcheck property round-trips  (forthcoming)
cargo test --test interop            # byte-identical vs. C zlib oracle (forthcoming)
cargo test --test gzip_compat        # gzip file-format compatibility   (forthcoming)
cargo test --test checksum           # Adler-32 / CRC-32 known answers  (forthcoming)
```

`tests/interop.rs` will use [`flate2`](https://crates.io/crates/flate2) — configured to
link **canonical C zlib** — as a reference oracle for byte-for-byte validation. `flate2`
is a **development-only** dependency: it is never linked into the shipped artifact, so the
released crate carries zero C dependency.

> **`no_std` testing note:** the test harness itself uses `std`, so the test suite runs
> under the default (hosted) configuration. The library compiles under
> `--no-default-features --features no-std` when built as an `rlib`
> (`cargo rustc --lib --crate-type rlib --no-default-features --features no-std`); the
> `cdylib` / `staticlib` artifacts require `std` for their allocator and panic handler, so
> they are not part of the `no_std` build.

## Benchmarks *(forthcoming)*

Performance will be tracked with [Criterion](https://crates.io/crates/criterion)
benchmarks, delivered alongside the compression engine they measure:

```sh
cargo bench                          # run all benchmark groups        (forthcoming)
cargo bench --bench deflate_bench    # compression throughput          (forthcoming)
cargo bench --bench inflate_bench    # decompression throughput        (forthcoming)
cargo bench --bench checksum_bench   # Adler-32 / CRC-32 throughput    (forthcoming)
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
├── ffi.rs            #[no_mangle] extern "C" C-ABI shim            (forthcoming)
├── deflate/          DEFLATE engine — foundational preview now;
│                     the stored/fast/slow/huff/rle driver is forthcoming
├── inflate/          INFLATE engine (state machine, fast loop, tables, callback)
├── checksum/         Adler-32 and CRC-32
├── gz/               gzip file I/O (open/read/write/close)         (forthcoming)
└── util/             version helpers now; compress/uncompress wrappers forthcoming
```

The layout above shows the **final-target** module tree. At the current checkpoint
`ffi.rs`, the `gz/` module, the `deflate/` compression driver, and the `util/` one-shot
helpers are forthcoming (see [Project status](#project-status)).

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

