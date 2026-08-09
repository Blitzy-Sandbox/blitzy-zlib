//! Runtime confirmation of every struct size, alignment and field offset the C
//! contract fixes.
//!
//! AAP §0.4.1.3 specifies this file as *"Runtime confirmation of every struct
//! size and field offset"*, sourced from `zlib.h`. It is the **runtime half** of
//! the machine-verified contract-immutability gate that AAP §0.7.1 (b) demands.
//! Its two counterparts are:
//!
//! | Mechanism | Where | When it fires |
//! |---|---|---|
//! | Compile-time assertions | `crates/libz-rs-sys/src/layout_assertions.rs` | every build of the crate |
//! | **This suite** | `crates/libz-rs-sys/tests/abi_layout.rs` | every `cargo test` |
//! | Generated-header comparison | root `cbindgen.toml` | when the header diff is run |
//!
//! **The three must never disagree.** Every number below was cross-checked
//! against `layout_assertions.rs` line by line and re-derived independently (see
//! "Provenance", below). If a number here ever contradicts one there, exactly one
//! of the two files is wrong and the disagreement must be *reconciled*, not
//! papered over.
//!
//! # Why a runtime suite as well as a compile-time one
//!
//! They fail in different places, and both places matter.
//!
//! A `const _: () = assert!(…)` item stops the *build*, which is the strongest
//! possible outcome and is why `layout_assertions.rs` exists. But it only ever
//! runs on the machine that compiles the crate, with that machine's target, and
//! its failure message is a const-evaluation error rather than a named test. This
//! suite adds three things the const items cannot give:
//!
//! * **A named failure.** `cargo test` reports `gz_header_layout_lp64 … FAILED`
//!   and prints the two numbers, so the struct at fault names itself. That is why
//!   the assertions are grouped into many small `#[test]` functions rather than
//!   one monolith.
//! * **Coverage of facts a `const` item is not placed to check.** Two of them:
//!   the internal `inflate_state` prefix that unmodified C *test* code writes
//!   through, and the non-zero-based `inflate_mode` ladder. See §9 and §10.
//! * **Execution under the sanitizers.** A `const` item is *evaluated*, not
//!   executed, so it is invisible to AddressSanitizer and to Miri; a test binary is
//!   not. This file touches no file, no socket and no subprocess — only `size_of`,
//!   `align_of`, `offset_of!` and one `CStr` read — and it was run under both
//!   `-Zsanitizer=address` and `cargo miri test` on nightly, clean in each.
//!
//! # ★ Two places where *unmodified C code* reaches into memory this port owns
//!
//! These deserve the most explicit comments in the file, because in both cases a
//! wrong number is not a wrong answer — it is a wrong *address*, computed inside
//! object code this port cannot recompile.
//!
//! 1. **The `struct gzFile_s` prefix.** `zlib.h` L1966-L1968 defines `gzgetc` as
//!    a macro — `((g)->have ? ((g)->have--, (g)->pos++, *((g)->next)++) :
//!    (gzgetc)(g))` — so the field arithmetic is compiled into every *caller's*
//!    object code. `gzguts.h` L170-L172 embeds `struct gzFile_s x` as the first
//!    member of `gz_state`, so the port's gz state must begin with exactly that
//!    prefix at offset 0. Any other layout is silent memory corruption in every
//!    caller that uses `gzgetc`. See §4.
//! 2. **`struct inflate_state`'s `mode` field.** `test/infcover.c` is compiled
//!    unmodified and linked against this library, and it writes the field
//!    directly: `((struct inflate_state *)strm.state)->mode = DICT;` at L330 and
//!    `state->mode = SYNC;` at L459. Both resolve to offset 8, so that offset is
//!    a hard ABI requirement rather than an internal detail. See §9.
//!
//! # ★ The three tiers, and why the absolute numbers are gated
//!
//! `uLong` is `unsigned long` (`zconf.h` L406): **8 bytes on LP64, 4 bytes on
//! LLP64 Windows**. `sizeof(z_stream)` is therefore 112 here and genuinely 88
//! there, so an unconditional `assert_eq!(size_of::<z_stream>(), 112)` would be a
//! portability bug wearing the costume of a safety check — correct on the target
//! it was measured on, and a spurious failure on a target where the layout is
//! right.
//!
//! Every assertion below therefore belongs to exactly one of three tiers, which
//! deliberately mirror the tiers in `layout_assertions.rs` so that the two files
//! agree mechanism for mechanism:
//!
//! | Tier | Gate | Checks on | Catches |
//! |---|---|---|---|
//! | Absolute, LP64 | [`is_lp64`] | LP64 only | any drift at all |
//! | Absolute, LLP64 | [`is_llp64`] | LLP64 only | any drift at all |
//! | Relational | none | every target | reordered, inserted, removed or widened fields |
//!
//! A gated-away tier **skips**; it never fails. The gate is a plain runtime `if`
//! with an early `return`, which is also what makes the gating auditable: flip
//! [`is_lp64`] to `false` and the absolute tests report `ok` having asserted
//! nothing, while [`relational_invariants_all_targets`] still passes on its own.
//! [`integer_model_is_recognised`] exists so that "asserted nothing" can never
//! happen silently on a 64-bit target.
//!
//! `#[cfg(…)]` cannot express the condition: the width of `c_ulong` is not a
//! `cfg` predicate, and `cfg(target_pointer_width = "64")` is wrong precisely on
//! LLP64, which is the case that matters.
//!
//! # Provenance of the numbers
//!
//! Measured, never assumed, and re-derived for this file rather than copied from
//! the plan: a C probe compiled against the real in-tree headers with
//! `gcc -I. -D_LARGEFILE64_SOURCE=1` (gcc 15.2.0,
//! `x86_64-unknown-linux-gnu`), printing `sizeof`, `_Alignof` and `offsetof` for
//! `z_stream`, `gz_header`, `struct gzFile_s`, `code`, `struct inflate_state`,
//! `gz_state` and every `zconf.h` typedef. `test/infcover.c` L16's own trick —
//! `#define ZLIB_INTERNAL` ahead of the private headers — is required, or
//! `inflate_table`'s prototype does not compile.
//!
//! | Type | Declared at | Size | Align | Field offsets |
//! |---|---|---|---|---|
//! | `z_stream` | `zlib.h` L90-L110 | 112 | 8 | 0, 8, 16, 24, 32, 40, 48, 56, 64, 72, 80, 88, 96, 104 |
//! | `gz_header` | `zlib.h` L118-L133 | 80 | 8 | 0, 8, 16, 20, 24, 32, 36, 40, 48, 56, 64, 68, 72 |
//! | `struct gzFile_s` | `zlib.h` L1956-L1960 | 24 | 8 | 0, 8, 16 |
//! | `code` | `inftrees.h` L24-L28 | 4 | 2 | 0, 1, 2 |
//! | `struct inflate_state` | `inflate.h` L82-L86 | 7160 | 8 | `strm` 0, `mode` 8, `last` 12, `wrap` 16 |
//!
//! `gz_header::time` sits at **8, not 4**: `uLong time` needs 8-byte alignment
//! after `int text`, leaving four bytes of padding. It is the single offset most
//! likely to be derived wrongly by hand, so §3 asserts it explicitly.
//!
//! # What is deliberately absent
//!
//! * **`zlibCompileFlags()`.** It belongs to `c_api_parity.rs`, which derives the
//!   expected value from `size_of` rather than hardcoding the measured `0xa9`.
//! * **The `ENOUGH_*` and `codetype` values in their facade spelling.**
//!   `crates/libz-rs-sys/src/types.rs` L943-L989 declares them `pub(crate)`, so an
//!   integration test — compiled as a separate crate — cannot name them, and
//!   reaching them would take a visibility change this file must not make. The
//!   compile-time counterpart already pins them. §5 asserts the same numbers
//!   through the core's public `usize` forms instead, which is a real check rather
//!   than a comment.
//! * **Any read through a live `z_stream::state` pointer.** That would pin the
//!   internal prefix exactly as `test/infcover.c` does, but it needs `unsafe` to
//!   reach a private item, which AAP §0.7.1 (a) forbids. §9 pins the same
//!   arithmetic with a `#[repr(C)]` mirror instead; the facade's own state prefix
//!   is separately pinned by `types.rs` L2903-L2928 and its unit tests.
//!
//! # Properties of this file worth stating
//!
//! * **Exactly one `unsafe` block**, in §8, for the `CStr::from_ptr` that reads
//!   the version string a C caller sees. `size_of`, `align_of` and `offset_of!`
//!   need no `unsafe` at all, so everything else here is safe by construction.
//! * **No dependency.** `core`, `std` and the two first-party crates already in
//!   this package's graph. AAP §0.7.1 (i) forbids adding `[dev-dependencies]`, and
//!   none is added.
//! * **`assert!` and `assert_eq!` are correct here.** AAP §0.7.1 (f)'s
//!   no-panic rule binds `src/**`, where a panic would abort a C caller's process.
//!   A test asserts, and a failing assertion panics; the root `clippy.toml` sets
//!   `allow-panic-in-tests` for exactly this reason. Those keys apply to `#[test]`
//!   context only, so the helpers in this file are written to be panic-free and no
//!   file-scope `allow` is needed.
//! * **Nothing here may be weakened.** If an assertion fails, the *layout* is
//!   wrong: fix `crates/libz-rs-sys/src/types.rs`, never this file.

