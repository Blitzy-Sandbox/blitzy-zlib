# zlib-rs

> A safe, pure-Rust core of the zlib compression library — DEFLATE, zlib, and
> gzip — engineered to be bit-identical to C zlib.

`zlib-rs` is an in-place C&nbsp;&rarr;&nbsp;Rust rewrite of zlib `1.3.2.1`. It
implements the full DEFLATE compressor and decompressor together with the three
wire formats that zlib speaks:

- **zlib** streams — [RFC 1950](https://datatracker.ietf.org/doc/html/rfc1950)
- **raw DEFLATE** — [RFC 1951](https://datatracker.ietf.org/doc/html/rfc1951)
- **gzip** containers — [RFC 1952](https://datatracker.ietf.org/doc/html/rfc1952)

This crate is the **safe core** of a three-member Cargo workspace. It exposes an
idiomatic Rust API; a thin sibling crate, `libz-rs-sys`, layers the exact C ABI
on top of it for drop-in use from existing C programs (see
[C&nbsp;ABI&nbsp;drop-in](#c-abi-drop-in) below).

## Highlights

- **Zero `unsafe` in the core.** The entire crate is compiled under
  `#![forbid(unsafe_code)]` — including the `inflate` fast path. Manual C memory
  management (`malloc`/`free`, `FAR` pointers, and the `zalloc`/`zfree`
  callbacks) is replaced by Rust ownership and `Drop`. Every `unsafe` block
  required at the C boundary lives exclusively in the sibling `libz-rs-sys` shim.
- **Bit-exact output.** Compressed bytes are designed to be byte-identical to C
  zlib for every `(level, strategy, windowBits)` tuple, and decompression
  accepts any valid zlib, raw-DEFLATE, or gzip stream. Correctness is validated
  byte-for-byte against a C-zlib oracle (the `flate2` crate).
- **`no_std`-capable.** With the default `std` feature disabled the crate is
  `#![no_std]` and relies only on `core` + `alloc`, mirroring the C library's
  `Z_SOLO` configuration.
- **Minimal dependencies.** Only two runtime crates, matching the C baseline's
  near-zero footprint: `cfg-if` (always) and `crc32fast` (optional, behind the
  `simd` feature).

## Installation

`zlib-rs` requires a Rust toolchain of **1.85 or newer** (MSRV 1.85) and is
written for Rust **edition 2024**.

While the crate is part of this workspace (and pending a crates.io release),
depend on it by path or git:

```toml
[dependencies]
# In-workspace / pre-publication:
zlib-rs = { path = "zlib-rs" }
# ...or from git:
# zlib-rs = { git = "https://github.com/your-org/your-repo" }
```

Once published, the crates.io form is:

```toml
[dependencies]
zlib-rs = "1.3.2"
```

The crate version `1.3.2` mirrors the upstream zlib release line whose algorithms
it ports; the runtime `zlibVersion()` string reports the fuller `1.3.2.1-motley`.

## Usage

The idiomatic API is grouped into modules and re-exported from `src/lib.rs`;
the lib name is `zlib_rs` (the package name `zlib-rs` with the conventional
dash-to-underscore conversion). See the full reference on
[docs.rs](https://docs.rs/zlib-rs).

### One-shot compress / uncompress

The one-shot helpers in the `util` module mirror C zlib's `compress` /
`uncompress`: the caller supplies an output buffer and receives the number of
bytes written. `compress_bound` returns a safe worst-case output size.

```rust
use zlib_rs::util::{compress, compress_bound, uncompress};

let source = b"the quick brown fox jumps over the lazy dog";

// Size the destination with the zlib `compressBound` formula, then compress.
let mut compressed = vec![0u8; compress_bound(source.len())];
let n = compress(&mut compressed, source).expect("compression failed");
compressed.truncate(n);

// Decompress back into a buffer large enough to hold the original bytes.
let mut restored = vec![0u8; source.len()];
let m = uncompress(&mut restored, &compressed).expect("decompression failed");
restored.truncate(m);

assert_eq!(&restored[..], &source[..]);
```

To choose a compression level (`0`–`9`, or `-1` for the default), use the
level-aware variant `compress2(&mut dest, source, 6)`.

### Checksums

Adler-32 (the zlib trailer) and CRC-32 (IEEE; the gzip trailer) live in the
`checksum` module. Each is a *running* checksum: seed Adler-32 with `1` and
CRC-32 with `0`, then fold in successive byte slices.

```rust
use zlib_rs::checksum::{adler32, crc32};

// One-shot Adler-32 over a whole buffer (seed = 1):
let _a = adler32(1, b"hello world");

// A CRC-32 computed in one call equals the same data fed in chunks (seed = 0):
let whole = crc32(0, b"hello world");
let part = crc32(0, b"hello ");
let streamed = crc32(part, b"world");
assert_eq!(whole, streamed);
```

> The examples above use the crate's stable, guaranteed behaviours (a
> compress&rarr;uncompress round-trip returns the original bytes; a running
> checksum is associative over its input). The exact public API surface — the
> streaming `ZStream` interface, preset dictionaries, `windowBits` overloading,
> and the gzip file API — is documented module-by-module on docs.rs.

## Feature flags

The crate's compile-time configurability mirrors C zlib's. The default build
enables `std`, `gzip`, `gz-io`, and `simd` (a typical native zlib build); only
`no-std` (an alias for disabling `std`) is opt-in.

| Feature  | Default | Description |
|----------|:-------:|-------------|
| `std`    |   yes   | Standard-library support: the `gz*` file-I/O layer, the `std::error::Error` impl, and `crc32fast`'s runtime SIMD detection. With `std` **off**, the crate is `#![no_std]` (`core` + `alloc` only). |
| `simd`   |   yes   | Hardware-accelerated CRC-32 via the [`crc32fast`](https://crates.io/crates/crc32fast) crate. Disable it (for example with `--no-default-features`) to select the always-correct scalar table fallback. |
| `gzip`   |   yes   | gzip **container** support in `deflate` / `inflate` (mirrors the C `GZIP` define). |
| `gz-io`  |   yes   | gzip **file I/O** (`gzopen` / `gzread` / `gzwrite` / …); implies `std` + `gzip`. |
| `no-std` |    no   | Explicit marker mirroring the C `Z_SOLO` build (compression / decompression / checksums only). For a true `no_std` build, use `--no-default-features` — the `no-std` feature is a discoverability alias, since the actual switch is the **absence** of `std`. |

For example, a minimal `no_std` build with the scalar CRC path:

```toml
[dependencies]
zlib-rs = { version = "1.3.2", default-features = false }
```

## Building, testing, and benchmarking

All commands are crate-scoped with `-p zlib-rs`, targeting this member of the
workspace.

```bash
# Build (debug, then optimized release)
cargo build -p zlib-rs
cargo build -p zlib-rs --release
```

```bash
# Run the full test suite (unit + integration + doc tests)
cargo test -p zlib-rs
```

The crate ships six integration suites under `tests/`:

| Suite              | What it covers |
|--------------------|----------------|
| `regression`       | Port of C `test/example.c` — the canonical regression driver. |
| `inflate_coverage` | Port of C `test/infcover.c` — inflate state-machine coverage plus the allocation-failure harness. |
| `interop`          | Byte-for-byte comparison against the `flate2` / C-zlib oracle. |
| `round_trip`       | `quickcheck` property-based compress&rarr;decompress round-trips. |
| `gzip_compat`      | Port of C `test/minigzip.c` — gzip wire-format compatibility. |
| `checksum`         | Adler-32 / CRC-32 known-answer vectors. |

```bash
# Benchmarks (criterion): deflate / inflate / checksum throughput
cargo bench -p zlib-rs
cargo bench -p zlib-rs --bench deflate_bench
cargo bench -p zlib-rs --bench inflate_bench
cargo bench -p zlib-rs --bench checksum_bench
```

```bash
# Quality gates
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

> The original zlib C sources remain in the repository tree as the
> transformation source and as the byte-for-byte interop reference oracle. They
> are **not** part of this crate's build: Cargo supersedes the legacy
> `./configure; make` workflow.

## C ABI drop-in

If you need an ABI-compatible drop-in for existing C programs — `libz.so` /
`libz.a` exporting the exact `zlib.h` symbol set — use the sibling
**`libz-rs-sys`** crate (the `extern "C"` / `#[repr(C)]` shim) together with the
**`libz-rs-sys-cdylib`** workspace member that emits the shared and static
libraries. That layer is gated by the `capi` feature and is documented in the
**workspace root `README.md`**; it is intentionally not reproduced here.

## License

`zlib-rs` is distributed under the **zlib/libpng license** (SPDX identifier
[`Zlib`](https://spdx.org/licenses/Zlib.html)) — the same permissive license as
upstream zlib. The full text is in the bundled [`LICENSE`](LICENSE) file.

> (C) 1995-2026 Jean-loup Gailly and Mark Adler

## Acknowledgments

The DEFLATE format used by zlib was defined by Phil Katz. The DEFLATE and zlib
specifications were written by L. Peter Deutsch. This crate is a Rust port of
their work and that of the wider zlib community led by Jean-loup Gailly and Mark
Adler; the upstream project lives at <https://zlib.net/>.
