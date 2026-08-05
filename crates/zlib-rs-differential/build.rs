//! Build script for `zlib-rs-differential`, the dev-only differential oracle harness.
//!
//! # What this script produces
//!
//! Exactly one linkable artifact: the static archive `libzlib_c_oracle.a` in `OUT_DIR`,
//! containing
//!
//! * the fifteen in-tree C translation units of the reference implementation, and
//! * a generated C shim that hands out pointers to the reference implementation's generated
//!   tables,
//!
//! with **every** externally visible symbol renamed to carry a `c_` prefix. The prefix is what
//! lets the Rust port and the C reference implementation be linked into one test binary and
//! called alternately so their output buffers can be compared directly, which is how
//! "compressed output is byte-identical to the reference" stops being an assertion and becomes a
//! measured fact. The intermediate objects, generated shim source and `objcopy` map also remain
//! in `OUT_DIR` as ordinary build intermediates; nothing is generated in the source tree.
//!
//! The archive is consumed only by this crate's tests and benches. It is never a dependency of
//! `zlib-rs` or `libz-rs-sys`, so `cargo build --release` for `libz.so`/`libz.a` never invokes a
//! C compiler. That one-way edge is the mechanical proof that reference zlib is an *oracle* and
//! not a build dependency.
//!
//! # The C sources are read-only
//!
//! Every C file in the repository root is reference material: it is compiled, never edited,
//! preprocessed in place, or regenerated. Objects, the generated shim and the archive are all
//! written to `OUT_DIR` and nowhere else, so the C build stays green and keeps its standing as
//! the oracle. Nothing here touches the network, vendors a copy of anything, or needs a
//! submodule -- the reference sources are already in the tree.
//!
//! # The fifteen translation units
//!
//! The set is `Makefile.in`'s `OBJZ` plus `OBJG` object lists, taken verbatim so that this
//! script and the C build can never disagree about what "the reference implementation" is:
//!
//! * `OBJZ`: `adler32.c`, `crc32.c`, `deflate.c`, `infback.c`, `inffast.c`, `inflate.c`,
//!   `inftrees.c`, `trees.c`, `zutil.c`
//! * `OBJG`: `compress.c`, `uncompr.c`, `gzclose.c`, `gzlib.c`, `gzread.c`, `gzwrite.c`
//!
//! Note `uncompr.c`. There is no `uncompress.c` in this repository; prose that says otherwise is
//! wrong about the filename.
//!
//! # Compile flags, and why `_LARGEFILE64_SOURCE` is not optional
//!
//! `-O3 -fPIC -D_LARGEFILE64_SOURCE=1 -DHAVE_HIDDEN -I<repo root>` -- the flags the in-tree
//! `configure` script actually applies (`-fPIC` at L285, `-D_LARGEFILE64_SOURCE=1` at L637-L639,
//! `-DHAVE_HIDDEN` at L947-L948).
//!
//! `-D_LARGEFILE64_SOURCE=1` is mandatory rather than a nicety: it is what makes the oracle
//! declare and define the large-file family -- `gzopen64`, `gzseek64`, `gztell64`, `gzoffset64`,
//! `adler32_combine64`, `crc32_combine64`, `crc32_combine_gen64` -- that the differential tests
//! compare against. Without it those symbols simply are not in the archive and the `*64` half of
//! the public surface goes untested. `test/CMakeLists.txt` propagates the same definition through
//! `INTERFACE_COMPILE_DEFINITIONS`, which corroborates the choice.
//!
//! Equally important is what is *not* defined. `DYNAMIC_CRC_TABLE`, `FASTEST`, `LIT_MEM`,
//! `UNALIGNED_OK`, `FORCE_STATIC`, `FORCE_STORED`, `GEN_TREES_H`, `MAKECRCH`, `Z_SOLO` and
//! `ZLIB_DEBUG` all change either emitted bytes or the shape of the generated tables. The Rust
//! port implements the *default* configuration, so the oracle has to be built in that same
//! default configuration or a byte-identity comparison compares nothing meaningful.
//!
//! # Symbol-collision resolution
//!
//! `libz-rs-sys` exports `deflate`, `inflate`, `crc32` and the rest as `#[no_mangle] extern "C"`
//! functions. Linking a plain oracle archive into the same test binary would duplicate every one
//! of them, so each externally visible C symbol is renamed to `c_<name>`. Two mechanisms are
//! needed, because neither one covers the whole surface.
//!
//! ## 1. Preprocessor renames: exactly 95 `-D<name>=c_<name>` macros
//!
//! The arithmetic is `94 + 7 - 6 = 95`:
//!
//! * **94** unique names from the column-zero `ZEXTERN` declarations in `zlib.h`. There are 95
//!   such declarations and 94 distinct names, because `gzprintf` is declared twice -- once for
//!   the varargs build and once for the fallback.
//! * **plus 7** large-file names that are declared only inside the indented `Z_LARGE64` /
//!   `Z_WANT64` block: `gzopen64`, `gzseek64`, `gztell64`, `gzoffset64`, `adler32_combine64`,
//!   `crc32_combine64`, `crc32_combine_gen64`.
//! * **minus 6** names that are `#define` macros in `zlib.h` rather than functions:
//!   `deflateInit`, `deflateInit2`, `inflateInit`, `inflateInit2`, `inflateBackInit` and
//!   `gzgetc`.
//!
//! That last subtraction is load-bearing, not tidiness. Passing `-DdeflateInit=c_deflateInit`
//! makes `zlib.h`'s own `#define deflateInit(strm, level) ...` a redefinition, so every
//! translation unit emits a `"deflateInit" redefined` warning and the build fails. With the six
//! excluded the fifteen files compile with zero warnings.
//!
//! The names are a hardcoded `const` array rather than something scraped out of `zlib.h` at
//! build time: the set is part of the frozen public contract, and a hardcoded list is auditable
//! and cannot silently shrink because a regex stopped matching.
//!
//! ## 2. `objcopy --redefine-syms`, for the 19 names macros cannot reach
//!
//! `gzgetc` is the reason this second pass exists at all. `gzread.c` does `#undef gzgetc`
//! immediately before defining the real function -- it has to, because `zlib.h` makes `gzgetc` a
//! function-like macro -- and that `#undef` deletes any command-line `-D` along with it. The
//! only way to rename it is on the object file.
//!
//! The same pass also covers the `ZLIB_INTERNAL` surface, which the 95 public renames do not
//! touch because those names are not `ZEXTERN` declarations. This matters concretely:
//! `crates/libz-rs-sys/tests/symbol_parity.rs` asserts a deliberate exception in which
//! `inflate_table` is *present* in `libz.a` -- the unmodified `test/infcover.c` calls it
//! directly from `cover_trees()` -- while staying absent from `libz.so`'s dynamic symbol table.
//! So `libz.a` and an un-renamed oracle archive would both define `inflate_table`, and the test
//! binary would not link.
//!
//! The 19 names are the union of both `local:` blocks in `zlib.map` (`ZLIB_1.2.0` and
//! `ZLIB_1.3.2`) with the `_tr_*` / `_dist_code` / `_length_code` family that `zlib.map` covers
//! there under its `_*` wildcard. Measured against a real build, that is exactly the set of
//! defined external symbols that survives the preprocessor pass -- no more, no less.
//!
//! `objcopy` rewrites undefined references as well as definitions, so cross-translation-unit
//! calls stay wired up: after the pass `deflate.o` refers to `c__tr_init`, not `_tr_init`. The
//! pass is also idempotent, because a second run finds no un-prefixed name left to match.
//!
//! A note on the alternative: compiling with `-DZ_PREFIX` would rename most of this surface in
//! one step, and it handles `gzgetc` (`gzread.c` undefines `z_gzgetc` instead when
//! `Z_PREFIX_SET` is defined). It is not used here for two reasons. The prefix the harness is
//! specified to expose is `c_`, not `z_`; and `zconf.h` states that standard zlib is compiled
//! *without* `Z_PREFIX`, which puts it at odds with the requirement that the oracle be the
//! default configuration.
//!
//! # Why a generated shim is required for the tables
//!
//! Every generated table is unreachable by the linker. `zutil.h` defines `local` as `static`,
//! and each table is declared with it: `crc_table`, `crc_big_table`, `crc_braid_table`,
//! `crc_braid_big_table` and `x2n_table` in `crc32.h`; `static_ltree`, `static_dtree`,
//! `base_length` and `base_dist` in `trees.h`; `lenfix` and `distfix` in `inffixed.h` are plain
//! `static`. A table-equality test therefore cannot declare any of them `extern "C"` -- there is
//! no symbol to bind to.
//!
//! The fix is a small C file, generated into `OUT_DIR`, that includes those three headers and
//! returns pointers to the tables from `c_oracle_*` accessor functions. It is compiled with the
//! same flags as the oracle and lands in the same archive. Every array accessor is paired with a
//! length accessor so the Rust side never hardcodes a bound.
//!
//! Three details in that shim are easy to get wrong and each one is handled explicitly:
//!
//! * `trees.h` also *defines* `_dist_code` and `_length_code` as non-`static` globals, so the
//!   shim would duplicate the definitions already in `trees.o`. It is compiled with
//!   `-D_dist_code=c_shim_dist_code -D_length_code=c_shim_length_code` to give its copies
//!   private names -- still `c_`-prefixed, so the archive has no un-prefixed export at all.
//! * `trees.h` references `DIST_CODE_LEN`, which is defined in `trees.c` and appears in no
//!   header, so the shim has to define it itself.
//! * `crc32.h` selects between many `#if`-guarded variants of `crc_braid_table` and
//!   `crc_big_table` using `N`, `W` and `z_word_t`. The shim replicates `crc32.c`'s own
//!   derivation of those three verbatim; deriving them any other way silently compiles the wrong
//!   variant.
//!
//! # Failure posture
//!
//! This script fails loudly. The "library code must not panic" rule constrains library paths and
//! deliberately does not apply here: a build script that swallowed an error to keep the build
//! green would produce an archive with a missing or half-renamed oracle, and every differential
//! test would then pass without comparing anything. A false pass is the worst outcome this crate
//! can produce, so every error path aborts with a diagnostic that names the artifact, the
//! command and the invariant at stake.
//!
//! # Platform notes
//!
//! The rename pass needs `objcopy` (GNU binutils or LLVM). `OBJCOPY` overrides the choice;
//! `objcopy`, `llvm-objcopy` and `gobjcopy` are probed in that order. A toolchain without any of
//! them cannot build this crate, and the script says so explicitly rather than emitting a
//! colliding archive. The archive itself is assembled with the archiver `cc` selects, invoked as
//! `ar crs`, which GNU `ar`, LLVM `ar` and BSD `ar` all accept.

