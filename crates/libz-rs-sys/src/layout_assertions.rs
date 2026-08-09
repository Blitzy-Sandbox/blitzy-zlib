//! The ABI gate: every struct size, field offset and integer width the C
//! contract fixes, asserted at compile time so that drift is a build failure.
//!
//! This module contains **no runtime code at all** — no functions, no data, no
//! exports. It is a wall of `const _: () = assert!(…)` items, and its entire
//! value is negative: a future edit that perturbs a layout stops the build
//! instead of silently corrupting the memory of every program that links the
//! result. A wrong number here is not a bug that shows up as a wrong answer.
//! It is a wrong *address*, computed inside caller object code this port cannot
//! recompile, and it corrupts whatever happens to live there.
//!
//! `zlib.h` and `zconf.h` are immutable, and four mechanisms are meant to enforce
//! that mechanically rather than by review: these compile-time assertions, a
//! cbindgen header comparison, an `nm` symbol-parity diff against the 111-symbol
//! reference surface, and the `c-std.yml` sweep proving the unchanged header still
//! compiles from C89 through gnu2x. Two of the four run on their own: this module,
//! because declaring it is what evaluates its `const` items, and `c-std.yml`,
//! because it is an in-tree workflow. The other two do not. `cbindgen` generates a
//! header but the normalised comparison `cbindgen.toml` specifies is not
//! implemented anywhere, and the `nm` diff exists only as `Makefile.in`'s
//! `rust-symbols` target, which nothing invokes automatically. So this module is
//! the one mechanism a build cannot skip -- which is also why its coverage has to
//! be exhaustive rather than representative.
//!
//! # ★ The two-tier scheme, and why the implications may not be "simplified"
//!
//! `uLong` is `unsigned long` (`zconf.h` L406): **8 bytes on this LP64 target,
//! 4 bytes on LLP64 Windows**. `sizeof(z_stream)` is therefore 112 *here* and
//! genuinely smaller there — 88 bytes, by the same arithmetic — so an
//! unconditional `assert!(size_of::<z_stream>() == 112)` would be a portability
//! bug wearing the costume of a safety check: correct on the target it was
//! measured on, and a hard build failure on a target where the layout is right.
//!
//! Every assertion below therefore belongs to exactly one of three tiers.
//!
//! | Tier | Form | Checks on | Catches |
//! |---|---|---|---|
//! | **Absolute, LP64** | `!IS_LP64 \|\| <exact number>` | LP64 only | any drift at all |
//! | **Absolute, LLP64** | `!IS_LLP64 \|\| <exact number>` | LLP64 only | any drift at all |
//! | **Relational** | `<offset/size relationship>` | every target | reordered, inserted, removed or widened fields |
//!
//! The absolutes are written as implications — read `!IS_LP64 || X` as
//! "under LP64, X" — because that is the one form that is simultaneously exact
//! where the measurement applies and vacuous where it does not. `#[cfg(…)]`
//! cannot express the condition, since the width of `c_ulong` is not a `cfg`
//! predicate; `cfg(target_pointer_width = "64")` is the nearest available
//! approximation and it is wrong precisely on LLP64, which is the case that
//! matters.
//!
//! **Both 64-bit models get exact numbers, and that is deliberate.** Gating the
//! exact tier on LP64 alone would leave `x86_64` Windows — the other 64-bit target
//! this contract has to hold on — checked only relationally, and a relational
//! chain cannot see a narrowing re-type whose bytes padding absorbs (see "the one
//! thing the relational tier cannot see", below). So `IS_LLP64` carries a second
//! absolute tier: `z_stream` at 88 bytes, `gz_header` at 72, `struct gzFile_s`
//! unchanged at 24. `IS_LLP64`'s own documentation states where those numbers come
//! from and how they were confirmed. The two gates are mutually exclusive, so at
//! most one absolute tier is ever live.
//!
//! **Do not delete the `!IS_LP64 ||` or `!IS_LLP64 ||` guards to "tidy" the
//! file**, and do not promote a relational assertion to an absolute one. The
//! relational tier is deliberately self-sufficient rather than decorative on the
//! targets neither absolute tier covers: verified by cross-checking the crate
//! against three real integer models rather than by assumption —
//!
//! | Target | Model | Gate | Result |
//! |---|---|---|---|
//! | `x86_64-unknown-linux-gnu` | LP64 | `IS_LP64` | LP64 absolutes + relational; clean |
//! | `x86_64-pc-windows-gnu` | LLP64 | `IS_LLP64` | LLP64 absolutes + relational; clean |
//! | `i686-unknown-linux-gnu` | ILP32 | neither | relational tier only; clean |
//!
//! and, with both gates false, reordering two `z_stream` fields or widening one
//! still fails the build, because both move a later offset.
//!
//! ## ★ The one thing the relational tier cannot see
//!
//! State the limit honestly rather than trust a tier further than it goes: a
//! *narrowing* re-type that the target's padding entirely absorbs is invisible
//! to offset arithmetic. Making `total_in` a `u8` on ILP32 leaves it at offset
//! 8, `next_out` still lands at 12 because a pointer needs 4-byte alignment, and
//! every link in the chain still holds. Two other mechanisms cover that case,
//! which is why neither is optional:
//!
//! * On the measured target the absolute tier catches it immediately and loudly
//!   — that exact `u8` substitution fires **14** of the assertions below.
//! * On *any* target a cbindgen header comparison would catch it, because the
//!   generated declaration reads `uint8_t total_in;` where `zlib.h` says
//!   `uLong total_in;`. Generating and reading that header is a manual step today,
//!   so this second net only catches the case when somebody runs it.
//!
//! # ★ MSRV: every `offset_of!` is exactly one field deep
//!
//! `core::mem::offset_of!` stabilised in Rust **1.77**, comfortably inside the
//! declared MSRV of 1.80 (`rust-version = "1.80"`, `edition = "2021"`), and it
//! is usable in const context. **Nested field access — `offset_of!(Outer,
//! inner.field)` — did not stabilise until 1.82 and must never appear in this
//! file.** It compiles on the toolchain in front of you and breaks the build at
//! MSRV, which is the single most likely way for this module to become the
//! thing it exists to prevent.
//!
//! No nested access is needed here. Every type asserted below is flat. Should a
//! nested offset ever be required, compute it as the sum of two single-level
//! offsets and write the arithmetic out.
//!
//! # Why the numbers are load-bearing beyond layout
//!
//! Two of these groups are observable through the *public API*, so getting them
//! wrong is a visible protocol violation and not merely internal untidiness:
//!
//! * `sizeof(z_stream)`. `deflateInit2_` (`deflate.c` L395), `inflateInit2_`
//!   (`inflate.c` L179) and `inflateBackInit_` (`infback.c` L31) each reject a
//!   caller whose `stream_size` argument disagrees with the library's own
//!   `sizeof(z_stream)`, returning `Z_VERSION_ERROR`. A library built with a
//!   112-byte struct and a caller compiled against a different one do not
//!   quietly misbehave — every stream the caller opens fails.
//! * The four primitive widths. `zlibCompileFlags` (`zutil.c` L32-L57) packs
//!   `sizeof(uInt)`, `sizeof(uLong)`, `sizeof(voidpf)` and `sizeof(z_off_t)`
//!   into bits 0-7 of its result under the encoding 2 → 0, 4 → 1, 8 → 2,
//!   anything else → 3. The reference build returns **`0xa9`**, whose low byte
//!   `0b1010_1001` decodes to (1, 2, 2, 2) — that is (4, 8, 8, 8) bytes, which
//!   is exactly what §5 asserts. `test/example.c` prints the value.
//!
//! # How the numbers were obtained
//!
//! Measured, never assumed, and re-measured independently for this module
//! rather than copied from the plan: a C probe compiled against the real
//! in-tree headers with `gcc -I. -D_LARGEFILE64_SOURCE=1` (gcc 15.2.0,
//! `x86_64-unknown-linux-gnu`) printing `sizeof`, `_Alignof` and `offsetof` for
//! `z_stream`, `gz_header`, `struct gzFile_s`, `code`, `struct inflate_state`
//! and every `zconf.h` typedef. Each group below cites the header line it
//! enforces.
//!
//! | Type | Declared at | Size | Align | Field offsets |
//! |---|---|---|---|---|
//! | `z_stream` | `zlib.h` L90-L110 | 112 | 8 | 0, 8, 16, 24, 32, 40, 48, 56, 64, 72, 80, 88, 96, 104 |
//! | `gz_header` | `zlib.h` L118-L133 | 80 | 8 | 0, 8, 16, 20, 24, 32, 36, 40, 48, 56, 64, 68, 72 |
//! | `gzFile_s` | `zlib.h` L1956-L1960 | 24 | 8 | 0, 8, 16 |
//! | `code` | `inftrees.h` L24-L28 | 4 | 2 | 0, 1, 2 |
//!
//! # Division of labour with `types.rs`
//!
//! [`crate::types`] *declares* the ABI types and carries the handful of
//! assertions that are inseparable from a declaration — the two cross-crate
//! agreements on `gzFile_s`/`GzFileExposed` and `code`/`Code`, and the two
//! width relationships its own entry-point helpers depend on. This module owns
//! the *permanent, exhaustive* size and offset contract, which is why a few
//! assertions appear in both places. That redundancy is deliberate and free:
//! these items produce no code, and this is the module a reviewer is directed
//! to when the question is "what pins the ABI?".
//!
//! # Three properties of this file worth stating
//!
//! * **No `unsafe`.** No `unsafe` block, no `unsafe fn` and no
//!   unsafe operation anywhere in this file, and none is needed: `size_of`,
//!   `align_of` and `offset_of!` are all safe in const context. This crate is
//!   the only one in the workspace permitted `unsafe` at all, and this module
//!   declines the permission.
//!
//!   One caveat for anyone grepping: the *token* `unsafe` does appear in §5,
//!   inside function-pointer **type** arguments such as
//!   `size_of::<unsafe extern "C" fn(voidpf, uInt, uInt) -> voidpf>()`. That is
//!   the spelling of a type, not an operation — `size_of` never calls anything —
//!   and it is written that way on purpose, so that the assertion names exactly
//!   the type sitting inside [`crate::types::alloc_func`] rather than a
//!   look-alike.
//! * **No panics.** A failing `const _: () = assert!(…)` is a *compile*
//!   error — the const evaluator refuses to produce a value — so nothing here
//!   can panic at run time. There is deliberately no `assert_eq!`, no
//!   `debug_assert!` and no test-time check in this file; a runtime assertion
//!   would let a broken library ship and fail in the field instead of on the
//!   build machine.
//! * **No added dependency.** `core` and [`crate::types`], plus two
//!   items from [`zlib_rs`] — the first-party safe core that is already this
//!   crate's sole dependency, so neither import adds one. Both exist to
//!   discharge an agreement the core's own documentation delegates here: see §3
//!   for `ZOff64` and §4 for `Code`.
//!
//! # This module exports nothing, and must not start to
//!
//! It is declared `mod`, not `pub mod`, and it contains no item that could be
//! named from outside. Nothing here may ever be `#[no_mangle]`: the reference
//! shared object exports exactly 111 dynamic globals — 95 functions plus the 16
//! symbol-version nodes declared in `zlib.map`, under the SONAME `libz.so.1` —
//! and the parity diff is an exact match, so one extra exported name fails it
//! just as surely as one missing name.

