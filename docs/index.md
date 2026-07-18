# blitzy-zlib

`zlib-rs` is a memory-safe, idiomatic Rust rewrite of the zlib 1.3.2.1
compression library with a C-compatible FFI drop-in layer. Its DEFLATE, zlib,
and gzip (RFC 1951 / RFC 1950 / RFC 1952) output is byte-identical to reference
zlib — validated by default (no C toolchain) against 300 vectors baked from
genuine C zlib 1.3.2.1-motley.

It is experimental and under active migration; its API surface may still shift while parity work continues.

- **Memory-safe** — Rust ownership, borrowing, and `Drop`-based cleanup replace manual `zcalloc`/`zcfree` allocation.
- **Full parity** — complete DEFLATE compression and decompression, with all 10 levels, 5 strategies, and 7 flush modes.
- **C ABI drop-in** — builds `cdylib` and `staticlib` artifacts exposing the zlib C API via `#[no_mangle] extern "C"` shims. Every public prototype is implemented; `gzprintf`/`gzvprintf` follow the documented no-`vsnprintf` zlib variant (they return `Z_STREAM_ERROR`, and `zlibCompileFlags` sets bit 27), while the idiomatic Rust `gzprintf` formats fully.
- **`no_std`** — a core-only build (no gz file I/O) is available via a Cargo feature flag; the full test suite runs under `--no-default-features` as a blocking CI gate.
