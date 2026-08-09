//! `zlib-rs-differential` — the oracle harness that holds the port to the reference bit for bit.
//!
//! This crate exists to answer one question mechanically: *does the Rust implementation produce the
//! same bytes as the C one?* It does that by linking both implementations into a single test binary
//! and running them against the same inputs, so a comparison is a memory comparison rather than a
//! comparison of two build artifacts across a process boundary.
//!
//! It is a development-only crate. Nothing it produces ships, and it appears in no `[dependencies]`
//! table of `zlib-rs` or `libz-rs-sys` — which is exactly what makes reference zlib an *oracle*
//! rather than a build dependency: `cargo build --release` for either shipped artifact never invokes
//! a C compiler. The workspace root reinforces that with `default-members`, so the advertised
//! release build does not even select this package.
//!
//! # Why byte-identity, and not merely RFC conformance
//!
//! RFC 1951 constrains the DEFLATE *format*, not the encoder's *choices*. Which of several equally
//! valid matches to emit, when to abandon a lazy match, how to break a Huffman frequency tie, when
//! to switch block type — all of it is implementation-defined, and callers depend on zlib's specific
//! answers. A fully conformant encoder that made different choices would still fail here. That is
//! why a handful of deliberately non-optimal heuristics in the reference are load-bearing, and why
//! this crate compares bytes rather than checking that a round trip happens to work.
//!
//! # How the oracle is built
//!
//! `build.rs` compiles the fifteen in-tree C translation units — `Makefile.in`'s `OBJZ` plus `OBJG`,
//! verbatim — with the flags the in-tree `configure` applies (`-O3 -fPIC
//! -D_LARGEFILE64_SOURCE=1 -DHAVE_HIDDEN`), together with two small generated files that expose the
//! `static` tables no linker can otherwise reach: a shim over the three generated-table headers,
//! and a wrapper that includes `deflate.c` itself, because the per-level `configuration_table` lives
//! inside that file and is declared in no header. It renames every externally visible symbol in the
//! result to carry a `c_` prefix, archives the objects into `libzlib_c_oracle.a` in `OUT_DIR`, and
//! audits the finished archive with `nm`, failing the build if any export was left unprefixed. The
//! C sources themselves are read-only inputs; nothing is written into the working tree, nothing is
//! vendored, and nothing is fetched.
//!
//! `-D_LARGEFILE64_SOURCE=1` is not optional. It is what makes the oracle declare and define
//! `gzopen64`, `gzseek64`, `gztell64`, `gzoffset64`, `adler32_combine64`, `crc32_combine64` and
//! `crc32_combine_gen64`, which are half of the large-file symbol duality the port has to reproduce.
//!
//! # The `c_` prefix
//!
//! Because both implementations are linked together, `deflate` cannot mean two things. Every oracle
//! symbol is therefore `c_`-prefixed, and [`oracle`] declares them under those names. So
//! `oracle::c_deflate` is `deflate.c`'s function and `libz_rs_sys::deflate` is the port's; a test
//! that calls both is comparing implementations and nothing else.
//!
//! # Corpus
//!
//! Correctness is measured against the committed, deterministic fixtures under `corpus/minimal/`,
//! so `cargo test` needs no network and no setup. The Silesia corpus is a second, opt-in tier used
//! only for throughput measurement: `corpus/fetch_silesia.sh` is run by a human, deliberately, and
//! by nothing else — not by `cargo test`, not by CI, and from no build script. Benchmarks report and
//! skip when it is absent, with a zero exit status. `corpus/README.md` is the published contract for
//! both tiers.
//!
//! # Modules
//!
//! * [`oracle`] — `#[repr(C)]` mirrors of the boundary types and `extern "C"` declarations for the
//!   `c_`-prefixed reference symbols, including the accessors for the generated tables.

pub mod oracle;