use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Base name of the oracle archive. Deliberately not `z`: `libzlib_c_oracle.a` can never be
/// confused with a system `libz.a` on a link line.
const ARCHIVE_STEM: &str = "zlib_c_oracle";

/// File name of the generated table-exposure shim inside `OUT_DIR`.
const SHIM_FILE_NAME: &str = "zlib_c_oracle_tables.c";

/// File name of the `objcopy --redefine-syms` map inside `OUT_DIR`.
const REDEFINE_MAP_FILE_NAME: &str = "zlib_c_oracle_redefine.map";

/// The prefix every externally visible symbol in the archive must carry.
const SYMBOL_PREFIX: &str = "c_";

/// The fifteen reference translation units, in `Makefile.in` `OBJZ` + `OBJG` order.
const C_SOURCES: [&str; 15] = [
    // OBJZ
    "adler32.c",
    "crc32.c",
    "deflate.c",
    "infback.c",
    "inffast.c",
    "inflate.c",
    "inftrees.c",
    "trees.c",
    "zutil.c",
    // OBJG
    "compress.c",
    "uncompr.c",
    "gzclose.c",
    "gzlib.c",
    "gzread.c",
    "gzwrite.c",
];

/// Every header the oracle or the generated shim consumes, so that editing one rebuilds the
/// archive. `zconf.h` is committed in the tree (it is a `configure`/`CMake` product, but the
/// generated copy is tracked), which is why `-I<repo root>` is sufficient and why this script
/// never runs `configure` -- doing so would overwrite tracked files.
const C_HEADERS: [&str; 11] = [
    "zlib.h",
    "zconf.h",
    "zutil.h",
    "deflate.h",
    "inflate.h",
    "inftrees.h",
    "inffast.h",
    "gzguts.h",
    "crc32.h",
    "trees.h",
    "inffixed.h",
];