// ★ THE CRATE IMPORT BINDING, and it is not the obvious one.
//
// `crates/libz-rs-sys/Cargo.toml` L164-L166 declares `[lib] name = "z"`, because
// that is what makes Cargo emit `libz.so` and `libz.a` instead of
// `liblibz_rs_sys.so` — the whole point of the facade. A library target's name is
// also its extern crate name, and a package's own test targets link it under that
// name with no opportunity to rename, so Cargo passes `--extern z=…` here and a
// bare `use libz_rs_sys::…;` does not resolve at all.
//
// Verified rather than assumed: both spellings were compiled against this manifest
// before this line was written. The alias is the form AAP §0.4.1.3's file
// specification calls for, and it lets the assertions below read against the
// crate's documented name while still naming the real target. Out-of-package
// consumers (`zlib-rs-differential`, the `fuzz/` targets, the benches) rename the
// dependency and reach the same items as `libz_rs_sys::…`; the sibling
// `tests/rust_api_surface.rs` records the same asymmetry from the other side.
//
// Neither spelling reaches a C caller, which resolves these symbols through the
// dynamic table, where Rust crate names do not exist.
extern crate z as libz_rs_sys;

use core::ffi::{c_int, c_uint, c_ulong, c_void};
// `size_of` and `align_of` joined the prelude in Rust 1.80, so writing
// `core::mem::size_of::<…>()` out in full trips the workspace's
// `unused_qualifications` lint (measured). Importing the three names and using the
// short forms is what `layout_assertions.rs` does, and it is also correct below
// the prelude change, so it is the spelling used throughout.
use core::mem::{align_of, offset_of, size_of};

// ★ Gated for a reason worth stating: `--no-default-features` is a SUPPORTED
// configuration of this crate, and this file must compile warning-free in it.
//
// Unlike the sibling `tests/rust_api_surface.rs`, this suite does NOT carry a
// file-scope `#![cfg(feature = "libz-compat")]`, because most of what it asserts is
// ungated: the `#[repr(C)]` ABI mirrors exist whether or not the exported surface is
// compiled, and their layout is exactly what must not drift. Only §8's version
// identity needs the feature, so the gate goes on §8 and on the handful of items
// that serve §8 alone: these two imports, the crate's own version items, and the two
// `C_ZLIB_*` constants. Without the attributes a `--no-default-features` build emits
// `unused_imports` and `dead_code`, which under CI's `-D warnings` is a build
// failure. Verified in every feature configuration the crate supports.
#[cfg(feature = "libz-compat")]
use core::ffi::c_char;
#[cfg(feature = "libz-compat")]
use std::ffi::CStr;

// The C ABI surface under test. Every one of these is re-exported at the crate
// root by `crates/libz-rs-sys/src/lib.rs` L1028-L1031; the `types` module itself is
// private, so this list is the only Rust path to them.
use libz_rs_sys::{
    alloc_func, charf, code, free_func, gzFile, gzFile_s, gz_header, gz_headerp, in_func,
    internal_state, intf, out_func, uInt, uIntf, uLong, uLongf, voidp, voidpc, voidpf, z_crc_t,
    z_off64_t, z_off_t, z_size_t, z_stream, z_streamp, Byte, Bytef, MAX_MEM_LEVEL, MAX_WBITS,
};

// The version identity, gated exactly as `crates/libz-rs-sys/src/lib.rs` L1036
// gates it: with `libz-compat` off there is no introspection module to take it
// from, and `--no-default-features` must still compile.
#[cfg(feature = "libz-compat")]
use libz_rs_sys::{
    zlibVersion, ZLIB_VERNUM, ZLIB_VERSION, ZLIB_VER_MAJOR, ZLIB_VER_MINOR, ZLIB_VER_REVISION,
    ZLIB_VER_SUBREVISION,
};

// The safe core, which is already this package's sole first-party dependency, so
// naming it adds nothing to the dependency graph — the same reasoning
// `crates/libz-rs-sys/src/layout_assertions.rs` L190-L203 applies to its own two
// `zlib_rs` imports. Cargo passes a test target the union of `[dependencies]` and
// `[dev-dependencies]`, so these paths resolve under every feature set, including
// `--no-default-features` (verified).
//
// They are used to check agreements that only the core can answer for: the
// `inftrees.h` table capacities, the `code` mirror, and the `inflate_mode` ladder.
use zlib_rs::inflate::inftrees::{Code, ENOUGH, ENOUGH_DISTS, ENOUGH_LENS};
use zlib_rs::inflate::Mode;

// The core's form of the caller-visible gz prefix. Gated exactly as `types.rs`
// L240-L241 gates its own import: the module needs `zlib-rs/std`, which this
// crate's `gz` feature turns on.
#[cfg(feature = "gz")]
use zlib_rs::gz::state::GzFileExposed;

// =============================================================================
//  The integer-model gates
// =============================================================================

/// True when the target uses the integer model the absolute numbers were measured
/// under: LP64 — 64-bit pointers with a 64-bit `unsigned long`.
///
/// Both halves are load-bearing and neither implies the other:
///
/// * `size_of::<*const c_void>() == 8` excludes ILP32 and the 16-bit
///   configurations, where every pointer field shrinks and every offset after the
///   first moves.
/// * `size_of::<c_ulong>() == 8` excludes **LLP64 Windows**, which has 64-bit
///   pointers and a 32-bit `unsigned long`. That target passes the first test and
///   fails the second, and it is the whole reason this is a computed predicate
///   rather than a `cfg`: no `cfg` reports the width of `c_ulong`.
///
/// Deliberately a plain `fn` and not a `const`. A `const bool` would let the
/// compiler fold the guard away, and `clippy::assertions_on_constants` would then
/// have an opinion about assertions built on it; more importantly, a runtime
/// predicate is what makes the "flip it to `false` and watch the absolute tests
/// skip rather than fail" audit in the module documentation possible.
///
/// The same spelling — pointer width *and* `c_ulong` width — is used by
/// `crates/libz-rs-sys/src/layout_assertions.rs` and by
/// `crates/zlib-rs-differential/src/oracle.rs`, so every layer gates identically.
fn is_lp64() -> bool {
    size_of::<*const c_void>() == 8 && size_of::<c_ulong>() == 8
}

/// True when the target uses the *other* 64-bit integer model: LLP64 — 64-bit
/// pointers with a **32-bit** `unsigned long`, which is `x86_64` Windows.
///
/// This gate exists because gating the exact numbers on LP64 alone would leave the
/// second-most-important target checked only relationally, and a relational chain
/// cannot see a *narrowing* re-type that the target's padding entirely absorbs.
/// So the LLP64 numbers are pinned exactly too, in their own tier beside each LP64
/// one, matching `layout_assertions.rs` tier for tier.
///
/// The LLP64 numbers are *derived*, not measured on hardware, and derived is
/// enough because `#[repr(C)]` placement is a function of nothing but field width
/// and field alignment: each field goes at the next offset satisfying its own
/// alignment, and the struct is padded up to a multiple of its alignment. If a
/// real Windows build ever disagrees, the test fails — which is the point, and is
/// strictly better than the silence a gated-away assertion gives.
///
/// The two gates are mutually exclusive by construction: `c_ulong` cannot be both
/// 4 and 8 bytes. [`integer_model_is_recognised`] asserts that.
fn is_llp64() -> bool {
    size_of::<*const c_void>() == 8 && size_of::<c_ulong>() == 4
}

/// Reports that a gated tier was skipped, naming the tier and the model that
/// caused the skip.
///
/// Printing is not decoration. A gated tier that vanishes without a trace is the
/// one way this suite could report `ok` while asserting nothing, so every skip
/// says so on stderr and [`integer_model_is_recognised`] independently proves that
/// a 64-bit target always matches one of the two absolute tiers.
///
/// Panic-free by construction — the `clippy.toml` in-tests allowances key on
/// `#[test]` context, which a helper does not have.
fn skipped(tier: &str) {
    eprintln!(
        "abi_layout: skipping {tier} — this target has {ptr}-byte pointers and a \
         {ulong}-byte c_ulong, which is not the integer model that tier pins",
        ptr = size_of::<*const c_void>(),
        ulong = size_of::<c_ulong>(),
    );
}

// =============================================================================
//  The C oracle, transcribed
// =============================================================================
//
// Every constant in this block is a number read out of an in-tree C header and
// confirmed by the probe described in the module documentation. They are named
// rather than inlined so that each assertion below cites its source, and so that
// the requirement survives in readable form even where a Rust item happens to
// hold the same value.

/// `inflate.h` L21: the `inflate_mode` enum's first discriminant, given
/// explicitly.
///
/// The remaining 31 names follow implicitly, so the ladder spans 16180 through
/// 16211 — **32** named variants. (AAP §0.1.2.1's "31-variant" figure undercounts;
/// the header text is authoritative and the probe counted 32.)
const C_HEAD: i32 = 16_180;

