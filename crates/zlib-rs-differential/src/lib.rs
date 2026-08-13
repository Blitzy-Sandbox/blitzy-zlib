//! `zlib-rs-differential` — the dev-only oracle harness that holds the Rust port to the C
//! reference, byte for byte.
//!
//! This crate exists to answer one question mechanically: *does the Rust implementation emit the
//! same bytes as the C one?* It answers it by making both implementations **co-resident in a
//! single test process** — the C reference linked in as a renamed static archive, the port linked
//! in as an ordinary Rust dependency — so that comparing them is a buffer-to-buffer memory
//! comparison rather than a comparison of two build artifacts across a process boundary. Nothing
//! is shelled out to, no output is round-tripped through a file, and no two-run diff has to be
//! trusted to have run both halves against the same input.
//!
//! # Why byte-identity, and not merely RFC conformance
//!
//! RFC 1951 constrains the DEFLATE *format*, not the encoder's *choices*. Which of several equally
//! valid matches to emit, when to abandon a lazy match, how to break a Huffman frequency tie, when
//! to switch block type — all of it is implementation-defined, and callers depend on this
//! library's specific answers. A fully conformant encoder that made different choices would still
//! fail here. That is why a handful of deliberately non-optimal heuristics in the reference are
//! load-bearing rather than incidental, and why this harness compares bytes instead of checking
//! that a round trip happens to succeed.
//!
//! # Dev-only by construction, not by convention
//!
//! Nothing this crate produces ships. It is `publish = false`, and — this is the part that
//! matters — it appears in **no `[dependencies]` table of either shipped crate**. `zlib-rs` has
//! an empty `[dependencies]` table and `libz-rs-sys` depends on nothing but `zlib-rs`, so
//! `cargo build --release` for `libz.so` or `libz.a` never compiles a line of the reference
//! implementation and never links this archive. The dependency edge runs one way only, from here
//! to them, and it is that direction that makes reference zlib an *oracle* rather than a build
//! dependency. It is checkable rather than asserted:
//!
//! ```text
//! cargo tree -p zlib-rs     -e normal   # must mention neither cc nor this crate
//! cargo tree -p libz-rs-sys -e normal   # must mention neither cc nor this crate
//! ```
//!
//! The workspace root reinforces the same boundary from the other side: `default-members` lists
//! only `crates/zlib-rs` and `crates/libz-rs-sys`, so a bare `cargo build --release` does not even
//! select this package.
//!
//! # No submodules, no vendoring, no network
//!
//! There is nothing to fetch and nothing to check out. The C reference sources are already in the
//! repository, as read-only inputs, and `build.rs` compiles them in place: every object, every
//! generated `.c` file and the finished archive are written to `OUT_DIR` and nowhere else, so the
//! working tree is never touched and the C build stays green and keeps its standing as the
//! baseline. Correctness likewise needs no network — see [Corpus](#corpus) below.
//!
//! # How the oracle is built
//!
//! `build.rs` compiles the fifteen in-tree translation units — `Makefile.in`'s `OBJZ` plus
//! `OBJG`, in that order, verbatim — with the flags the in-tree `configure` script applies
//! (`-O3 -fPIC -D_LARGEFILE64_SOURCE=1 -DHAVE_HIDDEN`), archives them into `libzlib_c_oracle.a`
//! in `OUT_DIR`, and emits the `cargo::rustc-link-search=native=<OUT_DIR>` and
//! `cargo::rustc-link-lib=static=zlib_c_oracle` directives that link it. The archive stem is
//! deliberately not `z`: `libzlib_c_oracle.a` can never be mistaken for a system `libz.a` on a
//! link line.
//!
//! Two small generated C files ride along, both written into `OUT_DIR`, because some of what a
//! differential test needs to read is unreachable by a linker. `zutil.h` defines `local` as
//! `static`, so every generated table in the reference is a translation-unit-private symbol: a
//! shim over `crc32.h`, `trees.h` and `inffixed.h` hands out pointers to them together with each
//! table's own element count, and a second file `#include`s `deflate.c` itself, because the
//! per-level `configuration_table` lives inside that file and is declared in no header at all.
//!
//! `-D_LARGEFILE64_SOURCE=1` is mandatory rather than a nicety. It is what makes the oracle
//! declare *and* define `gzopen64`, `gzseek64`, `gztell64`, `gzoffset64`, `adler32_combine64`,
//! `crc32_combine64` and `crc32_combine_gen64`, which are one half of the large-file symbol
//! duality the port has to reproduce; without it those seven names simply would not be there to
//! compare against.
//!
//! # Why every oracle symbol carries a `c_` prefix
//!
//! `libz-rs-sys` exports `deflate`, `inflate`, `crc32` and the rest as `#[no_mangle] extern "C"`
//! functions. Linking a plain oracle archive into the same test binary would define every one of
//! those names twice and the binary would not link at all, so each externally visible C symbol is
//! renamed to `c_<name>`. `c_deflate` is therefore `deflate.c`'s function and
//! `libz_rs_sys::deflate` is the port's, and a test that calls both is comparing implementations
//! and nothing else.
//!
//! Two mechanisms are needed, because neither one reaches the whole surface:
//!
//! 1. **95 preprocessor renames**, one `-D<name>=c_<name>` per public entry point. The count is
//!    derived, not guessed: 94 distinct names from the column-zero `ZEXTERN` declarations, plus the
//!    7 large-file names declared only inside `zlib.h`'s indented `Z_LARGE64` / `Z_WANT64` block,
//!    minus the 6 names that are `#define` macros rather than functions (`deflateInit`,
//!    `deflateInit2`, `inflateInit`, `inflateInit2`, `inflateBackInit` and `gzgetc`). That last
//!    subtraction is load-bearing: passing `-DdeflateInit=c_deflateInit` would make `zlib.h`'s own
//!    `#define deflateInit(strm, level) ...` a redefinition, and every translation unit would warn.
//! 2. **An `objcopy --redefine-syms` pass over the object files**, for the 19 names the
//!    preprocessor cannot reach. `gzgetc` is the reason this pass exists at all, and the reason is
//!    worth stating precisely because it is easy to assume a `-D` suffices: `zlib.h` itself makes
//!    `gzgetc` a function-like macro, so `gzread.c` has to `#undef gzgetc` immediately before
//!    defining the real function — and that `#undef` deletes any command-line `-D` along with it.
//!    The only place left to rename it is the object file. The same pass covers the
//!    `ZLIB_INTERNAL` surface, which is not `ZEXTERN`-declared and so is untouched by the 95:
//!    the union of both `local:` blocks in `zlib.map` with the `_tr_*`, `_dist_code` and
//!    `_length_code` family that its `_*` wildcard covers. `inflate_table` is the concrete case —
//!    the unmodified `test/infcover.c` calls it directly, so the reference `libz.a` keeps it
//!    reachable even though `zlib.map` keeps it out of `libz.so`'s dynamic symbol table.
//!
//! Because `objcopy` rewrites undefined references as well as definitions, cross-translation-unit
//! calls stay wired up: after the pass `deflate.o` refers to `c__tr_init`, not `_tr_init`. The
//! build then audits the finished archive with `nm` and fails if *any* defined external symbol was
//! left unprefixed, so a newly exported C symbol shows up as a build failure rather than as a
//! duplicate-symbol link error in whichever test happens to be built next.
//!
//! Compiling with `-DZ_PREFIX` would have renamed most of this in one step, and it is deliberately
//! not used: the prefix this harness is specified to expose is `c_`, not `z_`, and `zconf.h`
//! states that standard zlib is built *without* `Z_PREFIX`, which would put the oracle at odds
//! with the requirement that it be the default configuration.
//!
//! # Crate architecture
//!
//! Recorded explicitly, because the suites under `tests/` are written against it:
//!
//! ```text
//! src/oracle.rs  -> local #[repr(C)] mirrors + extern "C" c_/c_oracle_* declarations, safe slice
//!                   wrappers over the generated tables, and one safe gate per reference entry
//!                   point.  ONE of this crate's two files where `unsafe` appears.
//! src/port.rs    -> one safe gate per libz-rs-sys entry point, the gzFile handle, the
//!                   inflateBack callback bridge and the instrumented allocator.  THE OTHER.
//! src/lib.rs     -> this file: crate docs + `pub mod oracle;` and `pub mod port;`.  No unsafe
//!                   and no logic -- but see the ATTRIBUTES note below for why it still cannot
//!                   carry `#![forbid(unsafe_code)]`.
//! build.rs       -> the C oracle build.  No unsafe, and carries `#![forbid(unsafe_code)]`.
//! tests/*.rs     -> import `zlib_rs_differential::{oracle, port}` (this crate's lib target) and
//!                   `zlib_rs::*` for the idiomatic surface.  ALL differential, interop and
//!                   table-equality assertions live there, and every root carries
//!                   `#![forbid(unsafe_code)]`.
//! benches/*.rs   -> the three suites of the `benches/` package (its own manifest, its own
//!                   workspace, its own lock), which depends on THIS crate by path; nothing is
//!                   attached here by a [[bench]] entry.  Same posture as tests/.
//! ```
//!
//! # Both implementations are ordinary dependencies, and the direction of the edge is the point
//!
//! `zlib-rs` and `libz_rs_sys` sit in `[dependencies]`, not `[dev-dependencies]`, and that is a
//! requirement rather than a convenience. [`port`] is a module under `src/`, so it belongs to the
//! `lib` target, and Cargo resolves dev-dependencies for test, example and bench targets but *not*
//! for the lib target: a `use libz_rs_sys::…` in [`port`] does not compile while the facade is a
//! dev-dependency. The gates therefore could not have lived under `src/` at all — and they have to
//! live there, because a boundary spread across every suite that needs it is not a boundary.
//!
//! What keeps this crate out of the shipped crates' graphs is not an empty `[dependencies]` table
//! but the **direction** of the edge. This crate depends on them; neither of them names this crate,
//! in `[dependencies]`, `[build-dependencies]` or `[dev-dependencies]`. Measured:
//! `cargo tree -p libz-rs-sys -e normal,build,dev` mentions `zlib-rs-differential` nowhere, so
//! `cargo build --release -p zlib-rs -p libz-rs-sys` still invokes no C compiler — which is the
//! property that makes the reference an oracle rather than a build dependency.
//!
//! [`oracle`] nevertheless keeps its own `#[repr(C)]` mirrors of `z_stream`, `gz_header` and
//! `gzFile_s` rather than importing the facade's, and now that it *could* import them the reason is
//! worth stating plainly rather than leaving as an artefact: **the reference side's view of the ABI
//! has to be independent of the implementation under test.** One shared set of type definitions
//! would make a layout error invisible to precisely the comparison that exists to catch it. So
//! **those local mirrors must agree with the facade's field for field**, and two independent
//! mechanisms hold them to it: compile-time layout assertions in [`oracle`] that encode the same
//! measured sizes and offsets as `crates/libz-rs-sys/src/layout_assertions.rs`, and the
//! `c_oracle_ct_data_size` and `c_oracle_code_size` accessors, which report what the C compiler
//! actually produced rather than what Rust assumed. `ct_data` has no counterpart in the facade at
//! all — it is an internal `deflate.h` type, not part of the public ABI — so it could only ever
//! have been declared here.
//!
//! # The facade's import name is `libz_rs_sys`
//!
//! `crates/libz-rs-sys` sets `[lib] name = "z"` so that Cargo emits `libz.so` and `libz.a` under
//! the names a drop-in replacement must have. With a plain dependency key that would also make the
//! Rust path `z`, which is an unhelpfully short name for the crate under test, so this crate's
//! manifest uses Cargo's dependency-rename form —
//! `libz_rs_sys = { package = "libz-rs-sys", path = "../libz-rs-sys", version = "1.3.2" }` — and
//! Cargo passes `--extern libz_rs_sys`. **Every test here writes `use libz_rs_sys::…`, never
//! `use z::…`**, and the `benches/` package renames the dependency identically so its suites read
//! the same way. The manifest is the authority on that name; this
//! paragraph exists so the two never drift.
//!
//! # Corpus
//!
//! Correctness is measured against the committed, deterministic fixtures under `corpus/minimal/`,
//! so `cargo test` needs no network, no download and no setup. The Silesia corpus is a second,
//! opt-in tier used only for throughput measurement. FETCHING it is something a human does
//! deliberately: `corpus/fetch_silesia.sh` is invoked in fetching mode by nobody else — not by
//! `cargo test`, not by CI, and from no build script. VERIFYING is different, and worth stating
//! because "and by nothing else" used to be written here: `rust.yml`'s `bench` job runs that script
//! as `--verify-only`, which obtains nothing, writes nothing and reaches no network, against a
//! corpus provisioned outside the workflow (a cache keyed on the pinned digest, or
//! `ZLIB_RS_SILESIA_DIR`). So CI may verify and use a Silesia corpus while never acquiring one, and
//! when none is present both throughput benchmarks report and skip with a zero exit status.
//! `corpus/README.md` is the published contract for both tiers.
//!
//! # What this crate must never do
//!
//! * **Never `#[no_mangle]` anything.** This crate exports no C symbol. It *consumes* two sets of
//!   them, and a third definition of any name would break the link it exists to perform.
//! * **Never use `extern "C-unwind"`.** Every declaration is `extern "C"`. The reference
//!   implementation is C and does not unwind, and pinning the non-unwinding convention means a
//!   panic reaching this boundary aborts instead of becoming undefined behaviour.
//! * **Never reach for `zlib-rs`'s internals.** `inflate_table`, `inflate_fast`, `inflate_fixed`,
//!   `z_errmsg`, `gz_error`, `gz_intmax` and the `_tr_*` API are `pub(crate)` there, deliberately,
//!   because `zlib.map`'s `local:` blocks keep the C library's counterparts out of the dynamic
//!   symbol table. A test that needs one of those functions compares against the oracle's
//!   `c_`-prefixed copy — `c_inflate_table` and its siblings — and never widens the port's
//!   visibility to suit a test.
//!
//! # Miri and AddressSanitizer
//!
//! This crate is outside Miri and INSIDE AddressSanitizer, and the asymmetry has a cause rather
//! than being an oversight. Miri interprets Rust MIR and cannot execute compiled C at all, so the
//! Miri job is scoped to the safe core (`-p zlib-rs`) and this crate — whose whole purpose is to
//! call into a C archive — can never be run under it.
//!
//! AddressSanitizer has no such limitation, and `rust.yml`'s `asan` job does select this crate.
//! Its steps, in order: `cargo +nightly test -p libz-rs-sys` plain and again with `-Zbuild-std`;
//! an instrumented facade archive that the three relinked C drivers are linked against and run;
//! `cargo +nightly test -p zlib-rs-differential --target x86_64-unknown-linux-gnu`, which is THIS
//! crate's default sweeps with the oracle itself compiled `-fsanitize=address` through `CFLAGS`;
//! and finally `cargo +nightly bench --manifest-path benches/Cargo.toml`, which builds all three
//! suites under instrumentation and runs each in criterion's `--test` mode, one iteration per
//! benchmark. So the `unsafe` in `oracle.rs` and `port.rs` is exercised with instrumentation on,
//! on both sides of the boundary, which is exactly where a mismatched declaration would show up as
//! an over-read.
//!
//! One option is dropped for that step alone: `detect_leaks=0`, because the C oracle keeps `local`
//! state alive for the life of the process by design (`z_errmsg`, `x2n_table`, the CRC tables) and
//! `LeakSanitizer` cannot tell that from a leak at exit. Every other check — out-of-bounds,
//! use-after-free, use-after-return — is fully in force, and allocator discipline is covered far
//! more precisely by `port`'s instrumented allocator, which counts allocations and frees and
//! asserts on leaks, non-LIFO frees and rogue frees inside the tests themselves.
//!
//! Neither scope is expressible as a manifest key — both are CI job scopes — so they are recorded
//! here and in the manifest, and they must be kept in step with that workflow rather than
//! described from memory.
//!
//! # Modules, and where `unsafe` lives
//!
//! This crate has to drive two C ABIs — the port's and the reference's — and calling a
//! pointer-taking `extern "C"` function is an `unsafe` operation whichever side it belongs to.
//! Both are therefore confined to **exactly two files**, which together are this crate's whole
//! FFI boundary:
//!
//! * [`oracle`] — the C reference surface: `#[repr(C)]` mirrors of the boundary types,
//!   `extern "C"` declarations for the `c_`-prefixed symbols, safe accessors for the generated
//!   tables, and one safe gate per reference entry point.
//! * [`port`] — the port's surface: one safe gate per `libz-rs-sys` entry point, the `gzFile`
//!   handle, the `inflateBack` callback bridge, and the instrumented allocator both sides are
//!   driven through.
//!
//! Everything else — `build.rs`, every suite in `tests/`, the five `fuzz_targets/`, and the three
//! criterion suites of the excluded `benches/` package that depends on this crate by path —
//! carries `#![forbid(unsafe_code)]`, so "the harness contains no `unsafe` outside its boundary"
//! is a compiler-enforced property rather than a convention. This file is
//! the one place the attribute is absent, and it cannot be otherwise: a crate-root `forbid` would
//! cover the two modules above as well. It holds no code for the attribute to protect — crate
//! documentation, `pub mod oracle;` and `pub mod port;`, nothing else. See the ATTRIBUTES note
//! immediately below.
//! Neither gate module decides anything: they convert shapes, make one call each, and convert the
//! answer back, which is what keeps a measured difference attributable to the implementations
//! rather than to the harness.

