//! Library introspection: the four exports that describe the library rather than
//! compress with it.
//!
//! | Export | `zlib.h` | Ported from |
//! |---|---|---|
//! | [`zlibVersion`] | L224 | `zlibVersion`, `zutil.c` L27-L29 |
//! | [`zlibCompileFlags`] | L1216 | `zlibCompileFlags`, `zutil.c` L31-L121 |
//! | [`zError`] | L2033 | `zError`, `zutil.c` L139-L141 |
//! | [`get_crc_table`] | L2035 | `get_crc_table`, `crc32.c` L482-L487 |
//!
//! Between them they answer three questions a caller may ask before it trusts the
//! library with data: *which* library is this, *how* was it built, and *what* did
//! that status code mean. `get_crc_table` is the odd one out and is here rather
//! than with the checksums because it introspects the CRC-32 *table* rather than
//! computing a checksum; `crc32.c` L478-L481 gives its two purposes, and the
//! second of them -- forcing table generation before threads start -- is precisely
//! the thing this port has designed out.
//!
//! Nothing in this module touches a stream, allocates, locks, or performs a system
//! call. Every one of the four is a pure function of compile-time data, which is
//! also why every one of them is trivially thread-safe and reentrant.
//!
//! # ★ This module contains no `unsafe`, and that is a result rather than an
//! # accident
//!
//! The crate documentation lists six categories of site at which the FFI contract
//! *forces* unsafety, and returning a C string is category 5. It would therefore
//! be entirely defensible for the two string-returning exports here to carry an
//! `unsafe` block with a `// SAFETY:` comment. They do not, because none is
//! needed: [`CStr::as_ptr`] and `<[T]>::as_ptr` are safe operations. Producing a
//! pointer is safe; only *dereferencing* one is not, and nothing here dereferences
//! anything.
//!
//! The invariant such a comment would have had to state is instead discharged
//! structurally, by the types:
//!
//! * every pointer this module returns comes from a `'static` item, so it outlives
//!   any possible use by a caller -- there is no lifetime to get wrong;
//! * every string is a `c"..."` literal, so the NUL terminator is placed by the
//!   compiler and cannot be forgotten, and [`CStr`] additionally guarantees there
//!   is no interior NUL;
//! * no export can return a null pointer, because [`CStr::as_ptr`] and
//!   `<[T]>::as_ptr` on a reference never yield one. That matters concretely for
//!   [`zError`], whose two empty table slots are `c""` -- a pointer to a readable
//!   `"\0"` -- and not `NULL`, so a caller that passes the result straight to
//!   `printf` is safe for every input;
//! * every returned pointer is stable across calls: the same address every time,
//!   because it is the address of one compiler-materialised constant.
//!
//! So the audit statement for this file is the strongest one available -- there is
//! no unsafe block to review -- and a future change that introduces one should be
//! read as a sign that something has been modelled wrongly.
//!
//! # Panic discipline
//!
//! Each export routes its body through [`panic_guard::guard`], which aborts rather
//! than letting an unwind cross into a C caller. That is defence in depth and not
//! a working error path: the four bodies read constants and cannot fail, so there
//! is no reachable panic for the guard to catch. Note that the guard **diverges**
//! on the panic path -- it has no recovering variant by design -- so these exports
//! have no fallback return value, and none is needed. Every export is declared
//! `extern "C"`, never `extern "C-unwind"`.
//!
//! # Dependencies
//!
//! `core` plus [`crate::types`], [`crate::panic_guard`] and `zlib_rs`. The two
//! items taken from the safe core, [`zlib_rs::error::err_msg`] and
//! [`zlib_rs::crc32::get_crc_table`], are addressed by their full module paths
//! because the crate-root re-exports in `zlib-rs` are all gated behind that
//! crate's `rust-api` feature, which this crate does not enable.
//!
//! # ★ Version identity: never from Cargo
//!
//! `zlib.h` L44 fixes the version string at `"1.3.2.1-motley"`. That is four
//! components plus a suffix, which is **not valid Cargo semver**, so the crate
//! version cannot be its source: `env!("CARGO_PKG_VERSION")` yields the
//! three-component `1.3.2` that the workspace manifest is obliged to declare, and
//! substituting it would make [`zlibVersion`] disagree with the header every
//! consumer compiled against. The literal in [`ZLIB_VERSION`] is therefore the
//! single source of truth in this crate, and the root manifest records the same
//! rule beside its own `version` key and again under
//! `[workspace.metadata.zlib] c_version`.
//!
//! The consequence is visible immediately. `test/example.c` L502-L510, compiled
//! unmodified against this library, does:
//!
//! ```c
//! static const char* myVersion = ZLIB_VERSION;
//! if (zlibVersion()[0] != myVersion[0]) {
//!     fprintf(stderr, "incompatible zlib version\n");
//!     exit(1);
//! } else if (strcmp(zlibVersion(), ZLIB_VERSION) != 0) {
//!     fprintf(stderr, "warning: different zlib version linked: %s\n", zlibVersion());
//! }
//! ```
//!
//! A differing **first character** is fatal; a differing full string is a warning.
//! `deflateInit_`, `inflateInit_` and `inflateBackInit_` apply the same
//! first-character test to the caller's own compile-time string and return
//! `Z_VERSION_ERROR` when it fails. Both properties are pinned by compile-time
//! assertions below, so a hand edit that broke either one would not build.
//!
//! # ★ `zlibCompileFlags` is computed, never copied
//!
//! The reference build on `x86_64-unknown-linux-gnu` returns `0xa9`, measured
//! directly. That number is *not* written down here, because it is a property of
//! the target and not of the library: on LLP64 Windows `uLong` is four bytes
//! rather than eight, which changes bits 2-3 and therefore the whole value. The
//! four size fields are computed from this build's own `size_of` results, and the
//! configuration bits are named constants that a maintainer can see, one per line,
//! rather than a magic total.
//!
//! The full ladder, transcribed from `zutil.c` L31-L121 and documented at
//! `zlib.h` L1216-L1256. "Size" rows encode `2 -> 0`, `4 -> 1`, `8 -> 2`,
//! anything else `-> 3`:
//!
//! | Bits | C condition | This build | Why |
//! |---|---|---|---|
//! | 0-1 | `sizeof(uInt)` | `1` | 4 bytes |
//! | 2-3 | `sizeof(uLong)` | `2` | 8 bytes on LP64; **`1` on LLP64 Windows** |
//! | 4-5 | `sizeof(voidpf)` | `2` | 8 bytes |
//! | 6-7 | `sizeof(z_off_t)` | `2` | 8 bytes under `_LARGEFILE64_SOURCE` |
//! | 8 | `ZLIB_DEBUG` | 0 | no debug build of this port defines it |
//! | 9 | `ASMV`/`ASMINF` | 0 | ★ unreachable in C too -- see below |
//! | 10 | `ZLIB_WINAPI` | 0 | the exports use the C calling convention |
//! | 11 | -- | 0 | reserved (`zlib.h` L1229) |
//! | 12 | `BUILDFIXED` | 0 | the fixed tables are `const` data |
//! | 13 | `DYNAMIC_CRC_TABLE` | 0 | the CRC tables are `const` data |
//! | 14, 15 | -- | 0 | reserved (`zlib.h` L1234) |
//! | 16 | `NO_GZCOMPRESS` | 0, or **1 without the `gz` feature** | the gz layer can compress -- when it is compiled at all |
//! | 17 | `NO_GZIP` | 0 | gzip streams are written and detected; no feature removes this |
//! | 18, 19 | -- | 0 | reserved (`zlib.h` L1242) |
//! | 20 | `PKZIP_BUG_WORKAROUND` | 0 | not implemented, as in the reference |
//! | 21 | `FASTEST` | 0 | all ten levels exist |
//! | 22, 23 | -- | 0 | reserved (`zlib.h` L1247) |
//! | 24 | not `STDC`/`stdarg` | 0 | the variadic entry point is the `stdarg` one |
//! | 25 | insecure `*printf` | 0 | the formatter is bounded |
//! | 26 | void-returning `*printf` | 0 | the formatter reports a length |
//! | 27 | `gzprintf` absent | 0, or **1 without the `gz` feature** | `gzprintf` is present when the gz layer is |
//! | 28-31 | -- | 0 | reserved (`zlib.h` L1255) |
//!
//! ## Two of those rows are derived from the resolved feature set
//!
//! Bits 16 and 27 are the only configuration bits this crate can set, and they move
//! together because one Cargo feature -- `gz` -- governs both `gzwrite` and
//! `gzprintf`. In the default build (`gz` on, which is what ships) both are clear
//! and the total on LP64 is exactly the reference `0xa9`; in the supported
//! `libz-compat`-without-`gz` build the gz module is compiled out entirely and both
//! are set, because a flag word that advertises functions the artifact does not
//! contain is worse than useless.
//!
//! Bit 17 deliberately does NOT move with them: `NO_GZIP` describes `deflate`
//! writing and `inflate` decoding the RFC 1952 *container* (`windowBits` 25-31, and
//! `+32` auto-detection), which lives in the core's deflate and inflate and which no
//! feature in this workspace removes. Confusing the gzip wrapper with the `gzFile`
//! layer is the mistake this row exists to prevent.
//!
//! Three of the remaining zeros are determinations made elsewhere in the workspace
//! and are cited rather than assumed:
//!
//! * **bit 12** is clear because `crates/zlib-rs/src/inflate/fixed_tables.rs`
//!   transcribes `inffixed.h` as `const` data, so the fixed Huffman decode tables
//!   are never built at run time. That module says so itself, and names the cost:
//!   about 2K of extra `.rodata`, which is exactly the trade `inftrees.c`
//!   L359-L360 describes;
//! * **bit 13** is clear because `crates/zlib-rs/src/crc32/tables.rs` is pure
//!   `const` data -- no `OnceLock`, no `Once`, no `static mut`, no interior
//!   mutability. This is the honest answer and also the desirable one:
//!   `crc32.c` L13-L17 warns that "there is no mutex or semaphore protection on
//!   the static variables used to control the first-use generation of the crc
//!   tables", which is the hazard `const` evaluation removes outright, together
//!   with `zutil.h`'s `<stdatomic.h>` dependency;
//! * **bits 24, 25, 26 and 27** are clear because
//!   `crates/zlib-rs/src/gz/printf.rs` implements only the bounded formatting
//!   path. Rust always has a bounded formatter, so there is no `NO_vsnprintf`
//!   condition to report, no `ZLIB_INSECURE` opt-in to the unbounded
//!   `vsprintf`, and no void-returning variant. That module records the
//!   requirement and asks for the answer to be computed here rather than copied.
//!
//! ## ★ Bit 9 can never be set, in C either
//!
//! `zutil.c` L62-L66 reads:
//!
//! ```c
//! /*
//! #if defined(ASMV) || defined(ASMINF)
//!     flags += 1 << 9;
//! #endif
//!  */
//! ```
//!
//! The preprocessor conditional is inside a block comment, so no C build --
//! including one compiled with `-DASMV` -- can reach it. Bit 9 is consequently a
//! hard zero here, with no settable constant of its own, and it is listed in the
//! private `FORBIDDEN_CONFIGURATION_BITS` mask so that a future edit cannot set it
//! by accident. `zlib.h` L1228 still documents the bit's *meaning*, which is why it
//! appears in the table above rather than being dismissed as reserved.
//!
//! ## Where a wrong value would and would not be noticed
//!
//! `test/example.c` L512-L513 only *prints* this number, so a wrong one is visible
//! and not fatal -- which makes it easy to miss. The tests at the foot of this
//! module decode the value field by field, and the differential and symbol-parity
//! suites compare it against the C oracle.
//!
//! # Symbol surface
//!
//! Exactly four of the library's ninety-five exported functions live here.
//! `zlibVersion`, `zError` and `get_crc_table` are undecorated base-set names;
//! `zlibCompileFlags` is decorated `zlibCompileFlags@@ZLIB_1.2.0.2`, because
//! `zlib.map` L22-L26 places it in that node. The decoration is applied by the
//! `--version-script` link, not by anything in this file.
//!
//! The table behind [`zError`] stays hidden. `zlib.map` L9-L19 lists `z_errmsg`
//! in the `local:` block of its `ZLIB_1.2.0` node, so the C array is not an
//! exported symbol; the safe core keeps its copy `pub(crate)` and this module's
//! NUL-terminated mirror is a private constant. Neither is `#[no_mangle]`, and
//! neither may become so.
//!
//! # A note for the header gate
//!
//! cbindgen cannot render a string constant in any spelling -- `&str`, `&CStr`
//! and `&[u8; N]` are all skipped with "Unsupported literal expression" -- so
//! [`ZLIB_VERSION`] produces a skip warning and no `#define`. The five numeric
//! version constants do render, as decimal `#define`s; four match `zlib.h`
//! exactly and [`ZLIB_VERNUM`] differs only in radix, `4897` against `0x1321`.
//! A header diff therefore has to supply `ZLIB_VERSION` from `zlib.h`'s own text
//! and compare `ZLIB_VERNUM` numerically. This is recorded here so that the
//! difference is understood as a limitation of the generator rather than a defect
//! in the contract.