use core::ffi::{c_char, c_int, c_uint, c_ulong, c_void};
use core::mem::{align_of, offset_of, size_of};

use crate::types::{
    alloc_func, code, free_func, gzFile, gzFile_s, gz_header, gz_headerp, in_func, internal_state,
    out_func, uInt, uLong, voidpf, z_crc_t, z_off64_t, z_off_t, z_size_t, z_stream, z_streamp,
    Bytef, ENOUGH, ENOUGH_DISTS, ENOUGH_LENS,
};
// The core's counterpart of `code`. See §4: the core's documentation asks the
// facade to pin the agreement, and `zlib-rs` is already this crate's only
// dependency, so naming it here adds nothing to the dependency graph.
use zlib_rs::inflate::inftrees::Code;
// The core's counterpart of `z_off64_t`, gated exactly as `types.rs` gates its
// own `zlib_rs::gz` import: the module requires `zlib-rs/std`, which this
// crate's `gz` feature turns on. See §3.
#[cfg(feature = "gz")]
use zlib_rs::gz::state::ZOff64;

/// True when the target uses the integer model the absolute numbers below were
/// measured under: LP64 — 64-bit pointers with a 64-bit `unsigned long`.
///
/// Both halves of the test are load-bearing and neither implies the other:
///
/// * `size_of::<*const c_void>() == 8` excludes ILP32 and the 16-bit
///   configurations, where every pointer field shrinks and every offset after
///   the first moves.
/// * `size_of::<c_ulong>() == 8` excludes **LLP64 Windows**, which has 64-bit
///   pointers and a 32-bit `unsigned long`. That target passes the first test
///   and fails the second, and it is the whole reason this constant is a
///   computed `bool` rather than a `cfg` predicate: no `cfg` reports the width
///   of `c_ulong`.
///
/// The same spelling is used by `crates/zlib-rs-differential/src/oracle.rs`, so
/// the facade and the oracle harness gate their absolutes identically.
//
// ★ `#[allow(dead_code)]` is required at the declared MSRV and is not cosmetic.
// This constant is used by more than fifty assertions below, but every one of
// those uses sits inside a `const _: () = …` item, and **rustc 1.80's dead-code
// pass does not traverse those bodies**: it reports `constant IS_LP64 is never
// used`. Verified with a minimal reproduction — the warning appears under rustc
// 1.80.1 and is absent under 1.97.1, which counts the uses correctly. Since CI
// builds with `-D warnings`, without this attribute the crate would fail to
// build on exactly the compiler `rust-version = "1.80"` promises to support.
//
// The attribute is inert on newer toolchains, so it costs nothing there, and it
// is scoped to this one item rather than to the module — real dead code
// elsewhere in the file is still reported. Remove it only when the MSRV rises
// past the version that fixed the analysis, and re-verify with
// `cargo +<msrv> build -p libz-rs-sys --all-features` before doing so.
#[allow(dead_code)]
const IS_LP64: bool = size_of::<*const c_void>() == 8 && size_of::<c_ulong>() == 8;

/// True when the target uses the *other* 64-bit integer model: LLP64 — 64-bit
/// pointers with a **32-bit** `unsigned long`, which is `x86_64` Windows.
///
/// This gate exists because gating the exact numbers on LP64 alone left the
/// second-most-important target in the world checked only relationally, and a
/// relational tier cannot see a narrowing re-type that padding absorbs (the
/// module documentation's "one thing the relational tier cannot see"). So the
/// LLP64 numbers are pinned exactly too, in their own tier beside each LP64 one.
///
/// The two gates are mutually exclusive by construction — `c_ulong` cannot be
/// both 4 and 8 bytes — so at most one absolute tier is ever live, and on ILP32
/// neither is.
///
/// # Where the LLP64 numbers come from
///
/// They are *derived*, not measured on hardware, and derived is enough because
/// `#[repr(C)]` placement is a function of nothing but field width and field
/// alignment: each field goes at the next offset that satisfies its own
/// alignment, and the struct is padded up to a multiple of its alignment. Feeding
/// the `zlib.h` declarations through that rule with `unsigned long` at 4 bytes and
/// pointers at 8 gives, for `z_stream`, size 88 and offsets
/// 0, 8, 12, 16, 24, 28, 32, 40, 48, 56, 64, 72, 76, 80; for `gz_header`, size 72
/// and offsets 0, 4, 8, 12, 16, 24, 28, 32, 40, 48, 56, 60, 64; and for
/// `struct gzFile_s`, size 24 and offsets 0, 8, 16 — unchanged from LP64, because
/// that struct contains no `unsigned long`.
///
/// Those numbers were confirmed by re-declaring all three structs on this LP64
/// host with `unsigned long` substituted by `u32` and reading `size_of` and
/// `offset_of!` back, which reproduces the LLP64 model exactly for layout
/// purposes. If a real Windows build ever disagrees with one of them, the build
/// fails here — which is the entire point, and is strictly better than the
/// silence a gated-away assertion gives.
///
/// The same spelling is used by `crates/zlib-rs-differential/src/oracle.rs`.
//
// ★ `#[allow(dead_code)]` for the same measured reason as `IS_LP64` above: rustc
// 1.80's dead-code pass does not traverse `const _: () = …` bodies, so without it
// the declared MSRV fails under `-D warnings`.
#[allow(dead_code)]
const IS_LLP64: bool = size_of::<*const c_void>() == 8 && size_of::<c_ulong>() == 4;

