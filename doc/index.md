# blitzy-zlib

`zlib-rs` is a memory-safe, idiomatic Rust rewrite of the zlib 1.3.2.1
compression library with a C-compatible FFI drop-in layer. Its DEFLATE, zlib,
and gzip (RFC 1951 / RFC 1950 / RFC 1952) output is byte-identical to reference
zlib — validated by default (no C toolchain) against 300 vectors baked from
genuine C zlib `1.3.2.1-motley`.

## Highlights

- **Memory-safe** — Rust ownership, borrowing, and `Drop`-based cleanup replace
  manual `zcalloc`/`zcfree` allocation; the compression core (`src/deflate/`)
  contains zero `unsafe`.
- **Full parity** — complete DEFLATE compression and decompression, with all 10
  compression levels, 5 strategies, and 7 flush modes.
- **C ABI drop-in** — builds `cdylib` and `staticlib` artifacts
  (`libzlib_rs.so` / `libzlib_rs.a`) exposing the zlib C API via
  `#[unsafe(no_mangle)] extern "C"` shims, with all `unsafe` isolated to `src/ffi/`.
  The `gzprintf`/`gzvprintf` shims follow the documented no-`vsnprintf` zlib
  variant — they return `Z_STREAM_ERROR`, and `zlibCompileFlags` sets bit 27.
- **`no_std`** — a core-only build (no gz file I/O) is available via a Cargo
  feature flag; the full test suite runs under `--no-default-features` as a
  blocking CI gate.

## Documentation

- **[Project Guide](project-guide.md)** — build, test, benchmark, and fuzz
  workflow; feature flags; and how to continue development.
- **[Technical Specifications](technical-specifications.md)** — architecture,
  the C ABI contract, byte-identity guarantees, and checksums.

The vendored format specifications are served alongside these pages:
[RFC 1950](rfc1950.txt) (zlib), [RFC 1951](rfc1951.txt) (DEFLATE),
[RFC 1952](rfc1952.txt) (gzip), and the DEFLATE [algorithm notes](algorithm.txt).

## Status

**Experimental — active migration.** The library tracks upstream
`zlib 1.3.2.1-motley` (`ZLIB_VERNUM 0x1321`); its API surface may still shift
while parity work continues.