use core::ffi::{c_char, c_int, CStr};

use crate::panic_guard;
use crate::types::{uInt, uLong, voidpf, z_crc_t, z_off_t};

// ---------------------------------------------------------------------------
// Version identity -- `zlib.h` L44-L49
// ---------------------------------------------------------------------------

/// The library's self-reported version, NUL-terminated -- `zlib.h` L44.
///
/// The single source of truth for the version string in this crate, and the exact
/// bytes [`zlibVersion`] hands back. Written as a `c"..."` literal so that the
/// terminator is the compiler's responsibility and [`CStr`] can guarantee there is
/// no interior NUL.
///
/// **Never derive this from `env!("CARGO_PKG_VERSION")`.** See the module
/// documentation: `1.3.2.1-motley` is not valid Cargo semver, so the manifest
/// declares `1.3.2` and the two cannot be the same datum.
/// cbindgen:ignore
pub const ZLIB_VERSION: &CStr = c"1.3.2.1-motley";

/// The version as a single packed integer, one nibble per component -- `zlib.h` L45.
///
/// `0x1321` is `1.3.2.1`. Callers use it for compile-time comparisons, as in
/// `#if ZLIB_VERNUM >= 0x1290`, which is why it is a literal here rather than
/// something derived from [`ZLIB_VERSION`] at run time.
pub const ZLIB_VERNUM: c_int = 0x1321;

/// Major version -- `zlib.h` L46. The component whose *first character* the init
/// entry points and `test/example.c` compare, and the only one whose mismatch is
/// treated as fatal.
pub const ZLIB_VER_MAJOR: c_int = 1;

/// Minor version -- `zlib.h` L47.
pub const ZLIB_VER_MINOR: c_int = 3;

/// Revision -- `zlib.h` L48.
pub const ZLIB_VER_REVISION: c_int = 2;

/// Sub-revision -- `zlib.h` L49.
pub const ZLIB_VER_SUBREVISION: c_int = 1;

/// The ASCII digit that [`ZLIB_VER_MAJOR`] is spelled as inside [`ZLIB_VERSION`].
///
/// This is the byte the init entry points compare a caller's compile-time version
/// against: `crate::inflate::version_error` reads it directly, which is what makes
/// this constant the single spelling of the major-version character rather than a
/// duplicate of one derived inside that function.
///
/// Written as a character literal rather than computed with an `as` cast from
/// `c_int`: a narrowing cast is exactly what the workspace lint policy asks to be
/// justified individually, and there would be nothing to justify -- the major
/// version is one decimal digit and writing it as one is clearer than deriving it.
/// The assertions below are what make the pairing checked rather than assumed: one
/// ties this byte to [`ZLIB_VERSION`]'s first byte, the other to [`ZLIB_VER_MAJOR`].
///
/// ★ **This is the byte the two version gates compare against**, which is why it is
/// `pub(crate)` rather than private. `deflateInit_`, `inflateInit_` and
/// `inflateBackInit_` must decide whether a caller's compile-time `ZLIB_VERSION`
/// agrees with the library's, and `zlib.h` L225-L229 defines the test as "if the
/// first character differs, the library code actually used is not compatible with the
/// `zlib.h` header file used by the application". Both gates
/// (`crate::deflate::version_is_compatible` and `crate::inflate::version_error`) read
/// this constant, so the answer comes from one assertion-backed place instead of
/// being re-derived -- fallibly, since `.to_bytes().first()` returns an [`Option`] --
/// at each site.
///
/// Those two readers are also what keep the declared 1.80 floor warning-clean, and
/// the distinction matters: rustc 1.80's dead-code pass does NOT count a use that
/// occurs only inside an anonymous `const _: () = assert!(...)` item, so a version of
/// this constant that only the assertions below referenced was reported as
/// `never used` at the floor while passing on current stable. Liveness here comes
/// from production code reachable from a `#[no_mangle]` export, not from the
/// assertions -- which is why no `#[allow(dead_code)]` is needed. `make rust-msrv` is
/// the gate that keeps it that way.
/// cbindgen:ignore
pub(crate) const ZLIB_VER_MAJOR_DIGIT: u8 = b'1';

// `zlib.h` keeps L44-L49 consistent by hand. Here the consistency is mechanical:
// each assertion below is a build failure rather than a review note.

// 1. `ZLIB_VERNUM` packs the four components a nibble each, most significant
//    first. This is the relationship that lets a caller's `#if ZLIB_VERNUM >= ...`
//    mean what the dotted string says.
/// cbindgen:ignore
const _: () = assert!(
    ZLIB_VERNUM
        == (ZLIB_VER_MAJOR << 12)
            | (ZLIB_VER_MINOR << 8)
            | (ZLIB_VER_REVISION << 4)
            | ZLIB_VER_SUBREVISION
);