// ---------------------------------------------------------------------------------------------
// ATTRIBUTES.  Read the next paragraph before adding one, and in particular before adding the one
// that is missing.
//
// `#![forbid(unsafe_code)]` IS DELIBERATELY ABSENT, and this is not an omission to be helpfully
// corrected.  A crate-root `forbid` applies to the whole crate, `src/oracle.rs` and `src/port.rs`
// included, and those two files are nothing but `extern "C"` declarations, their call sites and the
// C-ABI callbacks the library invokes -- they are this crate's designated, and only, unsafe
// surface.  Adding the attribute here would make the crate uncompilable, not safer.  The divergence
// from `crates/zlib-rs/src/lib.rs`, which does carry `#![forbid(unsafe_code)]` and is a genuinely
// unsafe-free algorithmic core, is therefore intentional; `crates/libz-rs-sys/src/lib.rs` diverges
// the same way and for the same reason.  Containment is achieved instead by keeping every `unsafe`
// operation in those two audited files, each occurrence carrying its own `// SAFETY:` comment --
// the workspace lint set denies `clippy::undocumented_unsafe_blocks`, so that is enforced rather
// than encouraged -- and by every OTHER file that exercises them, in `tests/`, `benches/` and
// `fuzz_targets/`, carrying `#![forbid(unsafe_code)]` itself.  THIS file contains no `unsafe` at
// all, and must not acquire any: it is a module holder and a document.
//
// `#![no_std]` is absent for a simpler reason: this is not the safe core.  The tests do file I/O
// and the oracle links against libc, so the crate uses `std`.
// ---------------------------------------------------------------------------------------------