/// The 95 public names renamed by the preprocessor, `-D<name>=c_<name>`.
///
/// Derived as `94 + 7 - 6` exactly as the module documentation describes: the 94 unique
/// column-zero `ZEXTERN` names in `zlib.h`, plus the 7 large-file names declared only inside the
/// indented `Z_LARGE64` / `Z_WANT64` block, minus the 6 names `zlib.h` defines as macros.
///
/// The list is sorted, so a diff against a freshly extracted set is a one-line comparison. Do not
/// add `deflateInit`, `deflateInit2`, `inflateInit`, `inflateInit2`, `inflateBackInit` or
/// `gzgetc`: they are macros, and defining them here makes every translation unit fail to
/// compile. `gzgetc` is handled by the `objcopy` pass instead.
///
/// `gzopen_w` is declared only on Windows. Renaming a name no translation unit defines is
/// harmless, and keeping it here means a Windows build needs no separate list.
const PUBLIC_RENAMES: [&str; 95] = [
    "adler32",
    "adler32_combine",
    "adler32_combine64",
    "adler32_z",
    "compress",
    "compress2",
    "compress2_z",
    "compressBound",
    "compressBound_z",
    "compress_z",
    "crc32",
    "crc32_combine",
    "crc32_combine64",
    "crc32_combine_gen",
    "crc32_combine_gen64",
    "crc32_combine_op",
    "crc32_z",
    "deflate",
    "deflateBound",
    "deflateBound_z",
    "deflateCopy",
    "deflateEnd",
    "deflateGetDictionary",
    "deflateInit2_",
    "deflateInit_",
    "deflateParams",
    "deflatePending",
    "deflatePrime",
    "deflateReset",
    "deflateResetKeep",
    "deflateSetDictionary",
    "deflateSetHeader",
    "deflateTune",
    "deflateUsed",
    "get_crc_table",
    "gzbuffer",
    "gzclearerr",
    "gzclose",
    "gzclose_r",
    "gzclose_w",
    "gzdirect",
    "gzdopen",
    "gzeof",
    "gzerror",
    "gzflush",
    "gzfread",
    "gzfwrite",
    "gzgetc_",
    "gzgets",
    "gzoffset",
    "gzoffset64",
    "gzopen",
    "gzopen64",
    "gzopen_w",
    "gzprintf",
    "gzputc",
    "gzputs",
    "gzread",
    "gzrewind",
    "gzseek",
    "gzseek64",
    "gzsetparams",
    "gztell",
    "gztell64",
    "gzungetc",
    "gzvprintf",
    "gzwrite",
    "inflate",
    "inflateBack",
    "inflateBackEnd",
    "inflateBackInit_",
    "inflateCodesUsed",
    "inflateCopy",
    "inflateEnd",
    "inflateGetDictionary",
    "inflateGetHeader",
    "inflateInit2_",
    "inflateInit_",
    "inflateMark",
    "inflatePrime",
    "inflateReset",
    "inflateReset2",
    "inflateResetKeep",
    "inflateSetDictionary",
    "inflateSync",
    "inflateSyncPoint",
    "inflateUndermine",
    "inflateValidate",
    "uncompress",
    "uncompress2",
    "uncompress2_z",
    "uncompress_z",
    "zError",
    "zlibCompileFlags",
    "zlibVersion",
];

/// The 19 names `objcopy --redefine-syms` renames on the object files.
///
/// This is the union of both `local:` blocks in `zlib.map` -- `deflate_copyright`,
/// `inflate_copyright`, `inflate_fast`, `inflate_table`, `zcalloc`, `zcfree`, `z_errmsg`,
/// `gz_error`, `gz_intmax` from `ZLIB_1.2.0` and `inflate_fixed` from `ZLIB_1.3.2` -- together
/// with the `_tr_*` / `_dist_code` / `_length_code` family that `zlib.map` hides there through
/// its `_*` wildcard, plus `gzgetc`, which no `-D` can reach because `gzread.c` undefines it.
///
/// Measured against a real build this is precisely the set of defined external symbols left
/// un-prefixed after `PUBLIC_RENAMES` is applied. The `nm` gate at the end of this script is what
/// keeps that claim true as the reference sources evolve.
const INTERNAL_REDEFINES: [&str; 19] = [
    "_dist_code",
    "_length_code",
    "_tr_align",
    "_tr_flush_bits",
    "_tr_flush_block",
    "_tr_init",
    "_tr_stored_block",
    "_tr_tally",
    "deflate_copyright",
    "gz_error",
    "gz_intmax",
    "gzgetc",
    "inflate_copyright",
    "inflate_fast",
    "inflate_fixed",
    "inflate_table",
    "z_errmsg",
    "zcalloc",
    "zcfree",
];