/// `inflate.h` L31: `DICT`, the eleventh variant, reached when a preset dictionary
/// is required.
///
/// Named separately because it is one of the two values *unmodified* C test code
/// writes into the port's own state: `test/infcover.c` L330 does
/// `((struct inflate_state *)strm.state)->mode = DICT;`.
const C_DICT: i32 = 16_190;

/// `inflate.h` L52: `SYNC`, the last variant, entered by `inflateSync`.
///
/// The other value unmodified C test code writes: `test/infcover.c` L459's `pull`
/// callback does `state->mode = SYNC;` to force an otherwise impossible situation.
const C_SYNC: i32 = 16_211;

/// The number of names in `inflate.h`'s `inflate_mode` enum, counted from the
/// header text: `HEAD` through `SYNC` inclusive.
const C_MODE_COUNT: usize = 32;

/// The `inflate_mode` spellings, in `inflate.h` L20-L53 declaration order.
///
/// Transcribed name for name so that §10 can hold the port's enum to the header's
/// own ordering rather than merely to its endpoints. A reordering that preserved
/// the endpoints would still change what every intermediate discriminant means,
/// and this array is what catches it.
const C_MODE_NAMES: [&str; C_MODE_COUNT] = [
    "HEAD", "FLAGS", "TIME", "OS", "EXLEN", "EXTRA", "NAME", "COMMENT", "HCRC", "DICTID", "DICT",
    "TYPE", "TYPEDO", "STORED", "COPY_", "COPY", "TABLE", "LENLENS", "CODELENS", "LEN_", "LEN",
    "LENEXT", "DIST", "DISTEXT", "MATCH", "LIT", "CHECK", "LENGTH", "DONE", "BAD", "MEM", "SYNC",
];

/// `zlib.h` L44: the version string every caller's compile-time `ZLIB_VERSION`
/// is compared against.
///
/// Written out as a literal rather than taken from the crate, because the point of
/// the assertion is to hold the crate to the header. It must **never** be derived
/// from `CARGO_PKG_VERSION`, which cannot express a four-component version.
///
/// Gated with §8, its only consumer; see the note above the `CStr` import.
#[cfg(feature = "libz-compat")]
const C_ZLIB_VERSION: &CStr = c"1.3.2.1-motley";

/// `zlib.h` L45: `ZLIB_VERNUM`, the same version packed a nibble per component,
/// most significant first.
///
/// Callers write `#if ZLIB_VERNUM >= 0x1290`, so the numeric form is part of the
/// contract in its own right.
///
/// Gated with §8, its only consumer; see the note above the `CStr` import.
#[cfg(feature = "libz-compat")]
const C_ZLIB_VERNUM: c_int = 0x1321;

/// `inftrees.h` L49: the literal/length table capacity, from
/// `enough 286 9 15`.
const C_ENOUGH_LENS: usize = 852;

/// `inftrees.h` L50: the distance table capacity, from `enough 30 6 15`.
const C_ENOUGH_DISTS: usize = 592;

/// `inftrees.h` L51: `ENOUGH_LENS + ENOUGH_DISTS`.
///
/// `struct inflate_state` budgets `4 * ENOUGH` bytes for its inline arena, which
/// is why `test/infcover.c`'s allocation limiting depends on this number being
/// right.
const C_ENOUGH: usize = 1444;

/// `zconf.h` L287: the largest `windowBits` `deflateInit2_` and `inflateInit2_`
/// accept — a 32 KiB LZ77 window.
const C_MAX_WBITS: c_int = 15;

/// `zconf.h` L277: the largest `memLevel` `deflateInit2_` accepts.
const C_MAX_MEM_LEVEL: c_int = 9;

// =============================================================================
//  The internal inflate-state prefix, mirrored
// =============================================================================

/// The first four members of C's `struct inflate_state`, in declaration order.
///
/// `inflate.h` L82-L86:
///
/// ```c
/// struct inflate_state {
///     z_streamp strm;             /* pointer back to this zlib stream */
///     inflate_mode mode;          /* current inflate mode */
///     int last;                   /* true if processing last block */
///     int wrap;                   /* bit 0 true for zlib, … */
///     /* … 30 further members … */
/// };
/// ```
///
/// ★ **Why a mirror rather than the real thing.** `struct inflate_state` is
/// declared in a *private* header, and the port's counterpart —
/// `crates/libz-rs-sys/src/types.rs`'s `StatePrefix` — is `pub(crate)`, so an
/// integration test cannot name it and must not be given a way to. That is not a
/// gap: `types.rs` L2903-L2928 pins `StatePrefix` with `const` assertions (`strm`
/// at 0, the validity tag at 8, size 16) and its own unit tests repeat them at run
/// time.
///
/// What this mirror pins is the *other* half of the agreement, and the half no
/// assertion inside the crate can state: that the C declaration order in
/// `inflate.h`, laid out by the platform's `#[repr(C)]` rule with the facade's own
/// `z_streamp` and a 4-byte enum, really does put `mode` at offset 8. That is the
/// arithmetic baked into `test/infcover.c`'s object code, and §9 is where it is
/// checked.
///
/// `inflate_mode` is spelled `c_int` here because the probe measured C's enum at
/// exactly 4 bytes with 4-byte alignment on this target, which is what an `int`-
/// sized enum is.
#[repr(C)]
struct CInflateStatePrefix {
    /// Offset 0 — `z_streamp strm`, the back-pointer C compares against the
    /// incoming stream to reject a state belonging to a different one.
    strm: z_streamp,
    /// Offset 8 — `inflate_mode mode`. **The load-bearing offset.**
    mode: c_int,
    /// Offset 12 — `int last`.
    last: c_int,
    /// Offset 16 — `int wrap`.
    wrap: c_int,
}

// =============================================================================
//  §1  Relational invariants — hold on every target, gated on nothing
// =============================================================================

