# zlib-rs — a safe Rust rewrite of zlib

A production-ready, idiomatic **Rust rewrite of [zlib](https://zlib.net/) 1.3.2.1**, performed
in-place as a C → Rust migration. The pure-Rust core replaces zlib's manual C memory management
(`malloc`/`free`, raw pointer arithmetic, and `FAR` pointers) with Rust ownership, borrowing, and
RAII — while preserving **full DEFLATE format compatibility** and **exact zlib API/ABI equivalence**
with the upstream C implementation.

zlib is a general-purpose, lossless data-compression library. The data formats it implements are
described by the following RFCs:

- **[RFC 1950](https://datatracker.ietf.org/doc/html/rfc1950)** — the *zlib* container format.
- **[RFC 1951](https://datatracker.ietf.org/doc/html/rfc1951)** — the raw *deflate* compressed-data
  format.
- **[RFC 1952](https://datatracker.ietf.org/doc/html/rfc1952)** — the *gzip* file format.

## Project goals & guarantees

- **Bit-identical output.** Compressed bytes are identical to C zlib for every
  `(level, strategy, windowBits)` combination. The compressor reproduces zlib's exact LZ77 match
  decisions, Huffman code assignments, and block boundaries, and is validated byte-for-byte against
  a reference C-zlib oracle.
- **Full format support.** zlib (RFC 1950), raw deflate (RFC 1951), and gzip (RFC 1952) streams,
  including the buffered `gz*` file API, preset dictionaries, `inflateSync` recovery, and
  `windowBits` overloading (8–15 zlib, −8…−15 raw, 24–31 gzip, 40–47 auto-detect).
- **A safe core.** The entire `zlib-rs` core is compiled under `#![forbid(unsafe_code)]`. There is
  **zero `unsafe` in the compression logic**; all `unsafe` is confined to the thin FFI shim, where
  it is structurally unavoidable at the C boundary.
- **A genuine C-ABI drop-in.** The FFI layer exposes an `extern "C"` surface whose symbol names and
  signatures match `zlib.h` exactly, so existing C consumers can relink against the Rust artifact
  without source changes.
- **Minimal dependencies & `no_std`.** The core carries only two runtime crates and offers a
  `no_std`-capable build path mirroring zlib's `Z_SOLO` configuration.

## Workspace layout

The project is a three-member Cargo workspace (Rust **edition 2024**, **MSRV 1.85**). The dependency
chain is `libz-rs-sys-cdylib → libz-rs-sys → zlib-rs`: the drop-in member wraps the C-ABI shim, and
the shim wraps the safe core.

- **[`zlib-rs`](zlib-rs/)** — the safe, pure-Rust core, compiled under `#![forbid(unsafe_code)]` and
  `no_std`-capable. Implements the deflate compressor, the inflate decompressor, Adler-32 and CRC-32
  checksums, the `gz*` file layer, and the one-shot `compress`/`uncompress` helpers. Contains **no
  `unsafe`**.
- **[`libz-rs-sys`](libz-rs-sys/)** — a thin `extern "C"` / `#[no_mangle]` C-ABI shim that
  re-exposes the exact `zlib.h` symbol set on top of the safe core, using `#[repr(C)]`
  `z_stream`/`gz_header` types. It is the **only** member permitted to contain `unsafe`, all of it
  at the raw-pointer boundary. A [`cbindgen`](https://github.com/mozilla/cbindgen) step generates
  `include/zlib.h` from this surface.
- **[`libz-rs-sys-cdylib`](libz-rs-sys-cdylib/)** — the drop-in artifact member. It builds the shim
  into a `cdylib` (`libz.so`) and a `staticlib` (`libz.a`) — the library target is named `z` — with
  an optional linker version-script derived from `zlib.map`.

## Building

All commands are run from the workspace root.

```sh
# Debug build of the whole workspace
cargo build --workspace

# Optimized release build
cargo build --release
```

## Testing

```sh
# Run the full test suite across all members
cargo test --workspace
```

The test suite is organized into the following layers:

- **`regression`** — the canonical zlib regression tests ported from `test/example.c`
  (`test_compress`, `test_deflate`, `test_inflate`, `test_flush`, `test_sync`, the dictionary
  tests, and more).
- **`inflate_coverage`** — decompressor coverage and malformed-input handling ported from
  `test/infcover.c`, including the allocation-failure harness.
- **`gzip_compat`** — gzip wire-format tests ported from `test/minigzip.c`.
- **`interop`** — byte-for-byte comparison of compressed output against the
  [`flate2`](https://crates.io/crates/flate2) C-zlib oracle (the strongest correctness assertion).
- **`round_trip`** — property-based compress → decompress round-trip tests via
  [`quickcheck`](https://crates.io/crates/quickcheck).
- **`checksum`** — Adler-32 and CRC-32 known-answer vectors.

## Quality gates

The same gates enforced in CI:

```sh
cargo clippy --all-targets -- -D warnings   # lint; warnings are errors
cargo fmt --check                           # formatting
cargo bench                                 # criterion throughput benchmarks
```

## C-ABI drop-in usage

Build the drop-in shared and static libraries:

```sh
cargo build -p libz-rs-sys-cdylib --release
```

This emits `libz.so` (shared) and `libz.a` (static) under `target/release/`. Because the exported
symbols match `zlib.h` exactly, existing C programs can **relink against these artifacts with no
source changes** — link against the Rust-built `libz.a`/`libz.so` (e.g. with `-L target/release -lz`)
in place of the system zlib.

For an already-built dynamic executable, you can substitute the Rust implementation at load time
without relinking:

```sh
LD_PRELOAD=/path/to/target/release/libz.so ./your_existing_program
```

To prove signature parity, the shim's build step generates `libz-rs-sys/include/zlib.h` with
`cbindgen` and checks it against the canonical `zlib.h` — verifying that every one of the 73
exported symbols is present with matching argument arity and that the `#[repr(C)]` structs match the
canonical field layout. In **CI and delivery builds this check is a hard gate**: the `build`,
`lint`, and `cdylib` workflows set `LIBZ_RS_SYS_STRICT_HEADER=1`, so any signature drift **fails the
build**. In ordinary local and downstream builds the check is best-effort and reports drift as a
`cargo:warning` (so packaging, docs.rs, and read-only source trees never break); set
`LIBZ_RS_SYS_STRICT_HEADER=1` locally to opt into the same hard gate.

## Feature flags

`zlib-rs` mirrors zlib's compile-time configurability as Cargo features, which are forwarded across
the workspace:

- **`std`** *(default)* — the standard library: the `gz*` file-I/O layer, the `std::error::Error`
  implementation, and `crc32fast`'s runtime SIMD detection. With `std` disabled the crate is
  `#![no_std]`, using only `core` + `alloc`.
- **`simd`** *(default)* — hardware-accelerated CRC-32 via
  [`crc32fast`](https://crates.io/crates/crc32fast). Disabling it selects the always-correct
  scalar-table fallback.
- **`gzip`** — gzip container support (mirrors the C `GZIP` define).
- **`gz-io`** — gzip file I/O (mirrors C `#ifndef NO_GZCOMPRESS`); implies `std` + `gzip`.
- **`no-std`** — an explicit marker mirroring the C `Z_SOLO` configuration; the actual `no_std`
  switch is the *absence* of the `std` feature.
- **`capi`** — a build **marker** for the C-ABI layer, consumed by the `libz-rs-sys-cdylib` member
  and `cargo-c` tooling. It does **not** gate compilation of the FFI layer — the `extern "C"`
  symbols in `libz-rs-sys` are always compiled. (To build/test just the pure-Rust core standalone,
  build `-p zlib-rs`; there is no need to toggle this flag.)

For a pure-Rust `no_std` build of just the core:

```sh
cargo build -p zlib-rs --no-default-features
```

## Relationship to the original C sources

This is an **in-place** migration: the original zlib C sources remain in the repository tree. They
serve two roles — the **transformation source** for the Rust rewrite, and the **byte-for-byte
interop reference oracle** exercised by the test suite. The historical C build entry points
(`./configure && make test`, the `win32/` makefiles, `make_vms.com`, CMake, and Bazel) are
**superseded by Cargo** and are no longer the supported build path for the Rust library.

## License

`zlib-rs` is distributed under the **zlib/libpng license** (SPDX identifier
[`Zlib`](https://spdx.org/licenses/Zlib.html)), the same license as upstream zlib. The full text,
including its three conditions, is in the [`LICENSE`](LICENSE) file.

> (C) 1995-2026 Jean-loup Gailly and Mark Adler
>
> This software is provided 'as-is', without any express or implied warranty. In no event will the
> authors be held liable for any damages arising from the use of this software.

## Acknowledgments

The deflate format used by zlib was defined by **Phil Katz**. The deflate and zlib specifications
were written by **L. Peter Deutsch**. This Rust port preserves the algorithms and on-the-wire
behavior of the original library written by Jean-loup Gailly and Mark Adler.

## Links

- zlib home page: <https://zlib.net/>
- zlib FAQ: <https://zlib.net/zlib_faq.html>
- Format specifications: [RFC 1950](https://datatracker.ietf.org/doc/html/rfc1950),
  [RFC 1951](https://datatracker.ietf.org/doc/html/rfc1951),
  [RFC 1952](https://datatracker.ietf.org/doc/html/rfc1952)