/// Renames applied to the generated shim only.
///
/// `trees.h` *defines* these two as non-`static` globals, so including it from the shim would
/// duplicate the definitions already emitted by `trees.o`. Renaming the shim's copies keeps the
/// archive linkable, and the `c_shim_` spelling keeps every export in the archive `c_`-prefixed.
const SHIM_LOCAL_RENAMES: [(&str, &str); 2] = [
    ("_dist_code", "c_shim_dist_code"),
    ("_length_code", "c_shim_length_code"),
];

/// The generated table-exposure shim, written verbatim into `OUT_DIR`.
///
/// Kept as one deterministic string constant: the file has no timestamp, no host paths and no
/// configuration-dependent content, so an unchanged build regenerates a byte-identical shim and
/// nothing downstream is invalidated spuriously.
///
/// The `N` / `W` / `z_word_t` block is a verbatim transcription of `crc32.c`, including its
/// `Z_TESTN` and `Z_TESTW` escape hatches. That fidelity is the point: `crc32.h` contains one
/// `crc_braid_table` and one `crc_braid_big_table` per `(N, W)` combination, and the only way to
/// be sure the shim sees the same variant `crc32.c` compiled is to derive `N` and `W` the same
/// way rather than re-deriving them independently.
///
/// The word-typed tables (`crc_big_table`, `crc_braid_big_table`) are handed out as `const void *`
/// with a companion `c_oracle_word_size()`, because their element type is `z_word_t` -- 8 bytes
/// where `W == 8`, 4 bytes where `W == 4`, and non-existent where a braided calculation is
/// compiled out entirely. A `void` pointer plus an explicit element width keeps one stable C
/// signature across all three cases; `crc_braid_table` is `z_crc_t` in every variant and so keeps
/// its concrete type.
const TABLE_SHIM_SOURCE: &str = r#"/*
 * zlib_c_oracle_tables.c -- GENERATED FILE. DO NOT EDIT.
 *
 * Written into OUT_DIR by crates/zlib-rs-differential/build.rs; edit that script instead.
 *
 * Every generated table in the reference implementation is declared `local`, and zutil.h defines
 * `local` as `static`, so none of them has a linkable symbol.  This file includes the three
 * generated-table headers and hands out pointers to the tables through c_oracle_* accessors so
 * that the differential table-equality suite can compare the Rust ports against the C arrays
 * element for element.
 *
 * Every array accessor is paired with a length accessor: the Rust side must never hardcode a
 * bound that the C headers own.
 */

/* zutil.h pulls in zlib.h and zconf.h, and supplies z_crc_t, FAR, local, ZLIB_INTERNAL, uch,
 * Z_U4 and Z_U8. */
#include "zutil.h"
/* deflate.h supplies ct_data, L_CODES, D_CODES, LENGTH_CODES, MIN_MATCH and MAX_MATCH, all of
 * which trees.h uses to dimension its arrays. */
#include "deflate.h"
/* inftrees.h supplies `code`, the element type of the fixed inflate tables. */
#include "inftrees.h"

/* ---------------------------------------------------------------------------------------------
 * N, W and z_word_t, transcribed verbatim from crc32.c.
 *
 * crc32.h holds a separate crc_braid_table and crc_braid_big_table for every (N, W) pair and
 * selects between them with #if.  Deriving N and W any other way here would silently compile a
 * different variant than crc32.c did, and the table comparison would then be meaningless.
 * ------------------------------------------------------------------------------------------- */

#ifdef Z_TESTN
#  define N Z_TESTN
#else
#  define N 5
#endif
#if N < 1 || N > 6
#  error N must be in 1..6
#endif

#ifdef Z_TESTW
#  if Z_TESTW-1 != -1
#    define W Z_TESTW
#  endif
#else
#  if defined(__x86_64__) || defined(__aarch64__)
#    define W 8
#  else
#    define W 4
#  endif
#endif
#ifdef W
#  if W == 8 && defined(Z_U8)
     typedef Z_U8 z_word_t;
#  elif defined(Z_U4)
#    undef W
#    define W 4
     typedef Z_U4 z_word_t;
#  else
#    undef W
#  endif
#endif

#include "crc32.h"

/* DIST_CODE_LEN is defined in trees.c and appears in no header, but trees.h dimensions
 * _dist_code with it. */
#define DIST_CODE_LEN 512
#include "trees.h"

#include "inffixed.h"

/* Element count of a statically sized array.  Applied to a two-dimensional table it yields the
 * row count; applied to [0] of the same table it yields the column count. */
#define ORACLE_LEN(a) ((unsigned)(sizeof(a) / sizeof((a)[0])))

/* --- crc32.h ------------------------------------------------------------------------------- */

const z_crc_t *c_oracle_crc_table(void) { return crc_table; }
unsigned c_oracle_crc_table_len(void) { return ORACLE_LEN(crc_table); }

const z_crc_t *c_oracle_x2n_table(void) { return x2n_table; }
unsigned c_oracle_x2n_table_len(void) { return ORACLE_LEN(x2n_table); }

/* Number of braids.  Always defined, even when the braided path is compiled out. */
unsigned c_oracle_crc_braid_n(void) { return (unsigned)N; }

#ifdef W

