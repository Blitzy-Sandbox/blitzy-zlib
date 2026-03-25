**Objective:** Generate a production-ready Rust rewrite of the zlib C library that replaces all manual memory management with Rust ownership semantics while maintaining full DEFLATE format compatibility and API equivalence.

**In Scope:**

- Full rewrite of all zlib C source into idiomatic Rust
- DEFLATE compression and decompression
- zlib and gzip stream format support
- C-compatible FFI interface (for drop-in replacement)
- Compression level configuration
- Full test suite including compatibility tests against reference zlib output

**Out of Scope:**

- bzip2, lzma, or other compression formats
- New compression algorithms
- GUI or tooling beyond the library itself

**Constraints:**

- Output must be binary-compatible with zlib-produced streams
- FFI layer must match the zlib C API signature exactly
- Zero unsafe blocks in core compression logic
- Must pass the official zlib test vectors