/// The tier that runs everywhere: relationships between sizes and offsets that are
/// true under every integer model, so none of them may be gated.
///
/// This is the only tier that still checks anything on a target neither absolute
/// tier covers — ILP32, for instance — and it is deliberately self-sufficient
/// rather than decorative. Reordering two `z_stream` fields, inserting one,
/// removing one or *widening* one all move a later offset and fail here.
///
/// The one thing it cannot see is a **narrowing** re-type that the target's padding
/// entirely absorbs: making `total_in` a `u8` on ILP32 leaves it at offset 8 and
/// `next_out` still lands at 12, so every link in the chain holds. That case is
/// covered by the absolute tiers on the two 64-bit models (§2, §3, §4, §6, §9) and by
/// cbindgen header comparison anywhere else, which is why neither of those is
/// optional.
#[test]
fn relational_invariants_all_targets() {
    // ---- the `zconf.h` aliases really are the C types they claim to be -------
    // `zconf.h` L405-L406. Mapping `uLong` to a fixed-width integer would corrupt
    // `total_in`, `total_out`, `adler` and every checksum return value on any
    // target where `unsigned long` is not that width.
    assert_eq!(size_of::<uLong>(), size_of::<c_ulong>());
    assert_eq!(align_of::<uLong>(), align_of::<c_ulong>());
    assert_eq!(size_of::<uInt>(), size_of::<c_uint>());
    assert_eq!(align_of::<uInt>(), align_of::<c_uint>());

    // `zconf.h` L419-L427: every pointer alias is a plain data pointer.
    assert_eq!(size_of::<voidpf>(), size_of::<*mut c_void>());
    assert_eq!(align_of::<voidpf>(), align_of::<*mut c_void>());
    assert_eq!(size_of::<voidpc>(), size_of::<*const c_void>());
    assert_eq!(size_of::<voidp>(), size_of::<*mut c_void>());

    // `zconf.h` L403, L412 and L414: the byte and character types are single bytes on
    // every target, and `Bytef` is `Byte` with the (empty) `FAR` qualifier.
    assert_eq!(size_of::<Bytef>(), 1);
    assert_eq!(align_of::<Bytef>(), 1);
    assert_eq!(size_of::<Byte>(), size_of::<Bytef>());
    assert_eq!(size_of::<charf>(), 1);

    // `zconf.h` L440-L444: `z_crc_t` is chosen so that it is exactly 32 bits, which
    // is why this one is absolute rather than relational. `get_crc_table` hands a
    // `*const z_crc_t` to callers that index it, so the stride is contractual.
    assert_eq!(size_of::<z_crc_t>(), 4);
    assert_eq!(align_of::<z_crc_t>(), 4);

    // `zconf.h` L414-L417: the `FAR`-qualified aliases add only `FAR`, which is empty
    // on every supported target.
    assert_eq!(size_of::<uIntf>(), size_of::<uInt>());
    assert_eq!(size_of::<uLongf>(), size_of::<uLong>());
    assert_eq!(size_of::<intf>(), size_of::<c_int>());

    // `zconf.h` L522-L532: `z_off64_t` is 64 bits by definition — that is the
    // entire reason the `*64` symbol family exists — while `z_off_t` tracks the
    // platform's `off_t` and may be narrower.
    assert_eq!(size_of::<z_off64_t>(), 8);
    assert!(size_of::<z_off_t>() <= size_of::<z_off64_t>());

    // `zconf.h` L253-L270: `z_size_t` is the target's `size_t`.
    assert_eq!(size_of::<z_size_t>(), size_of::<usize>());
    assert_eq!(align_of::<z_size_t>(), align_of::<usize>());

    // ---- `z_stream`: first field, ordering, and the tail ---------------------
    // `zlib.h` L90-L110. `next_in` is first, always: a caller that walks the
    // struct's head starts here.
    assert_eq!(offset_of!(z_stream, next_in), 0);

    // `avail_in` follows `next_in` immediately, because a pointer's alignment
    // cannot require padding after a pointer.
    assert_eq!(offset_of!(z_stream, avail_in), size_of::<*const Bytef>());

    // Declaration order, asserted as a strictly increasing chain. Any reorder,
    // insertion, removal or widening moves at least one of these and fails here,
    // on every target.
    assert!(offset_of!(z_stream, next_in) < offset_of!(z_stream, avail_in));
    assert!(offset_of!(z_stream, avail_in) < offset_of!(z_stream, total_in));
    assert!(offset_of!(z_stream, total_in) < offset_of!(z_stream, next_out));
    assert!(offset_of!(z_stream, next_out) < offset_of!(z_stream, avail_out));
    assert!(offset_of!(z_stream, avail_out) < offset_of!(z_stream, total_out));
    assert!(offset_of!(z_stream, total_out) < offset_of!(z_stream, msg));
    assert!(offset_of!(z_stream, msg) < offset_of!(z_stream, state));
    assert!(offset_of!(z_stream, state) < offset_of!(z_stream, zalloc));
    assert!(offset_of!(z_stream, zalloc) < offset_of!(z_stream, zfree));
    assert!(offset_of!(z_stream, zfree) < offset_of!(z_stream, opaque));
    assert!(offset_of!(z_stream, opaque) < offset_of!(z_stream, data_type));
    assert!(offset_of!(z_stream, data_type) < offset_of!(z_stream, adler));
    assert!(offset_of!(z_stream, adler) < offset_of!(z_stream, reserved));

    // The struct is large enough to contain its last field, and aligned for its
    // widest member. `deflateInit2_`, `inflateInit2_` and `inflateBackInit_` all
    // compare a caller's `stream_size` against this size and return
    // `Z_VERSION_ERROR` on a mismatch, so it is a protocol value, not just layout.
    assert!(size_of::<z_stream>() >= offset_of!(z_stream, reserved) + size_of::<uLong>());
    assert!(align_of::<z_stream>() >= align_of::<*const Bytef>());
    assert!(align_of::<z_stream>() >= align_of::<uLong>());

    // `zlib.h` L112: `z_streamp` is a plain pointer to it.
    assert_eq!(size_of::<z_streamp>(), size_of::<*mut c_void>());

    // `zlib.h` L88: `struct internal_state` is declared and never defined, so the
    // Rust mirror is a zero-sized opaque tag and `*mut internal_state` is an
    // ordinary data pointer. The state behind it is validated on entry rather than
    // trusted, exactly as `deflateStateCheck` and `inflateStateCheck` do in C.
    assert_eq!(size_of::<internal_state>(), 0);
    assert_eq!(size_of::<*mut internal_state>(), size_of::<*mut c_void>());

    // ---- `gz_header`: first field, ordering, and the tail -------------------
    // `zlib.h` L118-L133.
    assert_eq!(offset_of!(gz_header, text), 0);

    assert!(offset_of!(gz_header, text) < offset_of!(gz_header, time));
    assert!(offset_of!(gz_header, time) < offset_of!(gz_header, xflags));
    assert!(offset_of!(gz_header, xflags) < offset_of!(gz_header, os));
    assert!(offset_of!(gz_header, os) < offset_of!(gz_header, extra));
    assert!(offset_of!(gz_header, extra) < offset_of!(gz_header, extra_len));
    assert!(offset_of!(gz_header, extra_len) < offset_of!(gz_header, extra_max));
    assert!(offset_of!(gz_header, extra_max) < offset_of!(gz_header, name));
    assert!(offset_of!(gz_header, name) < offset_of!(gz_header, name_max));
    assert!(offset_of!(gz_header, name_max) < offset_of!(gz_header, comment));
    assert!(offset_of!(gz_header, comment) < offset_of!(gz_header, comm_max));
    assert!(offset_of!(gz_header, comm_max) < offset_of!(gz_header, hcrc));
    assert!(offset_of!(gz_header, hcrc) < offset_of!(gz_header, done));

    assert!(size_of::<gz_header>() >= offset_of!(gz_header, done) + size_of::<c_int>());
    assert!(align_of::<gz_header>() >= align_of::<*mut Bytef>());
    assert!(align_of::<gz_header>() >= align_of::<uLong>());
    assert_eq!(size_of::<gz_headerp>(), size_of::<*mut c_void>());

    // ---- the `gzgetc` prefix must start at offset 0 on every target ----------
    // See §4 for why this one matters more than the rest.
    assert_eq!(offset_of!(gzFile_s, have), 0);
    assert!(offset_of!(gzFile_s, have) < offset_of!(gzFile_s, next));
    assert!(offset_of!(gzFile_s, next) < offset_of!(gzFile_s, pos));
    assert!(size_of::<gzFile_s>() >= offset_of!(gzFile_s, pos) + size_of::<z_off64_t>());
    assert_eq!(size_of::<gzFile>(), size_of::<*mut c_void>());
}

// =============================================================================
//  §2  `z_stream` — `zlib.h` L90-L110.  Measured: 112 bytes, align 8
// =============================================================================
//
//  The struct the entire streaming API is expressed in terms of.  The *caller*
//  allocates it, so every field offset is baked into caller object code, and
//  `deflate.c` L395 / `inflate.c` L179 / `infback.c` L31 additionally compare the
//  caller's `sizeof(z_stream)` against the library's and return
//  `Z_VERSION_ERROR` on a mismatch.
//
//  Declaration order, with the C type of each field:
//
//       0  next_in    z_const Bytef *              pointer
//       1  avail_in   uInt                         unsigned int
//       2  total_in   uLong                        unsigned long
//       3  next_out   Bytef *                      pointer
//       4  avail_out  uInt                         unsigned int
//       5  total_out  uLong                        unsigned long
//       6  msg        z_const char *               pointer
//       7  state      struct internal_state FAR *  pointer
//       8  zalloc     alloc_func                   function pointer
//       9  zfree      free_func                    function pointer
//      10  opaque     voidpf                       pointer
//      11  data_type  int                          int
//      12  adler      uLong                        unsigned long
//      13  reserved   uLong                        unsigned long

/// `z_stream` under LP64: 112 bytes, align 8, and all fourteen measured offsets.
///
/// Probe output, `gcc -I. -D_LARGEFILE64_SOURCE=1` on `x86_64-unknown-linux-gnu`:
/// `size=112 align=8`, offsets 0, 8, 16, 24, 32, 40, 48, 56, 64, 72, 80, 88, 96,
/// 104. Identical to `crates/libz-rs-sys/src/layout_assertions.rs` §1 tier 1.
#[test]
fn z_stream_layout_lp64() {
    if !is_lp64() {
        skipped("z_stream_layout_lp64");
        return;
    }

    assert_eq!(size_of::<z_stream>(), 112, "sizeof(z_stream) under LP64");
    assert_eq!(align_of::<z_stream>(), 8, "_Alignof(z_stream) under LP64");

    assert_eq!(offset_of!(z_stream, next_in), 0);
    assert_eq!(offset_of!(z_stream, avail_in), 8);
    assert_eq!(offset_of!(z_stream, total_in), 16);
    assert_eq!(offset_of!(z_stream, next_out), 24);
    assert_eq!(offset_of!(z_stream, avail_out), 32);
    assert_eq!(offset_of!(z_stream, total_out), 40);
    assert_eq!(offset_of!(z_stream, msg), 48);
    assert_eq!(offset_of!(z_stream, state), 56);
    assert_eq!(offset_of!(z_stream, zalloc), 64);
    assert_eq!(offset_of!(z_stream, zfree), 72);
    assert_eq!(offset_of!(z_stream, opaque), 80);
    assert_eq!(offset_of!(z_stream, data_type), 88);
    assert_eq!(offset_of!(z_stream, adler), 96);
    assert_eq!(offset_of!(z_stream, reserved), 104);
}

/// `z_stream` under LLP64 (`x86_64` Windows): 88 bytes, align 8.
///
/// Derived from the `#[repr(C)]` placement rule with `unsigned long` at 4 bytes and
/// pointers at 8, giving offsets 0, 8, 12, 16, 24, 28, 32, 40, 48, 56, 64, 72, 76,
/// 80 — the same numbers `layout_assertions.rs` §1 tier 2 pins. Gated, so it skips
/// everywhere else; if a real Windows build ever disagrees, this test says so
/// instead of staying silent.
#[test]
fn z_stream_layout_llp64() {
    if !is_llp64() {
        skipped("z_stream_layout_llp64");
        return;
    }

    assert_eq!(size_of::<z_stream>(), 88, "sizeof(z_stream) under LLP64");
    assert_eq!(align_of::<z_stream>(), 8, "_Alignof(z_stream) under LLP64");

    assert_eq!(offset_of!(z_stream, next_in), 0);
    assert_eq!(offset_of!(z_stream, avail_in), 8);
    assert_eq!(offset_of!(z_stream, total_in), 12);
    assert_eq!(offset_of!(z_stream, next_out), 16);
    assert_eq!(offset_of!(z_stream, avail_out), 24);
    assert_eq!(offset_of!(z_stream, total_out), 28);
    assert_eq!(offset_of!(z_stream, msg), 32);
    assert_eq!(offset_of!(z_stream, state), 40);
    assert_eq!(offset_of!(z_stream, zalloc), 48);
    assert_eq!(offset_of!(z_stream, zfree), 56);
    assert_eq!(offset_of!(z_stream, opaque), 64);
    assert_eq!(offset_of!(z_stream, data_type), 72);
    assert_eq!(offset_of!(z_stream, adler), 76);
    assert_eq!(offset_of!(z_stream, reserved), 80);
}