/* Bytes per word, and the row count of both braid tables: crc32.c declares them as
 * crc_braid_table[W][256] and crc_braid_big_table[W][256]. */
unsigned c_oracle_crc_braid_w(void) { return (unsigned)W; }
unsigned c_oracle_word_size(void) { return (unsigned)sizeof(z_word_t); }

const void *c_oracle_crc_big_table(void) { return (const void *)crc_big_table; }
unsigned c_oracle_crc_big_table_len(void) { return ORACLE_LEN(crc_big_table); }

const z_crc_t *c_oracle_crc_braid_table(void) { return &crc_braid_table[0][0]; }
unsigned c_oracle_crc_braid_table_rows(void) { return ORACLE_LEN(crc_braid_table); }
unsigned c_oracle_crc_braid_table_cols(void) { return ORACLE_LEN(crc_braid_table[0]); }

const void *c_oracle_crc_braid_big_table(void) { return (const void *)crc_braid_big_table; }
unsigned c_oracle_crc_braid_big_table_rows(void) { return ORACLE_LEN(crc_braid_big_table); }
unsigned c_oracle_crc_braid_big_table_cols(void) { return ORACLE_LEN(crc_braid_big_table[0]); }

#else /* !W -- no braided calculation, so crc32.h defined none of the word tables. */

unsigned c_oracle_crc_braid_w(void) { return 0u; }
unsigned c_oracle_word_size(void) { return 0u; }

const void *c_oracle_crc_big_table(void) { return NULL; }
unsigned c_oracle_crc_big_table_len(void) { return 0u; }

const z_crc_t *c_oracle_crc_braid_table(void) { return NULL; }
unsigned c_oracle_crc_braid_table_rows(void) { return 0u; }
unsigned c_oracle_crc_braid_table_cols(void) { return 0u; }

const void *c_oracle_crc_braid_big_table(void) { return NULL; }
unsigned c_oracle_crc_braid_big_table_rows(void) { return 0u; }
unsigned c_oracle_crc_braid_big_table_cols(void) { return 0u; }

#endif /* W */

/* --- trees.h ------------------------------------------------------------------------------- */

const ct_data *c_oracle_static_ltree(void) { return static_ltree; }
unsigned c_oracle_static_ltree_len(void) { return ORACLE_LEN(static_ltree); }

const ct_data *c_oracle_static_dtree(void) { return static_dtree; }
unsigned c_oracle_static_dtree_len(void) { return ORACLE_LEN(static_dtree); }

const int *c_oracle_base_length(void) { return base_length; }
unsigned c_oracle_base_length_len(void) { return ORACLE_LEN(base_length); }

const int *c_oracle_base_dist(void) { return base_dist; }
unsigned c_oracle_base_dist_len(void) { return ORACLE_LEN(base_dist); }

/* _dist_code and _length_code reach this file renamed to c_shim_dist_code and
 * c_shim_length_code, so that the shim's copies do not clash with the definitions in trees.o. */
const uch *c_oracle_dist_code(void) { return _dist_code; }
unsigned c_oracle_dist_code_len(void) { return ORACLE_LEN(_dist_code); }

const uch *c_oracle_length_code(void) { return _length_code; }
unsigned c_oracle_length_code_len(void) { return ORACLE_LEN(_length_code); }

/* --- inffixed.h ---------------------------------------------------------------------------- */

const code *c_oracle_lenfix(void) { return lenfix; }
unsigned c_oracle_lenfix_len(void) { return ORACLE_LEN(lenfix); }

const code *c_oracle_distfix(void) { return distfix; }
unsigned c_oracle_distfix_len(void) { return ORACLE_LEN(distfix); }

/* --- layout, so the Rust mirrors of these two structs can be checked rather than assumed --- */

unsigned c_oracle_ct_data_size(void) { return (unsigned)sizeof(ct_data); }
unsigned c_oracle_code_size(void) { return (unsigned)sizeof(code); }
"#;

/// Programs probed, in order, when `OBJCOPY` is unset or unusable.
///
/// `gobjcopy` is the name GNU binutils takes on Homebrew, where the system `objcopy` may be
/// absent altogether.
const OBJCOPY_CANDIDATES: [&str; 3] = ["objcopy", "llvm-objcopy", "gobjcopy"];

/// Programs probed, in order, when `NM` is unset or unusable.
const NM_CANDIDATES: [&str; 3] = ["nm", "llvm-nm", "gnm"];

fn main() {
    let manifest_dir = required_env_path("CARGO_MANIFEST_DIR");
    let out_dir = required_env_path("OUT_DIR");
    let repo_root = locate_repo_root(&manifest_dir);

    emit_rerun_directives(&repo_root);

    // Everything generated goes to OUT_DIR. The repository working tree is never written to.
    let shim = write_table_shim(&out_dir);
    let objects = compile_reference_sources(&repo_root, &shim);

    // Rename before archiving: the archive must never exist in a collidable state, not even
    // transiently, or a concurrent link could pick it up.
    let objcopy = locate_objcopy();
    let redefine_map = write_redefine_map(&out_dir);
    apply_symbol_renames(&objcopy, &redefine_map, &objects);

    let archive = archive_objects(&out_dir, &objects);
    verify_every_export_is_prefixed(&archive);

    emit_link_directives(&out_dir);
}

/// Aborts the build with `message`.
///
/// Every failure path in this script routes through here, which is what makes the single
/// `#[allow]` below sufficient and auditable.
///
/// `clippy::panic` is denied across the workspace, and rightly so for library code: a compression
/// library is linked into processes that must not die on malformed input, so it returns
/// `ReturnCode` instead. A build script is the opposite case. If the oracle cannot be built,
/// renamed and archived exactly as specified, then a differential test that "passes" has compared
/// nothing at all -- the worst outcome this crate can produce. Aborting the build is therefore the
/// correct behaviour, and swallowing an error to keep the build green would be the defect.
#[allow(clippy::panic)]
fn fail(message: &str) -> ! {
    panic!("{message}");
}