// 2. Every component is a single decimal digit, which is what makes the nibble
//    packing above lossless and the dotted spelling below unambiguous. Written as
//    range patterns rather than pairs of comparisons: `RangeInclusive::contains` is
//    not `const`, so the idiom clippy would otherwise ask for is unavailable here.
/// cbindgen:ignore
const _: () = assert!(matches!(ZLIB_VER_MAJOR, 0..=9));
/// cbindgen:ignore
const _: () = assert!(matches!(ZLIB_VER_MINOR, 0..=9));
/// cbindgen:ignore
const _: () = assert!(matches!(ZLIB_VER_REVISION, 0..=9));
/// cbindgen:ignore
const _: () = assert!(matches!(ZLIB_VER_SUBREVISION, 0..=9));

// 3. The version string is exactly the fourteen bytes of "1.3.2.1-motley". A
//    truncated or extended literal would otherwise pass every other check here.
//    `to_bytes().len()` rather than `count_bytes()`: the latter is `const` only
//    from Rust 1.81 and this workspace declares a 1.80 floor.
/// cbindgen:ignore
const _: () = assert!(ZLIB_VERSION.to_bytes().len() == 14);

// 4. Its first byte is the major version digit. This is the byte `test/example.c`
//    L502-L505 compares before doing anything else, and the one whose mismatch
//    makes `deflateInit_`, `inflateInit_` and `inflateBackInit_` return
//    `Z_VERSION_ERROR`. Matched as a pattern rather than indexed, so the check
//    needs neither `clippy::indexing_slicing` relief nor a panicking path.
/// cbindgen:ignore
const _: () = assert!(matches!(
    ZLIB_VERSION.to_bytes().first(),
    Some(&ZLIB_VER_MAJOR_DIGIT)
));

// 5. `ZLIB_VER_MAJOR_DIGIT` really is `ZLIB_VER_MAJOR` rendered in ASCII, so
//    assertion 4 is a statement about the declared major version and not merely
//    about an unrelated byte that happens to be there.
/// cbindgen:ignore
const _: () = assert!(ZLIB_VER_MAJOR_DIGIT == b'0' + 1 && ZLIB_VER_MAJOR == 1);

/// Returns the library's version string -- `zlib.h` L224.
///
/// The pointer addresses [`ZLIB_VERSION`], by way of the crate-private
/// `CHECKED_VERSION` -- spelled without a link because it is not part of the public
/// surface: `'static`, NUL-terminated, never null, and the same address on every call.
/// A caller may hold it indefinitely, compare it with `strcmp`, or print it.
///
/// `zlib.h` L225-L229 documents what a caller is expected to do with it: compare
/// it against the `ZLIB_VERSION` macro it was itself compiled against, because "if
/// the first character differs, the library code actually used is not compatible
/// with the zlib.h header file used by the application". The init entry points make
/// that check automatically.
///
/// Ported from `zlibVersion`, `zutil.c` L27-L29, whose body is `return ZLIB_VERSION;`.
// The C ABI fixes this spelling; `zlib.h` is immutable, so the name cannot be
// made snake case. The allow is per item deliberately -- a crate-wide one would
// also stop `non_snake_case` reporting ordinary local bindings.
#[allow(non_snake_case)]
#[no_mangle]
pub extern "C" fn zlibVersion() -> *const c_char {
    // No `unsafe`: `CStr::as_ptr` is a safe operation, and the `'static` lifetime
    // of the constant is what would otherwise have needed asserting by hand.
    //
    // The constant itself, whose five compile-time assertions above pin its length, its
    // terminator, its numeric components and its major-version digit -- so what this
    // export hands back is a string that could not have drifted from `zlib.h` L44
    // without failing the build.
    panic_guard::guard(|| ZLIB_VERSION.as_ptr())
}

// ---------------------------------------------------------------------------
// Compile-time configuration flags -- `zutil.c` L31-L121, `zlib.h` L1216-L1256
// ---------------------------------------------------------------------------

/// Encodes one type size the way the four `switch` statements at `zutil.c`
/// L35-L58 do: `2 -> 0`, `4 -> 1`, `8 -> 2`, anything else `-> 3`.
///
/// The four cases are the whole of the C encoding; `zlib.h` L1218 states the same
/// mapping as "two bits each, 00 = 16 bits, 01 = 32, 10 = 64, 11 = other". The
/// `default` arm is reachable in principle -- a target with a 16-byte `uLong`
/// would take it -- and reports "other" exactly as C does rather than being
/// treated as impossible.
///
/// Ported from `zlibCompileFlags`, `zutil.c` L35-L58.
const fn size_code(size: usize) -> uLong {
    match size {
        2 => 0,
        4 => 1,
        8 => 2,
        _ => 3,
    }
}

/// Bits 0-7: the four two-bit type-size fields, computed from *this* build.
///
/// The C source accumulates them with `+=`; `|` is used here because the four
/// fields are disjoint, so the two operators agree and the bitwise spelling makes
/// the disjointness evident. The order matches `zutil.c` L35-L58 and the
/// documentation at `zlib.h` L1219-L1222: `uInt`, then `uLong`, then `voidpf`,
/// then `z_off_t`.
///
/// Nothing here is a measured constant. On `x86_64-unknown-linux-gnu` the four
/// codes are 1, 2, 2, 2 and the field total is `0xa9`; on LLP64 Windows `uLong` is
/// four bytes and bits 2-3 become `1` instead, which is precisely why the value is
/// derived rather than written down.
/// cbindgen:ignore
const SIZE_BITS: uLong = size_code(size_of::<uInt>())
    | (size_code(size_of::<uLong>()) << 2)
    | (size_code(size_of::<voidpf>()) << 4)
    | (size_code(size_of::<z_off_t>()) << 6);

/// Bit 8 -- `ZLIB_DEBUG` (`zutil.c` L59-L61).
///
/// Clear. `ZLIB_DEBUG` enables the reference implementation's `Assert`, `Trace`
/// and `z_error` machinery, which this port replaces with ordinary Rust
/// `debug_assert!`s and typed results rather than with a build switch of its own.
/// There is no configuration of this crate that sets it.
/// cbindgen:ignore
const ZLIB_DEBUG_BIT: uLong = 0;

/// Bit 10 -- `ZLIB_WINAPI` (`zutil.c` L67-L69).
///
/// Clear. The bit reports that the exported functions use the `WINAPI`
/// (`__stdcall`) calling convention. Every export in this crate is declared
/// `extern "C"`, on every target, so the answer is unconditional.
/// cbindgen:ignore
const ZLIB_WINAPI_BIT: uLong = 0;

/// Bit 12 -- `BUILDFIXED` (`zutil.c` L70-L72).
///
/// Clear, because `crates/zlib-rs/src/inflate/fixed_tables.rs` transcribes
/// `inffixed.h` as `const` data: the fixed Huffman decode tables exist in
/// `.rodata` and are never constructed at run time. The alternative -- `inftrees.c`
/// L313-L352, which builds them on first use behind a `z_once_t` and, as
/// `inftrees.c` L314-L319 warns, is not thread-safe without atomics -- is not
/// implemented here at all.
/// cbindgen:ignore
const BUILDFIXED_BIT: uLong = 0;

/// Bit 13 -- `DYNAMIC_CRC_TABLE` (`zutil.c` L73-L75).
///
/// Clear, because `crates/zlib-rs/src/crc32/tables.rs` is pure `const` data: no
/// `OnceLock`, no `Once`, no `static mut`, no interior mutability. That is both the
/// honest answer and the safe one. `crc32.c` L13-L17 warns that "there is no mutex
/// or semaphore protection on the static variables used to control the first-use
/// generation of the crc tables", so a `DYNAMIC_CRC_TABLE` build has to call
/// `get_crc_table()` before letting a second thread near `crc32()`. Const
/// evaluation removes that requirement, and with it the `<stdatomic.h>` dependency
/// `zutil.h` would otherwise carry.
/// cbindgen:ignore
const DYNAMIC_CRC_TABLE_BIT: uLong = 0;

/// Bit 16 -- `NO_GZCOMPRESS` (`zutil.c` L76-L78).
///
/// **Derived from the resolved feature set, not asserted.** `zlib.h` L1237-L1238
/// defines the bit as "`gz*` functions cannot compress (to avoid linking deflate
/// code when not needed)".
///
/// * With the default features, `crates/zlib-rs/src/gz/write.rs` implements the
///   write path in full and the `gz*` exports are compiled, so the bit is clear.
/// * With `--no-default-features` (or any build that leaves the `gz` feature off)
///   the whole `gzFile` layer is compiled out, so no `gz*` function can compress --
///   or do anything else -- and the bit is set. `NO_GZCOMPRESS` is the only flag
///   `zlibCompileFlags` has for a missing `gz` layer; reporting the capability as
///   present in a build that does not contain it would be a lie told to every
///   caller that inspects the flags, which is the defect this derivation fixes.
///
/// One definition covers both cases deliberately: `cfg!()` is an expression, so the
/// feature is read here rather than by duplicating the item under `#[cfg]`, and there
/// is no configuration in which the name is declared twice.
/// cbindgen:ignore
const NO_GZCOMPRESS_BIT: uLong = if cfg!(feature = "gz") { 0 } else { 1 << 16 };