// =============================================================================
//  §3  `gz_header` — `zlib.h` L118-L133.  Measured: 80 bytes, align 8
// =============================================================================
//
//  The caller-owned gzip header block.  `deflateSetHeader` reads it and
//  `inflateGetHeader` *writes* through it, clamping to `extra_max`, `name_max` and
//  `comm_max` — historically the source of gzip-header overflow defects, which is
//  why the three capacity fields must sit exactly where a caller put them.

/// `gz_header` under LP64: 80 bytes, align 8, and all thirteen measured offsets.
///
/// ★ `time` is at **8, not 4**. `uLong time` needs 8-byte alignment after
/// `int text`, so four bytes of padding sit between them. It is the single offset
/// most likely to be derived wrongly by hand — hence an explicit assertion with an
/// explicit message rather than reliance on the size check catching it.
///
/// The same reasoning produces the second, less obvious pair: `extra_len` at 32 and
/// `extra_max` at 36 are two `uInt`s packed together, and `name` at 40 then needs no
/// padding, whereas `name_max` at 48 is followed by four bytes of padding before
/// `comment` at 56.
#[test]
fn gz_header_layout_lp64() {
    if !is_lp64() {
        skipped("gz_header_layout_lp64");
        return;
    }

    assert_eq!(size_of::<gz_header>(), 80, "sizeof(gz_header) under LP64");
    assert_eq!(align_of::<gz_header>(), 8, "_Alignof(gz_header) under LP64");

    assert_eq!(offset_of!(gz_header, text), 0);
    assert_eq!(
        offset_of!(gz_header, time),
        8,
        "uLong time is 8-byte aligned, so four bytes of padding follow int text"
    );
    assert_eq!(offset_of!(gz_header, xflags), 16);
    assert_eq!(offset_of!(gz_header, os), 20);
    assert_eq!(offset_of!(gz_header, extra), 24);
    assert_eq!(offset_of!(gz_header, extra_len), 32);
    assert_eq!(offset_of!(gz_header, extra_max), 36);
    assert_eq!(offset_of!(gz_header, name), 40);
    assert_eq!(offset_of!(gz_header, name_max), 48);
    assert_eq!(offset_of!(gz_header, comment), 56);
    assert_eq!(offset_of!(gz_header, comm_max), 64);
    assert_eq!(offset_of!(gz_header, hcrc), 68);
    assert_eq!(offset_of!(gz_header, done), 72);
}

/// `gz_header` under LLP64: 72 bytes, align 8.
///
/// With `unsigned long` at 4 bytes the padding after `int text` disappears
/// entirely, which is exactly why the LP64 numbers may not be asserted
/// unconditionally: offsets 0, 4, 8, 12, 16, 24, 28, 32, 40, 48, 56, 60, 64.
/// Matches `layout_assertions.rs` §2 tier 2.
#[test]
fn gz_header_layout_llp64() {
    if !is_llp64() {
        skipped("gz_header_layout_llp64");
        return;
    }

    assert_eq!(size_of::<gz_header>(), 72, "sizeof(gz_header) under LLP64");
    assert_eq!(
        align_of::<gz_header>(),
        8,
        "_Alignof(gz_header) under LLP64"
    );

    assert_eq!(offset_of!(gz_header, text), 0);
    assert_eq!(
        offset_of!(gz_header, time),
        4,
        "a 4-byte unsigned long needs no padding after int text"
    );
    assert_eq!(offset_of!(gz_header, xflags), 8);
    assert_eq!(offset_of!(gz_header, os), 12);
    assert_eq!(offset_of!(gz_header, extra), 16);
    assert_eq!(offset_of!(gz_header, extra_len), 24);
    assert_eq!(offset_of!(gz_header, extra_max), 28);
    assert_eq!(offset_of!(gz_header, name), 32);
    assert_eq!(offset_of!(gz_header, name_max), 40);
    assert_eq!(offset_of!(gz_header, comment), 48);
    assert_eq!(offset_of!(gz_header, comm_max), 56);
    assert_eq!(offset_of!(gz_header, hcrc), 60);
    assert_eq!(offset_of!(gz_header, done), 64);
}

// =============================================================================
//  §4  `struct gzFile_s` — `zlib.h` L1956-L1960.  Measured: 24 bytes, align 8
// =============================================================================
//
//  ★ THE HIGHEST-RISK ABI CONSTRAINT IN THE ENTIRE PORT.
//
//  `zlib.h` L1966-L1968 does not declare `gzgetc` as a function alone.  It also
//  defines it as a MACRO:
//
//      #define gzgetc(g) \
//            ((g)->have ? ((g)->have--, (g)->pos++, *((g)->next)++) : (gzgetc)(g))
//
//  Three consequences, and the third is the one that makes this section
//  load-bearing rather than tidy:
//
//    1. `(g)->have`, `(g)->pos` and `(g)->next` are read, decremented, incremented
//       and dereferenced by *the caller's own object code*.
//    2. That code was compiled against `zlib.h` and cannot be recompiled by this
//       port.  Whatever offsets it baked in are fixed for ever.
//    3. `gzguts.h` L170-L172 embeds `struct gzFile_s x` as the FIRST member of
//       `gz_state`, and the probe confirmed `offsetof(gz_state, x) == 0`.  So the
//       port's gz state must begin with exactly this three-field prefix at offset
//       0.  Any other layout is not a wrong result — it is a write to the wrong
//       address, silently corrupting whatever the port keeps there, in every
//       caller that uses `gzgetc`.
//
//  `zlib.h` L1948-L1955 says as much in the source: the exposed structure exists
//  "just enough for the gzgetc() macro", the user "should not mess with these
//  exposed elements", and "You have been warned."
//
//  The absolute numbers here are gated on a 64-bit pointer rather than on LP64
//  alone, because this struct contains no `unsigned long`: `unsigned`, a pointer
//  and `z_off64_t` are 4, 8 and 8 bytes under both LP64 and LLP64, so 24/8 and
//  0/8/16 hold on both.

/// The caller-visible `gzFile_s` prefix: 24 bytes, align 8, `have` 0, `next` 8,
/// `pos` 16.
///
/// See the section banner above for why these three offsets are the ones a
/// regression would hurt most. The gate accepts either 64-bit model because the
/// struct's layout is the same under both; on a 32-bit target the test skips and
/// [`relational_invariants_all_targets`] still holds `have` at 0, which is the part
/// that is true everywhere.
#[test]
fn gz_file_s_prefix() {
    if !is_lp64() && !is_llp64() {
        skipped("gz_file_s_prefix");
        return;
    }

    assert_eq!(
        size_of::<gzFile_s>(),
        24,
        "sizeof(struct gzFile_s): the gzgetc macro's view of a gzFile"
    );
    assert_eq!(align_of::<gzFile_s>(), 8, "_Alignof(struct gzFile_s)");

    assert_eq!(
        offset_of!(gzFile_s, have),
        0,
        "the gzgetc macro reads and decrements (g)->have here"
    );
    assert_eq!(
        offset_of!(gzFile_s, next),
        8,
        "the gzgetc macro dereferences and increments *((g)->next)++ here"
    );
    assert_eq!(
        offset_of!(gzFile_s, pos),
        16,
        "the gzgetc macro increments (g)->pos here"
    );

    // `zlib.h` L1354: `gzFile` is a pointer to it, and it is the handle every one
    // of the thirty `gz*` entry points takes or returns.
    assert_eq!(size_of::<gzFile>(), size_of::<*mut c_void>());
}

/// The facade's `gzFile_s` and the core's `GzFileExposed` are the same layout.
///
/// This is the *other* end of the agreement §4 pins. `crates/zlib-rs/src/gz/state.rs`
/// declares `GzFileExposed` `#[repr(C)]` and makes it `GzState`'s first member, so it
/// is the structure a caller's `gzgetc` macro actually reaches into at run time.
/// `crates/libz-rs-sys/src/types.rs` L793-L808 already asserts the agreement at
/// compile time; confirming it here means the check also runs under the nightly
/// AddressSanitizer job, where the pointer arithmetic in question is live.
///
/// Gated on `gz` exactly as the import is: `zlib_rs::gz` needs `zlib-rs/std`, which
/// this crate's `gz` feature turns on, and `--no-default-features` must still
/// compile.
#[cfg(feature = "gz")]
#[test]
fn gz_file_s_matches_core_gz_state_prefix() {
    assert_eq!(size_of::<gzFile_s>(), size_of::<GzFileExposed>());
    assert_eq!(align_of::<gzFile_s>(), align_of::<GzFileExposed>());
    assert_eq!(offset_of!(gzFile_s, have), offset_of!(GzFileExposed, have));
    assert_eq!(offset_of!(gzFile_s, next), offset_of!(GzFileExposed, next));
    assert_eq!(offset_of!(gzFile_s, pos), offset_of!(GzFileExposed, pos));
}