/// Reads an environment variable cargo is contractually required to set.
///
/// Uses the `OsString` form rather than the `String` form so a non-UTF-8 build directory is not a
/// spurious failure.
fn required_env_path(name: &str) -> PathBuf {
    let Some(value) = env::var_os(name) else {
        fail(&format!(
            "build.rs: the environment variable `{name}` is not set. This file is a cargo build \
             script and cargo is what supplies it; running it directly will not work."
        ))
    };
    PathBuf::from(value)
}

/// Derives the repository root from `CARGO_MANIFEST_DIR` and then proves the guess.
///
/// This crate lives at `<repo root>/crates/zlib-rs-differential`, so the root is two levels up.
/// The path is never hardcoded and the process working directory is never consulted, because
/// cargo does not promise what it will be.
///
/// The existence check is not ceremony. A silently wrong root would make `cc` fail with fifteen
/// "no such file" errors that say nothing about the cause, whereas this reports the resolved path
/// and the missing files.
fn locate_repo_root(manifest_dir: &Path) -> PathBuf {
    let Some(root) = manifest_dir.parent().and_then(Path::parent) else {
        fail(&format!(
            "build.rs: CARGO_MANIFEST_DIR is `{}`, which does not have the two parent \
             directories that this crate's location, <repo root>/crates/zlib-rs-differential, \
             implies. The repository root cannot be derived from it.",
            manifest_dir.display()
        ))
    };

    let mut missing: Vec<&str> = Vec::new();
    for name in C_SOURCES.into_iter().chain(C_HEADERS) {
        if !root.join(name).is_file() {
            missing.push(name);
        }
    }

    if !missing.is_empty() {
        fail(&format!(
            "build.rs: resolved the repository root to `{}` (two levels above \
             CARGO_MANIFEST_DIR `{}`), but {} of the required reference file(s) are not there: \
             {}. The differential oracle is compiled from the in-tree C sources, so this crate \
             must stay at <repo root>/crates/zlib-rs-differential.",
            root.display(),
            manifest_dir.display(),
            missing.len(),
            join_names(&missing)
        ));
    }

    root.to_path_buf()
}

/// Declares every input whose modification must rebuild the oracle.
///
/// Emitting any `rerun-if-changed` at all switches cargo from "rerun when anything in the package
/// changed" to "rerun when one of these changed", so the list has to be complete: all fifteen
/// translation units, all eleven headers they and the shim include, and this script itself.
fn emit_rerun_directives(repo_root: &Path) {
    println!("cargo::rerun-if-changed=build.rs");
    for name in C_SOURCES.into_iter().chain(C_HEADERS) {
        println!("cargo::rerun-if-changed={}", repo_root.join(name).display());
    }
}

/// Writes the generated table shim into `OUT_DIR` and returns its path.
fn write_table_shim(out_dir: &Path) -> PathBuf {
    let path = out_dir.join(SHIM_FILE_NAME);
    if let Err(error) = fs::write(&path, TABLE_SHIM_SOURCE) {
        fail(&format!(
            "build.rs: could not write the generated table shim to `{}`: {error}",
            path.display()
        ));
    }
    path
}

/// The flags shared by the oracle and the shim, so the two can never drift apart.
///
/// `opt_level` and `pic` are set explicitly rather than inherited from `OPT_LEVEL`, because the
/// reference build is `-O3 -fPIC` regardless of which cargo profile happens to be driving the
/// build. `debug` is deliberately left at `cc`'s default, which follows the profile: debug info is
/// output-neutral, and having it in a dev build is what makes an AddressSanitizer backtrace
/// through the oracle readable.
///
/// `warnings` is likewise left at `cc`'s default, which adds `-Wall -Wextra`. The reference
/// sources compile clean under both, so the extra scrutiny costs nothing and would report a real
/// regression in the C tree.
fn reference_build(repo_root: &Path) -> cc::Build {
    let mut build = cc::Build::new();
    build
        .include(repo_root)
        .define("_LARGEFILE64_SOURCE", "1")
        .define("HAVE_HIDDEN", None)
        .opt_level(3)
        .pic(true);
    build
}

/// Compiles the fifteen reference translation units and the generated shim into `OUT_DIR`.
///
/// `compile_intermediates` is used instead of `compile` on purpose: it stops after the objects and
/// emits no link metadata, which leaves room for the `objcopy` pass before anything is archived.
///
/// The two `cc::Build`s cannot collide in `OUT_DIR` even though the shim and `crc32.c` would both
/// like to be `crc32.o`-style names, because `cc` derives each object name from a hash of its
/// source's *directory* and the two live in different directories.
fn compile_reference_sources(repo_root: &Path, shim: &Path) -> Vec<PathBuf> {
    let mut oracle = reference_build(repo_root);
    for name in C_SOURCES {
        oracle.file(repo_root.join(name));
    }
    for name in PUBLIC_RENAMES {
        oracle.define(name, prefixed(name).as_str());
    }
    let mut objects = oracle.compile_intermediates();

    let mut shim_build = reference_build(repo_root);
    shim_build.file(shim);
    for (original, replacement) in SHIM_LOCAL_RENAMES {
        shim_build.define(original, replacement);
    }
    objects.extend(shim_build.compile_intermediates());

    let expected = C_SOURCES.len() + 1;
    if objects.len() != expected {
        fail(&format!(
            "build.rs: expected {expected} object files (fifteen reference translation units plus \
             the generated table shim) but the C compiler produced {}. The oracle would be \
             incomplete, so the build cannot continue.",
            objects.len()
        ));
    }

    objects
}