/// Bit 17 -- `NO_GZIP` (`zutil.c` L79-L81).
///
/// Clear, unconditionally, and the distinction from bit 16 above is the point: this
/// bit is NOT about the `gzFile` layer.
///
/// `NO_GZIP` reports that **`deflate` cannot write gzip streams and `inflate`
/// cannot detect or decode them** -- the RFC 1952 wrapper selected by a
/// `windowBits` of 25-31, and the automatic detection `windowBits + 32` enables.
/// In C that capability is compiled out by the `GZIP` macro in `deflate.c` and
/// `inflate.c`, which has nothing to do with `gzread.c`/`gzwrite.c`.
///
/// In this port the wrapper lives in `crates/zlib-rs/src/{deflate,inflate}/**` and
/// **no Cargo feature can remove it**: `zlib-rs` declares `default`, `rust-api`,
/// `simd` and `std`, and not one of them gates gzip container support. So the
/// capability is present in every configuration this crate can be built in,
/// including `--no-default-features`, and a zero here is a derivation from the
/// feature table rather than an assumption -- the `gz` feature that flips bit 16
/// deliberately does not flip this one.
/// cbindgen:ignore
const NO_GZIP_BIT: uLong = 0;

/// Bit 20 -- `PKZIP_BUG_WORKAROUND` (`zutil.c` L82-L84).
///
/// Clear. The bit reports a slightly more permissive `inflate` that tolerates
/// streams produced by a defective PKZip. The reference does not enable it by
/// default and neither does this port, because doing so would accept input the
/// reference rejects -- an observable behaviour change.
/// cbindgen:ignore
const PKZIP_BUG_WORKAROUND_BIT: uLong = 0;

/// Bit 21 -- `FASTEST` (`zutil.c` L85-L87).
///
/// Clear. `FASTEST` (`deflate.c` L106) substitutes a two-entry configuration table
/// and a different `longest_match`, so a `FASTEST` build emits different bytes.
/// This port implements the default configuration only, which is what the
/// byte-identical-output requirement demands.
/// cbindgen:ignore
const FASTEST_BIT: uLong = 0;

/// Bit 24 -- neither `STDC` nor `Z_HAVE_STDARG_H` (`zutil.c` L103-L104).
///
/// Clear. `zlib.h` L1249 describes the bit as "0 = vs\*, 1 = s\* -- 1 means
/// limited to 20 arguments after the format": C sets it when it has to fall back
/// from the `va_list` forms to the fixed-argument ones. The variadic entry point
/// this crate exports is the `stdarg` one, `gzvprintf` (`zlib.h` L2047), so the
/// fallback branch has no counterpart here.
/// cbindgen:ignore
const SPRINTF_NOT_VARIADIC_BIT: uLong = 0;

/// Bit 25 -- `NO_vsnprintf` together with `ZLIB_INSECURE` (`zutil.c` L89-L91,
/// L105-L107).
///
/// Clear. `zlib.h` L1250 spells out what setting it would mean: "1 means
/// `gzprintf()` is not secure!". `crates/zlib-rs/src/gz/printf.rs` implements only
/// the bounded path, and there is no `ZLIB_INSECURE` equivalent that could select
/// the unbounded `vsprintf` -- reintroducing it would restore exactly the overflow
/// class this port exists to remove.
/// cbindgen:ignore
const SPRINTF_INSECURE_BIT: uLong = 0;

/// Bit 26 -- a void-returning formatter: `HAS_vsprintf_void`,
/// `HAS_vsnprintf_void`, `HAS_sprintf_void` or `HAS_snprintf_void` (`zutil.c`
/// L95-L101, L111-L117).
///
/// Clear. `zlib.h` L1251 describes it as "0 = returns value, 1 = void -- 1 means
/// inferred string length returned", the workaround for platforms whose
/// `vsnprintf` returns nothing. Rust's formatting machinery always reports how much
/// it wrote, so the length is measured rather than inferred.
/// cbindgen:ignore
const SPRINTF_RETURNS_VOID_BIT: uLong = 0;

/// Bit 27 -- `NO_vsnprintf` without `ZLIB_INSECURE` (`zutil.c` L92-L93,
/// L108-L109).
///
/// **Derived from the artifact, not asserted.** `zlib.h` L1252 defines it as
/// "0 = `gzprintf()` present, 1 = not -- 1 means `gzprintf()` returns an error";
/// `gzwrite.c` L406-L412 and L505-L515 are the stub bodies such a build compiles.
///
/// `gzprintf` and `gzvprintf` are variadic, so they cannot be *defined* in stable
/// Rust; they are compiled from `csrc/gzprintf_shim.c` by `build.rs`, which drives the
/// platform C compiler and archives the object into the `libz.a` cargo produces.
/// `cfg(zlib_rs_gzprintf)` is set by that same script, and only when the compile
/// actually happened, so reading the cfg here ties the bit to the artifact rather than
/// to an intention. One configuration reports the bit *set*, and it is the only one
/// that can: with `libz-compat` or `gz` off there is no `gzFile` layer for the shim to
/// call into, `build.rs` compiles no C, and the library really has no `gzprintf`. The
/// test named below closes the loop by taking the address of both symbols, so a build
/// whose flags word claims them cannot even link without them.
///
/// # ★ The one artifact where this bit and the symbol table disagree
///
/// The cfg records that the shim was **compiled**; it cannot record whether a given
/// artifact **exported** it, and for one of the three cargo emits those differ. The
/// shim's objects are archived into `libz.a`, and rustc gives a C-contributed symbol no
/// entry in a `cdylib`'s dynamic table, so `target/<profile>/libz.so` reports this bit
/// clear while neither name appears in its `.dynsym`. No `cfg` could express the
/// difference even in principle: cargo passes `--crate-type` three times in **one**
/// rustc invocation, so all three artifacts contain the same compiled constant.
///
/// That is not a defect to work around here, because the `cdylib` is not an artifact
/// that ships -- `src/lib.rs`'s artifact matrix marks it not installable, and the two
/// that do ship both export the pair. What the port owes instead is proof, and there are
/// two gates rather than a comment:
/// `tests/symbol_parity.rs::compile_flags_bit_27_agrees_with_the_shipping_artifacts`
/// requires bit 27 clear to mean both names are present in `libz.a` **and** in the
/// packaged `libz.so.<ZLIB_VERSION>` (and bit 27 set to mean both are absent), and
/// `cargo_cdylib_matches_its_measured_shape` pins the development artifact's deviation
/// so it cannot widen unnoticed -- measured, that deviation is now exactly these two
/// names: the cdylib exports 93 of the 95 contract functions and no internal helper. Anyone tempted to "fix" the bit for the `cdylib` should
/// read those two first: clearing it there would misdescribe the static and packaged
/// libraries, which are the ones a consumer installs.
///
/// cbindgen:ignore
const GZPRINTF_UNAVAILABLE_BIT: uLong = if cfg!(zlib_rs_gzprintf) { 0 } else { 1 << 27 };

/// Bits 8-27: everything `zutil.c` derives from build configuration rather than
/// from a type size.
///
/// Every contribution is a determination documented at its own constant rather than
/// an omission, and all but two of them are zero in every configuration. The two
/// that are not -- [`NO_GZCOMPRESS_BIT`] (bit 16) and [`GZPRINTF_UNAVAILABLE_BIT`]
/// (bit 27) -- are `cfg`-selected from the `gz` feature, so this total is a property
/// of the build rather than a literal: `0` in the default configuration, and
/// `0x0801_0000` in a `libz-compat`-without-`gz` build (bits 16 and 27). Combining them here, one
/// name per line, is what makes the ladder auditable: enabling a configuration means
/// editing exactly one constant, and the assertion below then checks that the
/// constant landed on a legal bit.
///
/// Bit 9 has no constant of its own on purpose -- see the module documentation:
/// the C conditional that would set it is inside a block comment, so no build can
/// reach it, and it is listed in [`FORBIDDEN_CONFIGURATION_BITS`] instead.
/// cbindgen:ignore
const CONFIGURATION_BITS: uLong = ZLIB_DEBUG_BIT
    | ZLIB_WINAPI_BIT
    | BUILDFIXED_BIT
    | DYNAMIC_CRC_TABLE_BIT
    | NO_GZCOMPRESS_BIT
    | NO_GZIP_BIT
    | PKZIP_BUG_WORKAROUND_BIT
    | FASTEST_BIT
    | SPRINTF_NOT_VARIADIC_BIT
    | SPRINTF_INSECURE_BIT
    | SPRINTF_RETURNS_VOID_BIT
    | GZPRINTF_UNAVAILABLE_BIT;