// The two models cannot both hold, and this is asserted rather than asserted-by-
// comment so that a future third gate cannot be written in a way that overlaps.
const _: () = assert!(!(IS_LP64 && IS_LLP64));

// =============================================================================
//  §1  `z_stream` — `zlib.h` L90-L110.  Measured: 112 bytes, align 8
// =============================================================================
//
//  The struct the entire streaming API is expressed in terms of.  The caller
//  allocates it, so every field offset is baked into caller object code, and
//  `deflate.c` L395 / `inflate.c` L179 / `infback.c` L31 additionally compare
//  the caller's `sizeof(z_stream)` against the library's and return
//  `Z_VERSION_ERROR` on a mismatch — so the size is a protocol value, not just a
//  layout detail.
//
//  Declaration order, with the C type of each field, since the relational
//  assertions below are written directly from it:
//
//      0  next_in    z_const Bytef *              pointer
//      1  avail_in   uInt                         unsigned int
//      2  total_in   uLong                        unsigned long
//      3  next_out   Bytef *                      pointer
//      4  avail_out  uInt                         unsigned int
//      5  total_out  uLong                        unsigned long
//      6  msg        z_const char *               pointer
//      7  state      struct internal_state FAR *  pointer
//      8  zalloc     alloc_func                   function pointer
//      9  zfree      free_func                    function pointer
//     10  opaque     voidpf                       pointer
//     11  data_type  int                          int
//     12  adler      uLong                        unsigned long
//     13  reserved   uLong                        unsigned long

// ---- Tier 1: the measured LP64 contract ------------------------------------

const _: () = assert!(!IS_LP64 || size_of::<z_stream>() == 112);
const _: () = assert!(!IS_LP64 || align_of::<z_stream>() == 8);

const _: () = assert!(!IS_LP64 || offset_of!(z_stream, next_in) == 0);
const _: () = assert!(!IS_LP64 || offset_of!(z_stream, avail_in) == 8);
const _: () = assert!(!IS_LP64 || offset_of!(z_stream, total_in) == 16);
const _: () = assert!(!IS_LP64 || offset_of!(z_stream, next_out) == 24);
const _: () = assert!(!IS_LP64 || offset_of!(z_stream, avail_out) == 32);
const _: () = assert!(!IS_LP64 || offset_of!(z_stream, total_out) == 40);
const _: () = assert!(!IS_LP64 || offset_of!(z_stream, msg) == 48);
const _: () = assert!(!IS_LP64 || offset_of!(z_stream, state) == 56);
const _: () = assert!(!IS_LP64 || offset_of!(z_stream, zalloc) == 64);
const _: () = assert!(!IS_LP64 || offset_of!(z_stream, zfree) == 72);
const _: () = assert!(!IS_LP64 || offset_of!(z_stream, opaque) == 80);
const _: () = assert!(!IS_LP64 || offset_of!(z_stream, data_type) == 88);
const _: () = assert!(!IS_LP64 || offset_of!(z_stream, adler) == 96);
const _: () = assert!(!IS_LP64 || offset_of!(z_stream, reserved) == 104);

// ---- Tier 1b: the derived LLP64 contract -----------------------------------
//
//  x86_64 Windows: 64-bit pointers, 32-bit `unsigned long`.  `avail_in` and
//  `total_in` now pack into the 8 bytes that LP64 spends on `avail_in` alone, and
//  the same happens again at `avail_out`/`total_out` and at
//  `data_type`/`adler`/`reserved` — which is why the struct is 88 bytes rather
//  than 112 and why `next_out` lands at 16 instead of 24.  See `IS_LLP64` for how
//  these were derived and confirmed.

const _: () = assert!(!IS_LLP64 || size_of::<z_stream>() == 88);
const _: () = assert!(!IS_LLP64 || align_of::<z_stream>() == 8);

const _: () = assert!(!IS_LLP64 || offset_of!(z_stream, next_in) == 0);
const _: () = assert!(!IS_LLP64 || offset_of!(z_stream, avail_in) == 8);
const _: () = assert!(!IS_LLP64 || offset_of!(z_stream, total_in) == 12);
const _: () = assert!(!IS_LLP64 || offset_of!(z_stream, next_out) == 16);
const _: () = assert!(!IS_LLP64 || offset_of!(z_stream, avail_out) == 24);
const _: () = assert!(!IS_LLP64 || offset_of!(z_stream, total_out) == 28);
const _: () = assert!(!IS_LLP64 || offset_of!(z_stream, msg) == 32);
const _: () = assert!(!IS_LLP64 || offset_of!(z_stream, state) == 40);
const _: () = assert!(!IS_LLP64 || offset_of!(z_stream, zalloc) == 48);
const _: () = assert!(!IS_LLP64 || offset_of!(z_stream, zfree) == 56);
const _: () = assert!(!IS_LLP64 || offset_of!(z_stream, opaque) == 64);
const _: () = assert!(!IS_LLP64 || offset_of!(z_stream, data_type) == 72);
const _: () = assert!(!IS_LLP64 || offset_of!(z_stream, adler) == 76);
const _: () = assert!(!IS_LLP64 || offset_of!(z_stream, reserved) == 80);

// `reserved` ends at 84 and the struct is 88: four bytes of tail padding, for the
// same reason `gz_header` has four on LP64 — an 8-aligned struct rounds up.
const _: () = assert!(
    !IS_LLP64
        || size_of::<z_stream>() - (offset_of!(z_stream, reserved) + size_of::<c_ulong>()) == 4
);

// ---- Tier 2: the general case, on every target ------------------------------
//
//  `#[repr(C)]` guarantees the first declared field sits at offset zero, that
//  later fields never move backwards, and that each is placed at the first
//  suitably aligned address at or after the end of its predecessor.  That is
//  enough to pin field *order* and field *width* exactly, without assuming
//  anything about how much padding a particular target inserts.

// The first field is at zero on every target, so this one is absolute.
const _: () = assert!(offset_of!(z_stream, next_in) == 0);

// `avail_in` follows a single pointer.  The equality — rather than a `>=` — is
// sound on every target: `uInt` never has stricter alignment than a pointer, and
// a pointer's size is a whole multiple of its own alignment, so no padding can
// appear between them.
const _: () = assert!(offset_of!(z_stream, avail_in) == size_of::<*const Bytef>());

// `state` follows `msg`, and both are plain data pointers of identical size and
// alignment, so again no padding is possible.  This pair is worth stating
// explicitly because `state` is the field every entry point round-trips through
// and the one a caller must never look inside.
const _: () =
    assert!(offset_of!(z_stream, state) == offset_of!(z_stream, msg) + size_of::<*const c_char>());