// =============================================================================
//  §5  `code` and the `inftrees.h` capacities — `inftrees.h` L24-L28, L49-L51
// =============================================================================

/// The `code` entry: 4 bytes, **align 2**, fields at 0, 1 and 2.
///
/// `inftrees.h` L24-L28 declares `{ unsigned char op; unsigned char bits; unsigned
/// short val; }`, and the header's own comment states "Each entry is four bytes."
///
/// ★ **`align_of` is asserted explicitly, and that is the point of this test.** A
/// naive mirror built from a single `u32` field would also report size 4 and would
/// pass a size-only check, while placing nothing at offsets 1 and 2 and reporting
/// alignment 4. Only the size/alignment/offset triple distinguishes the real layout
/// from that impostor, so all three are checked.
///
/// These numbers are target-independent: `u8`, `u8` and `u16` have fixed widths
/// everywhere, so no gate applies.
#[test]
fn code_struct_layout() {
    assert_eq!(size_of::<code>(), 4, "inftrees.h: each entry is four bytes");
    assert_eq!(
        align_of::<code>(),
        2,
        "_Alignof(code) is 2 (unsigned short), NOT 4 — a u32-based mirror would fail here"
    );

    assert_eq!(offset_of!(code, op), 0);
    assert_eq!(offset_of!(code, bits), 1);
    assert_eq!(offset_of!(code, val), 2);

    // No padding anywhere: the three fields exactly fill the four bytes.
    assert_eq!(size_of::<code>(), 2 * size_of::<u8>() + size_of::<u16>());

    // ---- the core's counterpart: size and alignment ONLY --------------------
    //
    // `zlib_rs::inflate::inftrees::Code` is what `inflate_table` actually fills, and
    // the two types agree on width and alignment. `crates/libz-rs-sys/src/types.rs`
    // L773-L774 pins exactly that pair at compile time, and it is what makes the
    // `4 * ENOUGH` arena budget below come out the same on both sides.
    assert_eq!(size_of::<code>(), size_of::<Code>());
    assert_eq!(align_of::<code>(), align_of::<Code>());

    // ★ FIELD OFFSETS ARE DELIBERATELY *NOT* PART OF THAT AGREEMENT, and asserting
    // them here would be a bug in this file rather than a check.
    //
    // `Code` carries no `#[repr(C)]`, so Rust is free to order its fields as it
    // likes, and it does. Measured on this toolchain: `val` at 0, `op` at 2 and
    // `bits` at 3 — size 4 and align 2, exactly matching `code`, with all three
    // offsets different.
    //
    // The port relies on that being *unpromised* rather than working around it. The
    // exported `inflate_table` (`crates/libz-rs-sys/src/inflate.rs` L2821-L2825 and
    // L3008-L3023) never reinterprets a caller's `code *` as a `*mut Code`: it builds
    // into a local `[Code]` arena and copies each entry out through
    // `impl From<Code> for code` (`types.rs` L883), which moves the three fields by
    // name. So the only thing C ever reads is the `#[repr(C)]` `code` above, whose
    // 0/1/2 offsets are asserted, and the core keeps the freedom to lay `Code` out
    // however it wants.
    //
    // Re-adding `assert_eq!(offset_of!(code, op), offset_of!(Code, op))` would pin a
    // layout the core explicitly declines to guarantee, and it would fail today.
}

/// The dynamic-table capacities from `inftrees.h` L49-L51: 852, 592 and 1444.
///
/// The values were found by exhaustive search — `enough 286 9 15` returns 852 for
/// literal/length codes and `enough 30 6 15` returns 592 for distance codes — and
/// they are not free-floating trivia: `struct inflate_state` budgets `4 * ENOUGH`
/// bytes for its inline arena, so `test/infcover.c`'s `mem_limit` accounting depends
/// on the port agreeing.
///
/// ★ **Asserted through the core's public `usize` forms.** The facade's own
/// `c_uint` twins are `pub(crate)` (`crates/libz-rs-sys/src/types.rs` L943-L962), so
/// an integration test cannot name them and this file must not add a visibility
/// change to reach them; `layout_assertions.rs` §4 already pins those. The core's
/// `zlib_rs::inflate::inftrees` constants hold the same three numbers and *are*
/// public, so checking them is a real assertion rather than a comment.
#[test]
fn inftrees_capacities() {
    assert_eq!(ENOUGH_LENS, C_ENOUGH_LENS);
    assert_eq!(ENOUGH_DISTS, C_ENOUGH_DISTS);
    assert_eq!(ENOUGH, C_ENOUGH);
    assert_eq!(ENOUGH, ENOUGH_LENS + ENOUGH_DISTS);

    // The arena budget the two numbers above imply, in bytes.
    assert_eq!(ENOUGH * size_of::<code>(), 5776);
}

// =============================================================================
//  §6  The `zconf.h` primitive widths — `zconf.h` L403-L444, L494-L532
// =============================================================================
//
//  These are the *inputs* to every layout above: change one and the struct offsets
//  in §2 and §3 move.  Four of them are also observable through the public API,
//  because `zlibCompileFlags` (`zutil.c` L32-L57) packs `sizeof(uInt)`,
//  `sizeof(uLong)`, `sizeof(voidpf)` and `sizeof(z_off_t)` into bits 0-7 under the
//  encoding 2 -> 0, 4 -> 1, 8 -> 2, anything else -> 3.  The reference build
//  returns 0xa9, whose low byte 0b1010_1001 decodes to (1, 2, 2, 2) — that is
//  (4, 8, 8, 8) bytes, exactly what this section asserts.  The *value* of
//  `zlibCompileFlags()` is deliberately checked in `c_api_parity.rs` instead, which
//  derives it from `size_of` rather than hardcoding 0xa9.

/// Every `zconf.h` primitive width, at its measured LP64 value.
///
/// Probe output on `x86_64-unknown-linux-gnu`: `uInt` 4, `uLong` 8, `uLongf` 8,
/// `uIntf` 4, `voidpf` 8, `voidpc` 8, `voidp` 8, `z_off_t` 8, `z_off64_t` 8,
/// `z_size_t` 8, `z_crc_t` 4, `Bytef` 1, `Byte` 1, `charf` 1, `intf` 4, and both
/// hook types 8.
///
/// LP64-gated because `uLong` is `unsigned long`, which is 4 bytes on LLP64
/// Windows; the target-independent halves of the same facts live in
/// [`relational_invariants_all_targets`], which is where a non-LP64 target still
/// gets coverage.
#[test]
fn primitive_type_sizes_lp64() {
    if !is_lp64() {
        skipped("primitive_type_sizes_lp64");
        return;
    }

    // `zconf.h` L405-L406 — the two integer aliases.
    assert_eq!(size_of::<uInt>(), 4);
    assert_eq!(size_of::<uLong>(), 8);

    // `zconf.h` L416-L417 — the `FAR`-qualified spellings of the same two.
    assert_eq!(size_of::<uIntf>(), 4);
    assert_eq!(size_of::<uLongf>(), 8);

    // `zconf.h` L419-L427 — the three pointer aliases.
    assert_eq!(size_of::<voidpf>(), 8);
    assert_eq!(size_of::<voidpc>(), 8);
    assert_eq!(size_of::<voidp>(), 8);

    // `zconf.h` L494-L532 — the two offset types. `z_off_t` follows the platform's
    // `off_t`; `z_off64_t` is always 64 bits.
    assert_eq!(size_of::<z_off_t>(), 8);
    assert_eq!(size_of::<z_off64_t>(), 8);

    // `zconf.h` L253-L270 — `size_t`, the type of the `_z` entry points.
    assert_eq!(size_of::<z_size_t>(), 8);

    // `zconf.h` L440-L444 — the CRC word `get_crc_table` publishes.
    assert_eq!(size_of::<z_crc_t>(), 4);

    // `zconf.h` L403, L412 and L414 — the byte and character types.
    assert_eq!(size_of::<Bytef>(), 1);
    assert_eq!(size_of::<Byte>(), 1);
    assert_eq!(size_of::<charf>(), 1);

    // `zconf.h` L415 — `int` with `FAR`, and plain `c_int` beside it, since every
    // `Z_*` constant and every status return is an `int`.
    assert_eq!(size_of::<intf>(), 4);
    assert_eq!(size_of::<c_int>(), 4);

    // `zlib.h` L85-L86 — the two caller-supplied allocator hooks, as raw widths.
    // §7 checks the property that actually matters about them.
    assert_eq!(size_of::<alloc_func>(), 8);
    assert_eq!(size_of::<free_func>(), 8);
}

// =============================================================================
//  §7  The nullable-function-pointer niche — `zlib.h` L85-L86, L1129-L1136
// =============================================================================