/// Bit positions no configuration bit may ever occupy.
///
/// Each literal below is one group, and a bit landing in any of them would be an
/// ABI lie rather than a mere untidiness:
///
/// | Literal | Bits | Why it is forbidden |
/// |---|---|---|
/// | `0x0000_00ff` | 0-7 | the four type-size fields: a stray bit misreports a width |
/// | `0x0000_0200` | 9 | `ASMV`/`ASMINF`, unreachable -- see below |
/// | `0x0000_0800` | 11 | reserved, `zlib.h` L1229 |
/// | `0x0000_c000` | 14, 15 | reserved, `zlib.h` L1234 |
/// | `0x000c_0000` | 18, 19 | reserved, `zlib.h` L1242 |
/// | `0x00c0_0000` | 22, 23 | reserved, `zlib.h` L1247 |
/// | `0xf000_0000` | 28-31 | reserved, `zlib.h` L1255 |
///
/// Bit 9 is in that list for two independent reasons: `zutil.c` L62-L66 cannot set
/// it, because its `#if` is inside a block comment, and this port could not
/// honestly claim it in any case -- assembly fast paths are out of scope.
///
/// The complement -- 8, 10, 12, 13, 16, 17, 20, 21 and 24-27 -- is exactly the set
/// a differently configured build may legitimately set, and the tests check the
/// mask from both sides so that it can neither over- nor under-reach.
///
/// Every literal fits in 32 bits, so the mask is exact whether `uLong` is four
/// bytes or eight.
//
// ★ `#[allow(dead_code)]` for the reason given at `ZLIB_VER_MAJOR_DIGIT` above: this
// mask is used by the `const _: () = assert!(CONFIGURATION_BITS & … == 0)` item
// immediately below, and rustc 1.80 does not see uses inside `const _` bodies. The
// run-time test `no_configuration_bit_occupies_a_forbidden_position` reads it too,
// but a `#[cfg(test)]` use does not satisfy the non-test build.
#[allow(dead_code)]
/// cbindgen:ignore
const FORBIDDEN_CONFIGURATION_BITS: uLong =
    0x0000_00ff | 0x0000_0200 | 0x0000_0800 | 0x0000_c000 | 0x000c_0000 | 0x00c0_0000 | 0xf000_0000;

/// The complete answer [`zlibCompileFlags`] returns, evaluated at compile time.
///
/// The export cannot itself be `const fn` -- `const extern "C" fn` is not stable --
/// but the value it returns is a constant, so the whole ladder is folded during
/// compilation and the exported function is a single load of an immediate.
///
/// ★ **The forbidden-bit check lives inside this initialiser, not beside it.** Two
/// reasons, and the second is a hard requirement rather than a preference:
///
/// * The assertion cannot drift away from the value it guards. A bit that strayed onto
///   a size field, onto bit 9, or onto a reserved position would make the library
///   describe itself incorrectly to every caller that inspects the flags, and the
///   check now runs as part of computing the very number that would carry the lie.
/// * It keeps [`FORBIDDEN_CONFIGURATION_BITS`] a *used* constant on the declared
///   1.80 MSRV. rustc 1.80's dead-code pass does not count a use that occurs only
///   inside an anonymous `const _` item, so the mask -- referenced nowhere else
///   outside `#[cfg(test)]` -- was reported as "never used" and, with warnings denied,
///   failed the build. Current stable does count it, which is why the failure was
///   MSRV-only. Reachability from the exported [`zlibCompileFlags`] is what makes it
///   live on both.
///
/// cbindgen:ignore
const COMPILE_FLAGS: uLong = {
    assert!(CONFIGURATION_BITS & FORBIDDEN_CONFIGURATION_BITS == 0);
    SIZE_BITS | CONFIGURATION_BITS
};

/// Returns a bit set describing how the library was built -- `zlib.h` L1216.
///
/// The layout is documented at `zlib.h` L1217-L1256 and reproduced in full, with
/// this port's answer and reasoning for every field, in the module documentation.
/// In outline: bits 0-7 hold four two-bit type-size codes, bits 8-27 hold
/// compiler, assembler, debug, table-generation, library-content, behaviour and
/// `*printf`-variant flags, and bits 28-31 are reserved.
///
/// The value is **computed** from this build's own `size_of` results, never copied
/// from a measurement. On `x86_64-unknown-linux-gnu` it is `0xa9`, identical to the
/// reference build; on LLP64 Windows it differs, because `uLong` is four bytes
/// there and bits 2-3 change accordingly. Hardcoding either answer would be wrong
/// on the other target.
///
/// `test/example.c` L512-L513 prints this value rather than asserting on it, so a
/// wrong answer is visible but not fatal in that harness; the tests in this module
/// decode it field by field instead.
///
/// Ported from `zlibCompileFlags`, `zutil.c` L31-L121.
// See `zlibVersion` for why the allow is per item rather than crate-wide.
#[allow(non_snake_case)]
#[no_mangle]
pub extern "C" fn zlibCompileFlags() -> uLong {
    panic_guard::guard(|| COMPILE_FLAGS)
}

// ---------------------------------------------------------------------------
// Status messages -- `zutil.c` L13-L24 and L139-L141, `zutil.h` L62-L65
// ---------------------------------------------------------------------------

// `zError` takes a C `int` and the safe core's accessor takes an `i32`. The two are
// the same type on every target this crate supports, and passing the value straight
// through -- rather than widening it with a conversion that would be an identity on
// the primary target and so draw `clippy::useless_conversion` -- is what pins that
// assumption. On a target where `c_int` were narrower, this file would fail to
// compile rather than silently truncate, which is the outcome to want. The rest of
// the crate relies on the same equivalence, for instance where
// `panic_guard::fallback` assigns `ReturnCode::as_i32()` to a `c_int`.
/// cbindgen:ignore
const _: () = assert!(size_of::<c_int>() == size_of::<i32>());

/// The NUL-terminated mirror of the status-message table.
///
/// Transcribed character for character from `z_errmsg[10]` (`zutil.c` L13-L24), in
/// the same order and with the same ten-slot shape. The safe core holds the same
/// ten strings as ordinary `&str` values and says outright that NUL-terminated
/// construction belongs to this crate, because the core never holds a pointer and
/// therefore has no use for a terminator. This constant is that construction, and
/// it is the only place in the facade where the strings appear.
///
/// The two empty entries are deliberately kept distinct: slot 2 is `Z_OK` and slot
/// 9 is the out-of-range sentinel, exactly as in C, where `zutil.h` L62-L63
/// explains both the `2 - zlib_error` indexing and why the length is written out.
/// They are byte-identical, so which of them a lookup selects is unobservable to a
/// caller; collapsing them would nonetheless lose the correspondence with the C
/// table that makes this constant reviewable.
///
/// Private, and it must stay private. `zlib.map` L9-L19 lists `z_errmsg` in the
/// `local:` block of its `ZLIB_1.2.0` node, so the array is not part of the ABI;
/// only [`zError`] is.
///
/// Ported from `z_errmsg[10]`, `zutil.c` L13-L24.
/// cbindgen:ignore
const Z_ERRMSG_C: [&CStr; 10] = [
    c"need dictionary",      // Z_NEED_DICT       2
    c"stream end",           // Z_STREAM_END      1
    c"",                     // Z_OK              0
    c"file error",           // Z_ERRNO         (-1)
    c"stream error",         // Z_STREAM_ERROR  (-2)
    c"data error",           // Z_DATA_ERROR    (-3)
    c"insufficient memory",  // Z_MEM_ERROR     (-4)
    c"buffer error",         // Z_BUF_ERROR     (-5)
    c"incompatible version", // Z_VERSION_ERROR (-6)
    c"",                     // out-of-range sentinel
];

/// The out-of-range answer, and the answer for `Z_OK`: an empty but perfectly
/// readable C string.
///
/// Slot 9 of [`Z_ERRMSG_C`] spelled once more, so that the fall-through below has a
/// named value rather than a bare literal. It is `c""`, a pointer to a valid
/// `"\0"`, and never `NULL`: a caller that hands [`zError`]'s result straight to
/// `printf` must be safe for every possible input, including inputs no documented
/// status code covers.
/// cbindgen:ignore
const EMPTY_MESSAGE: &CStr = c"";

/// Maps any C `int` onto the NUL-terminated status message the reference pairs with
/// it.
///
/// The index arithmetic is **not** reproduced here. `zutil.h` L65 defines it as
///
/// ```text
/// #define ERR_MSG(err) z_errmsg[(err) < -6 || (err) > 2 ? 9 : 2 - (err)]
/// ```
///
/// and [`zlib_rs::error::err_msg`] already implements exactly that, including the
/// slot-9 clamp for out-of-range codes and -- unlike the naive `2 - err` of the
/// macro, which would overflow -- a provably panic-free answer for [`i32::MIN`].
/// Re-deriving it in the facade would create a second copy of the one calculation
/// that must not disagree with itself, so the core is asked for the message and
/// this function only translates it.
///
/// The translation is a scan rather than an index, and that is the point: no array
/// in this file is ever indexed with a value derived from a caller-supplied `int`,
/// so there is no bounds check to get wrong and no panicking path to reason about.
/// Ten short byte comparisons is a rounding error in a function that exists to
/// describe a failure, and the scan is total -- [`Z_ERRMSG_C`] contains every string
/// the core's table contains, which the tests below verify code by code.
///
/// Crate-visible so that any other export needing a `'static char *` for a status
/// code uses this one mapping rather than growing a second one. It is not, and must
/// not become, `#[no_mangle]`.
///
/// Ported from `ERR_MSG` (`zutil.h` L65) via `zError` (`zutil.c` L139-L141).
pub(crate) fn error_message(err: c_int) -> &'static CStr {
    // The core owns the index arithmetic; see above. Its answer then selects the
    // NUL-terminated mirror.
    message_cstr(zlib_rs::error::err_msg(err))
}