// The full declaration-order chain.  Each field starts at or after the end of
// its predecessor, which is simultaneously a strict-monotonicity check (every
// size below is non-zero) and a partial width check: widening a field, or
// reordering two of them, moves a later offset and breaks a link.  See the
// module documentation for the one case this cannot see — a narrowing that the
// target's padding absorbs — and for the two mechanisms that cover it.
const _: () =
    assert!(offset_of!(z_stream, total_in) >= offset_of!(z_stream, avail_in) + size_of::<uInt>());
const _: () =
    assert!(offset_of!(z_stream, next_out) >= offset_of!(z_stream, total_in) + size_of::<uLong>());
const _: () = assert!(
    offset_of!(z_stream, avail_out) >= offset_of!(z_stream, next_out) + size_of::<*mut Bytef>()
);
const _: () =
    assert!(offset_of!(z_stream, total_out) >= offset_of!(z_stream, avail_out) + size_of::<uInt>());
const _: () =
    assert!(offset_of!(z_stream, msg) >= offset_of!(z_stream, total_out) + size_of::<uLong>());
const _: () = assert!(
    offset_of!(z_stream, zalloc) >= offset_of!(z_stream, state) + size_of::<*mut internal_state>()
);
const _: () =
    assert!(offset_of!(z_stream, zfree) >= offset_of!(z_stream, zalloc) + size_of::<alloc_func>());
const _: () =
    assert!(offset_of!(z_stream, opaque) >= offset_of!(z_stream, zfree) + size_of::<free_func>());
const _: () =
    assert!(offset_of!(z_stream, data_type) >= offset_of!(z_stream, opaque) + size_of::<voidpf>());
const _: () =
    assert!(offset_of!(z_stream, adler) >= offset_of!(z_stream, data_type) + size_of::<c_int>());
const _: () =
    assert!(offset_of!(z_stream, reserved) >= offset_of!(z_stream, adler) + size_of::<uLong>());

// `reserved` is the last field, so the struct must be at least large enough to
// contain it.  This is what catches a field being *removed* from the tail, which
// none of the offset links above would notice.
const _: () = assert!(size_of::<z_stream>() >= offset_of!(z_stream, reserved) + size_of::<uLong>());

// The struct is at least as strictly aligned as its most demanding member.  A
// weaker alignment would put `total_in` or `next_in` on an address the caller's
// compiler assumed could not occur.
const _: () = assert!(align_of::<z_stream>() >= align_of::<*const Bytef>());
const _: () = assert!(align_of::<z_stream>() >= align_of::<uLong>());

// `z_streamp` (`zlib.h` L112) is a thin pointer: `z_stream` is `Sized`, so a
// `*mut z_stream` is one machine pointer with no metadata.  A fat pointer here
// would silently change the width of every exported signature.
const _: () = assert!(size_of::<z_streamp>() == size_of::<*mut c_void>());

// `struct internal_state` (`zlib.h` L88) is an incomplete type in C: declared,
// never defined, and only ever the pointee of a raw pointer.  The Rust mirror is
// zero-sized so that it has no layout to get wrong, and the pointer to it must
// stay thin for the same reason `z_streamp` must.
const _: () = assert!(size_of::<internal_state>() == 0);
const _: () = assert!(size_of::<*mut internal_state>() == size_of::<*mut c_void>());

// =============================================================================
//  §2  `gz_header` — `zlib.h` L118-L133.  Measured: 80 bytes, align 8
// =============================================================================
//
//  The gzip header exchanged with `deflateSetHeader` and `inflateGetHeader`.
//  Like `z_stream` it is caller-allocated, and `inflateGetHeader` *writes
//  through it* — into the caller's `extra`, `name` and `comment` buffers, each
//  write clamped to the matching `extra_max` / `name_max` / `comm_max` the
//  caller set.  A wrong offset here therefore does not merely misreport a field;
//  it reads a length from the wrong place and then writes that many bytes.
//
//  Declaration order, with the C type of each field:
//
//      0  text       int          7  name       Bytef *
//      1  time       uLong        8  name_max   uInt
//      2  xflags     int          9  comment    Bytef *
//      3  os         int         10  comm_max   uInt
//      4  extra      Bytef *     11  hcrc       int
//      5  extra_len  uInt        12  done       int
//      6  extra_max  uInt
//
//  ★ TWO NUMBERS THAT LOOK WRONG AND ARE NOT.  Both were measured; neither is a
//  transcription slip, and both have been "corrected" by people reading the
//  field list without compiling it:
//
//   1. `time` is at offset **8, not 4**.  `text` is an `int` occupying bytes
//      0..4, and `time` is a `uLong` needing 8-byte alignment on LP64, so bytes
//      4..8 are padding.  The first two fields of this struct span 16 bytes to
//      carry 12 bytes of data.
//
//   2. The struct is **80 bytes, not 76**.  `done` is an `int` at offset 72 and
//      ends at 76, but the struct's alignment is 8 (it contains `uLong` and
//      pointer members), so the size is rounded up to the next multiple of 8 and
//      four bytes of *tail padding* follow `done`.  Do not "fix" the 80 to 76:
//      that would make `sizeof` disagree with every C compiler on the platform
//      and would misplace every element of an array of `gz_header`.

// ---- Tier 1: the measured LP64 contract ------------------------------------

const _: () = assert!(!IS_LP64 || size_of::<gz_header>() == 80);
const _: () = assert!(!IS_LP64 || align_of::<gz_header>() == 8);

const _: () = assert!(!IS_LP64 || offset_of!(gz_header, text) == 0);
const _: () = assert!(!IS_LP64 || offset_of!(gz_header, time) == 8);
const _: () = assert!(!IS_LP64 || offset_of!(gz_header, xflags) == 16);
const _: () = assert!(!IS_LP64 || offset_of!(gz_header, os) == 20);
const _: () = assert!(!IS_LP64 || offset_of!(gz_header, extra) == 24);
const _: () = assert!(!IS_LP64 || offset_of!(gz_header, extra_len) == 32);
const _: () = assert!(!IS_LP64 || offset_of!(gz_header, extra_max) == 36);
const _: () = assert!(!IS_LP64 || offset_of!(gz_header, name) == 40);
const _: () = assert!(!IS_LP64 || offset_of!(gz_header, name_max) == 48);
const _: () = assert!(!IS_LP64 || offset_of!(gz_header, comment) == 56);
const _: () = assert!(!IS_LP64 || offset_of!(gz_header, comm_max) == 64);
const _: () = assert!(!IS_LP64 || offset_of!(gz_header, hcrc) == 68);
const _: () = assert!(!IS_LP64 || offset_of!(gz_header, done) == 72);

// The tail padding, asserted rather than only described, so that the four bytes
// are a checked property of the layout instead of a comment somebody may
// disbelieve.  `done` ends at 76; the struct is 80.
const _: () = assert!(
    !IS_LP64 || size_of::<gz_header>() - (offset_of!(gz_header, done) + size_of::<c_int>()) == 4
);

// ---- Tier 1b: the derived LLP64 contract -----------------------------------
//
//  The only `unsigned long` in this struct is `time`, so on LLP64 it shrinks to
//  four bytes and sits at offset 4 immediately after `text` with no padding
//  between them — where LP64 pads `text` out to 8.  Everything up to `extra`
//  therefore moves down by 8, and `comment` still needs its 8-byte alignment, so
//  the struct settles at 72 bytes.  See `IS_LLP64`.

const _: () = assert!(!IS_LLP64 || size_of::<gz_header>() == 72);
const _: () = assert!(!IS_LLP64 || align_of::<gz_header>() == 8);