/// `Option<unsafe extern "C" fn(…)>` is exactly one pointer wide, with no
/// discriminant.
///
/// The four hook types are declared as `Option<…>` on purpose:
///
/// * `alloc_func` and `free_func` are `zalloc`/`zfree` (`zlib.h` L85-L86), which a
///   caller may leave as `Z_NULL` to ask for the library's default allocator.
/// * `in_func` and `out_func` are `inflateBack`'s two callbacks
///   (`zlib.h` L1129-L1136).
///
/// C spells "absent" as a null pointer; Rust spells it [`None`]. The two agree only
/// because of the null-pointer niche optimisation, which lets `Option` reuse the
/// pointer's own null representation instead of adding a discriminant word. If it
/// ever failed to apply, `Option<fn>` would be 16 bytes, every field after `zalloc`
/// in `z_stream` would shift, and `Z_NULL` would stop meaning `None` — so this is
/// the assertion that licenses modelling the hooks as `Option` at all.
///
/// Unconditional: the niche is a property of the Rust type system, not of the
/// target's integer model, so it must hold everywhere.
#[test]
fn nullable_fn_ptr_niche() {
    assert_eq!(
        size_of::<alloc_func>(),
        size_of::<*mut c_void>(),
        "Option<alloc_func> must be pointer-sized: no discriminant word"
    );
    assert_eq!(align_of::<alloc_func>(), align_of::<*mut c_void>());

    assert_eq!(
        size_of::<free_func>(),
        size_of::<*mut c_void>(),
        "Option<free_func> must be pointer-sized: no discriminant word"
    );
    assert_eq!(align_of::<free_func>(), align_of::<*mut c_void>());

    assert_eq!(
        size_of::<in_func>(),
        size_of::<*mut c_void>(),
        "Option<in_func> must be pointer-sized: no discriminant word"
    );
    assert_eq!(align_of::<in_func>(), align_of::<*mut c_void>());

    assert_eq!(
        size_of::<out_func>(),
        size_of::<*mut c_void>(),
        "Option<out_func> must be pointer-sized: no discriminant word"
    );
    assert_eq!(align_of::<out_func>(), align_of::<*mut c_void>());

    // Naming the bare function types as well proves the `Option` wrapper is what is
    // free, rather than the two happening to coincide for some other reason.
    assert_eq!(
        size_of::<alloc_func>(),
        size_of::<unsafe extern "C" fn(voidpf, uInt, uInt) -> voidpf>()
    );
    assert_eq!(
        size_of::<free_func>(),
        size_of::<unsafe extern "C" fn(voidpf, voidpf)>()
    );

    // And the hooks sit where §2 says they sit, which is the other half of why the
    // niche matters: a 16-byte `Option` would move `zfree` and everything after it.
    assert!(offset_of!(z_stream, zalloc) < offset_of!(z_stream, zfree));
    assert_eq!(
        offset_of!(z_stream, zfree) - offset_of!(z_stream, zalloc),
        size_of::<alloc_func>()
    );
}

// =============================================================================
//  §8  Version identity and the `zconf.h` bounds — `zlib.h` L44-L45,
//      `zconf.h` L273-L287
// =============================================================================

/// `zlibVersion()` returns `"1.3.2.1-motley"` and `ZLIB_VERNUM` is `0x1321`.
///
/// This is a *protocol* check, not cosmetics. `deflateInit_`, `inflateInit_` and
/// `inflateBackInit_` each compare the caller's compile-time `ZLIB_VERSION` against
/// the library's and return `Z_VERSION_ERROR` when the major components disagree,
/// and `test/example.c` L502-L505 performs the same `strcmp(zlibVersion(),
/// ZLIB_VERSION)` check at start-up before running anything else. Substituting the
/// shorthand `"1.3.2"` would fail every caller in existence.
///
/// The expected text is written out as [`C_ZLIB_VERSION`], quoted from `zlib.h`
/// L44, so the assertion holds the crate to the header rather than to itself. It
/// must never be derived from `CARGO_PKG_VERSION`: Cargo semver has three
/// components and this version has four, which is exactly why
/// `crates/libz-rs-sys/src/util.rs` keeps it as a literal.
///
/// Gated on `libz-compat`, because that feature is what compiles the introspection
/// module the function and the constants come from.
#[cfg(feature = "libz-compat")]
#[test]
fn version_string_and_vernum() {
    let reported = zlibVersion();
    assert!(
        !reported.is_null(),
        "zlibVersion() must never return NULL: callers strcmp it unconditionally"
    );

    // SAFETY: `zlibVersion` returns `ZLIB_VERSION.as_ptr()`
    // (`crates/libz-rs-sys/src/util.rs` L382-L391), which is the address of a
    // `&'static CStr` built from the C string literal `c"1.3.2.1-motley"`. It is
    // therefore non-null (asserted above as well), correctly aligned for `c_char`,
    // NUL-terminated, valid for reads for the whole program's lifetime, and never
    // written to by anything. `CStr::from_ptr`'s requirements are met exactly, and
    // the borrow it produces is confined to this function.
    let reported = unsafe { CStr::from_ptr(reported) };

    assert_eq!(
        reported, C_ZLIB_VERSION,
        "zlibVersion() must reproduce zlib.h L44 verbatim"
    );
    assert_eq!(
        ZLIB_VERSION, C_ZLIB_VERSION,
        "the crate constant must reproduce zlib.h L44 verbatim"
    );
    assert_eq!(reported, ZLIB_VERSION);

    // Fourteen bytes before the NUL. Pinned as a number so that a stray trailing
    // space or a doubled component cannot slip through a comparison that some
    // future edit turns into a prefix test.
    assert_eq!(reported.to_bytes().len(), 14);
    assert_eq!(reported.to_bytes_with_nul().len(), 15);

    // `zlib.h` L45. Callers write `#if ZLIB_VERNUM >= 0x1290`, so the numeric form
    // is part of the contract in its own right.
    assert_eq!(ZLIB_VERNUM, C_ZLIB_VERNUM);

    // The packing rule: one nibble per component, most significant first. This is
    // what makes a caller's `#if` comparison mean what it looks like it means, and
    // it ties the four numeric constants to the single packed one.
    assert_eq!((C_ZLIB_VERNUM >> 12) & 0xf, ZLIB_VER_MAJOR);
    assert_eq!((C_ZLIB_VERNUM >> 8) & 0xf, ZLIB_VER_MINOR);
    assert_eq!((C_ZLIB_VERNUM >> 4) & 0xf, ZLIB_VER_REVISION);
    assert_eq!(C_ZLIB_VERNUM & 0xf, ZLIB_VER_SUBREVISION);

    // `zlib.h` L46-L49, restated so a component drifting on its own is caught.
    assert_eq!(ZLIB_VER_MAJOR, 1);
    assert_eq!(ZLIB_VER_MINOR, 3);
    assert_eq!(ZLIB_VER_REVISION, 2);
    assert_eq!(ZLIB_VER_SUBREVISION, 1);

    // A `*const c_char` is what the signature promises, so the width the caller
    // receives is a pointer width.
    assert_eq!(size_of::<*const c_char>(), size_of::<*mut c_void>());
}

/// `MAX_WBITS` is 15 and `MAX_MEM_LEVEL` is 9 — `zconf.h` L287 and L273-L277.
///
/// These are the values `deflateInit2_` and `inflateInit2_` range-check their
/// arguments against, so a divergence would accept a configuration the core cannot
/// honour or reject one it can. `MAX_WBITS` is a 32 KiB LZ77 window; `zconf.h`
/// warns that reducing it makes `minigzip` unable to extract files produced by
/// `gzip`, which is why the installed header declares the full 15.
///
/// Both constants are declared unconditionally at the crate root, so this test needs
/// no feature gate.
#[test]
fn bounds_constants() {
    assert_eq!(MAX_WBITS, C_MAX_WBITS);
    assert_eq!(MAX_MEM_LEVEL, C_MAX_MEM_LEVEL);

    // The window size the bound denotes, spelled out: `1 << 15` is 32 KiB.
    assert_eq!(1_usize << C_MAX_WBITS, 32 * 1024);
}

// =============================================================================
//  §9  The internal inflate-state prefix — `inflate.h` L82-L86
// =============================================================================
//
//  ★ The SECOND place unmodified C code reaches into memory this port owns.
//
//  `test/infcover.c` is compiled without a single edit — that invariance is the
//  operational definition of drop-in compatibility — includes the private
//  `inflate.h`, and then writes the field directly, twice:
//
//      test/infcover.c L330   ((struct inflate_state *)strm.state)->mode = DICT;
//      test/infcover.c L459   state->mode = SYNC;
//
//  Both stores land at `((char *)state) + 8` on this target, computed by the C
//  compiler from `inflate.h`'s declaration order.  So offset 8 is a hard ABI
//  requirement of the acceptance suite, not an internal implementation detail the
//  port is free to choose.
//
//  `crates/libz-rs-sys/src/layout_assertions.rs` does not cover this, which is why
//  it is here: the facade's own state prefix is pinned inside the crate
//  (`types.rs` L2903-L2928 place the validity tag at offset 8 and the whole prefix
//  at 16 bytes), and this section pins the C side of the same agreement using only
//  public types.