/// Returns `name` with the archive's mandatory symbol prefix applied.
fn prefixed(name: &str) -> String {
    let mut renamed = String::with_capacity(SYMBOL_PREFIX.len() + name.len());
    renamed.push_str(SYMBOL_PREFIX);
    renamed.push_str(name);
    renamed
}

/// Finds an `objcopy`, honouring `$OBJCOPY` first.
///
/// A missing `objcopy` is fatal rather than something to work around. Without the object-level
/// rename the archive would define `gzgetc`, `inflate_table` and the whole `ZLIB_INTERNAL` set
/// under their real names, and any test binary that also links `libz-rs-sys` would fail with
/// duplicate symbols -- or, worse, silently bind the wrong definition.
fn locate_objcopy() -> OsString {
    println!("cargo::rerun-if-env-changed=OBJCOPY");

    if let Some(explicit) = env::var_os("OBJCOPY").filter(|value| !value.is_empty()) {
        if !can_run(&explicit) {
            fail(&format!(
                "build.rs: OBJCOPY is set to `{}`, but that program could not be executed. \
                 Point it at a working GNU or LLVM objcopy, or unset it to let this script probe \
                 for one.",
                Path::new(&explicit).display()
            ));
        }
        return explicit;
    }

    for candidate in OBJCOPY_CANDIDATES {
        let program = OsString::from(candidate);
        if can_run(&program) {
            return program;
        }
    }

    fail(&format!(
        "build.rs: no usable objcopy was found. Tried $OBJCOPY, then {}. One of them is required: \
         it is the only way to rename gzgetc -- gzread.c undefines the macro, so no -D can reach \
         it -- and the ZLIB_INTERNAL symbols that zlib.map hides, notably inflate_table, which \
         libz.a deliberately keeps for test/infcover.c's cover_trees(). Install GNU binutils or \
         the LLVM tools and rebuild.",
        OBJCOPY_CANDIDATES.join(", ")
    ))
}

/// Finds an `nm`, honouring `$NM` first. `None` means the symbol audit has to be skipped.
fn locate_nm() -> Option<OsString> {
    if let Some(explicit) = env::var_os("NM").filter(|value| !value.is_empty()) {
        if can_run(&explicit) {
            return Some(explicit);
        }
        println!(
            "cargo::warning=zlib-rs-differential: NM is set to `{}`, which could not be \
             executed; probing for another nm instead.",
            Path::new(&explicit).display()
        );
    }

    for candidate in NM_CANDIDATES {
        let program = OsString::from(candidate);
        if can_run(&program) {
            return Some(program);
        }
    }

    None
}