const _: () = assert!(!IS_LLP64 || offset_of!(gz_header, text) == 0);
const _: () = assert!(!IS_LLP64 || offset_of!(gz_header, time) == 4);
const _: () = assert!(!IS_LLP64 || offset_of!(gz_header, xflags) == 8);
const _: () = assert!(!IS_LLP64 || offset_of!(gz_header, os) == 12);
const _: () = assert!(!IS_LLP64 || offset_of!(gz_header, extra) == 16);
const _: () = assert!(!IS_LLP64 || offset_of!(gz_header, extra_len) == 24);
const _: () = assert!(!IS_LLP64 || offset_of!(gz_header, extra_max) == 28);
const _: () = assert!(!IS_LLP64 || offset_of!(gz_header, name) == 32);
const _: () = assert!(!IS_LLP64 || offset_of!(gz_header, name_max) == 40);
const _: () = assert!(!IS_LLP64 || offset_of!(gz_header, comment) == 48);
const _: () = assert!(!IS_LLP64 || offset_of!(gz_header, comm_max) == 56);
const _: () = assert!(!IS_LLP64 || offset_of!(gz_header, hcrc) == 60);
const _: () = assert!(!IS_LLP64 || offset_of!(gz_header, done) == 64);

// `done` ends at 68; the struct is 72.  The same four bytes of tail padding as on
// LP64, arrived at from different field widths.
const _: () = assert!(
    !IS_LLP64 || size_of::<gz_header>() - (offset_of!(gz_header, done) + size_of::<c_int>()) == 4
);

// ---- Tier 2: the general case, on every target ------------------------------

const _: () = assert!(offset_of!(gz_header, text) == 0);

// The full declaration-order chain, on the same `>=` principle as §1.  Every
// pointer-to-pointer step could be an equality, but a uniform chain is easier to
// audit against the header, and the weaker link is still sufficient: it fails
// the moment two fields swap or one changes width.
const _: () =
    assert!(offset_of!(gz_header, time) >= offset_of!(gz_header, text) + size_of::<c_int>());
const _: () =
    assert!(offset_of!(gz_header, xflags) >= offset_of!(gz_header, time) + size_of::<uLong>());
const _: () =
    assert!(offset_of!(gz_header, os) >= offset_of!(gz_header, xflags) + size_of::<c_int>());
const _: () =
    assert!(offset_of!(gz_header, extra) >= offset_of!(gz_header, os) + size_of::<c_int>());
const _: () = assert!(
    offset_of!(gz_header, extra_len) >= offset_of!(gz_header, extra) + size_of::<*mut Bytef>()
);
const _: () = assert!(
    offset_of!(gz_header, extra_max) >= offset_of!(gz_header, extra_len) + size_of::<uInt>()
);
const _: () =
    assert!(offset_of!(gz_header, name) >= offset_of!(gz_header, extra_max) + size_of::<uInt>());
const _: () = assert!(
    offset_of!(gz_header, name_max) >= offset_of!(gz_header, name) + size_of::<*mut Bytef>()
);
const _: () =
    assert!(offset_of!(gz_header, comment) >= offset_of!(gz_header, name_max) + size_of::<uInt>());
const _: () = assert!(
    offset_of!(gz_header, comm_max) >= offset_of!(gz_header, comment) + size_of::<*mut Bytef>()
);
const _: () =
    assert!(offset_of!(gz_header, hcrc) >= offset_of!(gz_header, comm_max) + size_of::<uInt>());
const _: () =
    assert!(offset_of!(gz_header, done) >= offset_of!(gz_header, hcrc) + size_of::<c_int>());
const _: () = assert!(size_of::<gz_header>() >= offset_of!(gz_header, done) + size_of::<c_int>());

// The three buffer pointers a caller supplies and `inflateGetHeader` writes
// through, each paired with the `uInt` bound that limits the write.  Stating the
// pairing here keeps the clamped-write contract of `zlib.h` L118-L133 visible
// from the module that pins the offsets it depends on.
const _: () = assert!(offset_of!(gz_header, extra) < offset_of!(gz_header, extra_max));
const _: () = assert!(offset_of!(gz_header, name) < offset_of!(gz_header, name_max));
const _: () = assert!(offset_of!(gz_header, comment) < offset_of!(gz_header, comm_max));

const _: () = assert!(align_of::<gz_header>() >= align_of::<*mut Bytef>());
const _: () = assert!(align_of::<gz_header>() >= align_of::<uLong>());

// `gz_headerp` (`zlib.h` L135) must stay thin, for the reason `z_streamp` must.
const _: () = assert!(size_of::<gz_headerp>() == size_of::<*mut c_void>());

// =============================================================================
//  §3  `struct gzFile_s` — `zlib.h` L1956-L1960.  Measured: 24 bytes, align 8
// =============================================================================
//
//  ★ THE HIGHEST-RISK LAYOUT IN THE ENTIRE PORT.  Not because it is complicated
//  — three fields — but because it is the only one whose field arithmetic is
//  performed by code this project cannot see, cannot change and cannot
//  recompile.
//
//  `gzgetc` is a macro, not merely a function.  `zlib.h` L1966-L1968:
//
//      #define gzgetc(g) \
//            ((g)->have ? ((g)->have--, (g)->pos++, *((g)->next)++) : (gzgetc)(g))
//
//  with an identical `z_gzgetc` under `Z_PREFIX_SET` at L1964-L1965, and a
//  `gzgetc_` function at L1961 retained for callers that took the function's
//  address before the macro existed.
//
//  So on its fast path a caller performs three mutations per byte with no call
//  into this library at all: it decrements `have`, increments `pos`, and reads
//  through `next` and then post-increments the pointer itself.  Those loads and
//  stores are compiled into caller object code as fixed displacements — 0, 8 and
//  16 on this target.  `gzguts.h` L169-L172 is the other half of the contract:
//  `gz_state` embeds `struct gzFile_s x;` as its **first** member ("x" for
//  exposed), so the real state begins with exactly this prefix and a `gzFile`
//  handed to a caller points at it.
//
//  The consequence of getting any of it wrong is not a diagnosable failure.  It
//  is a load or a store at an address inside some other field, in every program
//  that uses `gzgetc`, with no error path and no symptom until something
//  unrelated breaks.  `zlib.h` L1949-L1954 warns callers that these names and
//  behaviours "could change in the future, perhaps even capriciously" — but that
//  licence belongs to upstream zlib, not to a drop-in replacement, which must
//  match whatever the installed header already promised.
//
//  Coordination with the core, which is where the state actually lives:
//
//   * `crates/zlib-rs/src/gz/state.rs` L303-L319 asserts the same absolutes
//     (24 / 0 / 8 / 16) on its own `GzFileExposed`, and L325-L333 the relational
//     forms, including `offset_of!(GzState, x) == 0` for the outer struct.
//   * `crates/libz-rs-sys/src/types.rs` L700-L712 joins the two declarations,
//     asserting equal size, equal alignment and equal offsets for all three
//     fields — the one assertion no other module can make, because no other
//     module imports both types.
//   * This module pins the C-side numbers themselves.  All three layers must
//     agree; each catches a different way of breaking the chain.

// ---- Tier 1: the measured LP64 contract ------------------------------------

const _: () = assert!(!IS_LP64 || size_of::<gzFile_s>() == 24);
const _: () = assert!(!IS_LP64 || align_of::<gzFile_s>() == 8);
const _: () = assert!(!IS_LP64 || offset_of!(gzFile_s, have) == 0);
const _: () = assert!(!IS_LP64 || offset_of!(gzFile_s, next) == 8);
const _: () = assert!(!IS_LP64 || offset_of!(gzFile_s, pos) == 16);

// ---- Tier 1b: the derived LLP64 contract -----------------------------------
//
//  IDENTICAL to LP64, and that is the finding rather than a copy-paste: this
//  struct contains no `unsigned long`.  `have` is `unsigned`, `next` is a pointer
//  and `pos` is `z_off64_t`, which is 64 bits on every target by construction (see
//  §5), so the only model-dependent quantity is the pointer width — the same 8
//  bytes on both 64-bit models.  Asserting it anyway is what makes the sameness
//  checked: the `gzgetc` macro at `zlib.h` L1966-L1968 dereferences all three
//  fields inside CALLER-compiled code, so a Windows-only shift here would corrupt
//  memory in every program that uses the macro, with nothing in this library able
//  to notice.