/// Returns the NUL-terminated mirror of a message the core recorded.
///
/// This is the *one* mapping [`error_message`] describes, lifted out so that a caller
/// holding a message rather than a status code uses it too instead of growing a second
/// copy. [`crate::deflate::recorded_msg`] is that caller: a stream's recorded message
/// does not always belong to the status the entry point ends up returning, so it cannot
/// go back through the code.
///
/// `find` returns the first match, so the empty string resolves to slot 2 rather than
/// slot 9; the two are byte-identical, so the choice cannot be observed. `unwrap_or`
/// rather than `unwrap`: a message the table does not contain would mean the core had
/// grown an entry this mirror lacks, and the correct response to that is the empty
/// string a C caller can print, not an abort.
pub(crate) fn message_cstr(message: &str) -> &'static CStr {
    Z_ERRMSG_C
        .iter()
        .copied()
        .find(|candidate| candidate.to_bytes() == message.as_bytes())
        .unwrap_or(EMPTY_MESSAGE)
}

/// Returns the message describing a zlib status code -- `zlib.h` L2033.
///
/// Accepts **any** `int`, not merely a documented status code, and answers with the
/// empty string for anything outside `Z_VERSION_ERROR ..= Z_NEED_DICT`. That is the
/// documented behaviour of the C macro rather than a fallback for a case that
/// "should not happen": `zError(1000)` returns `""` in the reference and returns
/// `""` here, and so do `zError(i32::MIN)` and `zError(i32::MAX)`.
///
/// The pointer is `'static`, NUL-terminated, never null -- including for the two
/// empty slots, where it addresses a readable `"\0"` -- and stable across calls, so
/// a caller may print it, compare it, or keep it. The exact strings are those of
/// `z_errmsg` (`zutil.c` L13-L24):
///
/// | Code | Message |
/// |---|---|
/// | `Z_NEED_DICT` (2) | `"need dictionary"` |
/// | `Z_STREAM_END` (1) | `"stream end"` |
/// | `Z_OK` (0) | `""` |
/// | `Z_ERRNO` (-1) | `"file error"` |
/// | `Z_STREAM_ERROR` (-2) | `"stream error"` |
/// | `Z_DATA_ERROR` (-3) | `"data error"` |
/// | `Z_MEM_ERROR` (-4) | `"insufficient memory"` |
/// | `Z_BUF_ERROR` (-5) | `"buffer error"` |
/// | `Z_VERSION_ERROR` (-6) | `"incompatible version"` |
/// | anything else | `""` |
///
/// These are the generic, per-code phrases. They are **not** what a caller reading
/// `strm->msg` after a failed `inflate` normally sees: the inflate state machine
/// constructs far more specific diagnostics of its own -- "incorrect header check",
/// "invalid distance too far back" and the rest -- and the gz layer formats
/// `"<path>: <reason>"` per stream. `zError` is only ever the code's own phrase.
///
/// `zlib.h` groups this among the undocumented functions, and the comment at
/// `zutil.c` L136-L138 records why it is exported at all: to let `compress()` and
/// `uncompress()` callers turn a status code into a string.
///
/// Ported from `zError`, `zutil.c` L139-L141, whose body is `return ERR_MSG(err);`.
// See `zlibVersion` for why the allow is per item rather than crate-wide.
#[allow(non_snake_case)]
#[no_mangle]
pub extern "C" fn zError(err: c_int) -> *const c_char {
    // No `unsafe`: the argument is a plain integer and `CStr::as_ptr` is safe.
    panic_guard::guard(|| error_message(err).as_ptr())
}

// ---------------------------------------------------------------------------
// CRC-32 table access -- `crc32.c` L478-L487
// ---------------------------------------------------------------------------

// `z_crc_t` is `u32` (`zconf.h` L429-L444, and `crate::types` records the
// preprocessor arithmetic that chooses it), so the reference the safe core returns
// already has the pointee type the C signature promises and no cast is involved.
// The primary guard against that changing is the type system -- redefining the alias
// would make `get_crc_table` below fail to compile -- and these two assertions are
// the belt to that pair of braces, stating the width and alignment the C contract
// actually depends on.
/// cbindgen:ignore
const _: () = assert!(size_of::<z_crc_t>() == size_of::<u32>());
/// cbindgen:ignore
const _: () = assert!(align_of::<z_crc_t>() == align_of::<u32>());

/// Returns the byte-wise CRC-32 table -- `zlib.h` L2035.
///
/// 256 entries, one per possible input byte, addressed by a `'static`, never-null,
/// call-stable pointer into read-only data. The length is not communicated through
/// the C signature -- as in the reference, the caller is expected to know it -- but
/// it is fixed in the Rust type the safe core returns, `&'static [u32; 256]`, so it
/// cannot drift.
///
/// The comment at `crc32.c` L478-L481 gives the function's two purposes, and only
/// the first survives the port:
///
/// * it lets an assembler `crc32()` share the table. Still meaningful, in the sense
///   that any consumer may read the same table this library uses;
/// * it forces "the generation of the CRC tables in a threaded application".
///   **Not applicable here, and that is by design.** The table is `const` data
///   materialised by the compiler, so there is nothing to generate, no first call to
///   get out of the way before starting threads, and no unsynchronised
///   initialisation of the kind `crc32.c` L13-L17 warns about. The same fact is what
///   makes `zlibCompileFlags` bit 13, `DYNAMIC_CRC_TABLE`, clear -- the private
///   `DYNAMIC_CRC_TABLE_BIT` constant carries the same reasoning.
///
/// Consequently this function is safe to call from any thread at any time, needs no
/// ordering with respect to `crc32()`, and returns the same address on every call.
///
/// Ported from `get_crc_table`, `crc32.c` L482-L487.
#[no_mangle]
pub extern "C" fn get_crc_table() -> *const z_crc_t {
    // No `unsafe`: `<[T]>::as_ptr` on a `'static` reference is a safe operation, and
    // the array reference the core hands back is what supplies both the lifetime and
    // the length.
    panic_guard::guard(|| zlib_rs::crc32::get_crc_table().as_ptr())
}

// Every expected value below was captured from the REFERENCE C library, not derived
// from this implementation: the in-tree C sources were built out of tree with
// `./configure && make`, a probe was linked against the resulting
// `libz.so.1.3.2.1-motley` with `ldd` confirming that artifact was the one bound
// rather than the system zlib, and its output is what these tests assert. That
// distinction is the whole value of the file -- a test written from the Rust code
// would agree with a wrong answer.
//
// The reference answers, for the record:
//
//   zlibVersion()      = "1.3.2.1-motley"
//   zlibCompileFlags() = 0xa9   (sizeof uInt 4, uLong 8, voidpf 8, z_off_t 8)
//   zError(e)          = the nine phrases below, "" for every out-of-range e,
//                        including INT_MIN and INT_MAX, with no crash
//   get_crc_table()    = [0] 0x00000000, [1] 0x77073096, [255] 0x2d02ef8d,
//                        and the same pointer on every call
//
// Nothing here dereferences a raw pointer, so no test needs `unsafe` either. An
// exported pointer is checked for non-nullness, for STABILITY ACROSS CALLS, and for
// the contents of the constant it addresses -- read safely through that constant --
// which proves what a raw read would without weakening this module's "no unsafe
// anywhere" property. A C caller genuinely reading through the pointers is exercised
// separately, by the probe and by the unmodified `test/example.c` that
// `Makefile.in`'s `rust-test` target compiles against the staged library.
//
// One thing deliberately NOT asserted, because it is a build artefact rather than a
// contract: identity between a pointer an export returned and `SOME_CONST.as_ptr()`
// evaluated here. A `const` is inlined at every use site, so the two referents need
// not be the same allocation -- measured under `-Zbuild-std -Zsanitizer=address`,
// where they differ, against an ordinary build, where they are deduplicated and the
// assertion passed by luck. Stability across repeated calls through one export is the
// address property C really does imply, and that is what is checked instead.
#[cfg(test)]
mod tests {
    use super::{
        error_message, get_crc_table, size_code, zError, zlibCompileFlags, zlibVersion,
        CONFIGURATION_BITS, EMPTY_MESSAGE, FORBIDDEN_CONFIGURATION_BITS, SIZE_BITS, ZLIB_VERNUM,
        ZLIB_VERSION, ZLIB_VER_MAJOR, ZLIB_VER_MINOR, ZLIB_VER_REVISION, ZLIB_VER_SUBREVISION,
        Z_ERRMSG_C,
    };
    use core::ffi::c_int;

    use crate::types::{uInt, uLong, voidpf, z_off_t};

    /// The reference value of `zlibCompileFlags()` on a target whose `uInt`,
    /// `uLong`, `voidpf` and `z_off_t` measure 4, 8, 8 and 8 bytes.
    const REFERENCE_FLAGS_LP64: uLong = 0xa9;