/// Reports whether `program` can be executed, without letting its output into the build log.
fn can_run(program: &OsStr) -> bool {
    Command::new(program)
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

/// Writes the `objcopy --redefine-syms` map into `OUT_DIR` and returns its path.
///
/// One `old new` pair per line, which is the format `--redefine-syms` expects.
fn write_redefine_map(out_dir: &Path) -> PathBuf {
    let mut contents = String::new();
    for name in INTERNAL_REDEFINES {
        contents.push_str(name);
        contents.push(' ');
        contents.push_str(SYMBOL_PREFIX);
        contents.push_str(name);
        contents.push('\n');
    }

    let path = out_dir.join(REDEFINE_MAP_FILE_NAME);
    if let Err(error) = fs::write(&path, contents) {
        fail(&format!(
            "build.rs: could not write the objcopy symbol map to `{}`: {error}",
            path.display()
        ));
    }
    path
}

/// Rewrites the residual symbol names in every object, in place.
///
/// `--redefine-syms` rewrites undefined references as well as definitions, so the objects stay
/// linked to each other: after this pass `deflate.o` calls `c__tr_init`, not `_tr_init`. The pass
/// is idempotent, because a second run finds no un-prefixed name left to match, and a name absent
/// from a given object is simply ignored -- which is why one map can be applied to all sixteen.
fn apply_symbol_renames(objcopy: &OsStr, redefine_map: &Path, objects: &[PathBuf]) {
    let mut flag = OsString::from("--redefine-syms=");
    flag.push(redefine_map);

    for object in objects {
        match Command::new(objcopy).arg(&flag).arg(object).status() {
            Ok(status) if status.success() => {}
            Ok(status) => fail(&format!(
                "build.rs: `{} --redefine-syms={} {}` exited with {status}. Without the rename \
                 the oracle archive collides with libz-rs-sys, so the build cannot continue.",
                Path::new(objcopy).display(),
                redefine_map.display(),
                object.display()
            )),
            Err(error) => fail(&format!(
                "build.rs: could not execute `{}` to rename the symbols in `{}`: {error}",
                Path::new(objcopy).display(),
                object.display()
            )),
        }
    }
}

/// Bundles the renamed objects into `OUT_DIR/lib<ARCHIVE_STEM>.a`.
///
/// The archive is removed first. `ar` in replace mode updates members it recognises but leaves
/// unknown ones behind, so a stale member from an earlier source list would otherwise survive and
/// quietly contribute a second definition.
fn archive_objects(out_dir: &Path, objects: &[PathBuf]) -> PathBuf {
    let archive = out_dir.join(format!("lib{ARCHIVE_STEM}.a"));
    remove_if_present(&archive);

    let mut archiver = match cc::Build::new().try_get_archiver() {
        Ok(command) => command,
        Err(error) => fail(&format!(
            "build.rs: could not determine which archiver to bundle the oracle objects with: \
             {error}"
        )),
    };

    // Mirrors what `cc` does for its own archives: the Apple archiver zeroes member timestamps
    // when this is set, so a rebuild from unchanged inputs produces an identical archive.
    archiver.env("ZERO_AR_DATE", "1");
    // `c` create, `r` replace, `s` write the symbol index. GNU ar, LLVM ar and BSD ar all accept
    // this spelling, and the index is what lets the linker resolve members without a ranlib pass.
    archiver.arg("crs").arg(&archive).args(objects);

    match archiver.status() {
        Ok(status) if status.success() => {}
        Ok(status) => fail(&format!(
            "build.rs: archiving the oracle objects into `{}` exited with {status}.",
            archive.display()
        )),
        Err(error) => fail(&format!(
            "build.rs: could not execute the archiver to create `{}`: {error}",
            archive.display()
        )),
    }

    archive
}

/// Deletes `path` if it exists, and fails loudly if it exists but cannot be deleted.
fn remove_if_present(path: &Path) {
    if let Err(error) = fs::remove_file(path) {
        let already_absent = error.kind() == io::ErrorKind::NotFound;
        if !already_absent {
            fail(&format!(
                "build.rs: could not remove the stale artifact `{}`: {error}",
                path.display()
            ));
        }
    }
}

/// The collision-freedom gate: every defined external symbol in the archive must be prefixed.
///
/// This is what keeps the two rename lists honest as the reference sources evolve. A newly
/// exported C symbol shows up here as a precise diagnostic naming the symbol and the list to add
/// it to, instead of as a `multiple definition of ...` from the linker weeks later.
///
/// Names beginning with a double underscore are reported but tolerated: that spelling is reserved
/// to the implementation, zlib defines no such symbol, and toolchains legitimately emit their own
/// -- 32-bit x86 `-fPIC` builds, for instance, emit `__x86.get_pc_thunk.bx`. Nothing of zlib's can
/// hide behind the allowance, and anything that appears is surfaced as a warning rather than
/// passed over in silence.
///
/// A missing `nm` downgrades the check to a warning rather than failing the build: unlike
/// `objcopy`, `nm` only *observes*, so its absence cannot produce a wrong archive.
fn verify_every_export_is_prefixed(archive: &Path) {
    println!("cargo::rerun-if-env-changed=NM");

    let Some(nm) = locate_nm() else {
        println!(
            "cargo::warning=zlib-rs-differential: no usable nm was found, so the oracle \
             archive's exports could not be audited for the `{SYMBOL_PREFIX}` prefix. An \
             incomplete rename will surface later as a duplicate-symbol link error."
        );
        return;
    };

    // POSIX format puts the symbol name first, which parses identically for objects and archives
    // and is understood by both GNU nm and llvm-nm.
    let listing = match Command::new(&nm)
        .arg("--defined-only")
        .arg("--extern-only")
        .arg("--format=posix")
        .arg(archive)
        .output()
    {
        Ok(output) if output.status.success() => output.stdout,
        Ok(output) => fail(&format!(
            "build.rs: `{} --defined-only --extern-only --format=posix {}` exited with {}: {}",
            Path::new(&nm).display(),
            archive.display(),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )),
        Err(error) => fail(&format!(
            "build.rs: could not execute `{}` to audit `{}`: {error}",
            Path::new(&nm).display(),
            archive.display()
        )),
    };

    let listing = String::from_utf8_lossy(&listing);
    let mut unprefixed: Vec<&str> = Vec::new();
    let mut toolchain: Vec<&str> = Vec::new();
    for line in listing.lines() {
        let mut fields = line.split_whitespace();
        let Some(symbol) = fields.next() else {
            continue;
        };
        // POSIX output interleaves `<archive>[<member>]:` header lines, which carry a single
        // field; a real symbol line always has at least a name and a type.
        if fields.next().is_none() || symbol.starts_with(SYMBOL_PREFIX) {
            continue;
        }
        if symbol.starts_with("__") {
            toolchain.push(symbol);
        } else {
            unprefixed.push(symbol);
        }
    }

    if !toolchain.is_empty() {
        toolchain.sort_unstable();
        toolchain.dedup();
        println!(
            "cargo::warning=zlib-rs-differential: the oracle archive exports {} symbol(s) \
             reserved to the toolchain, left unrenamed because zlib defines no `__`-prefixed \
             name: {}",
            toolchain.len(),
            join_names(&toolchain)
        );
    }

    if !unprefixed.is_empty() {
        unprefixed.sort_unstable();
        unprefixed.dedup();
        fail(&format!(
            "build.rs: {} symbol(s) exported by `{}` do not carry the mandatory \
             `{SYMBOL_PREFIX}` prefix: {}. Each one will collide with libz-rs-sys when both are \
             linked into a test binary. Add every public name to PUBLIC_RENAMES and every \
             ZLIB_INTERNAL name to INTERNAL_REDEFINES in this script.",
            unprefixed.len(),
            archive.display(),
            join_names(&unprefixed)
        ));
    }
}

/// Formats a name list for a diagnostic.
fn join_names(names: &[&str]) -> String {
    names.join(", ")
}

/// Publishes the archive to the crate's own targets: the library, the tests and the attached
/// benches.
///
/// Linking it crate-wide rather than for tests alone is harmless -- the crate is dev-only, so
/// nothing shipped is affected, and the linker pulls no member that nothing references.
fn emit_link_directives(out_dir: &Path) {
    println!("cargo::rustc-link-search=native={}", out_dir.display());
    println!("cargo::rustc-link-lib=static={ARCHIVE_STEM}");
}