const _: () = assert!(!IS_LLP64 || size_of::<gzFile_s>() == 24);
const _: () = assert!(!IS_LLP64 || align_of::<gzFile_s>() == 8);
const _: () = assert!(!IS_LLP64 || offset_of!(gzFile_s, have) == 0);
const _: () = assert!(!IS_LLP64 || offset_of!(gzFile_s, next) == 8);
const _: () = assert!(!IS_LLP64 || offset_of!(gzFile_s, pos) == 16);

// ---- Tier 2: the general case, on every target ------------------------------
//
//  On a 32-bit target the prefix collapses to `have` at 0, `next` at 4 and `pos`
//  at 8, for a total of 16 bytes with no padding after `have` at all — which is
//  exactly why the numbers above are gated and these are not.

const _: () = assert!(offset_of!(gzFile_s, have) == 0);
const _: () =
    assert!(offset_of!(gzFile_s, next) >= offset_of!(gzFile_s, have) + size_of::<c_uint>());
const _: () =
    assert!(offset_of!(gzFile_s, pos) >= offset_of!(gzFile_s, next) + size_of::<*mut u8>());
const _: () = assert!(size_of::<gzFile_s>() >= offset_of!(gzFile_s, pos) + size_of::<z_off64_t>());

// `pos` is a *signed* 64-bit position on every target: `gzseek` and `gztell`
// return negative values to report failure, and `gzungetc` decrements it.  A
// narrower `pos` would make the caller's `pos++` wrap after 2 GiB — inside a
// macro expansion the library never sees.  §5 asserts the same width again from
// the `zconf.h` side; here it is a property of the caller-visible prefix.
const _: () = assert!(size_of::<z_off64_t>() == 8);

// `gzFile` (`zlib.h` L1354) must stay thin: it is the caller's whole handle.
const _: () = assert!(size_of::<gzFile>() == size_of::<*mut c_void>());

// The agreement `crates/zlib-rs/src/gz/state.rs` L204-L212 explicitly asks the
// facade to make: "must const-assert `size_of::<z_off64_t>() == size_of::<ZOff64>()`
// so that a platform where the two disagree fails to build rather than silently
// truncating a file position."  The core fixes `ZOff64` at `i64` because
// `gzFile_s::pos` is part of the caller-visible `gzgetc` prefix and cannot be
// allowed to vary; this is the assertion that keeps the public alias in step with
// it.  Gated on `gz` because that is the feature under which the core's `gz`
// module exists at all — `gzFile_s` itself is unconditional, since it is an ABI
// type the generated header must always carry.
#[cfg(feature = "gz")]
const _: () = assert!(size_of::<z_off64_t>() == size_of::<ZOff64>());
#[cfg(feature = "gz")]
const _: () = assert!(align_of::<z_off64_t>() == align_of::<ZOff64>());

// =============================================================================
//  §4  `code` and the inflate-table capacities — `inftrees.h`
//      Measured: `code` is 4 bytes, align 2
// =============================================================================
//
//  `inftrees.h` L24-L28:
//
//      typedef struct {
//          unsigned char op;      /* operation, extra bits, table bits */
//          unsigned char bits;    /* bits in this part of the code */
//          unsigned short val;    /* offset in table or code value */
//      } code;
//
//  and its own comment at L23 states the requirement outright: "Each entry is
//  four bytes."
//
//  Why an ABI mirror of a *private* header type has to be pinned here at all:
//  `inflate_table` is a real symbol.  The reference build emits it, `zlib.map`
//  lists it in the `local:` block so it stays hidden from the dynamic symbol
//  table, and `test/infcover.c` declares and calls it directly: L623 is
//  `code *next, table[ENOUGH_DISTS];` and L632/L636 pass `&next` straight to
//  `inflate_table`.  That array's stride is `sizeof(code)` as computed by the
//  *test's* compiler, from the unmodified header.
//  `crates/libz-rs-sys/src/inflate.rs` therefore presents the exact C signature
//
//      int inflate_table(codetype, unsigned short *, unsigned, code **,
//                        unsigned *, unsigned short *);
//
//  and must be able to hand the core's arena straight through it.  If the two
//  sides disagreed on the entry size, a table written by one and read by the
//  other would be silently misaligned from the second entry onward.
//
//  Unlike §1 to §3, none of these numbers is platform-dependent: `u8`, `u8` and
//  `u16` have the same widths and the same alignments on every target Rust
//  supports.  They are therefore asserted unconditionally, with no LP64 gate.

const _: () = assert!(size_of::<code>() == 4);
const _: () = assert!(align_of::<code>() == 2);
const _: () = assert!(offset_of!(code, op) == 0);
const _: () = assert!(offset_of!(code, bits) == 1);
const _: () = assert!(offset_of!(code, val) == 2);

// No padding anywhere: `op` and `bits` are single bytes and `val` needs only
// 2-byte alignment, so the four bytes are entirely payload.  Stated as an
// independent relation because it is the property `test/infcover.c`'s array
// stride actually depends on.
const _: () = assert!(size_of::<code>() == 2 * size_of::<u8>() + size_of::<u16>());

// ---- Layout agreement with the core's `Code` --------------------------------
//
//  `zlib_rs::inflate::inftrees::Code` (`crates/zlib-rs/src/inflate/inftrees.rs`
//  L261-L268) holds the same three fields in the same order and is what the
//  core's own decode tables are built from.
//
//  ★ Agreement in size and alignment is a *sanity check, not a licence to
//  transmute.*  The core's type is deliberately **not** `#[repr(C)]` — nothing
//  in that crate crosses the C ABI, which is this crate's job — and its
//  documentation forbids casting a caller's `code *` to a `*mut Code`.
//  Conversion goes field by field through the `From` implementations in
//  `types.rs`, which are sound whatever layout the core's type happens to have.
//  What these assertions protect is the *budget*: `struct inflate_state` reserves
//  `4 * ENOUGH` bytes for its inline arena and `test/infcover.c` accounts for
//  that budget when it caps allocation, so an entry that grew to six or eight
//  bytes would overrun the reservation on one side of the boundary while the
//  other still believed in four.
//
//  Field offsets are deliberately *not* asserted on `Code`: pinning them would
//  over-constrain a type whose whole point is that it owes the ABI nothing.
//  Size and alignment are the two properties the arena budget and the array
//  stride depend on, so they are the two that are checked.  `types.rs`
//  L780-L781 carries the same pair of equalities; the redundancy is intentional,
//  since this is the module a reviewer consults for the layout contract.
const _: () = assert!(size_of::<code>() == size_of::<Code>());
const _: () = assert!(align_of::<code>() == align_of::<Code>());

// ---- Table capacities — `inftrees.h` L49-L51 --------------------------------
//
//  Not layout, but the same class of contract: fixed numbers a caller's array
//  declaration bakes in.  `test/infcover.c` L623 sizes its table with
//  `ENOUGH_DISTS` taken from its own copy of the header, so a library that
//  disagreed would either overrun the caller's array or refuse a table that fits.
//
//  The comment at `inftrees.h` L38-L48 records how the figures were obtained —
//  `enough 286 9 15` returns 852 for the literal/length code and `enough 30 6 15`
//  returns 592 for the distance code, using `examples/enough.c` — and warns that
//  both must be recalculated if the root table size (the fifth argument to the
//  `inflate_table` calls in `inflate.c` and `infback.c`) ever changes.  These
//  assertions are what would force that recalculation to be deliberate.
const _: () = assert!(ENOUGH_LENS == 852);
const _: () = assert!(ENOUGH_DISTS == 592);
const _: () = assert!(ENOUGH == 1444);

