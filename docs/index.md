# blitzy-zlib

`zlib-rs` is a memory-safe, idiomatic Rust rewrite of the zlib 1.3.2.1
compression library with a C-compatible FFI drop-in layer. It preserves
byte-identical DEFLATE, zlib, and gzip (RFC 1951 / RFC 1950 / RFC 1952)
wire-format compatibility with reference zlib.

- **Memory-safe** — Rust ownership, borrowing, and `Drop`-based cleanup replace manual `zcalloc`/`zcfree` allocation.
- **Full parity** — complete DEFLATE compression and decompression, with all 10 levels, 5 strategies, and 7 flush modes.
- **C ABI drop-in** — builds `cdylib` and `staticlib` artifacts exposing the exact zlib C API via `#[no_mangle] extern "C"` shims.
- **`no_std`** — a core-only build (no gz file I/O) is available via a Cargo feature flag.