    /// Every documented status code paired with the exact phrase the reference
    /// prints for it, in table order.
    const REFERENCE_MESSAGES: [(c_int, &str); 9] = [
        (2, "need dictionary"),
        (1, "stream end"),
        (0, ""),
        (-1, "file error"),
        (-2, "stream error"),
        (-3, "data error"),
        (-4, "insufficient memory"),
        (-5, "buffer error"),
        (-6, "incompatible version"),
    ];

    /// Codes outside `Z_VERSION_ERROR ..= Z_NEED_DICT`, all of which the reference
    /// answers with the empty string. `i32::MIN` is the one that matters most: the C
    /// macro's `2 - err` would overflow on it.
    const OUT_OF_RANGE_CODES: [c_int; 8] = [3, 4, -7, -8, 1000, -1000, c_int::MIN, c_int::MAX];

    // -- zlibVersion ------------------------------------------------------

    #[test]
    fn the_version_string_is_the_header_literal_verbatim() {
        // `zlib.h` L44. Not "1.3.2", which is what a Cargo-derived answer would be.
        assert_eq!(ZLIB_VERSION.to_bytes(), b"1.3.2.1-motley");
        assert_eq!(ZLIB_VERSION.to_bytes_with_nul(), b"1.3.2.1-motley\0");
    }

    #[test]
    fn zlib_version_returns_that_literal_and_never_null() {
        let first = zlibVersion();
        assert!(!first.is_null());
        // Stable across calls: one materialised constant, not a fresh temporary. This is
        // the address property the C contract does imply -- `zutil.c` L27-L29 returns the
        // same string literal every time -- and it holds here because `zlibVersion`'s body
        // holds a single use site of the constant.
        assert_eq!(first, zlibVersion());
        // Identity against a SEPARATE use site of the constant, which is what
        // `assert_eq!(first, ZLIB_VERSION.as_ptr())` would be, is deliberately not
        // asserted. `ZLIB_VERSION` is a `const`, a `const` is inlined at every use site,
        // and the referent this test would materialise therefore need not be the one the
        // function body materialised. Measured, not supposed: under
        // `-Zbuild-std -Zsanitizer=address` the two addresses genuinely differ, while an
        // ordinary build happens to deduplicate them -- so the assertion described a
        // build artefact, not the contract. `ZLIB_VERSION` cannot become a `static` to
        // force one address either, because the compile-time guards near the top of this
        // file read it in `const` context and a constant may not refer to a static.
        //
        // What a C caller actually depends on is the BYTES -- every
        // `strcmp(zlibVersion(), ZLIB_VERSION)` check, `test/example.c` L502-L505
        // included. Those are pinned by
        // `the_version_string_is_the_header_literal_verbatim` above, and the end-to-end
        // read through the raw pointer is exercised by the unmodified `test/example.c`
        // that `Makefile.in`'s `rust-test` target links against the staged library.
        assert_eq!(ZLIB_VERSION.to_bytes(), b"1.3.2.1-motley");
    }

    #[test]
    fn the_first_character_is_the_major_version_digit() {
        // The check `test/example.c` L502-L505 makes fatally, and the one
        // `deflateInit_`, `inflateInit_` and `inflateBackInit_` turn into
        // `Z_VERSION_ERROR`. A wrong answer here aborts every caller's test suite.
        assert_eq!(ZLIB_VERSION.to_bytes().first().copied(), Some(b'1'));
    }

    #[test]
    fn the_dotted_prefix_spells_the_four_numeric_components() {
        // The string and the five numbers are separate declarations in `zlib.h`
        // L44-L49 and separate constants here; this is what keeps them honest.
        let expected =
            format!("{ZLIB_VER_MAJOR}.{ZLIB_VER_MINOR}.{ZLIB_VER_REVISION}.{ZLIB_VER_SUBREVISION}");
        assert_eq!(expected, "1.3.2.1");
        let version = ZLIB_VERSION.to_str().unwrap_or_default();
        assert!(
            version.starts_with(&expected),
            "{version} does not begin with {expected}"
        );
        // The remainder is the distribution suffix, which is exactly why the string
        // is not valid Cargo semver and cannot come from `CARGO_PKG_VERSION`.
        assert_eq!(version.strip_prefix(&expected), Some("-motley"));
    }

    #[test]
    fn the_packed_version_number_agrees_with_the_components() {
        // `zlib.h` L45: one nibble per component, most significant first.
        assert_eq!(ZLIB_VERNUM, 0x1321);
        assert_eq!(
            ZLIB_VERNUM,
            (ZLIB_VER_MAJOR << 12)
                | (ZLIB_VER_MINOR << 8)
                | (ZLIB_VER_REVISION << 4)
                | ZLIB_VER_SUBREVISION
        );
    }

    #[test]
    fn the_version_string_holds_no_interior_nul() {
        // Guaranteed by `CStr`, asserted anyway: an interior NUL would truncate the
        // string every C caller compares with `strcmp`.
        assert!(!ZLIB_VERSION.to_bytes().contains(&0));
    }

    // -- zlibCompileFlags -------------------------------------------------

    #[test]
    fn the_size_encoding_is_the_c_switch() {
        // `zutil.c` L35-L58 and `zlib.h` L1218: 2 -> 0, 4 -> 1, 8 -> 2, else 3.
        assert_eq!(size_code(2), 0);
        assert_eq!(size_code(4), 1);
        assert_eq!(size_code(8), 2);
        // The `default` arm, which reports "other" rather than pretending.
        assert_eq!(size_code(0), 3);
        assert_eq!(size_code(1), 3);
        assert_eq!(size_code(3), 3);
        assert_eq!(size_code(16), 3);
        assert_eq!(size_code(usize::MAX), 3);
    }

    #[test]
    fn each_size_field_reports_this_targets_own_type_width() {
        let flags = zlibCompileFlags();
        assert_eq!(flags & 0b11, size_code(size_of::<uInt>()), "bits 0-1: uInt");
        assert_eq!(
            (flags >> 2) & 0b11,
            size_code(size_of::<uLong>()),
            "bits 2-3: uLong"
        );
        assert_eq!(
            (flags >> 4) & 0b11,
            size_code(size_of::<voidpf>()),
            "bits 4-5: voidpf"
        );
        assert_eq!(
            (flags >> 6) & 0b11,
            size_code(size_of::<z_off_t>()),
            "bits 6-7: z_off_t"
        );
    }

    /// The gz-capability bits this build is expected to set: none with the `gz`
    /// feature on, and bit 16 (`NO_GZCOMPRESS`) together with bit 27 (`gzprintf`
    /// unavailable) with it off.
    ///
    /// Written as a helper rather than repeated in each test, so that the two
    /// tests below and the implementation cannot drift from one another: this is
    /// the same derivation `NO_GZCOMPRESS_BIT` and `GZPRINTF_UNAVAILABLE_BIT`
    /// make, stated once, from the test side.
    const fn expected_gz_bits() -> uLong {
        if cfg!(feature = "gz") {
            0
        } else {
            (1 << 16) | (1 << 27)
        }
    }

    #[test]
    fn the_flags_equal_the_reference_value_where_the_types_match_the_reference() {
        // Guarded on the measured widths rather than on `cfg`, so the test states
        // the condition under which 0xa9 is the right answer instead of assuming a
        // target. On LLP64 Windows `uLong` is four bytes and the total differs,
        // which is why the implementation computes the value.
        let widths = (
            size_of::<uInt>(),
            size_of::<uLong>(),
            size_of::<voidpf>(),
            size_of::<z_off_t>(),
        );
        if widths == (4, 8, 8, 8) && CONFIGURATION_BITS == 0 {
            assert_eq!(
                zlibCompileFlags(),
                REFERENCE_FLAGS_LP64 | expected_gz_bits(),
                "the reference C library returns 0xa9 for these type widths, plus \
                 whichever gz-capability bits this feature set removes"
            );
        } else {
            // Still a real assertion: whatever the widths, the two halves must be
            // exactly the size codes plus this build's own content bits. The
            // `CONFIGURATION_BITS == 0` guard above is what keeps 0xa9 as the claim
            // for a full-featured build only -- a `--no-default-features` build
            // legitimately sets bits 16 and 27, and reporting 0xa9 there would be
            // the introspection defect this test now covers.
            assert_eq!(zlibCompileFlags(), SIZE_BITS | CONFIGURATION_BITS);
        }
    }