// Mirrors `crates/libz-rs-sys/src/lib.rs`: an `unsafe fn` does not get to be an implicit `unsafe`
// block, so every unsafe operation inside `oracle.rs` and `port.rs` -- the crate's two boundary
// files -- must be individually scoped and individually justified.
#![deny(unsafe_op_in_unsafe_fn)]
// Mirrors both sibling crate roots.  A harness whose whole purpose is to be read by whoever writes
// the next differential test has no undocumented public items.
#![deny(missing_docs)]
// An outer doc comment on `pub mod oracle;` and the inner `//!` docs inside `oracle.rs` are
// fragments of one doc string, and rustdoc resolves the merged result from this file's scope.  A
// link that resolved inside `oracle.rs` alone can therefore stop resolving once the two are joined,
// silently, as a warning in a doc build nobody reads.  Denying it makes that a hard error.  Its
// companion catches the other way a link can be wrong here: pointing a public item's docs at one of
// `oracle.rs`'s private helpers, which would render as an unresolvable name for every reader who
// does not have the source open.
#![deny(rustdoc::broken_intra_doc_links)]
#![deny(rustdoc::private_intra_doc_links)]

/// Bindings to the C reference implementation, compiled as a test-only oracle.
///
/// Declares the `c_`-prefixed public entry points renamed out of the fifteen reference translation
/// units, the `c_oracle_*` accessors that reach the generated tables the C build keeps `static`,
/// safe `&'static [T]` wrappers over those tables, and the `#[repr(C)]` ABI mirrors the
/// declarations are written in terms of. The mirrors are transcribed from `zlib.h`, `zconf.h`,
/// `inftrees.h`, `deflate.h` and `zutil.h`; the functions from the translation units themselves.
pub mod oracle;

/// The port's C ABI, exposed to the harness as safe Rust.
///
/// One safe gate per `libz-rs-sys` entry point the suites reach, the [`port::GzFile`] handle that
/// keeps the opaque `gzFile` pointer out of harness code, the `inflateBack` callback bridge, and
/// the [`port::TrackingAllocator`] port of `test/infcover.c`'s `mem_zone` that both sides are
/// driven through. Together with [`oracle`] this is the whole of this crate's `unsafe`.
pub mod port;

/// The retention discipline both boundary modules hand their callers.
///
/// Three zlib entry points keep a pointer past the call that installed it -- the `gz_header` of
/// `deflateSetHeader`/`inflateGetHeader`, the `inflateBack` window, and the ledger behind
/// `opaque` -- and a `&mut` borrow that ends with the call cannot express that. [`retain::Session`]
/// owns the stream, holds every retained allocation borrowed for the stream's whole life, and makes
/// declaring the storage after the stream a compile error rather than a comment. It contains no
/// `unsafe` and makes no library call of its own: [`port`] and [`oracle`] keep every `extern "C"`
/// call and every `// SAFETY:` comment.
pub mod retain;