/// `struct inflate_state`'s first four members lay out at 0, 8, 12 and 16.
///
/// Asserted through [`CInflateStatePrefix`], a `#[repr(C)]` mirror of
/// `inflate.h` L82-L86 built from the facade's own `z_streamp` and an `int`-sized
/// enum. That is deliberately *not* the same thing as reading the port's private
/// state: what it establishes is that the platform's `#[repr(C)]` rule, applied to
/// the header's declaration order, really does put `mode` at offset 8 — which is
/// the arithmetic `test/infcover.c` compiled into its own object code.
///
/// Reaching the port's actual state instead would need `unsafe` to dereference a
/// private item through `z_stream::state`, which AAP §0.7.1 (a) forbids and which
/// this file therefore does not do.
#[test]
fn inflate_state_mode_abi() {
    // Target-independent first: `mode` immediately follows the back-pointer,
    // because a 4-byte enum needs no padding after a pointer.
    assert_eq!(offset_of!(CInflateStatePrefix, strm), 0);
    assert_eq!(
        offset_of!(CInflateStatePrefix, mode),
        size_of::<z_streamp>(),
        "inflate_mode mode directly follows z_streamp strm"
    );
    assert!(offset_of!(CInflateStatePrefix, mode) < offset_of!(CInflateStatePrefix, last));
    assert!(offset_of!(CInflateStatePrefix, last) < offset_of!(CInflateStatePrefix, wrap));

    // The field is four bytes wide: the probe measured C's `inflate_mode` at
    // `size=4 align=4`, which is what an `int`-sized enum is. A wider or narrower
    // mirror would move `last` and `wrap`.
    assert_eq!(size_of::<c_int>(), 4);

    if !is_lp64() && !is_llp64() {
        skipped("inflate_state_mode_abi absolute offsets");
        return;
    }

    assert_eq!(
        offset_of!(CInflateStatePrefix, mode),
        8,
        "test/infcover.c L330 and L459 write ->mode at offset 8"
    );
    assert_eq!(offset_of!(CInflateStatePrefix, last), 12);
    assert_eq!(offset_of!(CInflateStatePrefix, wrap), 16);
}

// =============================================================================
//  §10  The non-zero-based `inflate_mode` ladder — `inflate.h` L20-L53
// =============================================================================
//
//  `inflate.h` L21 gives the first discriminant explicitly — `HEAD = 16180` — and
//  lets the remaining names follow implicitly, so the enum spans 16180 through
//  16211 across **32** named variants.  (AAP §0.1.2.1 says "31"; the header text is
//  authoritative and the C probe counted 32.  The undercount is recorded here so
//  the discrepancy is not rediscovered as a bug.)
//
//  The odd base is not decoration.  It is what makes a stale, foreign or
//  zero-filled `z_stream::state` detectable: `inflate.c` L94's `inflateStateCheck`
//  pairs an owner-identity test with a range test on the mode, and a range starting
//  at 16180 is very unlikely to be hit by accident.  `test/infcover.c` L330 stores
//  `DICT` and L459 stores `SYNC` into that field from unmodified C, so the *values*
//  travel across the boundary exactly as the offset in §9 does.

/// The port's `Mode` reproduces `inflate.h`'s ladder value for value and name for
/// name.
///
/// Held against [`C_MODE_NAMES`], transcribed from `inflate.h` L20-L53 in
/// declaration order, so that a reordering which preserved the endpoints still
/// fails: the endpoints alone cannot detect two swapped variants, and every
/// intermediate discriminant is a value C code can store.
///
/// `zlib_rs::inflate::Mode` is public, and `zlib-rs` is already this package's sole
/// first-party dependency, so asserting it directly costs no dependency and is
/// strictly better than recording the requirement in a comment. Nothing private is
/// touched, and no `unsafe` is involved.
#[test]
fn inflate_mode_discriminant_ladder() {
    // The count, from two independent directions.
    assert_eq!(Mode::COUNT, C_MODE_COUNT);
    assert_eq!(Mode::ALL.len(), C_MODE_COUNT);
    assert_eq!(C_MODE_NAMES.len(), C_MODE_COUNT);

    // The three named checkpoints. `HEAD` is the only discriminant `inflate.h`
    // writes down; `DICT` and `SYNC` are the two values unmodified C test code
    // stores into the port's own state.
    assert_eq!(Mode::Head.as_raw(), C_HEAD, "inflate.h L21: HEAD = 16180");
    assert_eq!(
        Mode::Dict.as_raw(),
        C_DICT,
        "test/infcover.c L330 stores DICT"
    );
    assert_eq!(
        Mode::Sync.as_raw(),
        C_SYNC,
        "test/infcover.c L459 stores SYNC"
    );

    // The endpoints, as the core reports them.
    assert_eq!(Mode::MIN_DISCRIMINANT, C_HEAD);
    assert_eq!(Mode::MAX_DISCRIMINANT, C_SYNC);
    // The span and the count have to agree, which is what makes the ladder
    // contiguous. Compared through `try_from` rather than a cast, so the check
    // carries no lossy conversion of its own.
    assert_eq!(usize::try_from(C_SYNC - C_HEAD + 1), Ok(C_MODE_COUNT));

    // The full ladder: contiguous, ascending, starting at HEAD, with each variant
    // carrying its C spelling. This is the assertion that makes the intermediate
    // 29 discriminants real rather than implied.
    let mut expected = C_HEAD;
    for (mode, c_name) in Mode::ALL.into_iter().zip(C_MODE_NAMES) {
        assert_eq!(
            mode.as_raw(),
            expected,
            "inflate_mode ladder: {c_name} must be {expected}"
        );
        assert_eq!(
            mode.c_name(),
            c_name,
            "inflate.h declaration order: variant at {expected} must be {c_name}"
        );
        assert_eq!(
            Mode::from_raw(expected),
            Some(mode),
            "from_raw must accept the tag C stores for {c_name}"
        );
        assert!(
            Mode::is_valid_tag(expected),
            "{c_name} is a live state and must pass the tag check"
        );
        expected += 1;
    }
    assert_eq!(expected, C_SYNC + 1, "the ladder must end exactly at SYNC");

    // ★ The property the odd base exists for: values outside the range are rejected,
    // which is what lets `inflateStateCheck` recognise a zeroed or overwritten
    // state. Zero in particular must never be a valid tag — freshly `calloc`ed
    // memory is all zeros.
    assert_eq!(Mode::from_raw(0), None);
    assert!(!Mode::is_valid_tag(0));
    assert_eq!(Mode::from_raw(C_HEAD - 1), None);
    assert_eq!(Mode::from_raw(C_SYNC + 1), None);
    assert!(!Mode::is_valid_tag(i32::MIN));
    assert!(!Mode::is_valid_tag(i32::MAX));
    assert_ne!(C_HEAD, 0);

    // The ladder has to fit the four-byte field §9 pins, and it does so with room to
    // spare: the largest discriminant, 16211, needs only 15 value bits, so it is
    // representable in a C `int` even on a conforming target where `int` is the
    // 16-bit minimum the language permits. Both bounds are derived from the target
    // rather than written down, so they track it.
    assert!(
        size_of::<c_int>() >= 2,
        "C guarantees int is at least 16 bits wide"
    );
    assert!(
        i64::from(C_SYNC) <= i64::from(c_int::MAX),
        "the whole inflate_mode ladder must be representable in a C int"
    );
}

// =============================================================================
//  §11  The gate itself
// =============================================================================

/// The integer-model gates are coherent, and on a 64-bit target one of them is
/// live.
///
/// Without this test the gated tiers could all skip and the suite would still
/// report `ok` — the one failure mode a contract test must not have. Three things
/// are established:
///
/// * The two models are mutually exclusive, so at most one absolute tier ever runs.
///   A future third gate cannot be written in a way that overlaps them without
///   failing here.
/// * On any 64-bit-pointer target, exactly one of them holds — so §2, §3, §4, §6 and
///   §9 really did assert their exact numbers rather than returning early.
/// * The relational tier's premise holds: whatever the model, `c_ulong` is one of
///   the two widths the absolute tiers were derived for, or the target falls through
///   to relational-only checking by design.
///
/// The model is printed either way, so a CI log records which tier ran.
#[test]
fn integer_model_is_recognised() {
    let pointer_width = size_of::<*const c_void>();
    let ulong_width = size_of::<c_ulong>();

    assert!(
        !(is_lp64() && is_llp64()),
        "LP64 and LLP64 cannot both hold: c_ulong cannot be 4 and 8 bytes at once"
    );

    if pointer_width == 8 {
        assert!(
            is_lp64() || is_llp64(),
            "a 64-bit target must match one of the two absolute tiers, but c_ulong \
             is {ulong_width} bytes — add a tier rather than leaving it unchecked"
        );
    }

    let model = if is_lp64() {
        "LP64 (absolute tier active)"
    } else if is_llp64() {
        "LLP64 (absolute tier active)"
    } else {
        "neither LP64 nor LLP64 (relational tier only, by design)"
    };
    eprintln!("abi_layout: pointer {pointer_width} bytes, c_ulong {ulong_width} bytes -> {model}");
}