// The relationship, not just the three values: `ENOUGH` is defined as the sum,
// so changing one operand without the total must not compile.
const _: () = assert!(ENOUGH == ENOUGH_LENS + ENOUGH_DISTS);

// The arena budget these capacities imply, in bytes.  Four bytes per entry — the
// figure asserted at the top of this section — times 1444 entries is 5776 bytes,
// which is the dominant term in `struct inflate_state`'s measured 7160 (see §6).
// Written in the `c_uint` domain rather than with a cast to `usize`, because the
// entry size is already pinned above and a narrowing cast here would buy nothing.
const _: () = assert!(4 * ENOUGH == 5776);

// =============================================================================
//  §5  Primitive typedef widths — `zconf.h`
// =============================================================================
//
//  The widths every struct above is built out of, and the widths every exported
//  signature is written in terms of.  Four of them are additionally *published*
//  through `zlibCompileFlags` (`zutil.c` L32-L57), whose low byte the reference
//  build reports as `0b1010_1001` — see the module documentation — so a wrong
//  width here is visible to any caller that asks.
//
//  A note on what these assertions can and cannot catch, because it decides how
//  much weight they carry.  A form such as
//
//      size_of::<uLong>() == size_of::<c_ulong>()
//
//  is *not* a tautology to the compiler: it re-checks that the alias still
//  resolves to `c_ulong` and not to something someone spelled `u64` in a hurry.
//  But on LP64 a wrongly hardcoded `u64` satisfies it, because `c_ulong` is 8
//  bytes here too.  Only the LLP64 build catches that one — which is precisely
//  why the alias must be `core::ffi::c_ulong` in the first place, why §1's
//  relational chain is written in terms of the aliases rather than of literal
//  widths, and why the cbindgen header diff exists as a third check.

// ---- `uInt` — `zconf.h` L405, "unsigned int, 16 bits or more" ---------------
//
//  The type of every `avail_*` count and of `extra_len`, `extra_max`, `name_max`
//  and `comm_max`.  `gzFile_s::have` is the same width, declared as a bare
//  `unsigned` in `zlib.h` L1957 and mirrored as `c_uint`.
const _: () = assert!(!IS_LP64 || size_of::<uInt>() == 4);
const _: () = assert!(size_of::<uInt>() == size_of::<c_uint>());
const _: () = assert!(align_of::<uInt>() == align_of::<c_uint>());
// The header's own floor: "16 bits or more".
const _: () = assert!(size_of::<uInt>() >= 2);
// Every entry point in this crate widens a `uInt` count to `usize` to index a
// slice, so the widening must be lossless.  `types.rs` L324 relies on the same
// relationship for the same reason.
const _: () = assert!(size_of::<uInt>() <= size_of::<usize>());

// ---- `uLong` — `zconf.h` L406, "unsigned long, 32 bits or more" -------------
//
//  ★ The platform-integer-model hazard in person.  8 bytes on LP64 (measured
//  here), 4 bytes on LLP64 Windows.  This is the type of `total_in`, `total_out`,
//  `adler`, `reserved`, `gz_header::time`, and the return value of `adler32`,
//  `crc32`, `crc32_combine_gen` and `zlibCompileFlags`.  Mapping it to a
//  fixed-width integer corrupts all of them on one platform family or the other.
const _: () = assert!(!IS_LP64 || size_of::<uLong>() == 8);
const _: () = assert!(size_of::<uLong>() == size_of::<c_ulong>());
const _: () = assert!(align_of::<uLong>() == align_of::<c_ulong>());
// The header's own floor: "32 bits or more".  This is what makes a 32-bit
// Adler-32 or CRC-32 result representable at all.
const _: () = assert!(size_of::<uLong>() >= 4);

// ---- `voidpf` — `zconf.h` L421, `void FAR *` --------------------------------
//
//  The type of `z_stream::opaque` and of both the argument and the result of
//  `alloc_func`.
const _: () = assert!(!IS_LP64 || size_of::<voidpf>() == 8);
const _: () = assert!(size_of::<voidpf>() == size_of::<*mut c_void>());
const _: () = assert!(align_of::<voidpf>() == align_of::<*mut c_void>());

// ---- `z_off_t` and `z_off64_t` — `zconf.h` L494-L496, L518-L520, L522-L532 --
//
//  `z_off_t` is `off_t` when `<unistd.h>` supplied one (the reference build sets
//  `-D_LARGEFILE64_SOURCE=1`, so it did) and `long long` otherwise; `z_off64_t`
//  is `off64_t`, `long long`, `__int64` or `offset_t` depending on the toolchain,
//  and every one of those spellings is 64 bits wherever large-file support
//  exists at all.
const _: () = assert!(!IS_LP64 || size_of::<z_off_t>() == 8);
const _: () = assert!(size_of::<z_off64_t>() == 8);
// The narrowing contract, and the reason it must hold on every target rather than
// only on this one: `gzseek` computes in `z_off64_t`, checks
// `ret == (z_off_t)ret` and only then narrows (`gzlib.c` L438-L442), and `gztell`
// and `gzoffset` do the same (L461-L465, L490-L495).  A `z_off_t` *wider* than
// `z_off64_t` would make that guard meaningless.
const _: () = assert!(size_of::<z_off_t>() <= size_of::<z_off64_t>());

// ---- `z_size_t` — `zconf.h` L253-L270 ---------------------------------------
//
//  The type of the `_z` entry points (`compressBound_z`, `deflateBound_z`,
//  `compress_z`, `compress2_z`, `uncompress_z`, `uncompress2_z`), of `adler32_z`
//  and `crc32_z`, and of `gzfread` and `gzfwrite`.  `zconf.h` L265 resolves it to
//  `size_t` on any ordinary `STDC` build, so it must be exactly `usize`: a
//  narrower or wider spelling would change those signatures rather than merely
//  their range.  `types.rs` L329 asserts the size for that reason; the alignment
//  is added here to complete the pairing.
const _: () = assert!(!IS_LP64 || size_of::<z_size_t>() == 8);
const _: () = assert!(size_of::<z_size_t>() == size_of::<usize>());
const _: () = assert!(align_of::<z_size_t>() == align_of::<usize>());

// ---- `z_crc_t` — `zconf.h` L429-L444 ----------------------------------------
//
//  `Z_U4` is selected by preprocessor arithmetic: `unsigned` when
//  `UINT_MAX == 0xffffffff` (measured true here), otherwise `unsigned long`,
//  otherwise `unsigned short`.  Whichever branch is taken it is a 32-bit unsigned
//  integer, which is what the CRC-32 tables are made of and what `get_crc_table`
//  hands back as `const z_crc_t *`.  Unconditional: the CRC-32 word width is
//  fixed by RFC 1952, not by the platform.
const _: () = assert!(size_of::<z_crc_t>() == 4);
const _: () = assert!(align_of::<z_crc_t>() == 4);

// ---- `Bytef` — `zconf.h` L403 and L412 --------------------------------------
//
//  `Bytef` is `Byte FAR` is `unsigned char`, and `FAR` expands to nothing on
//  every target this crate supports (`zconf.h` L398-L400).  Every buffer in the
//  API is counted in these, so `size_of::<Bytef>() == 1` is what makes a byte
//  count and a byte offset the same number.
const _: () = assert!(size_of::<Bytef>() == 1);
const _: () = assert!(align_of::<Bytef>() == 1);