    #[test]
    fn every_configuration_bit_matches_this_builds_capabilities() {
        // Enumerated one position at a time rather than asserted as a single value,
        // so a future build that sets an unexpected one is reported by bit number.
        // Bits 16 and 27 are the two this port derives rather than asserts, and each
        // is checked against the same condition the implementation reads: the `gz`
        // feature for the `gz*` layer, and `cfg(zlib_rs_gzprintf)` -- which `build.rs`
        // sets only when `csrc/gzprintf_shim.c` was actually compiled in -- for
        // `gzprintf`.
        let flags = zlibCompileFlags();
        let expected = |bit: u32| -> uLong {
            match bit {
                16 => uLong::from(!cfg!(feature = "gz")),
                27 => uLong::from(!cfg!(zlib_rs_gzprintf)),
                _ => 0,
            }
        };
        for bit in [
            8_u32, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28,
            29, 30, 31,
        ] {
            assert_eq!(
                flags >> bit & 1,
                expected(bit),
                "bit {bit} does not describe this build"
            );
        }
        assert_eq!(
            CONFIGURATION_BITS,
            (0..32).map(|bit| expected(bit) << bit).sum::<uLong>()
        );
    }

    // The bit-27 claim above is a statement about the artifact, so it is checked
    // against the artifact: when `cfg(zlib_rs_gzprintf)` is set, the two variadic
    // symbols must really be linked into this binary, and taking their addresses is
    // what proves it -- a missing definition is a link error rather than a test
    // failure, which is the strongest form the check can take. `test/example.c`
    // L153-L155 exercises the same two symbols for real, through
    // `gzprintf(file, ", %s!", "hello")`.
    #[cfg(zlib_rs_gzprintf)]
    #[test]
    fn the_variadic_shim_is_linked_when_the_flags_say_it_is() {
        use crate::types::gzFile;
        use core::ffi::{c_char, c_void};

        extern "C" {
            fn gzprintf(file: gzFile, format: *const c_char, ...) -> c_int;
            fn gzvprintf(file: gzFile, format: *const c_char, va: *mut c_void) -> c_int;
        }

        let printf: unsafe extern "C" fn(gzFile, *const c_char, ...) -> c_int = gzprintf;
        let vprintf: unsafe extern "C" fn(gzFile, *const c_char, *mut c_void) -> c_int = gzvprintf;
        // The assertion is the LINK, not the comparison: naming both symbols means
        // this test binary cannot be produced unless the shim was compiled and
        // linked, and `black_box` keeps the references from being optimised away
        // before that happens. A null check would be the wrong instrument -- a
        // function pointer is never null, and clippy says so.
        core::hint::black_box((printf as *const c_void, vprintf as *const c_void));
        assert_eq!(zlibCompileFlags() >> 27 & 1, 0);
    }

    #[test]
    fn no_configuration_bit_occupies_a_forbidden_position() {
        // The build-time assertion restated at run time, plus a check that the mask
        // itself still covers what it claims: the size fields, bit 9, and every
        // position `zlib.h` documents as reserved.
        assert_eq!(CONFIGURATION_BITS & FORBIDDEN_CONFIGURATION_BITS, 0);
        for bit in [
            0_u32, 1, 2, 3, 4, 5, 6, 7, 9, 11, 14, 15, 18, 19, 22, 23, 28, 29, 30, 31,
        ] {
            assert_eq!(
                FORBIDDEN_CONFIGURATION_BITS >> bit & 1,
                1,
                "bit {bit} must be forbidden to configuration"
            );
        }
        // ... and nothing else. Bits 8, 10, 12, 13, 16, 17, 20, 21 and 24-27 are
        // legitimately settable by a differently configured build.
        for bit in [8_u32, 10, 12, 13, 16, 17, 20, 21, 24, 25, 26, 27] {
            assert_eq!(
                FORBIDDEN_CONFIGURATION_BITS >> bit & 1,
                0,
                "bit {bit} must remain available to configuration"
            );
        }
    }

    // -- zError -----------------------------------------------------------

    #[test]
    fn the_message_table_mirrors_the_c_array() {
        // `zutil.c` L13-L24: ten slots, in this order, with slot 2 (`Z_OK`) and slot
        // 9 (the sentinel) both empty and both present.
        assert_eq!(Z_ERRMSG_C.len(), 10);
        assert_eq!(Z_ERRMSG_C.get(2).map(|s| s.to_bytes()), Some(&b""[..]));
        assert_eq!(Z_ERRMSG_C.get(9).map(|s| s.to_bytes()), Some(&b""[..]));
        for (slot, entry) in Z_ERRMSG_C.iter().enumerate() {
            assert!(
                !entry.to_bytes().contains(&0),
                "slot {slot} holds an interior NUL"
            );
        }
    }

    #[test]
    fn every_documented_code_gets_its_reference_phrase() {
        for (code, expected) in REFERENCE_MESSAGES {
            let message = error_message(code);
            assert_eq!(
                message.to_bytes(),
                expected.as_bytes(),
                "zError({code}) should be {expected:?}"
            );
            // The export hands back a pointer to that very message.
            assert_eq!(zError(code), message.as_ptr());
            assert!(!zError(code).is_null());
        }
    }

    #[test]
    fn every_out_of_range_code_gets_the_empty_string_and_a_valid_pointer() {
        // Including `i32::MIN`, where the C macro's `2 - err` would overflow. The
        // point is not only the value but that the call returns at all.
        for code in OUT_OF_RANGE_CODES {
            let message = error_message(code);
            assert_eq!(message.to_bytes(), b"", "zError({code}) should be empty");
            assert_eq!(
                message.to_bytes_with_nul(),
                EMPTY_MESSAGE.to_bytes_with_nul(),
                "zError({code}) should be the empty-message constant"
            );
            // Stability of the address across calls is the property C's
            // `z_errmsg[Z_NEED_DICT - err]` implies, and it holds because
            // `error_message` contains a single use site of `EMPTY_MESSAGE`. Identity
            // against a *separate* use site of that `const` -- which
            // `assert_eq!(message.as_ptr(), EMPTY_MESSAGE.as_ptr())` would be -- is not
            // asserted, for the reason spelled out at
            // `zlib_version_returns_that_literal_and_never_null`: a `const` is inlined per
            // use site, and under `-Zbuild-std -Zsanitizer=address` the two referents are
            // measurably distinct allocations.
            assert_eq!(message.as_ptr(), error_message(code).as_ptr());
            assert!(!zError(code).is_null());
        }
    }

    #[test]
    fn the_mirror_agrees_with_the_safe_core_for_every_code_in_a_wide_sweep() {
        // The core owns the index arithmetic and this module owns the
        // NUL-terminated mirror; this is the seam between them. Sweeping well past
        // the documented range confirms the translation is total, so the
        // `unwrap_or` fall-through in `error_message` is unreachable in practice.
        for code in -64_i32..=64 {
            assert_eq!(
                error_message(code).to_bytes(),
                zlib_rs::error::err_msg(code).as_bytes(),
                "mirror disagrees with the core for {code}"
            );
        }
        for code in [c_int::MIN, c_int::MIN + 1, c_int::MAX - 1, c_int::MAX] {
            assert_eq!(
                error_message(code).to_bytes(),
                zlib_rs::error::err_msg(code).as_bytes()
            );
        }
    }

    #[test]
    fn the_returned_message_pointer_is_stable_across_calls() {
        for (code, _) in REFERENCE_MESSAGES {
            assert_eq!(zError(code), zError(code));
        }
    }

    #[test]
    fn the_empty_message_is_a_readable_string_and_not_a_null_pointer() {
        // A caller printing `zError(Z_OK)` must not be handed `NULL`.
        assert_eq!(EMPTY_MESSAGE.to_bytes_with_nul(), b"\0");
        assert!(!zError(0).is_null());
        // `assert!(!EMPTY_MESSAGE.as_ptr().is_null())` is deliberately absent:
        // `clippy::useless_ptr_null_checks` rejects it because `CStr::as_ptr`
        // provably never returns null. That refusal is a stronger statement of this
        // module's non-null guarantee than any assertion could be, so it is recorded
        // here rather than worked around.
    }

    // -- get_crc_table ----------------------------------------------------

    #[test]
    fn the_crc_table_pointer_addresses_the_cores_const_table() {
        let table = zlib_rs::crc32::get_crc_table();
        let exported = get_crc_table();
        assert!(!exported.is_null());
        assert_eq!(exported, table.as_ptr());
        // Stable across calls, because it is one item in `.rodata` and not a
        // lazily built copy -- the property that makes bit 13 clear.
        assert_eq!(exported, get_crc_table());
    }

    #[test]
    fn the_crc_table_holds_the_reference_values() {
        // `crc32.h` opens `0x00000000, 0x77073096, 0xee0e612c, 0x990951ba` and the
        // probe read 0x2d02ef8d at index 255 from the reference library.
        let table = zlib_rs::crc32::get_crc_table();
        assert_eq!(table.len(), 256);
        let head: &[u32] = table.get(..4).unwrap_or_default();
        assert_eq!(head, [0x0000_0000, 0x7707_3096, 0xee0e_612c, 0x9909_51ba]);
        assert_eq!(table.last().copied(), Some(0x2d02_ef8d));
    }

    #[test]
    fn the_crc_entry_type_is_the_width_the_c_signature_promises() {
        // `z_crc_t` is 32 bits (`zconf.h` L429-L444). A wider or narrower entry
        // would make every caller walking the table read the wrong addresses.
        assert_eq!(size_of::<super::z_crc_t>(), 4);
        assert_eq!(align_of::<super::z_crc_t>(), align_of::<u32>());
    }
}