// ---- `c_int` — the width of `data_type`, `hcrc`, `done` and every return -----
//
//  Every exported function that reports a status returns a C `int`, and the
//  `Z_*` constants are `int`s.  Pinning it to 32 bits is what lets a `ReturnCode`
//  and a state tag round-trip through the boundary unchanged; `types.rs` L1818
//  asserts the same equality where it converts tags.
//
//  The `i32` equality is stated unconditionally rather than under the gate, and
//  the distinction is deliberate: C guarantees only "at least 16 bits", and Rust
//  does define `c_int` as `i16` on AVR and MSP430 — but neither can host this
//  crate, which builds a `cdylib` against `std`.  On every target that can (LP64,
//  LLP64 and ILP32 alike) a C `int` is 32 bits.  The `>= 2` floor below records
//  what the language actually promises.
const _: () = assert!(!IS_LP64 || size_of::<c_int>() == 4);
const _: () = assert!(size_of::<c_int>() == size_of::<i32>());
const _: () = assert!(size_of::<c_int>() >= 2);

// ---- The four nullable hook types — `zlib.h` L85-L86 and L1134-L1136 --------
//
//  ★ THE NULL-POINTER-OPTIMISATION PROOF, and it is the load-bearing assertion of
//  this section.
//
//  C spells the absent hook `Z_NULL`, i.e. the integer 0 in a function-pointer
//  slot.  Rust cannot mirror that with a bare `extern "C" fn`, because a bare
//  Rust function pointer is *non-null* — passing C's null through one is instant
//  undefined behaviour.  `types.rs` therefore declares all four hooks as
//  `Option<unsafe extern "C" fn(…)>`, relying on the niche at zero so that `None`
//  *is* the null pointer with no discriminant word anywhere.
//
//  That reliance is exactly the kind of thing that is true today, silently
//  assumed everywhere, and catastrophic if it ever stopped being true: eight
//  bytes of discriminant on each of `zalloc` and `zfree` would make `z_stream`
//  sixteen bytes larger and shift `zfree`, `opaque`, `data_type`, `adler` and
//  `reserved`, so every caller in existence would read the hooks — and write
//  `opaque` — at the wrong addresses.  These assertions are what `types.rs`
//  L79-L80 promises when it says the property is pinned here.
//
//  Each hook is checked twice, because the two checks fail for different reasons:
//
//   1. against a data pointer — the width C assigns `Z_NULL` through, and the
//      width the struct layout in §1 depends on;
//   2. against the corresponding *bare* function pointer type — which is the
//      no-discriminant property itself, and the one a wider `Option` breaks.

// `voidpf (*alloc_func)(voidpf opaque, uInt items, uInt size)` — `zlib.h` L85.
const _: () = assert!(size_of::<alloc_func>() == size_of::<*mut c_void>());
const _: () = assert!(align_of::<alloc_func>() == align_of::<*mut c_void>());
const _: () = assert!(
    size_of::<alloc_func>() == size_of::<unsafe extern "C" fn(voidpf, uInt, uInt) -> voidpf>()
);

// `void (*free_func)(voidpf opaque, voidpf address)` — `zlib.h` L86.
const _: () = assert!(size_of::<free_func>() == size_of::<*mut c_void>());
const _: () = assert!(align_of::<free_func>() == align_of::<*mut c_void>());
const _: () = assert!(size_of::<free_func>() == size_of::<unsafe extern "C" fn(voidpf, voidpf)>());

// `unsigned (*in_func)(void FAR *, z_const unsigned char FAR * FAR *)` —
// `zlib.h` L1134-L1135.  `inflateBack`'s input callback.
const _: () = assert!(size_of::<in_func>() == size_of::<*mut c_void>());
const _: () = assert!(align_of::<in_func>() == align_of::<*mut c_void>());
const _: () = assert!(
    size_of::<in_func>()
        == size_of::<unsafe extern "C" fn(*mut c_void, *mut *const u8) -> c_uint>()
);

// `int (*out_func)(void FAR *, unsigned char FAR *, unsigned)` — `zlib.h` L1136.
// `inflateBack`'s output callback.
const _: () = assert!(size_of::<out_func>() == size_of::<*mut c_void>());
const _: () = assert!(align_of::<out_func>() == align_of::<*mut c_void>());
const _: () = assert!(
    size_of::<out_func>()
        == size_of::<unsafe extern "C" fn(*mut c_void, *mut u8, c_uint) -> c_int>()
);

// =============================================================================
//  §6  Adjacent contracts this module records but cannot assert
// =============================================================================
//
//  Three measured facts belong with the numbers above, because anyone changing
//  those numbers needs them — but none can be asserted from this file, and saying
//  so explicitly is more useful than an assertion in the wrong place.
//
//  ── 1. The `struct inflate_state` size floor that `test/infcover.c` imposes ──
//
//  `test/infcover.c` L428 caps allocation with the library's own struct size:
//
//      mem_limit(&strm, (sizeof(struct inflate_state) << 1) + 256);
//
//  which is `2 * 7160 + 256` = **14576** bytes on this target, since
//  `sizeof(struct inflate_state)` measures **7160**.  Ten lines later, at L438,
//  it asserts
//
//      ret = inflateCopy(&copy, &strm);            assert(ret == Z_MEM_ERROR);
//
//  The stream was opened with `inflateInit2(&strm, -8)`, so its window is
//  `1 << 8` = 256 bytes.  Writing S for the state size, the budget arithmetic is:
//  the original stream has already spent `S + 256`, and the copy needs another
//  `S + 256`, so the copy's *window* allocation is refused — and the test passes
//  — only while `2S + 512 > 14576`, i.e. only while **S > 7032**.
//
//  A Rust `InflateState` of 7032 bytes or fewer lets both of the copy's
//  allocations through, `inflateCopy` returns `Z_OK`, and this assertion in an
//  unmodified C test fails.  The upper bound is the port's per-stream memory
//  budget, 115% of the C state: 8234 bytes.  So the state must land in
//  `(7032, 8234]`.
//
//  This file cannot assert that: naming `zlib_rs::inflate::InflateState` here
//  would pin a *core* invariant from the *facade*, in a module whose job is the C
//  ABI.  The enforcing assertion belongs in — and already lives in —
//  `crates/zlib-rs/src/inflate/state.rs` (see its module documentation and the
//  assertion beside its state declaration).  It is recorded here so that whoever
//  arrives to shrink that struct finds the constraint next to the numbers that
//  motivated it.
//
//  ── 2. The `struct inflate_state` prefix that `test/infcover.c` writes through ─
//
//  Measured from `inflate.h` L82-L84 with the same probe:
//
//      offset 0, width 8   `z_streamp strm`      pointer back to the stream
//      offset 8, width 4   `inflate_mode mode`   the current state
//
//  and `inflate_mode` (`inflate.h` L20-L53) is **not 0-based**: `HEAD = 16180`
//  and the 32 enumerators run consecutively to `SYNC = 16211`, with
//  `DICT = 16190` in between.  The odd base is deliberate — `inflate.c` L88-L98's
//  `inflateStateCheck` tests `state->strm != strm || state->mode < HEAD ||
//  state->mode > SYNC`, so an arbitrary integer landing in that field is
//  overwhelmingly likely to be rejected rather than mistaken for a valid state.
//  A zero-based enum would accept far too much.
//
//  It matters here because `test/infcover.c` reaches *into* that prefix, and the
//  facade has to survive it.  These are private-header internals, so they are not
//  part of the ABI this module pins, and the core's `Mode` enum is idiomatic Rust
//  rather than a `#[repr]`-fixed mirror.  Recorded, not asserted.
//
//  ── 3. Nothing in this module may become an exported symbol ─────────────────
//
//  Restating the module-level rule where a future edit would land: this file
//  declares no item that can be named from outside, and adding a `#[no_mangle]`
//  here — even a diagnostic helper — would put an extra name in `libz.so`'s
//  dynamic symbol table and fail the parity diff against the reference build's
//  111 globals (95 functions of type `T` plus the 16 version nodes of type `A`,
//  under the SONAME `libz.so.1`).  The diff is an exact match in both directions:
//  an extra export fails it exactly as a missing one does.
