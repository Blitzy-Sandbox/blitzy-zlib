//! The seventeen exported `deflate*` entry points: the DEFLATE compressor's C surface.
//!
//! The C ABI half of `deflate.c`. Every algorithmic decision -- the per-level
//! `configuration_table`, the hash-chain walk, the `TOO_FAR` lazy rejection, the Huffman
//! tie-breaker, the block-type arithmetic -- lives in `crates/zlib-rs/src/deflate/**` and
//! `crates/zlib-rs/src/trees/**`. This module contributes exactly five things and nothing
//! else:
//!
//! 1. **Pointer validation**, reproducing the halves of `deflateStateCheck`
//!    (`deflate.c` L538-L556) that are pointer facts, before anything is dereferenced.
//! 2. **Allocator construction** from the caller's `(zalloc, zfree, opaque)` triple, so that
//!    every byte the compressor owns is obtained from, and returned to, the caller's own
//!    hooks.
//! 3. **Slice reconstruction**, performed once on entry, so that only safe slices travel
//!    inward.
//! 4. **The opaque `state` round-trip**: allocate the state object, record it in
//!    `z_stream.state`, and recover it -- tag-validated -- on every later call.
//! 5. **Width mapping and write-back**: `uInt` and `uLong` versus Rust lengths, and pushing
//!    the results back into the caller's `z_stream` in the order C writes them.
//!
//! # The exported set: seventeen symbols
//!
//! Membership was measured, by building the reference library and grouping
//! `nm -D --defined-only --extern-only`. This module owns seventeen of the 95 exported
//! functions:
//!
//! | Symbol | `zlib.h` | Derived from |
//! |---|---|---|
//! | [`deflateInit_`] | L1903 | `deflate.c` L379-L383 |
//! | [`deflateInit2_`] | L1907 | `deflate.c` L387-L533 |
//! | [`deflate`] | L254 | `deflate.c` L981-L1290 |
//! | [`deflateEnd`] | L367 | `deflate.c` L1293-L1310 |
//! | [`deflateSetDictionary`] | L618 | `deflate.c` L559-L622 |
//! | [`deflateGetDictionary`] | L662 | `deflate.c` L625-L641 |
//! | [`deflateCopy`] | L684 | `deflate.c` L1317-L1377 |
//! | [`deflateReset`] | L702 | `deflate.c` L704-L711 |
//! | [`deflateResetKeep`] | L2040 | `deflate.c` L644-L677 |
//! | [`deflateParams`] | L713 | `deflate.c` L774-L816 |
//! | [`deflateTune`] | L751 | `deflate.c` L819-L830 |
//! | [`deflateBound`] | L768 | `deflate.c` L929-L932 |
//! | [`deflateBound_z`] | L769 | `deflate.c` L856-L928 |
//! | [`deflatePending`] | L786 | `deflate.c` L722-L734 |
//! | [`deflateUsed`] | L804 | `deflate.c` L737-L742 |
//! | [`deflatePrime`] | L816 | `deflate.c` L745-L771 |
//! | [`deflateSetHeader`] | L833 | `deflate.c` L715-L720 |
//!
//! ## ★ `deflateInit` and `deflateInit2` are deliberately absent
//!
//! `zlib.h` L232 and L543 declare them, but both are **macros** over the `_`-suffixed real
//! functions, and neither appears in the reference library's dynamic symbol table. Exporting
//! either would add a symbol the 111-symbol parity diff does not expect and would fail it.
//! The same is true of `inflateInit`, `inflateInit2` and `inflateBackInit`, which belong to
//! the sibling modules.
//!
//! Every one of the seventeen is `#[no_mangle]` with the `"C"` ABI and **never**
//! `extern "C-unwind"`: a panic must abort at the boundary rather than unwind into a caller
//! that was not compiled to support it. See [`crate::panic_guard`].
//!
//! All seventeen are additionally declared `unsafe`, because all seventeen dereference a
//! caller's raw pointer. That is a Rust-side type marker with **no ABI effect whatsoever** --
//! the emitted symbol, the calling convention and the C prototype cbindgen generates are
//! identical either way -- and each therefore carries a `# Safety` section stating its
//! contract.
//!
//! Version decoration is **not** declared here and must not be. `deflateBound` belongs to
//! the `ZLIB_1.2.0` node, `deflatePrime` to `ZLIB_1.2.0.8`, `deflateSetHeader` and
//! `deflateTune` to `ZLIB_1.2.2` and `ZLIB_1.2.2.3`, `deflatePending` to `ZLIB_1.2.5.1`,
//! `deflateResetKeep` to `ZLIB_1.2.5.2`, `deflateGetDictionary` to `ZLIB_1.2.9`,
//! `deflateUsed` to `ZLIB_1.3.1.2` and `deflateBound_z` to `ZLIB_1.3.2`; the other eight are
//! undecorated base-set names. The decoration is applied by the `--version-script` link
//! argument `crates/libz-rs-sys/build.rs` passes, which is the single place that knows about
//! it.
//!
//! # Where the `unsafe` is, and why each block is unavoidable
//!
//! Five of AAP §0.6.1's six categories appear here, and no operation in this module falls
//! outside them:
//!
//! | Category | Site |
//! |---|---|
//! | **1** -- `z_streamp` validation | every entry point: non-null and aligned before any dereference, then each `z_stream` member read or written through a raw place |
//! | **2** -- slice reconstruction | `(next_in, avail_in)` and `(next_out, avail_out)`, and the dictionary buffers, all through [`crate::types::input_slice`] and [`crate::types::output_region`] |
//! | **3** -- the opaque `state` round-trip | [`crate::types::reserve_state`], [`crate::types::publish_state`], [`crate::types::commit_state`], [`crate::types::checked_state`], [`crate::types::checked_state_mut`] and [`crate::types::take_state_with`] |
//! | **4** -- invoking `zalloc`/`zfree` | [`crate::types::StreamAllocator`], built from the caller's triple and handed to the core as its [`zlib_rs::allocate::Allocator`] |
//! | **5** -- C string handling | the one-byte `version[0]` comparison in [`deflateInit2_`], and the NUL-terminated `name`/`comment` fields of a caller's `gz_header` |
//! | **6** -- a caller's `gz_header` | [`deflateSetHeader`], which stores borrows of the caller's `extra`, `name` and `comment` buffers |
//!
//! ★ Category **6** appears here in its *read* direction only. `deflateSetHeader` performs
//! no write through the caller's `gz_header` -- `inflateGetHeader` is the writing side -- but
//! it inherits the whole of the lifetime obligation, because `deflate.c` L717 stores the
//! pointer and the compressor reads through it much later, during `deflate()`. See
//! [`deflateSetHeader`] for the contract that places on the caller.
//!
//! ## ★ No `&z_stream` or `&mut z_stream` is ever formed
//!
//! Every access to a caller's `z_stream` in this module goes through
//! [`core::ptr::addr_of!`] or [`core::ptr::addr_of_mut!`]. `crates/libz-rs-sys/src/types.rs`
//! deliberately provides no constructor for a `&z_stream` or a `&mut z_stream`, and that is a
//! soundness requirement rather than a preference; the same file records the evidence:
//! creating a `&mut z_stream` from the caller's pointer *invalidates that pointer*, so the
//! later `z_stream.state` read inside `checked_state_mut` fails Miri's borrow-stack check
//! even though the addresses still compare equal. Reading and writing one member at a time
//! through a raw place has neither problem, and it is also the closer translation of the C it
//! replaces, where every access is spelled `strm->next_in`.
//!
//! It is also what makes a *partly initialised* `z_stream` safe to accept, which is not a
//! hypothetical: `test/example.c` declares `z_stream c_stream;` on the stack, sets only
//! `zalloc`, `zfree` and `opaque`, and calls `deflateInit`. Ten of the fourteen members are
//! indeterminate at that point, so reading the struct as a value -- `ptr::read::<z_stream>` --
//! would be undefined behaviour. Each entry point therefore reads exactly the members C
//! reads, and no more.
//!
//! # Byte identity: this module's contribution is entirely negative
//!
//! The eight decision points that determine the compressed bytes (AAP §0.6.2) all live in
//! the core. What this module must not do:
//!
//! * **not reorder, batch or coalesce calls into the core.** The `deflate()` driver is
//!   resumable and its per-call state transitions are observable -- a caller with a one-byte
//!   output buffer must make progress one byte at a time.
//! * **not clamp `avail_in` or `avail_out`** beyond what the core does.
//! * **not substitute a "better" status.** Every `Z_OK`, `Z_BUF_ERROR`, `Z_STREAM_ERROR`,
//!   `Z_DATA_ERROR`, `Z_MEM_ERROR` and `Z_VERSION_ERROR` must land exactly where C lands it,
//!   and `msg` must be set on exactly the paths C sets it on -- which, in the whole of
//!   `deflate.c`, is four `ERR_RETURN`s (L993, L995, L1020, L1025) plus L400, L511 and L652.
//! * **not get a width wrong.** `total_in`, `total_out` and `adler` are `uLong`, which is
//!   eight bytes on LP64 and four on LLP64 Windows, so they go through
//!   [`crate::types::uLong`] and never through a fixed-width integer.
//!
//! [`deflateBound`] and [`deflateBound_z`] deserve separate mention: `zlib.h` L770-L780
//! makes their result the number a caller allocates its output buffer with, so a bound that
//! is too small is a heap overflow *in the caller's code*. Both must return numerically
//! identical values to C, and both are deliberately tolerant of an invalid stream rather
//! than reporting an error, exactly as `deflate.c` L875-L879 is.
//!
//! # Provenance
//!
//! Ported from `deflate.c` L379-L1377; declared at `zlib.h` L254-L833, L1903-L1910 and
//! L2040.

// The exported names are C's, and `zlib.h` is immutable, so `deflateInit_`, `deflateBound_z`
// and the rest cannot be renamed to satisfy Rust's casing convention. The crate root makes
// the same allowance for the ABI *types* through `#![allow(non_camel_case_types)]`; this is
// its function-name counterpart, and it is scoped to the module rather than the crate so
// that any future non-ABI helper added here is still held to the convention.
#![allow(non_snake_case)]

use core::cell::Cell;
use core::ffi::{c_char, c_int, c_uint, CStr};
use core::mem::size_of;
use core::ptr;

use zlib_rs::config::{
    validate_deflate_flush, DeflateConfig, DEF_MEM_LEVEL, MAX_WBITS, Z_DEFAULT_STRATEGY, Z_DEFLATED,
};
use zlib_rs::deflate::state::GzHeaderView;
use zlib_rs::deflate::{
    deflate as core_deflate, deflate_bound as core_deflate_bound,
    deflate_bound_z as core_deflate_bound_z, deflate_copy as core_deflate_copy,
    deflate_end as core_deflate_end, deflate_get_dictionary as core_deflate_get_dictionary,
    deflate_init2 as core_deflate_init2, deflate_params as core_deflate_params,
    deflate_pending as core_deflate_pending, deflate_prime as core_deflate_prime,
    deflate_reset as core_deflate_reset, deflate_reset_keep as core_deflate_reset_keep,
    deflate_reset_snapshot as core_deflate_reset_snapshot,
    deflate_set_dictionary as core_deflate_set_dictionary,
    deflate_set_header as core_deflate_set_header, deflate_tune as core_deflate_tune,
    deflate_used as core_deflate_used, DeflateReset, DeflateState, DeflateStream,
};
use zlib_rs::error::ReturnCode;
use zlib_rs::read_buf::OutputRegion;

use crate::panic_guard::{fallback, guard, guard_code};
use crate::types::{
    checked_state, checked_state_mut, commit_state, copy_stream, discard_reserved_state,
    gz_headerp, input_slice, output_region, publish_state, ranges_are_disjoint, reserve_state,
    scratch_allocator, scratch_view, streams_are_disjoint, take_state_with, uLong, widen, z_size_t,
    z_stream, z_streamp, AliasScratch, Bytef, StateBlock, StateKind, StreamAllocator,
};
use crate::util::error_message;

// ---------------------------------------------------------------------------
// Width agreements this module depends on
// ---------------------------------------------------------------------------

/// `uLong` must be no wider than the `u64` the core models it with.
///
/// `uLong` is `unsigned long` (`zconf.h` L406): eight bytes on LP64, four on LLP64 Windows,
/// four on 32-bit. `zlib_rs::deflate::DeflateStream` gives `total_in` and `total_out` the
/// type `u64` for exactly that reason -- it is the widest `unsigned long` any supported
/// target has. Stating the relation as an assertion turns a hypothetical silent truncation
/// in [`widen_uLong`] into a build failure.
/// cbindgen:ignore
const _: () = assert!(
    size_of::<uLong>() <= size_of::<u64>(),
    "uLong must be no wider than u64, or widening a caller's total would truncate"
);

/// `z_size_t` must be exactly `usize`, because [`deflateBound_z`] passes it straight through
/// to the core without conversion.
/// cbindgen:ignore
const _: () = assert!(
    size_of::<z_size_t>() == size_of::<usize>(),
    "z_size_t is size_t, whose Rust mirror is usize"
);

/// The core's status and flush values are `i32`, and the ABI's are [`c_int`].
///
/// `c_int` is `i32` on every target that has both `std` and a C ABI, so the two travel
/// between the layers with no conversion. The assertion turns a hypothetical target where
/// that fails into a build error rather than a silent narrowing.
/// cbindgen:ignore
const _: () = assert!(size_of::<c_int>() == size_of::<i32>());

/// `sizeof(z_stream)` must be expressible as the [`c_int`] `stream_size` argument.
///
/// The structure is a handful of pointers and integers on every supported target -- 112 bytes
/// on LP64, less where pointers and `unsigned long` are narrower -- so this has enormous
/// headroom whatever the target. It exists so that [`Z_STREAM_SIZE`]'s narrowing cast below is
/// provably exact rather than merely obviously so.
///
/// cbindgen:ignore
const _: () = assert!(size_of::<z_stream>() <= c_int::MAX as usize);

/// `sizeof(z_stream)` as the `int` the two `_`-suffixed initialisers compare against.
///
/// `deflate.c` L396 spells the test `stream_size != sizeof(z_stream)`, comparing the
/// *caller's* compile-time view of the struct against the library's. The caller's value
/// arrives as a [`c_int`], so the library's must be one too.
///
/// The value is target-dependent, and deliberately so: what has to match is the caller's own
/// `sizeof(z_stream)`, which varies with pointer width and with the width of `unsigned long`
/// (112 bytes on LP64; smaller on LLP64 Windows and on 32-bit targets). Deriving it from
/// `size_of` rather than writing a number is what makes that agreement automatic.
/// `crates/libz-rs-sys/src/layout_assertions.rs` pins the LP64 size and every field offset
/// behind it at compile time, and states the rest of the layout relationally so the assertions
/// hold on the narrower targets too.
// Neither `cast_possible_truncation` nor `cast_possible_wrap` can occur: the assertion
// immediately above bounds the value by `c_int::MAX`, so a target on which either could
// happen fails to build rather than reporting a wrong size. Written as comments rather than
// as the attribute's `reason` field, which was stabilised in Rust 1.81 and therefore fails to
// compile on the declared 1.80 floor.
#[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
/// cbindgen:ignore
const Z_STREAM_SIZE: c_int = size_of::<z_stream>() as c_int;

/// Widens a caller's `uLong` total to the `u64` the core carries it in.
///
/// Lossless by the assertion above. Named rather than written inline so that the
/// justification lives in one place and every call site reads the same, exactly as
/// [`crate::types::widen`] does for `uInt`.
// Three allowances, all forced by `uLong` being a *platform alias* rather than a fixed-width
// type, and none of them hiding a defect:
//
// * `trivial_numeric_casts` -- the cast is `u64 as u64` on LP64, and genuinely widening
//   `u32 as u64` on LLP64 Windows and on 32-bit. It is trivial on this target only, so the
//   coercion the lint suggests would not compile everywhere.
// * `unnecessary_cast` -- the same fact seen by clippy rather than by rustc: on LP64 the
//   source and target types are both `u64`.
// * `cast_lossless` -- `u64::from(value)` would be the lossless spelling, but `From` is not
//   callable in a `const fn`, and making this one non-const would take `widen_uLong` out of
//   the two `const fn` helpers that use it.
// * `cast_possible_truncation` -- cannot occur, by the assertion above.
//
// Written as comments rather than as the attribute's `reason` field, which was stabilised in
// Rust 1.81 and therefore fails to compile on the declared 1.80 floor.
#[inline]
#[must_use]
#[allow(
    trivial_numeric_casts,
    clippy::unnecessary_cast,
    clippy::cast_lossless,
    clippy::cast_possible_truncation
)]
const fn widen_uLong(value: uLong) -> u64 {
    value as u64
}

/// Narrows a core `u64` total back to a caller's `uLong`, truncating as C's cast does.
///
/// On LP64 this is the identity. On LLP64 Windows, where `unsigned long` is four bytes, it
/// discards the high half -- which is exactly what C's own `unsigned long total_in` does
/// when a stream moves more than 4 GiB there, because the field itself is only that wide.
///
/// ★ Truncation is deliberate and is the faithful behaviour. Saturating instead would report
/// a *different* number from the reference on a target where the cast bites, which is a
/// behavioural change.
// The truncation is the documented intent above; it reproduces C's narrowing assignment
// exactly. `trivial_numeric_casts` and `unnecessary_cast` are allowed for the reason
// `widen_uLong` gives: this is `u64 as u64` on LP64 and a real narrowing elsewhere, so the
// coercion those two lints suggest would not compile on every target.
#[inline]
#[must_use]
#[allow(
    trivial_numeric_casts,
    clippy::unnecessary_cast,
    clippy::cast_possible_truncation
)]
const fn narrow_uLong(value: u64) -> uLong {
    value as uLong
}

/// Reads a caller's `adler` member as the `u32` the core keeps a check value in.
///
/// `z_stream.adler` is a `uLong` (`zlib.h` L108) but never holds more than 32 significant
/// bits: RFC 1950's Adler-32 and RFC 1952's CRC-32 are both four-byte quantities, and
/// `deflate.c` L1269-L1283 emits exactly four bytes of it. Truncating the read therefore
/// loses nothing that the reference would have kept.
// See `narrow_uLong`: the narrowing is the operation, not an accident. `trivial_numeric_casts`
// and `unnecessary_cast` join it because `uLong` is `u32` on i686 and on LLP64 Windows, where
// this same expression is `u32 as u32` and both lints fire; there is no single spelling that is
// lint-free on every supported target, since `u32::from` would then be a `useless_conversion`
// and `u32::try_from` an `unnecessary_fallible_conversion`. The allowance is scoped to this
// one-line function so it cannot hide a narrowing anywhere else.
#[inline]
#[must_use]
#[allow(
    trivial_numeric_casts,
    clippy::unnecessary_cast,
    clippy::cast_possible_truncation
)]
const fn check_of(adler: uLong) -> u32 {
    adler as u32
}

/// Widens a core check value back to the caller's `uLong` member.
///
/// The inverse of [`check_of`], and lossless in this direction on every target: `uLong` is
/// at least four bytes wide wherever a C ABI exists.
// `cast_lossless` cannot be satisfied on LP64 by a spelling that also compiles clean elsewhere:
// `uLong::from(check)` resolves to `u64::from` here but to the reflexive `u32::from` on i686 and
// on LLP64 Windows, where clippy then reports `useless_conversion`. On those same targets this
// expression is `u32 as u32`, so `trivial_numeric_casts` and `unnecessary_cast` are allowed too.
// Scoped to this one-line function, and lossless on every target -- `uLong` is at least four
// bytes wherever a C ABI exists.
#[inline]
#[must_use]
#[allow(trivial_numeric_casts, clippy::unnecessary_cast, clippy::cast_lossless)]
const fn adler_of(check: u32) -> uLong {
    check as uLong
}

/// Narrows a Rust length to the [`c_uint`] a `z_stream`'s `avail_*` member has.
///
/// The counterpart of [`crate::types::widen`]. Every value that reaches it is a count the
/// caller itself supplied as a `uInt`, or a prefix of one, so the clamp names an unreachable
/// case rather than a fallback -- and clamping rather than wrapping is what keeps the
/// function total without a panicking path.
#[inline]
#[must_use]
fn narrow_uInt(value: usize) -> c_uint {
    c_uint::try_from(value).unwrap_or(c_uint::MAX)
}

// ---------------------------------------------------------------------------
// The object behind `z_stream.state`
// ---------------------------------------------------------------------------

/// The compression state as it exists behind a caller's `z_stream.state`.
///
/// Two parameters are fixed here, once, so that the seventeen entry points cannot disagree
/// about what the opaque pointer addresses -- a disagreement that
/// [`crate::types::checked_state_mut`] could not detect, because it validates a tag and an
/// owner rather than a type.
///
/// * The allocator is [`StreamAllocator`], which is what routes every byte through the
///   caller's `zalloc` and `zfree` (AAP §0.6.1 category 4).
/// * The lifetime is `'static`. That is not a claim that the storage is immortal; it is the
///   FFI statement that its extent is not tracked by the Rust type system on this side of
///   the boundary. `StreamAllocator` implements
///   [`zlib_rs::allocate::Allocator`]`<'a>` for any `'a`, and the state's real extent is
///   "until `deflateEnd`", which only [`take_state_with`] can end.
///
/// cbindgen:ignore
type DeflateStateC = DeflateState<'static, StreamAllocator>;

/// What the facade keeps per stream: the core's state, plus the caller's `gz_header`
/// pointer as a RAW pointer.
///
/// # ★ Why the header is retained as a pointer and not as a value
///
/// `deflateSetHeader` stores a pointer in C -- `strm->state->gzhead = head`
/// (`deflate.c` L717) -- and everything that needs the header afterwards reads
/// *through* it: the emission sequence at L1092-L1182 and, separately,
/// `deflateBound`'s wrapper-length computation at L893-L910. So the values a caller
/// sees emitted are the ones present in its structure at the moment `deflate` or
/// `deflateBound` runs, not the ones present when it called `deflateSetHeader`. A
/// caller may legally fill the structure in afterwards, or point `extra`/`name`/
/// `comment` at buffers it fills in later, and `zlib.h` L838-L845 assigns the
/// lifetime of all three buffers to the caller.
///
/// Snapshotting the fields at `deflateSetHeader` time would therefore be wrong twice
/// over: later edits would be silently ignored, and the snapshot would hold Rust
/// shared borrows over C-owned memory that C is entitled to mutate -- an aliasing
/// violation for a program that has done nothing wrong. Keeping the pointer and
/// forming a [`GzHeaderView`] for the duration of one core call, which is what
/// [`with_header`] does, reproduces C exactly and holds no borrow between calls.
struct DeflateSlot {
    /// The core's compressor state.
    state: DeflateStateC,
    /// The caller's `gz_header`, or null. Never dereferenced except inside
    /// [`with_header`], and never for a `wrap != 2` stream, because
    /// `deflateSetHeader` refuses to record one.
    head: gz_headerp,
}

/// The allocated block `z_stream.state` points at: the C-visible prefix plus the slot.
///
/// [`StateBlock`] puts `{ z_streamp strm; int status; }` at offset 0, matching how the C
/// `deflate_state` begins (`deflate.h` L105-L106), which is what makes the owner-identity and
/// tag halves of `deflateStateCheck` (`deflate.c` L544-L555) possible at all.
type DeflateBlock = StateBlock<DeflateSlot>;

/// Runs `body` with the caller's `gz_header` installed in the core state for exactly
/// the duration of the call.
///
/// The core reads `state.gzhead` while it emits the gzip header (`deflate.c`
/// L1092-L1182) and while it sizes the wrapper (L893-L910), so the view has to be
/// present for those calls and must not outlive them. Installing it here and clearing
/// it before returning is what keeps that window exactly one call wide: between calls
/// the core holds `None` and this crate holds only a raw pointer, so no Rust borrow of
/// the caller's structure exists and the caller may edit it freely -- which C permits
/// and which the previous snapshot-at-`deflateSetHeader` design did not.
///
/// The `'static` lifetime the view carries is a forgery bounded by this function, in
/// the same way [`crate::types::input_slice`]'s is bounded by its call. Sound because
/// the view is dropped from the state before this returns, and because `deflate` is
/// documented (`zlib.h` L838-L845) to read the structure and its buffers during the
/// call.
///
/// # Safety
///
/// `slot.head` must be null or address a live [`gz_header`](crate::types::gz_header)
/// whose `extra`, `name` and `comment` are null or readable as
/// [`borrow_gz_header`] requires, for the duration of this call.
unsafe fn with_header<R>(slot: &mut DeflateSlot, body: impl FnOnce(&mut DeflateStateC) -> R) -> R {
    let view = if slot.head.is_null() {
        None
    } else {
        // SAFETY: unsafe-site categories 5 and 6 -- `borrow_gz_header`'s contract is
        // this function's, and `head` is non-null by the test above.
        Some(unsafe { borrow_gz_header(slot.head) })
    };

    slot.state.set_gzhead(view);
    let result = body(&mut slot.state);
    // Unconditional, and it must stay that way: the state outlives this call and must
    // not keep a borrow of the caller's structure. `panic = "abort"` in the release
    // profile means there is no unwinding path that could skip it.
    slot.state.set_gzhead(None);
    result
}

/// Runs `body` with the caller's `gz_header` installed **only while the compressor is
/// still emitting the header**, and with the structure left entirely untouched once it
/// is out.
///
/// ★ **This is the header's lifetime contract, and it is narrower than
/// [`with_header`]'s.** `zlib.h` L836-L852 asks the application to keep the
/// `gz_header` and its `extra`, `name` and `comment` buffers available *until the
/// header has been written*, and no longer — so a conforming caller frees the whole
/// structure the moment the gzip header is out. C makes that safe by construction:
/// every `s->gzhead` dereference inside `deflate()` sits under a
/// `s->status == GZIP_STATE | EXTRA_STATE | NAME_STATE | COMMENT_STATE | HCRC_STATE`
/// test (`deflate.c` L1066-L1200), the status only advances, and once `HCRC_STATE`
/// hands over to `BUSY_STATE` at L1195 the pointer is never followed again.
///
/// [`with_header`] reads seven members and scans two NUL-terminated strings, so using
/// it for every `deflate` call would read a structure the caller was entitled to have
/// released — a use-after-free reachable from conforming C, observed as a `SIGSEGV`
/// where the reference completes normally. Asking
/// [`Status::dereferences_gzip_header`](zlib_rs::deflate::state::Status::dereferences_gzip_header)
/// first is what reproduces C's own discipline.
///
/// The status is read on entry, which is the right instant for the same reason it is
/// in C: the status ladder cannot re-enter a header stage inside one call. A
/// `deflateReset` does return it to `GZIP_STATE`, and that is also correct — C's
/// `deflateResetKeep` deliberately does **not** clear `s->gzhead` (`deflate.c`
/// L640-L668), so a reset stream re-emits the caller's header and must read it again.
///
/// ★ **Not for the bound functions.** `deflateBound_z` reads `s->gzhead` whenever
/// `|wrap| == 2` and a header is installed, regardless of status (`deflate.c`
/// L891-L910), and goes on doing so for the whole life of the stream: a caller that
/// withdraws `extra`, `name` or `comment` sees the bound shrink by exactly the field
/// cost. Those two entry points therefore keep using [`with_header`] unconditionally.
///
/// # Safety
///
/// The same as [`with_header`]'s, and discharged on strictly fewer calls: `slot.head`
/// must be null or address a live `gz_header` whose three buffers are readable as
/// [`borrow_gz_header`] requires — but only for the calls that reach the read, which
/// are the ones during which `zlib.h` L836-L852 obliges the caller to keep it alive.
unsafe fn with_header_while_emitting<R>(
    slot: &mut DeflateSlot,
    body: impl FnOnce(&mut DeflateStateC) -> R,
) -> R {
    if !slot.state.status().dereferences_gzip_header() {
        // The header is out (or the stream never had one), so C would not look at the
        // structure on this call and neither does this. The core's own `gzhead` is
        // already `None` — `with_header` clears it before returning from every call
        // that installs it — so there is nothing to clear either, and the compressor
        // sees exactly the state it would see for a stream with no header installed,
        // which is what it needs past `BUSY_STATE`.
        return body(&mut slot.state);
    }

    // SAFETY: unsafe-site category 6 -- `with_header`'s contract is this function's,
    // narrowed to the header-emitting statuses above.
    unsafe { with_header(slot, body) }
}

// ---------------------------------------------------------------------------
// `z_stream` member access -- AAP §0.6.1 unsafe-site category 1
// ---------------------------------------------------------------------------

/// The four `z_stream` members that a *reset* initialises and every later call carries.
///
/// Separated from the four buffer members deliberately, and the separation is load-bearing
/// rather than tidy. `deflateReset` assigns `total_in`, `total_out`, `data_type` and `adler`
/// (`deflate.c` L651-L671) but says nothing about `next_in`, `avail_in`, `next_out` or
/// `avail_out`, so **after initialisation these four are the only members known to hold a
/// value**. `test/example.c` declares its `z_stream` on the stack and sets nothing but the
/// three allocator members, so an entry point that reads a buffer member it does not need
/// would be reading indeterminate memory -- and reading an indeterminate `*mut Bytef` into a
/// Rust pointer is undefined behaviour, not merely a garbage value.
///
/// Every entry point therefore reads exactly what C reads: this struct where C reads only the
/// scalars, and [`StreamFields`] where C also reaches the buffers.
#[derive(Debug, Clone, Copy)]
struct StreamScalars {
    /// `strm->total_in` (`zlib.h` L93).
    total_in: uLong,
    /// `strm->total_out` (`zlib.h` L97).
    total_out: uLong,
    /// `strm->adler` (`zlib.h` L108), the running check value.
    adler: uLong,
    /// `strm->data_type` (`zlib.h` L106), informational only.
    data_type: c_int,
}

impl StreamScalars {
    /// Reads the four members through raw places, forming no reference to the stream.
    ///
    /// # Safety
    ///
    /// `strm` must be non-null, aligned, and address a live [`z_stream`] whose four members
    /// below are initialised and which nothing else is concurrently writing. Every caller
    /// establishes the first two by having already obtained a validated state through
    /// [`deflate_block_mut`] or [`deflate_block_ref`], and the third by the same token: a
    /// stream that owns a state has been through `deflateInit2_`, which resets all four.
    #[must_use]
    unsafe fn read(strm: z_streamp) -> Self {
        // SAFETY: unsafe-site category 1 -- reading four members of the caller's stream.
        // `strm` is non-null, aligned and addresses a live `z_stream` whose four members below
        // are initialised, all by this function's contract, so each is in bounds and readable.
        // `addr_of!` plus `read` forms a raw place rather than a `&z_stream`, so nothing here
        // disturbs the borrow stack of the caller's own pointer -- which the state borrow and
        // the write-back both depend on.
        unsafe {
            Self {
                total_in: ptr::addr_of!((*strm).total_in).read(),
                total_out: ptr::addr_of!((*strm).total_out).read(),
                adler: ptr::addr_of!((*strm).adler).read(),
                data_type: ptr::addr_of!((*strm).data_type).read(),
            }
        }
    }
}

/// The `z_stream` members an entry point reads when it may compress.
///
/// Exactly the eight C reads through `strm->…` in `deflate()` (L989-L998, and the compressors'
/// accesses through `s->strm`), and no more. Snapshotting them in one struct has two purposes:
/// the read happens once, at a single audited site; and the *entry* values stay available
/// afterwards, which is what lets the write-back compute `next_in += used` and
/// `avail_in -= used` from the core's consumed count.
///
/// The members the caller may legitimately have left indeterminate -- `msg`, `state` and
/// `reserved` -- are deliberately absent.
#[derive(Debug, Clone, Copy)]
struct StreamFields {
    /// `strm->next_in` (`zlib.h` L91). May be null while `avail_in` is zero.
    next_in: *const Bytef,
    /// `strm->avail_in` (`zlib.h` L92).
    avail_in: c_uint,
    /// `strm->next_out` (`zlib.h` L95). A null here is an error `deflate()` reports.
    next_out: *mut Bytef,
    /// `strm->avail_out` (`zlib.h` L96).
    avail_out: c_uint,
    /// The four members a reset initialises; see [`StreamScalars`].
    scalars: StreamScalars,
}

impl StreamFields {
    /// Reads the eight members through raw places, forming no reference to the stream.
    ///
    /// # Safety
    ///
    /// As [`StreamScalars::read`], and additionally: the four buffer members must be
    /// initialised too. That is the caller's own obligation before calling `deflate`, which
    /// `zlib.h` L136-L139 states, and it is why this function is used only where C also reads
    /// them.
    #[must_use]
    unsafe fn read(strm: z_streamp) -> Self {
        // SAFETY: unsafe-site category 1 -- reading four further members of the caller's
        // stream, under the same conditions `StreamScalars::read` documents, with this
        // function's contract additionally making the four buffer members initialised. Raw
        // places throughout, so the caller's pointer keeps its provenance.
        unsafe {
            Self {
                next_in: ptr::addr_of!((*strm).next_in).read(),
                avail_in: ptr::addr_of!((*strm).avail_in).read(),
                next_out: ptr::addr_of!((*strm).next_out).read(),
                avail_out: ptr::addr_of!((*strm).avail_out).read(),
                scalars: StreamScalars::read(strm),
            }
        }
    }

    /// Whether the caller's input and output ranges share a byte.
    ///
    /// ★ **The precondition [`borrow_stream`] cannot check and cannot do without.** It
    /// builds a `&[u8]` over `(next_in, avail_in)` and a `&mut [u8]` over
    /// `(next_out, avail_out)`, and two such borrows over one region are undefined
    /// behaviour *whether or not either is ever touched*. `zlib.h` L136-L139 describes
    /// the two pairs independently and never says they must be distinct, so an
    /// overlapping pair is an input this library can be handed; `deflate.c` L1000 simply
    /// `LOAD()`s both into locals and lets the compressor read and write one buffer.
    ///
    /// This reports the condition; it does not decide what to do about it. The answer is
    /// **not** a refusal -- `test/example.c`'s `test_large_deflate` overlaps them
    /// deliberately at L275-L277 and the suite asserts the resulting byte count -- but a
    /// snapshot of the input, taken by [`capture_overlapping_input`] before any mutable
    /// borrow exists and handed to [`borrow_stream`] in the caller's place. See
    /// [`AliasScratch`] for why copying is the only response that both preserves the call's
    /// consumption accounting and keeps the two borrows apart.
    ///
    /// Nothing is dereferenced: [`ranges_are_disjoint`] compares addresses, so this is
    /// safe to run on the raw members before any borrow exists, which is the only point
    /// at which the answer can still be acted on.
    #[must_use]
    fn buffers_overlap(&self) -> bool {
        !ranges_are_disjoint(
            self.next_in,
            widen(self.avail_in),
            self.next_out.cast_const(),
            widen(self.avail_out),
        )
    }
}

/// Writes `strm->msg`, either a `'static` message or C's `Z_NULL`.
///
/// The library stores only `'static` strings in `msg` -- the `z_errmsg` table of
/// `zutil.c` L13-L24 -- so publishing one costs no allocation and the pointee outlives every
/// caller's use of it. [`None`] is the faithful spelling of `strm->msg = Z_NULL`
/// (`deflate.c` L400 and L652).
///
/// # Safety
///
/// `strm` must be non-null, aligned and address a live [`z_stream`] that nothing else is
/// concurrently writing. The member need not be initialised: this only writes it, which is
/// what makes it safe to call on the partly initialised stream `deflateInit2_` receives.
unsafe fn write_msg(strm: z_streamp, msg: Option<&'static CStr>) {
    let text = match msg {
        Some(message) => message.as_ptr(),
        None => ptr::null(),
    };
    // SAFETY: unsafe-site category 1 -- writing one member of the caller's stream. `strm` is
    // non-null, aligned and live by this function's contract, so `msg` is in bounds and
    // writable. `addr_of_mut!` forms a raw place, so no `&mut z_stream` is materialised and
    // the caller's pointer keeps its provenance. The value written is either null or a
    // `'static` NUL-terminated string from `crate::util`, so the pointee outlives the stream.
    unsafe {
        ptr::addr_of_mut!((*strm).msg).write(text);
    }
}

/// Writes `strm->state = Z_NULL`.
///
/// Needed on the out-of-memory path of [`deflateInit2_`], where C reaches the same end state
/// by calling `deflateEnd(strm)` (`deflate.c` L512). [`take_state_with`] performs the same
/// clearing for the ordinary teardown.
///
/// # Safety
///
/// As [`write_msg`]: `strm` must be non-null, aligned and live. The member need not be
/// initialised, since it is only written.
unsafe fn write_state_null(strm: z_streamp) {
    // SAFETY: unsafe-site category 1 -- writing one member of the caller's stream. Non-null,
    // aligned and live by this function's contract; written through a raw place so the
    // caller's pointer keeps its provenance. A null `state` is the value every entry point
    // rejects, which is precisely the intent.
    unsafe {
        ptr::addr_of_mut!((*strm).state).write(ptr::null_mut());
    }
}

/// Applies the `z_stream` half of a reset to the caller's stream.
///
/// `deflateResetKeep` assigns five members of the caller's structure (`deflate.c` L651-L653
/// and L667-L671), and the core returns them as a [`DeflateReset`] rather than writing them,
/// because it has no `z_stream`. This is the writing half.
///
/// ★ `adler` is the easily missed one. A freshly zeroed `z_stream` has `adler == 0`, but
/// `deflateResetKeep` seeds it to `adler32(0, Z_NULL, 0)`, which is **1**, for a zlib or raw
/// stream. Omitting this leaves a zlib stream reporting the wrong check value before any
/// input has been read -- something `test/example.c` observes.
///
/// # Safety
///
/// As [`write_msg`]. None of the five members needs to be initialised beforehand.
unsafe fn write_reset(strm: z_streamp, reset: DeflateReset) {
    // SAFETY: unsafe-site category 1 -- writing five members of the caller's stream. Non-null,
    // aligned and live by this function's contract, so all five are in bounds and writable,
    // and all five are written rather than read. Raw places throughout, so the caller's
    // pointer keeps its provenance.
    unsafe {
        ptr::addr_of_mut!((*strm).total_in).write(narrow_uLong(reset.total_in));
        ptr::addr_of_mut!((*strm).total_out).write(narrow_uLong(reset.total_out));
        ptr::addr_of_mut!((*strm).data_type).write(reset.data_type);
        ptr::addr_of_mut!((*strm).adler).write(adler_of(reset.adler));
    }
    // `strm->msg = Z_NULL;`  (L652). The core reports that assignment as `None`, and a reset
    // has no other message to report -- `DeflateReset::msg` documents itself as exactly this
    // assignment. Routed through `recorded_msg` rather than written as an unconditional null
    // so that a core which ever did report one would publish it instead of dropping it.
    //
    // SAFETY: unsafe-site category 1 -- one further member of the caller's stream, written
    // through a raw place. `strm` is non-null, aligned and live by this function's contract, so
    // `msg` is in bounds and writable, and it is written rather than read, so its previous
    // contents may be indeterminate. The value published is either null or a `'static`
    // NUL-terminated string from `crate::util`, so it outlives every caller that reads it, and
    // no `&mut z_stream` is formed here or above, so the caller's pointer keeps its provenance.
    unsafe {
        write_msg(strm, recorded_msg(reset.msg, ReturnCode::OK));
    }
}

/// Turns the message the core recorded into the `'static` C string to publish in `strm->msg`.
///
/// The deflate core records a message only through [`ReturnCode::record_msg`], which stores
/// `err_msg` of the status *that* call was about to return (`zutil.h` L65-L68, the
/// `ERR_MSG`/`ERR_RETURN` pair). Every recorded message is therefore an entry of `z_errmsg`,
/// and [`crate::util::message_cstr`] is the NUL-terminated mirror of that same table, so no
/// second copy of the table is needed here.
///
/// ★ **The recorded message does not always belong to the returned status, and C's does not
/// either.** `deflateParams` is the case that proves it. It flushes by calling `deflate`
/// internally and then *discards that call's return value* unless it was `Z_STREAM_ERROR`
/// (`deflate.c` L794-L800). So a caller who issues `Z_SYNC_FLUSH` and then changes the level
/// with no new input takes the `RANK(flush) <= RANK(old_flush)` path at `deflate.c`
/// L1018-L1021, whose `ERR_RETURN` records `"buffer error"` -- and `deflateParams` then goes on
/// to succeed and return `Z_OK`. C leaves `strm->msg` pointing at that stale `"buffer error"`,
/// because `ERR_RETURN` is a plain assignment and nothing clears it; `zlib.h` L100-L101 makes
/// `msg` meaningful only when an error is returned, which is what makes that harmless.
///
/// This function therefore publishes **the message the core recorded**, not the message
/// belonging to `code`. That is the C-faithful answer -- deriving it from `code` would publish
/// the empty `Z_OK` string where C publishes `"buffer error"` -- and it is why the lookup goes
/// through [`crate::util::message_cstr`] rather than [`error_message`].
///
/// [`None`] in means [`None`] out, which is what leaves `strm->msg` untouched: C's plain
/// `return Z_STREAM_ERROR` at `deflate.c` L985 is deliberately not an `ERR_RETURN`, so a
/// caller's previous message survives it.
///
/// The debug assertion is the self-check that keeps the two tables from drifting. What it can
/// honestly assert is that the recorded message is *an entry in the table at all*, since a
/// message the mirror lacks is the one real drift risk; it deliberately does **not** assert a
/// correspondence with `code`, which the paragraph above shows does not hold. It is a
/// `debug_assert!` rather than a hard failure for the reason the whole crate is careful about:
/// aborting a C caller's process over a diagnostic string would be a far worse outcome than
/// publishing the empty string.
#[inline]
#[must_use]
fn recorded_msg(message: Option<&'static str>, code: ReturnCode) -> Option<&'static CStr> {
    let message = message?;
    let published = crate::util::message_cstr(message);
    debug_assert!(
        published.to_bytes() == message.as_bytes(),
        "the deflate core recorded a message the facade's NUL-terminated mirror does not \
         contain (status {})",
        code.as_i32()
    );
    Some(published)
}

// ---------------------------------------------------------------------------
// The opaque `state` round-trip -- AAP §0.6.1 unsafe-site categories 3 and 4
// ---------------------------------------------------------------------------

/// Reproduces the two halves of `deflateStateCheck` that live outside the core, and yields a
/// mutable borrow of the state.
///
/// `deflateStateCheck` (`deflate.c` L538-L556) rejects a stream for six reasons. Five of them
/// are pointer facts and belong here; the sixth -- that `status` is one of the eight legal
/// values -- is [`zlib_rs::deflate::deflate_state_check`], which
/// [`crate::types::checked_state_mut`] calls through [`StateKind::Deflate`]:
///
/// | C test | `deflate.c` | Performed by |
/// |---|---|---|
/// | `strm == Z_NULL` | L540 | [`checked_state_mut`] |
/// | `strm->zalloc == 0 \|\| strm->zfree == 0` | L541 | [`StreamAllocator::from_stream_ptr`], below |
/// | `s == Z_NULL` | L544 | [`checked_state_mut`] |
/// | `s->strm != strm` | L544 | [`checked_state_mut`] |
/// | `s->status` is none of the eight | L546-L555 | the core, via [`StateKind::Deflate`] |
///
/// ★ The allocator test is performed **here** rather than inside
/// [`crate::types::checked_state_mut`], and it is exactly the test C performs. C overwrites a
/// null `zalloc` or `zfree` with `zcalloc`/`zcfree` during initialisation
/// (`deflate.c` L401-L414) -- and so does this port, through
/// [`StreamAllocator::adopt_hooks`], writing the substituted pointers back into the caller's
/// structure -- so after `deflateInit_` neither member is ever null. `deflateStateCheck`
/// (L536-L539) nevertheless begins by testing both, because a stream can arrive here without
/// having been initialised, or with its members cleared afterwards, which is precisely what
/// `test/infcover.c`'s `mem_done` does (L231-L233). [`StreamAllocator::from_stream_ptr`]
/// returns [`None`] for that case and for a null or misaligned `strm`.
///
/// [`None`] means the entry point must return `Z_STREAM_ERROR`.
///
/// # Safety
///
/// If `strm` is non-null it must address a live [`z_stream`], and if that stream's `state` is
/// non-null it must address a [`DeflateBlock`] produced by [`commit_state`]. No other borrow
/// of that block may exist for `'a`; recover it once on entry.
#[must_use]
unsafe fn deflate_block_mut<'a>(strm: z_streamp) -> Option<&'a mut DeflateBlock> {
    // SAFETY: unsafe-site category 4 -- reading the caller's three allocator members. The
    // helper checks non-null and alignment itself and forms no reference to the stream, so
    // the caller's pointer stays usable for the state read below; liveness is this function's
    // documented obligation.
    unsafe { StreamAllocator::from_stream_ptr(strm) }?;
    // SAFETY: unsafe-site category 3 -- the opaque `state` round-trip. The helper performs
    // the four validity checks (stream non-null and aligned, state non-null and aligned,
    // owner identity, tag in range) before forming any reference, and this function's
    // contract supplies the liveness, provenance and exclusivity requirements it cannot test.
    unsafe { checked_state_mut::<DeflateSlot>(strm, StateKind::Deflate) }
}

/// The shared-borrow counterpart of [`deflate_block_mut`], for the five entry points that
/// only read the state: [`deflateGetDictionary`], [`deflateBound`], [`deflateBound_z`],
/// [`deflatePending`] and [`deflateUsed`].
///
/// # Safety
///
/// As [`deflate_block_mut`], except that other *shared* borrows are permitted.
#[must_use]
unsafe fn deflate_block_ref<'a>(strm: z_streamp) -> Option<&'a DeflateBlock> {
    // SAFETY: unsafe-site category 4 -- reading the caller's three allocator members. The
    // helper checks non-null and alignment itself and forms no reference to the stream, so
    // the caller's pointer stays usable for the state read below; liveness is this function's
    // documented obligation.
    unsafe { StreamAllocator::from_stream_ptr(strm) }?;
    // SAFETY: unsafe-site category 3 -- the opaque `state` round-trip, yielding a shared
    // borrow. The helper performs the same four validity checks (stream non-null and aligned,
    // state non-null and aligned, owner identity, tag in range) before forming any reference,
    // and this function's contract supplies the liveness and provenance it cannot test. Only
    // exclusivity is relaxed relative to `deflate_block_mut`: the borrow is shared, so other
    // shared borrows of the same block may coexist with it, and nothing writes through it.
    unsafe { checked_state::<DeflateSlot>(strm, StateKind::Deflate) }
}

/// Copies the state's `status` into the C-visible tag at offset 8 of the block.
///
/// The tag and the Rust `Status` are separate storage, so they stay in step only because
/// every entry point that can change the status syncs them. Keeping them in step is what
/// makes the tag half of `deflateStateCheck` (`deflate.c` L546-L555) meaningful on the next
/// call rather than merely non-rejecting.
fn sync_tag(block: &mut DeflateBlock) {
    let tag = block.state().state.status().as_raw();
    block.set_tag(tag);
}

// ---------------------------------------------------------------------------
// The version gate -- AAP §0.6.1 unsafe-site categories 1 and 5
// ---------------------------------------------------------------------------

/// The `ZLIB_VERSION` and `sizeof(z_stream)` gate both `_`-suffixed initialisers open with.
///
/// Reproduces `deflate.c` L394-L397:
///
/// ```text
/// if (version == Z_NULL || version[0] != my_version[0] ||
///     stream_size != sizeof(z_stream)) {
///     return Z_VERSION_ERROR;
/// }
/// ```
///
/// ★ **This gate precedes the null-stream test**, and the order is observable. `zlib.h`
/// L224-L229 explains the intent -- "If the first character differs, the library code actually
/// used is not compatible with the zlib.h header file used by the application" -- and
/// `test/infcover.c` L481 asserts it directly for the inflate counterpart:
/// `inflateBackInit_(Z_NULL, 0, win, 0, 0)` must report `Z_VERSION_ERROR`, not
/// `Z_STREAM_ERROR`, even though the stream is null. Testing the version first is what makes
/// a version mismatch diagnosable by an application whose `z_stream` is a different size from
/// the library's, which is exactly the situation the check exists for.
///
/// Only the **major** character is compared, because that is all C compares: a library and a
/// header that agree on it are ABI-compatible, so `1.2.11` may legitimately call into
/// `1.3.2.1-motley`. The library's own string is [`crate::util::ZLIB_VERSION`], the verbatim
/// `"1.3.2.1-motley"` of `zlib.h` L44 -- never `env!("CARGO_PKG_VERSION")`, which is the
/// three-component `1.3.2` and would answer a caller's `1.x` check correctly by accident while
/// misreporting the library's identity everywhere else.
///
/// Returns `true` when the caller is compatible, i.e. when the entry point may proceed.
///
/// # Safety
///
/// If `version` is non-null it must address at least one readable byte for the duration of the
/// call. C requires more of it -- a NUL-terminated string -- but this function reads exactly
/// one byte, which is all `deflate.c` L395 reads.
#[must_use]
unsafe fn version_is_compatible(version: *const c_char, stream_size: c_int) -> bool {
    // `version == Z_NULL || stream_size != sizeof(z_stream)`: both halves are decidable
    // without a dereference, so they are settled first. C's `||` short-circuits in the same
    // order for the null half, which is what makes `version[0]` safe to read below.
    if version.is_null() || stream_size != Z_STREAM_SIZE {
        return false;
    }

    // SAFETY: unsafe-site categories 1 and 5 -- reading one byte of a caller-supplied C
    // string. `version` is non-null by the test above, trivially aligned for `c_char`, and
    // readable for at least one byte by this function's contract. Exactly one byte is read,
    // so no NUL scan and no bound beyond the first element is required, and nothing is written.
    let caller_major = unsafe { version.read() };

    // C compares two `char`s, i.e. two bytes, and `c_char` is signed on some targets and
    // unsigned on others. Going through the one-byte native representation compares the same
    // bit pattern C does without a cast, so no `cast_sign_loss` allowance is needed and the
    // code is correct for both signednesses.
    let caller_major = u8::from_ne_bytes(caller_major.to_ne_bytes());

    // `my_version[0]`, where `my_version` is `ZLIB_VERSION`. Read from the constant rather
    // than re-derived with `ZLIB_VERSION.to_bytes().first()`, which returns an `Option` and
    // would need an unreachable fallback named here: `crate::util` asserts at build time
    // that this byte *is* `ZLIB_VERSION`'s first byte and that it is `ZLIB_VER_MAJOR`
    // rendered in ASCII, so the derivation is checked once, centrally, and is infallible at
    // every use.
    let library_major = crate::util::ZLIB_VER_MAJOR_DIGIT;

    caller_major == library_major
}

// ---------------------------------------------------------------------------
// The `z_stream` <-> `DeflateStream` bridge
// ---------------------------------------------------------------------------

/// Turns the caller's two pointer/length pairs into the core's stream view.
///
/// The `LOAD()` half of C's macro pair (`deflate.c` L959-L968 as used at L1000): the
/// compressor reaches `next_in`, `avail_in`, `next_out`, `avail_out`, `total_in`, `total_out`,
/// `adler` and `data_type` through `s->strm`, and this is the safe equivalent.
///
/// ★ **Both cursors start at zero, and the slices start where the caller's pointers point.**
/// That is not an approximation of C, it *is* C: `deflate_stored` reads the window's history
/// *backwards* from `strm->next_in` (`deflate.c` L1766 and L1780), but never further back than
/// the bytes consumed during the current call, and the core reaches them through
/// `InputCursor::consumed_tail`, which is bounded by the cursor. So the prefix the core needs
/// is exactly the prefix this call produces, and a slice that began earlier would be neither
/// available at the ABI -- the caller's `next_in` has already advanced -- nor required.
///
/// ★ `msg` is seeded [`None`], never read back from `strm->msg`. That is what reproduces C's
/// distinction between a plain `return Z_STREAM_ERROR` (L985, which leaves the caller's
/// message alone) and an `ERR_RETURN` (L993, which replaces it): the write-back publishes a
/// message only when the core recorded one. Reading the caller's `msg` back in would also mean
/// dereferencing a `char *` this library did not necessarily write.
///
/// # Safety
///
/// ★ `input_override` is how an overlapping buffer pair is served rather than refused. When
/// it is [`Some`], that slice is used as the input instead of a borrow of the caller's
/// `(next_in, avail_in)` -- see [`AliasScratch`], which is what produces it. It must be
/// exactly `entry.avail_in` bytes long, because the write-back derives the caller's new
/// `avail_in` from the cursor into it.
///
/// # Safety
///
/// `entry` must have come from [`StreamFields::read`] on a live stream, and its two buffers
/// must satisfy the ordinary C contract: `avail_in` bytes readable at `next_in` when that
/// count is non-zero, `avail_out` bytes writable at `next_out` when that count is non-zero,
/// and the output region not aliased by anything else -- including by the [`z_stream`]
/// itself -- for the duration of the call. When `input_override` is [`None`] the input
/// region must additionally be disjoint from the output region; when it is [`Some`] that
/// requirement moves to the override, which an [`AliasScratch`] satisfies by owning its
/// bytes, and the caller's input region is not borrowed at all. Call this **once** per entry
/// point: two live mutable slices over one output buffer would be undefined behaviour even
/// if neither were written.
#[must_use]
unsafe fn borrow_stream(
    entry: &StreamFields,
    input_override: Option<&'static [u8]>,
) -> DeflateStream<'static, 'static> {
    let input = match input_override {
        Some(captured) => captured,
        // SAFETY: unsafe-site category 2 -- slice reconstruction, performed once. The helper
        // branches on a zero length and on a null pointer, so a `z_stream` with
        // `avail_in == 0` and `next_in == Z_NULL` -- which `zlib.h` L138-L139 invites and
        // `deflate.c` L990 explicitly permits -- yields a genuine empty slice rather than a
        // dangling one. For a non-zero count this function's contract makes the bytes
        // readable, stable and disjoint from the output.
        None => unsafe { input_slice(entry.next_in, entry.avail_in) },
    };

    // SAFETY: unsafe-site category 2 -- as above, for the output. The same zero-length rule
    // applies; a null `next_out` has already been rejected by the entry point on the paths
    // where C rejects it, and yields an empty region on the paths where C does not, so no
    // dangling slice can be formed. The region is writable, unaliased and stable by this
    // function's contract, and this is the only mutable view of it. It is **write-only**
    // storage rather than a byte slice, because `avail_out` bytes of room is all `zlib.h`
    // L94-L95 promises; `output_region` states the argument in full.
    let output = unsafe { output_region(entry.next_out, entry.avail_out) };

    entry.scalars.into_stream(input, output)
}

/// Captures the caller's input when it overlaps the caller's output, so the call can run.
///
/// Returns [`None`] when the two ranges are already disjoint -- the overwhelmingly common
/// case, in which nothing is copied and [`borrow_stream`] borrows the caller's buffer
/// directly -- and also when they overlap but the snapshot could not be allocated. The two
/// are distinguished by [`StreamFields::buffers_overlap`], which the callers test first, so
/// a `None` from an overlapping pair means "allocation failed" and is the one case that
/// still has to be refused.
///
/// ★ `allocator` is the stream's own, from [`scratch_allocator`], so a caller's `zalloc`
/// sees the request and a caller's `mem_limit` can refuse it. A refusal arrives here as
/// [`None`] and is reported, not worked around.
///
/// # Safety
///
/// `entry` must have come from [`StreamFields::read`] on a live stream whose
/// `(next_in, avail_in)` pair is readable, and no mutable borrow of that region may exist
/// yet -- which is why this runs before [`borrow_stream`] rather than beside it.
#[must_use]
unsafe fn capture_overlapping_input(
    allocator: &StreamAllocator,
    entry: &StreamFields,
) -> Option<AliasScratch> {
    // SAFETY: unsafe-site category 2 -- `AliasScratch::capture`'s contract is this
    // function's: the region is readable for `avail_in` bytes and no mutable borrow of it
    // exists, because the only one this library ever creates is the output borrow that
    // `borrow_stream` makes afterwards.
    unsafe { AliasScratch::capture(allocator, entry.next_in, widen(entry.avail_in)) }
}

impl StreamScalars {
    /// Builds a stream view over `input` and `output` carrying these four scalars.
    ///
    /// Shared by [`borrow_stream`], which supplies the caller's buffers, and by
    /// [`deflateParams`], which supplies two empty slices on the path where C provably never
    /// reaches the caller's buffers at all. Keeping the construction in one place is what
    /// guarantees the two agree about how `msg`, `adler` and `data_type` are seeded.
    #[must_use]
    fn into_stream(
        self,
        input: &'static [u8],
        output: OutputRegion<'static>,
    ) -> DeflateStream<'static, 'static> {
        DeflateStream {
            input,
            next_in: 0,
            output,
            next_out: 0,
            total_in: widen_uLong(self.total_in),
            total_out: widen_uLong(self.total_out),
            msg: None,
            adler: check_of(self.adler),
            data_type: self.data_type,
        }
    }
}

/// Writes the core's results back into the caller's `z_stream`, in the order C writes them.
///
/// The `RESTORE()` half of the macro pair. C reaches the same end state from twelve different
/// `return` sites; the core funnels all twelve through one write-back, and so does this.
///
/// The order below is C's, and it is documented rather than merely followed because a
/// consumer reading the stream from another thread -- which `zlib.h` does not sanction, but
/// which happens -- sees the members in this sequence:
///
/// 1. `next_in` advances by the bytes consumed, and `avail_in` drops by the same count;
/// 2. `next_out` advances by the bytes produced, and `avail_out` drops by the same count;
/// 3. `total_in` and `total_out` take their new absolute totals;
/// 4. `adler` takes the running check value;
/// 5. `data_type` takes the compressor's data-type guess;
/// 6. `msg` is published **only if the core recorded one**.
///
/// ★ Step 6 is conditional, and that is the faithful behaviour rather than a shortcut.
/// `deflate.c` writes `strm->msg` in exactly seven places in the whole file: L400 and L652
/// clear it, L511 reports an out-of-memory condition, and the four `ERR_RETURN`s at L993,
/// L995, L1020 and L1025 set it alongside the status they return. A successful `deflate()`
/// call therefore leaves a message an earlier failure recorded exactly where it was, and
/// clearing it here would discard a diagnostic a caller is entitled to still find.
///
/// `total_in` and `total_out` are absolute rather than incremental because
/// [`borrow_stream`] seeded them from the caller's current values, which is how C's
/// `read_buf` (L237) and `flush_pending` (L957) accumulate them.
///
/// # Safety
///
/// `strm` must be non-null, aligned and address a live [`z_stream`] that nothing else is
/// concurrently writing, `entry` must be the [`StreamFields`] read from that same stream on
/// entry, and `stream` must be the view [`borrow_stream`] built from `entry`.
unsafe fn publish(
    strm: z_streamp,
    entry: &StreamFields,
    stream: &DeflateStream<'_, '_>,
    code: ReturnCode,
) {
    // The core's cursors are offsets into the two slices, and both slices began at the
    // caller's pointers with the cursor at zero, so each offset *is* the count moved during
    // this call. Neither can exceed its slice length, hence neither conversion clamps and
    // neither subtraction saturates in practice; both are written in their total form so that
    // no arithmetic in this function can panic in a debug build.
    let consumed = narrow_uInt(stream.next_in);
    let produced = narrow_uInt(stream.next_out);

    // SAFETY: unsafe-site category 1 -- writing eight members of the caller's stream. `strm`
    // is non-null, aligned and live by this function's contract, so all eight are in bounds
    // and writable, and every one of them is written rather than read. `addr_of_mut!` forms
    // raw places, so no `&mut z_stream` is materialised and the caller's own pointer keeps its
    // provenance -- which matters because the block borrow taken on entry is still alive here.
    // The two pointer advances use `wrapping_add`, which is defined for every input including
    // a null base with a zero offset; the counts are bounded by the slice lengths, so each
    // result is inside the caller's buffer or one past its end, exactly as C's
    // `strm->next_in += used` produces.
    unsafe {
        ptr::addr_of_mut!((*strm).next_in).write(entry.next_in.wrapping_add(stream.next_in));
        ptr::addr_of_mut!((*strm).avail_in).write(entry.avail_in.saturating_sub(consumed));
        ptr::addr_of_mut!((*strm).next_out).write(entry.next_out.wrapping_add(stream.next_out));
        ptr::addr_of_mut!((*strm).avail_out).write(entry.avail_out.saturating_sub(produced));
    }

    // SAFETY: unsafe-site category 1 -- `publish_scalars` writes the four scalars and, if the
    // core recorded one, `msg`, and the obligation it places on its caller is the one this
    // function places on its own: `strm` is non-null, aligned and live, so all five members are
    // in bounds and writable, and each is written rather than read. It writes through raw places
    // exactly as the block above does, so no `&mut z_stream` is formed and the caller's pointer
    // keeps its provenance.
    unsafe {
        publish_scalars(strm, stream, code);
    }
}

/// Writes back the four scalars and, conditionally, `msg` -- steps 3 to 6 of [`publish`].
///
/// Split out because [`deflateParams`] needs exactly these four on the path where C's
/// `deflateParams` never reaches the caller's buffers, and must not write the four buffer
/// members there: doing so would replace the caller's `next_out` and `avail_out` with values
/// derived from an empty slice.
///
/// # Safety
///
/// `strm` must be non-null, aligned and address a live [`z_stream`] that nothing else is
/// concurrently writing, and `stream` must be the view the matching entry point handed to the
/// core.
unsafe fn publish_scalars(strm: z_streamp, stream: &DeflateStream<'_, '_>, code: ReturnCode) {
    // SAFETY: unsafe-site category 1 -- writing four members of the caller's stream through
    // raw places. `strm` is non-null, aligned and live by this function's contract, so all
    // four are in bounds and writable, and every one is written rather than read. No
    // `&mut z_stream` is materialised, so the caller's own pointer keeps its provenance --
    // which matters because the state borrow taken on entry is still alive here.
    unsafe {
        ptr::addr_of_mut!((*strm).total_in).write(narrow_uLong(stream.total_in));
        ptr::addr_of_mut!((*strm).total_out).write(narrow_uLong(stream.total_out));
        ptr::addr_of_mut!((*strm).adler).write(adler_of(stream.adler));
        ptr::addr_of_mut!((*strm).data_type).write(stream.data_type);
    }

    if let Some(message) = recorded_msg(stream.msg, code) {
        // SAFETY: unsafe-site category 1 -- `write_msg`'s contract is this function's, and
        // the value published is a `'static` NUL-terminated string from `crate::util` that
        // outlives every caller.
        unsafe {
            write_msg(strm, Some(message));
        }
    }
}

// ---------------------------------------------------------------------------
// Initialisation -- `deflate.c` L379-L533
// ---------------------------------------------------------------------------

/// Initialises a stream for compression with the reference defaults -- `zlib.h` L1903.
///
/// The real function behind the `deflateInit` macro (`zlib.h` L232): the macro exists so that
/// the *caller's* `ZLIB_VERSION` and `sizeof(z_stream)` are captured at the caller's compile
/// time and checked against the library's here. `deflateInit` itself is therefore **not** an
/// exported symbol, and must not become one.
///
/// The four omitted parameters are supplied exactly as `deflate.c` L381-L382 supplies them:
/// `method` = `Z_DEFLATED` (8), `windowBits` = `MAX_WBITS` (15), `memLevel` = `DEF_MEM_LEVEL`
/// (8) and `strategy` = `Z_DEFAULT_STRATEGY` (0). That combination selects a zlib-wrapped
/// stream with a 32 KiB window, which is what every unqualified `deflateInit` caller expects.
///
/// # Parameters
///
/// * `strm` -- the caller's stream. `zalloc`, `zfree` and `opaque` must already be set, or all
///   three left `Z_NULL` for the library's own allocator.
/// * `level` -- `0 ..= 9`, or `Z_DEFAULT_COMPRESSION` (-1), which resolves to 6.
/// * `version` -- the caller's compile-time `ZLIB_VERSION`.
/// * `stream_size` -- the caller's compile-time `sizeof(z_stream)`.
///
/// # Returns
///
/// `Z_OK`; `Z_MEM_ERROR` if the compressor could not allocate; `Z_STREAM_ERROR` for an
/// invalid `level` or a null `strm`; `Z_VERSION_ERROR` for an incompatible header
/// (`zlib.h` L245-L250).
///
/// # Safety
///
/// `strm` must be null or address a caller-allocated [`z_stream`] -- at least
/// `size_of::<z_stream>()` bytes, aligned -- whose three allocator members are initialised and
/// which nothing else is concurrently accessing. `version` must be null or address at least
/// one readable byte. A null in either position is diagnosed and reported, never dereferenced.
///
/// Ported from `deflateInit_`, `deflate.c` L379-L383.
#[no_mangle]
pub unsafe extern "C" fn deflateInit_(
    strm: z_streamp,
    level: c_int,
    version: *const c_char,
    stream_size: c_int,
) -> c_int {
    // No guard of its own: the whole body is the delegation C performs at L380-L382, and
    // `deflateInit2_` applies `guard_code` to everything that can panic. Wrapping twice would
    // nest two landing pads around one call for no benefit.
    //
    // SAFETY: unsafe-site categories 1, 4 and 5, every one of them forwarded unchanged rather
    // than performed here. The obligations are this function's own, and they are exactly the
    // ones `deflateInit2_` states: `strm` is non-null, aligned and live, with its three
    // allocator members initialised (both hooks null, or both valid); and `version` is null or
    // the first byte of a readable NUL-terminated string that outlives the call. `deflateInit2_`
    // additionally validates the four configuration constants supplied here, which are
    // compile-time literals, so a caller satisfying this contract satisfies that one.
    unsafe {
        deflateInit2_(
            strm,
            level,
            Z_DEFLATED,
            MAX_WBITS,
            DEF_MEM_LEVEL,
            Z_DEFAULT_STRATEGY,
            version,
            stream_size,
        )
    }
}

/// Initialises a stream for compression with a full parameter set -- `zlib.h` L1907.
///
/// The real function behind the `deflateInit2` macro (`zlib.h` L543), which for the same
/// reason as `deflateInit` is not an exported symbol.
///
/// # The seven steps, in C's order
///
/// The order matters at three points, and each is called out below:
///
/// 1. The version and `stream_size` gate (L394-L397) -- **before** the null-stream test, per
///    `version_is_compatible`.
/// 2. `strm == Z_NULL` (L398).
/// 3. `strm->msg = Z_NULL` (L400) -- before anything can fail, so that a caller reading `msg`
///    after a rejection sees the library's answer rather than a stale one.
/// 4. The allocator (L401-L414). C substitutes `zcalloc`/`zcfree` for a null pair *in the
///    caller's structure*; this port leaves the caller's members exactly as they were and
///    keeps the substitution internal to `StreamAllocator`, for the reason
///    `deflate_block_mut` documents.
/// 5. Parameter validation and the state allocation (L419-L530), both in the core.
/// 6. Recording the state in `strm->state` (L445).
/// 7. `return deflateReset(strm)` (L532), whose effect on the caller's five stream members is
///    part of this function's observable contract.
///
/// ★ Step 7 is not optional and is the easiest thing in this module to leave out.
/// [`zlib_rs::deflate::deflate_init2`] performs the reset internally but discards the
/// [`DeflateReset`] it produces, because it has no `z_stream` to apply it to. If this function did
/// not publish those five values, a freshly initialised zlib stream would report `adler == 0`
/// instead of `1`, which `test/example.c` observes.
///
/// What it publishes them from is [`zlib_rs::deflate::deflate_reset_snapshot`], which computes them
/// and changes nothing. Calling `deflate_reset` a second time would also produce them and is sound --
/// the reset is idempotent on a just-initialised state -- but it would repeat `_tr_init` and the whole
/// of `lm_init` for values that are already in place, and `lm_init`'s `CLEAR_HASH` writes 64 KiB at the
/// default `memLevel` and 128 KiB at `memLevel 9`. Every `deflateInit_` and `deflateInit2_` would pay
/// that twice. The reset therefore runs exactly once, in the core, and this function only reads its
/// result.
///
/// ## ★ `windowBits` carries the container
///
/// Negative values select raw DEFLATE with no wrapper, `9 ..= 15` select the zlib wrapper, and
/// `25 ..= 31` (that is, `windowBits + 16`) select the gzip wrapper. The decoding, the
/// `windowBits == 8` promotion to 9 "until 256-byte window bug fixed" (L439) and the five-way
/// rejection at L434-L438 all belong to `zlib_rs::config`, which is why this function forwards
/// five integers rather than range-checking them itself.
///
/// # Returns
///
/// `Z_OK`; `Z_MEM_ERROR`; `Z_STREAM_ERROR` for any invalid parameter or a null `strm`;
/// `Z_VERSION_ERROR` for an incompatible header (`zlib.h` L610-L615).
///
/// # Safety
///
/// As [`deflateInit_`].
///
/// Ported from `deflateInit2_`, `deflate.c` L387-L533.
#[no_mangle]
pub unsafe extern "C" fn deflateInit2_(
    strm: z_streamp,
    level: c_int,
    method: c_int,
    windowBits: c_int,
    memLevel: c_int,
    strategy: c_int,
    version: *const c_char,
    stream_size: c_int,
) -> c_int {
    guard_code(|| {
        // 1. L394-L397, before every other test.
        //
        // SAFETY: unsafe-site categories 1 and 5 -- `version_is_compatible` reads at most one
        // byte of `version`, having tested it for null first, and this function's contract
        // makes that byte readable.
        if !unsafe { version_is_compatible(version, stream_size) } {
            return ReturnCode::VERSION_ERROR;
        }

        // 2. `if (strm == Z_NULL) return Z_STREAM_ERROR;` (L398). The alignment half has no C
        //    counterpart because C cannot express it; a misaligned `z_stream *` is a caller
        //    defect that would make every member access undefined, so it is refused here
        //    rather than acted upon.
        if strm.is_null() || !strm.is_aligned() {
            return fallback::STREAM_ERROR_CODE;
        }

        // 3 and 4. `strm->msg = Z_NULL;` (L400) and the hook substitution (L401-L414),
        //    which `adopt_hooks` performs together because C performs them together and in
        //    that order: a null `zalloc` becomes the library's own `zalloc` and takes the
        //    `opaque` clear with it, then a null `zfree` is decided separately, and all four
        //    members are written back so the caller sees the substitution `zlib.h` L151-L153
        //    promises. It happens BEFORE the parameter validation below, exactly as C's does,
        //    so even an init that fails on `windowBits` has filled the hooks in.
        //
        // SAFETY: unsafe-site categories 1 and 4 -- reading and writing four of the caller's
        // members through raw places, forming no reference, so `strm` stays usable for
        // `publish_state`. Non-null and aligned above, live by this function's contract. The
        // members written may hold indeterminate bytes beforehand -- they do for the
        // stack-allocated `z_stream` of `test/example.c` -- which is sound because they are
        // written, not read.
        let Some(allocator) = (unsafe { StreamAllocator::adopt_hooks(strm) }) else {
            // Unreachable: `strm` was established non-null and aligned two statements ago,
            // and the substitution itself cannot fail. Reported rather than asserted,
            // because an abort at an FFI boundary is a worse outcome than a status.
            return fallback::STREAM_ERROR_CODE;
        };

        // 5a. The five arguments as the caller wrote them. `method` and `strategy` are typed
        //     here and rejected if they name no documented value (L434-L437); the three
        //     integers are bounded by the core's own validation, inside `deflate_init2`.
        let config = match DeflateConfig::from_raw(level, method, windowBits, memLevel, strategy) {
            Ok(config) => config,
            Err(code) => return code,
        };

        // 5b. L419-L439: every bound on the three integer parameters, applied before anything
        //     is allocated. C validates first (L419-L438) and allocates second (L440), so a
        //     rejected parameter set must leave both the caller's `state` member and the
        //     caller's allocator untouched -- which means this function has to know the
        //     parameters are good *before* it asks for the state block. `core_deflate_init2`
        //     applies the same validation again inside; it is a pure function of `config`, so
        //     the second answer cannot disagree with the first, and `DeflateConfig` is `Copy`,
        //     so nothing is consumed by asking twice.
        if let Err(code) = config.validate() {
            // L438: `return Z_STREAM_ERROR;`, having touched nothing but `msg`. In particular
            // `strm->state` is left alone, so a caller that re-initialises a live stream with
            // an invalid parameter set keeps the stream it had.
            return code;
        }

        // 5c. `s = (deflate_state *) ZALLOC(strm, 1, sizeof(deflate_state));` (L440) -- the
        //     FIRST request this call makes of the caller's `zalloc`, ahead of the window,
        //     `prev`, `head` and `pending_buf` requests the core makes at L468-L479, and the
        //     LAST block `deflateEnd` gives back (L1300-L1306). Reserving it here rather than
        //     installing it at the end is what makes the pair of sequences a caller's hooks see
        //     strictly last-in-first-out, which `test/infcover.c`'s tracking allocator records
        //     and reports on.
        //
        // The block is uninitialised until `commit_state` below, and every recovery path
        // validates the tag and the owner first, so no intervening call can mistake it for a
        // usable state.
        let slot = match reserve_state::<DeflateSlot>(&allocator) {
            Ok(slot) => slot,
            // L443-L444: `if (s == Z_NULL) return Z_MEM_ERROR;` -- no message and no write to
            // `strm->state`, because C has not reached L445 either. A caller re-initialising a
            // live stream therefore still holds the state it had, exactly as in C.
            Err(code) => return code,
        };

        // 5d. `strm->state = (struct internal_state FAR *)s;` (L445), published here because C
        //     publishes it here -- before the buffers, not after them. A caller's `zalloc`,
        //     invoked for those buffers, therefore sees the same non-null `state` member it
        //     would see in C.
        //
        // SAFETY: unsafe-site categories 1 and 3 -- recording the reserved block's address in
        // one member of the caller's stream, through a raw place. `strm` is non-null and
        // aligned as established above and live by this function's contract; `slot` came from
        // `reserve_state` one statement ago. The member is written, not read, so its previous
        // contents need not have been initialised.
        unsafe {
            publish_state(strm, slot);
        }

        // 5e. L449-L530: the four buffers and every field assignment, then `deflateReset`.
        let state = match core_deflate_init2(config, allocator) {
            Ok(state) => state,
            Err(code) => {
                // L508-L513: C sets the out-of-memory message and calls `deflateEnd(strm)`,
                // whose effects here are `strm->state = Z_NULL` and the release of the state
                // object. The core has already returned every block it took, in `deflateEnd`'s
                // order, so the state block goes back *after* them -- which is C's order -- and
                // the member is cleared before the release, so it never dangles.
                //
                // SAFETY: unsafe-site categories 1, 3 and 4 -- two members written through raw
                // places on a non-null, aligned, live stream, both written rather than read;
                // then the reserved block returned to the allocator that produced it, having
                // been detached from the stream by the write above, never committed, and so
                // owing no destructor.
                unsafe {
                    if code == ReturnCode::MEM_ERROR {
                        write_msg(strm, Some(error_message(ReturnCode::MEM_ERROR.as_i32())));
                    }
                    write_state_null(strm);
                    discard_reserved_state(&allocator, slot);
                }
                return code;
            }
        };

        // 6. `s->strm = strm; s->status = INIT_STATE;` (L446-L447): the finished state moves
        //    into the block reserved above, with the C-visible prefix seeded so that the block
        //    passes its own owner-identity and tag checks from the moment it exists. C's
        //    comment at L447 gives the reason for setting the status this early -- "to pass
        //    state test in deflateReset()". The header pointer starts null because
        //    `s->gzhead = Z_NULL` (L494) does; only `deflateSetHeader` ever changes it.
        let tag = state.status().as_raw();

        // SAFETY: unsafe-site categories 3 and 4 -- initialising the state block. `slot` came
        // from `reserve_state` on `allocator`, which carries the caller's own triple, so the
        // block can only be released through the matching `zfree`; it has not been committed
        // before; and `state` is moved in, so the buffers it holds are now owned by the block
        // the stream points at.
        unsafe {
            commit_state(
                slot,
                &allocator,
                strm,
                StateKind::Deflate,
                tag,
                DeflateSlot {
                    state,
                    head: ptr::null_mut(),
                },
            );
        }

        // 7. `return deflateReset(strm);` (L532).
        //
        // SAFETY: unsafe-site categories 3 and 4 -- the helper's contract is this function's,
        // and the state it recovers is the one `commit_state` initialised in `strm->state` one
        // statement ago. That state was moved into the stream, so no borrow of it exists here,
        // and the `state` member is the only thing pointing at it.
        let Some(block) = (unsafe { deflate_block_mut(strm) }) else {
            // Unreachable: the block was installed one statement ago with a tag drawn from its
            // own status and an owner recorded as `strm`, so all four checks pass. Reported
            // rather than asserted, because an abort at an FFI boundary is a worse outcome than
            // a status a caller can act on.
            return fallback::STREAM_ERROR_CODE;
        };
        // The reset itself already happened, inside `deflate_init2`; this reads the five values it
        // left in the state. `sync_tag` still runs, and is a store of the value the tag already
        // holds: `tag` above was taken from `state.status()` *after* `deflate_init2` returned, so it
        // is already the post-reset status. Keeping the call is what makes "the tag mirrors the
        // status" true by construction at every point a reader looks, rather than by an argument
        // about which line ran first.
        let reset = core_deflate_reset_snapshot(&block.state().state);
        sync_tag(block);

        // SAFETY: unsafe-site category 1 -- four members written through raw places, plus
        // `msg`. Non-null, aligned and live as established above. The block borrow above is
        // over separate storage, so writing the stream cannot alias it.
        unsafe {
            write_reset(strm, reset);
        }

        ReturnCode::OK
    })
}

// ---------------------------------------------------------------------------
// Preset dictionaries -- `deflate.c` L559-L641
// ---------------------------------------------------------------------------

/// Primes the compressor's window and hash chains from a dictionary -- `zlib.h` L618.
///
/// The dictionary is compressed history the decompressor is expected to have too: it produces
/// no output of its own, and for a zlib stream its Adler-32 is folded into `strm->adler` so
/// that the decompressor can identify which dictionary is required
/// (`zlib.h` L648-L653).
///
/// ★ **`total_in` grows.** The window is loaded through the same `read_buf` path ordinary input
/// takes, and `deflate.c` L237's `strm->total_in += len` is unconditional, so a
/// `deflateSetDictionary(strm, "hello", 5)` leaves the stream reporting `total_in == 5`. That
/// was measured against the reference build. It is why this function writes two members back
/// rather than one.
///
/// C temporarily swaps `next_in`/`avail_in` to point at the dictionary and restores them
/// afterwards (L591-L619); here the dictionary gets its own cursor inside the core, so those
/// two members are never disturbed and there is nothing to restore.
///
/// # Parameters
///
/// * `dictionary` -- the history to install. Must not be null **even when `dictLength` is
///   zero**: `deflate.c` L567 rejects a null unconditionally, so an empty dictionary is
///   expressed as a non-null pointer with a zero length.
/// * `dictLength` -- its length. If it exceeds the window size the *tail* is kept, which is why
///   `zlib.h` L633-L646 tells callers to place the most useful strings last.
///
/// # Returns
///
/// `Z_OK`, or `Z_STREAM_ERROR` for a null `dictionary`, an inconsistent stream, a gzip stream,
/// a zlib stream on which `deflate` has already run, or a raw stream that is not at a block
/// boundary (`zlib.h` L655-L659).
///
/// # Safety
///
/// `strm` must be null or address a live [`z_stream`] holding a state this library installed,
/// with nothing else concurrently accessing it. `dictionary` must be null or readable for
/// `dictLength` bytes, and that region must not overlap the [`z_stream`]. A null in either
/// position is diagnosed and reported, never dereferenced.
///
/// Ported from `deflateSetDictionary`, `deflate.c` L559-L622.
#[no_mangle]
pub unsafe extern "C" fn deflateSetDictionary(
    strm: z_streamp,
    dictionary: *const Bytef,
    dictLength: c_uint,
) -> c_int {
    guard_code(|| {
        // `if (deflateStateCheck(strm) || dictionary == Z_NULL) return Z_STREAM_ERROR;` (L567)
        //
        // SAFETY: unsafe-site categories 3 and 4 -- recovering the state block, the
        // first thing this body does. Nothing is dereferenced before it is checked:
        // `deflate_block_mut` tests `strm` for null and alignment, the `state` member for null
        // and alignment, the owner back-pointer for identity and the tag for range, and
        // answers `None` -- the documented `Z_STREAM_ERROR` -- if any of them fails. What this
        // call site supplies is what those checks cannot: a non-null `strm` addresses a live
        // `z_stream` whose `state`, if non-null, is a `DeflateBlock` this library installed,
        // and whose `zalloc`/`zfree` pair is either both null or both valid.
        // The borrow is the only one taken for this call and ends when the body returns; the
        // block is a separate allocation from the `z_stream`, so the raw reads and writes of
        // the caller's members below cannot alias it.
        let Some(block) = (unsafe { deflate_block_mut(strm) }) else {
            return fallback::STREAM_ERROR_CODE;
        };
        if dictionary.is_null() {
            return fallback::STREAM_ERROR_CODE;
        }

        // `wrap == 2 || (wrap == 1 && s->status != INIT_STATE) || s->lookahead` (L570-L572).
        //
        // ★ **Applied here, before the dictionary becomes a slice, because that is where C
        // applies it.** C returns at L572 without `adler32` (L575) or `read_buf` (L237) having
        // read a single dictionary byte, so a caller whose stream is in the wrong state may
        // legitimately pass a stale or wild non-null pointer and still be told
        // `Z_STREAM_ERROR`. Reconstructing the slice first would turn that documented refusal
        // into undefined behaviour. The core applies the identical predicate, so the answer
        // cannot depend on which side asked.
        // No `sync_tag` on this path, matching the `dictionary == Z_NULL` return above and
        // C's own L572: nothing has changed the status, so there is nothing to publish.
        if !block.state().state.accepts_dictionary() {
            return fallback::STREAM_ERROR_CODE;
        }

        // SAFETY: unsafe-site category 1 -- four members read through raw places on a non-null,
        // aligned, live stream, so all four are in bounds and readable. They are also
        // initialised: a stream that owns a state has been through `deflateInit2_`, whose
        // closing reset assigns all four.

        let scalars = unsafe { StreamScalars::read(strm) };

        // SAFETY: unsafe-site category 2 -- slice reconstruction, once. `dictionary` is
        // non-null by the test above and trivially aligned for `u8`; the helper additionally
        // branches on a zero length, so a zero-length dictionary yields an empty slice. This
        // function's contract makes the bytes readable and stable, and nothing writes through
        // the shared borrow.
        let dictionary = unsafe { input_slice(dictionary, dictLength) };

        // The two `z_stream` members C's dictionary load touches, and only those (L575 and,
        // through `read_buf`, L237).
        let mut check = check_of(scalars.adler);
        let mut total_in = widen_uLong(scalars.total_in);
        let code = core_deflate_set_dictionary(
            &mut block.state_mut().state,
            &mut check,
            &mut total_in,
            dictionary,
        );
        sync_tag(block);

        // SAFETY: unsafe-site category 1 -- two members written through raw places, on a
        // non-null, aligned, live stream. Written rather than read; no `&mut z_stream` is
        // materialised, so the block borrow above is unaffected. On the rejection paths the core
        // leaves both values untouched, so this writes back exactly what it read, as C does by
        // returning before it assigns anything.
        unsafe {
            ptr::addr_of_mut!((*strm).adler).write(adler_of(check));
            ptr::addr_of_mut!((*strm).total_in).write(narrow_uLong(total_in));
        }

        code
    })
}

/// Reads back the sliding dictionary the compressor is maintaining -- `zlib.h` L662.
///
/// `len` is `min(strstart + lookahead, w_size)` (`deflate.c` L633-L635) and those bytes are the
/// window's newest history. Both out-parameters are independently optional, exactly as C's two
/// `Z_NULL` tests at L636 and L638 make them: a caller may ask only how much history there is.
///
/// ## ★ Why the core is consulted twice
///
/// C's `dictionary` parameter carries **no length** -- `zlib.h` L666-L671 requires the caller to
/// provide at least 32768 bytes and copies `len` of them with no bound of its own. A Rust slice
/// cannot be built without a length, so this function first asks the core for `len` with no
/// destination at all, then builds a slice of exactly that many bytes and asks again for the
/// copy. Both calls take a shared borrow of the state and neither has a side effect, so the
/// second returns the same `len` as the first, and the caller's buffer receives exactly the
/// `len` bytes C would have written -- no fewer, and never one more.
///
/// # Returns
///
/// `Z_OK`, or `Z_STREAM_ERROR` if the stream state is inconsistent (`zlib.h` L679-L680).
///
/// # Safety
///
/// `strm` must be null or address a live [`z_stream`] holding a state this library installed.
/// `dictionary` must be null or writable for at least the length this function reports through
/// `dictLength`, which never exceeds the stream's window size and hence never exceeds 32768.
/// `dictLength` must be null or a valid, aligned, writable `uInt`. Neither region may overlap
/// the other or the [`z_stream`].
///
/// Ported from `deflateGetDictionary`, `deflate.c` L625-L641.
#[no_mangle]
pub unsafe extern "C" fn deflateGetDictionary(
    strm: z_streamp,
    dictionary: *mut Bytef,
    dictLength: *mut c_uint,
) -> c_int {
    guard_code(|| {
        // `if (deflateStateCheck(strm)) return Z_STREAM_ERROR;` (L630)
        //
        // SAFETY: unsafe-site categories 3 and 4 -- recovering the state block for reading, the
        // first thing this body does. Nothing is dereferenced before it is checked:
        // `deflate_block_ref` tests `strm` for null and alignment, the `state` member for null
        // and alignment, the owner back-pointer for identity and the tag for range, and
        // answers `None` -- the documented `Z_STREAM_ERROR` -- if any of them fails. What this
        // call site supplies is what those checks cannot: a non-null `strm` addresses a live
        // `z_stream` whose `state`, if non-null, is a `DeflateBlock` this library installed,
        // and whose `zalloc`/`zfree` pair is either both null or both valid.
        // The borrow is shared, no mutable borrow of the block is taken anywhere in this call,
        // and it ends when the body returns; the block is a separate allocation from the
        // caller's dictionary buffer, so the copy below cannot alias it.
        let Some(block) = (unsafe { deflate_block_ref(strm) }) else {
            return fallback::STREAM_ERROR_CODE;
        };
        let state = &block.state().state;

        // `len = s->strstart + s->lookahead; if (len > s->w_size) len = s->w_size;`
        // (L633-L635), asked for on its own. `dict_length` is `Some` and `dictionary` is
        // `None`, which is the "only the length is returned" shape of L636-L639.
        let mut len: c_uint = 0;
        let probe = core_deflate_get_dictionary(state, None, Some(&mut len));
        if probe != ReturnCode::OK {
            // The core documents no failure once a valid state is in hand; the status is
            // forwarded rather than assumed so that this function can never invent a `Z_OK`.
            return probe;
        }

        // `if (dictionary != Z_NULL && len)` (L636). Both conditions are C's, in C's order.
        if !dictionary.is_null() && len != 0 {
            // SAFETY: unsafe-site category 2 -- slice reconstruction, once, for the one
            // destination this function writes. `dictionary` is non-null by the test above and
            // trivially aligned for `u8`; `len` is the count the core just reported, which is
            // at most the window size, and this function's contract makes that many bytes
            // writable, unaliased and stable. This is the only mutable slice over the region.
            let mut target = unsafe { output_region(dictionary, len) };
            let code = core_deflate_get_dictionary(state, Some(&mut target), None);
            if code != ReturnCode::OK {
                return code;
            }
        }

        // `if (dictLength != Z_NULL) *dictLength = len;` (L638-L639) -- after the copy, as C
        // orders it, and unconditionally on the value from the probe so that a caller which
        // passed a null `dictionary` still learns the length.
        if !dictLength.is_null() {
            // SAFETY: unsafe-site category 1 -- writing the caller's out-parameter. Non-null by
            // the test above, and a valid, aligned, writable `uInt` by this function's
            // contract. Written rather than read, so its previous contents may be
            // indeterminate.
            unsafe {
                dictLength.write(len);
            }
        }

        ReturnCode::OK
    })
}

// ---------------------------------------------------------------------------
// Resets -- `deflate.c` L644-L711
// ---------------------------------------------------------------------------

/// Resets the stream but keeps the window's contents and the hash chains -- `zlib.h` L2040.
///
/// One of the five functions `zlib.h` groups under "various hacks, don't look :)" (L1899) and
/// documents nowhere else, but a real exported symbol nonetheless -- it belongs to the
/// `ZLIB_1.2.5.2` version node. Its use is to start a fresh deflate stream that may still match
/// against the previous one's history, which is what `gzsetparams` needs
/// (`gzlib.c` L294-L327).
///
/// The five caller-visible effects are `total_in` and `total_out` to zero, `msg` to `Z_NULL`,
/// `data_type` to `Z_UNKNOWN`, and -- the easily missed one -- `adler` to the container's
/// initial check value, which is `1` for zlib and raw and `0` for gzip (`deflate.c` L667-L671).
///
/// # Returns
///
/// `Z_OK`, or `Z_STREAM_ERROR` if the stream state is inconsistent.
///
/// # Safety
///
/// `strm` must be null or address a live [`z_stream`] holding a state this library installed,
/// with nothing else concurrently accessing it.
///
/// Ported from `deflateResetKeep`, `deflate.c` L644-L677.
#[no_mangle]
pub unsafe extern "C" fn deflateResetKeep(strm: z_streamp) -> c_int {
    guard_code(|| {
        // `if (deflateStateCheck(strm)) return Z_STREAM_ERROR;` (L647-L649)
        //
        // SAFETY: unsafe-site categories 3 and 4 -- recovering the state block, the
        // first thing this body does. Nothing is dereferenced before it is checked:
        // `deflate_block_mut` tests `strm` for null and alignment, the `state` member for null
        // and alignment, the owner back-pointer for identity and the tag for range, and
        // answers `None` -- the documented `Z_STREAM_ERROR` -- if any of them fails. What this
        // call site supplies is what those checks cannot: a non-null `strm` addresses a live
        // `z_stream` whose `state`, if non-null, is a `DeflateBlock` this library installed,
        // and whose `zalloc`/`zfree` pair is either both null or both valid.
        // The borrow is the only one taken for this call, and the block is a separate
        // allocation from the `z_stream`, so `write_reset`'s raw writes below cannot alias it.
        let Some(block) = (unsafe { deflate_block_mut(strm) }) else {
            return fallback::STREAM_ERROR_CODE;
        };

        let reset = core_deflate_reset_keep(&mut block.state_mut().state);
        // The status changes here -- to `INIT_STATE` or `GZIP_STATE` per L662-L666 -- so the
        // C-visible tag must follow it.
        sync_tag(block);

        // SAFETY: unsafe-site category 1 -- five members written through raw places on a
        // non-null, aligned, live stream. All five are written rather than read.
        unsafe {
            write_reset(strm, reset);
        }

        ReturnCode::OK
    })
}

/// Resets the stream completely, as `deflateEnd` followed by `deflateInit` would -- `zlib.h`
/// L702.
///
/// [`deflateResetKeep`] plus `lm_init` (`deflate.c` L682-L701), which clears the hash chains and
/// reloads the four tuning parameters from the level's `configuration_table` entry. The
/// compression level, strategy, `windowBits` and `memLevel` are all left as they were, and no
/// memory is freed or reallocated -- which is the whole point of preferring it to an
/// end-and-reinitialise pair.
///
/// # Returns
///
/// `Z_OK`, or `Z_STREAM_ERROR` if the stream state is inconsistent (`zlib.h` L708-L709).
///
/// # Safety
///
/// As [`deflateResetKeep`].
///
/// Ported from `deflateReset`, `deflate.c` L704-L711.
#[no_mangle]
pub unsafe extern "C" fn deflateReset(strm: z_streamp) -> c_int {
    guard_code(|| {
        // C reaches the state check through its call to `deflateResetKeep` (L708) and returns
        // its status unchanged; performing it here directly is the same test in the same place.
        //
        // SAFETY: unsafe-site categories 3 and 4 -- recovering the state block, the
        // first thing this body does. Nothing is dereferenced before it is checked:
        // `deflate_block_mut` tests `strm` for null and alignment, the `state` member for null
        // and alignment, the owner back-pointer for identity and the tag for range, and
        // answers `None` -- the documented `Z_STREAM_ERROR` -- if any of them fails. What this
        // call site supplies is what those checks cannot: a non-null `strm` addresses a live
        // `z_stream` whose `state`, if non-null, is a `DeflateBlock` this library installed,
        // and whose `zalloc`/`zfree` pair is either both null or both valid.
        // The borrow is the only one taken for this call, and the block is a separate
        // allocation from the `z_stream`, so `write_reset`'s raw writes below cannot alias it.
        let Some(block) = (unsafe { deflate_block_mut(strm) }) else {
            return fallback::STREAM_ERROR_CODE;
        };

        let reset = core_deflate_reset(&mut block.state_mut().state);
        sync_tag(block);

        // SAFETY: unsafe-site category 1 -- `write_reset` writes five members of the caller's
        // stream, and its contract is discharged by this function's: `strm` is non-null, aligned
        // and live, so all five are in bounds and writable, and all five are written rather than
        // read, so none of them needs to have been initialised. The writes go through raw
        // places, so no `&mut z_stream` is formed and the caller's pointer keeps its provenance;
        // and the state block borrowed above is a separate allocation, so they cannot alias it.
        unsafe {
            write_reset(strm, reset);
        }

        ReturnCode::OK
    })
}

// ---------------------------------------------------------------------------
// The gzip header -- `deflate.c` L715-L720, AAP §0.6.1 unsafe-site category 6
// ---------------------------------------------------------------------------

/// The low 32 bits of a caller's `gz_header.time`, which is all RFC 1952 transmits.
///
/// The field is a `uLong` (`zlib.h` L120) but the gzip header's `MTIME` is four bytes, and
/// `deflate.c` L1098-L1101 emits exactly those four in little-endian order. Truncating here
/// therefore discards only bits the reference discards too.
// The truncation is the operation, as documented above; see `Z_STREAM_SIZE` on why the reason
// is a comment rather than the attribute's `reason` field. `trivial_numeric_casts` and
// `unnecessary_cast` are allowed for the reason `check_of` gives: `uLong` is `u32` on i686 and
// on LLP64 Windows, where this is `u32 as u32`.
#[inline]
#[must_use]
#[allow(
    trivial_numeric_casts,
    clippy::unnecessary_cast,
    clippy::cast_possible_truncation
)]
const fn mtime_of(time: uLong) -> u32 {
    time as u32
}

/// Borrows a NUL-terminated `gz_header` field, terminator included.
///
/// `name` and `comment` are C strings, and the reference writes them with a loop that stops
/// *after* emitting the zero byte (`deflate.c` L1158-L1160 and L1180-L1182), because RFC 1952
/// requires the terminator to be part of the stream.
/// [`zlib_rs::deflate::state::GzHeaderView`] therefore contracts these two slices to **include**
/// the terminator, so a C string of `n` visible bytes becomes a slice of `n + 1`. A slice that
/// omitted it would silently produce a gzip header with no field separator.
///
/// # ★ Why the result is a `Cell` slice and not a `&[u8]`
///
/// The bytes belong to the **caller**, and the compressor reads them during a later
/// `deflate()` call rather than now -- `deflateSetHeader` only records the pointer
/// (`deflate.c` L717) and the `FNAME`/`FCOMMENT` states copy from it at L1158 and L1180. So
/// there is a window, spanning one or more entry-point calls, in which C code owns storage
/// that this library is holding a borrow of. A `&[u8]` asserts that nothing writes those bytes
/// for the borrow's whole life, and a C caller that edits its own buffer would break that
/// assertion and make the program undefined -- silently, since nothing dereferences a stale
/// value. `&[Cell<u8>]` makes no such promise: it is the shared-mutable borrow, so a
/// concurrent write from C is *permitted* rather than undefined, and the compressor reads each
/// byte through [`core::cell::Cell::get`] at the moment it needs it, which is exactly when C
/// reads it.
///
/// # ★ Why the length is measured with raw reads rather than through `CStr`
///
/// `CStr::from_ptr` would perform the identical scan, but it yields a `&CStr` -- a *shared,
/// immutable* reference -- and a `Cell` slice derived from that reference inherits its
/// read-only permission. Writing through it would then be undefined behaviour even though the
/// pointer the caller supplied is perfectly writable, which is precisely the promise this
/// function exists to avoid making. Miri rejects the derivation outright. Scanning through raw
/// reads creates no reference at all, so the returned slice is derived from the caller's own
/// pointer and carries the caller's own permission -- and the scan is then a literal
/// transcription of C's `while (*str++)`.
///
/// # Safety
///
/// `text` must be non-null and address a NUL-terminated string that stays alive until the
/// header has been written -- which is to say, until deflation reaches the end of the gzip
/// header. `zlib.h` L838-L845 places exactly that obligation on the caller. The bytes may be
/// *written* by the caller during that time; they may not be freed or unmapped.
#[must_use]
unsafe fn field_with_nul(text: *mut Bytef) -> &'static [Cell<u8>] {
    let mut len: usize = 0;
    loop {
        // SAFETY: unsafe-site categories 5 and 6 -- scanning a NUL-terminated field of the
        // caller's `gz_header` for its terminator, one byte at a time, exactly as
        // `deflate.c` L1158-L1160 emits it. `text` is non-null by this function's contract and
        // trivially aligned for `u8`; that contract makes every byte up to and including the
        // terminator readable, so each offset reached here is in bounds -- the loop stops at
        // the first zero and a field with no terminator is already undefined in C. Reading
        // through a raw place forms no reference, so the caller's provenance is preserved for
        // the shared-mutable view built below.
        if unsafe { text.add(len).read() } == 0 {
            break;
        }
        // `saturating_add` rather than `+`: a wrap is unreachable, because reaching
        // `usize::MAX` would have required reading the whole address space, but the library
        // may not contain an operation that could panic (AAP §0.7.1 (f)).
        len = len.saturating_add(1);
    }
    // The terminator is part of the field, so the slice is one byte longer than the scan.
    let len = len.saturating_add(1);

    // SAFETY: unsafe-site categories 2 and 6 -- viewing the caller's field as shared-mutable.
    // `Cell<u8>` is `#[repr(transparent)]` over `u8`, so the two have identical size, alignment
    // and validity; `text` is non-null and trivially aligned, and the scan above established
    // that `len` bytes -- the string and its terminator -- are readable. The region stays live
    // for the borrow by this function's contract, and the pointer is the caller's own rather
    // than one derived from a shared reference, so writing through it from C is defined.
    // The `'static` lifetime is the FFI statement that the extent is the caller's
    // responsibility, which `zlib.h` L838-L845 assigns to it explicitly.
    unsafe { core::slice::from_raw_parts(text.cast::<Cell<u8>>(), len) }
}

/// Re-views a counted region of the caller's `gz_header` as a shared-mutable byte slice.
///
/// The counted-field counterpart of [`field_with_nul`], for `extra`, whose length comes from
/// `extra_len` rather than from a terminator. See that function for why the element type is
/// [`Cell`] and not `u8`.
///
/// A zero length yields an empty slice without calling [`core::slice::from_raw_parts`], so a
/// non-null pointer with `extra_len == 0` -- which `deflate.c` L1094 still counts as a
/// *present* field, setting the `FEXTRA` bit -- is represented faithfully rather than as
/// `Z_NULL`.
///
/// # Safety
///
/// `extra` must be non-null and valid for reads of `len` bytes, and that region must stay live
/// until the gzip header has been written. As with [`field_with_nul`], the caller may write
/// those bytes during that time.
#[must_use]
unsafe fn counted_field(extra: *mut Bytef, len: usize) -> &'static [Cell<u8>] {
    if len == 0 {
        return &[];
    }
    // SAFETY: unsafe-site categories 2 and 6 -- slice reconstruction over a counted field of
    // the caller's `gz_header`. `extra` is non-null by this function's contract and trivially
    // aligned for `u8`, hence for the `#[repr(transparent)]` `Cell<u8>`; `len` bytes are
    // readable and stay live for the borrow by that same contract, and the zero case was
    // branched out above so `from_raw_parts` never sees a null pointer.
    unsafe { core::slice::from_raw_parts(extra.cast::<Cell<u8>>(), len) }
}

/// Converts a caller's `gz_header` into the borrowed view the core reads it through.
///
/// Only the members the compressor reads are taken, and each only when it is valid. `xflags` is
/// *computed* from the level and strategy rather than read (`deflate.c` L1102-L1104), and
/// `extra_max`, `name_max`, `comm_max` and `done` are documented as being for *reading* a header
/// rather than writing one (`zlib.h` L125-L132) -- so a caller writing a gzip stream need never
/// have initialised any of the five, and reading them would be reading indeterminate memory.
/// `extra_len` is subject to the same rule for the same reason -- `zlib.h` L124 makes it valid
/// only when `extra` is non-null -- so it is read inside that branch rather than with the
/// unconditional scalars.
///
/// ★ **The view lasts one call, not one stream.** [`with_header`] builds it from the caller's
/// stored pointer on entry to each `deflate` or `deflateBound` and drops it before returning, so
/// the four scalars -- `text`, `time`, `os`, `hcrc` -- and the three field lengths are re-read
/// every time, which is what makes an edit between calls visible exactly as it is in C. The field
/// *contents* are not read here at all: the three slices borrow the caller's buffers and the
/// bytes are read during emission, as C reads them. `zlib.h` L838-L855 requires the `gz_header`
/// and every buffer it points at to stay alive and unmodified until the header has been written,
/// which bounds the one window in which this view exists.
///
/// # Safety
///
/// `head` must be non-null, aligned, and address a live [`crate::types::gz_header`] whose seven
/// unconditional members below are initialised. If `extra` is non-null, `extra_len` must be
/// initialised too and `extra` must be readable for that many bytes; if `name` or `comment` is
/// non-null it must be NUL-terminated. All of it must stay
/// alive and unmodified until the gzip header has been written.
#[must_use]
unsafe fn borrow_gz_header(head: gz_headerp) -> GzHeaderView<'static> {
    // SAFETY: unsafe-site category 6 -- reading seven members of the caller's `gz_header`.
    // `head` is non-null, aligned and live in those seven members by this function's contract,
    // so each is in bounds and readable. `addr_of!` plus `read` forms raw places rather than a
    // `&gz_header`, which is what keeps the five members a writer need not initialise -- and
    // the tail padding -- entirely untouched. Nothing is written.
    let (text, time, os, extra, name, comment, hcrc) = unsafe {
        (
            ptr::addr_of!((*head).text).read(),
            ptr::addr_of!((*head).time).read(),
            ptr::addr_of!((*head).os).read(),
            ptr::addr_of!((*head).extra).read(),
            ptr::addr_of!((*head).name).read(),
            ptr::addr_of!((*head).comment).read(),
            ptr::addr_of!((*head).hcrc).read(),
        )
    };

    // `s->gzhead->extra == Z_NULL` clears bit 2 of the gzip `FLG` byte (`deflate.c` L1094), so
    // a null is `None` rather than an empty field. The slice length is the caller's `extra_len`
    // verbatim; the 16-bit clamp the header's two length bytes impose is
    // `GzHeaderView::extra_len`'s, not this function's.
    let extra = if extra.is_null() {
        None
    } else {
        // ★ `extra_len` is read **here**, inside the non-null branch, and not with the scalars
        // above. `zlib.h` L124 spells out why: "extra field length (valid if extra != Z_NULL)",
        // and `deflate.c` reads it only inside its own `s->gzhead->extra != Z_NULL` branch
        // (L1094, L1102). A caller writing a header with no extra field has no reason to have
        // set it, so reading it unconditionally would read indeterminate memory -- the same
        // rule `inflateGetHeader` follows for `extra_max`, `name_max` and `comm_max`.
        //
        // SAFETY: unsafe-site category 6 -- one `uInt` member read through a raw place on the
        // non-null, aligned, live structure this function's contract establishes. Reading it
        // forms no reference to the header, so the caller's pointer stays usable for the two
        // reads below.
        let extra_len = unsafe { ptr::addr_of!((*head).extra_len).read() };
        // SAFETY: unsafe-site categories 2 and 6 -- `counted_field`'s contract is this
        // function's. `extra` is non-null by the test above, trivially aligned for `u8`, and
        // readable for `extra_len` bytes by that same contract; the helper additionally
        // branches on a zero length. The view is a `Cell` slice rather than a plain one
        // because `zlib.h` L838-L855 lets the caller keep the buffer live while the header is
        // emitted, so a concurrent write from C must stay defined; nothing on this side writes
        // through it.
        Some(unsafe { counted_field(extra, widen(extra_len)) })
    };

    // The same rule for the two NUL-terminated fields, which clear bits 3 and 4 of `FLG`
    // (L1095-L1096) when absent.
    let name = if name.is_null() {
        None
    } else {
        // SAFETY: unsafe-site categories 5 and 6 -- `field_with_nul`'s contract is this
        // function's, and `name` is non-null by the test above.
        Some(unsafe { field_with_nul(name) })
    };
    let comment = if comment.is_null() {
        None
    } else {
        // SAFETY: unsafe-site categories 5 and 6 -- reading a NUL-terminated field of the
        // caller's `gz_header`. `comment` is non-null by the test above and trivially aligned
        // for `c_char`, and this function's contract makes the bytes up to and including the
        // terminator readable and stable for as long as the returned view is held. Nothing is
        // written through the pointer, and the `'static` lifetime `field_with_nul` hands back is
        // the FFI statement that the extent is the caller's responsibility -- which `zlib.h`
        // L838-L845 assigns to them explicitly.
        Some(unsafe { field_with_nul(comment) })
    };

    GzHeaderView::new(
        // C tests `s->gzhead->text ? 1 : 0` when building `FLG` (L1092), which is exactly the
        // `int`-to-`bool` conversion here.
        text != 0,
        mtime_of(time),
        os,
        hcrc != 0,
        extra,
        name,
        comment,
    )
}

/// Installs the gzip header the compressor will emit, or clears it -- `zlib.h` L833.
///
/// The whole of C's effect is `strm->state->gzhead = head;` (`deflate.c` L717). A null `head`
/// reproduces `deflateSetHeader(strm, Z_NULL)`, which C accepts and which makes the compressor
/// emit the ten-byte default header instead.
///
/// # ★ The caller owns the header, and must keep it alive
///
/// C stores the **pointer** and reads through it much later, while `deflate()` is emitting the
/// header. `zlib.h` L838-L855 therefore requires that the `gz_header` and its `extra`, `name`
/// and `comment` buffers stay alive until the header has been written, and that `name` and
/// `comment` be NUL-terminated and `extra` be readable for `extra_len` bytes. This port keeps
/// that contract unchanged: [`borrow_gz_header`] captures the four scalars and the three
/// lengths, and borrows the three buffers as `Cell` slices, so the field contents are still read
/// late and a caller that edits them in between is no worse defined than under C. Freeing or
/// shortening any of the three before deflation reaches the end of the gzip header is undefined
/// behaviour here for the same reason it is in C; freeing them *after* that point is legal, and
/// [`GzHeaderView::release_fields`] is what makes it so.
///
/// The struct is not copied, because C does not copy it and because copying it would change
/// when a caller's edits stop being visible -- a difference no test could justify.
///
/// # Returns
///
/// `Z_OK`, or `Z_STREAM_ERROR` if the stream state is inconsistent **or the stream is not a gzip
/// stream** -- that is, unless `deflateInit2_` was given a `windowBits` above 15
/// (`deflate.c` L715-L716, `zlib.h` L862-L864).
///
/// # Safety
///
/// `strm` must be null or address a live [`z_stream`] holding a state this library installed.
/// `head` must be null or satisfy `borrow_gz_header`'s contract, including the lifetime
/// obligation described above. A null in either position is diagnosed and reported, never
/// dereferenced.
///
/// Ported from `deflateSetHeader`, `deflate.c` L715-L720.
#[no_mangle]
pub unsafe extern "C" fn deflateSetHeader(strm: z_streamp, head: gz_headerp) -> c_int {
    guard_code(|| {
        // `if (deflateStateCheck(strm) || strm->state->wrap != 2) return Z_STREAM_ERROR;`
        // (L715-L716). The state half is here; the `wrap` half is the core's.
        //
        // SAFETY: unsafe-site categories 3 and 4 -- recovering the state block, the
        // first thing this body does. Nothing is dereferenced before it is checked:
        // `deflate_block_mut` tests `strm` for null and alignment, the `state` member for null
        // and alignment, the owner back-pointer for identity and the tag for range, and
        // answers `None` -- the documented `Z_STREAM_ERROR` -- if any of them fails. What this
        // call site supplies is what those checks cannot: a non-null `strm` addresses a live
        // `z_stream` whose `state`, if non-null, is a `DeflateBlock` this library installed,
        // and whose `zalloc`/`zfree` pair is either both null or both valid.
        // The borrow is the only one taken for this call and ends when the body returns; the
        // block is a separate allocation from the caller's `gz_header`, so the header read
        // below cannot alias it.
        let Some(block) = (unsafe { deflate_block_mut(strm) }) else {
            return fallback::STREAM_ERROR_CODE;
        };

        // The `wrap` half of the guard, and the core's own record cleared with it: between
        // calls the core holds no header at all, because `with_header` installs one for the
        // duration of each `deflate` or `deflateBound` and takes it away again.
        let outcome = core_deflate_set_header(&mut block.state_mut().state, None);
        if outcome != ReturnCode::OK {
            return outcome;
        }

        // `strm->state->gzhead = head;` (L717) -- the POINTER, exactly as C stores it, null
        // included. Nothing is read through it here: a caller is entitled to fill the
        // structure in after this call, and `deflate` reads whatever is there at the time.
        block.state_mut().head = head;
        outcome
    })
}

// ---------------------------------------------------------------------------
// Pending output and bit-level insertion -- `deflate.c` L722-L771
// ---------------------------------------------------------------------------

/// Reports output that has been generated but not yet handed to the caller -- `zlib.h` L786.
///
/// The bytes are output the compressor holds because the caller's buffer filled; the bits are
/// the 0 to 7 that await company to complete a byte (`zlib.h` L790-L795).
///
/// Both out-parameters are optional, and the order below is C's: `*bits` is written **first**
/// (L724-L725), then `*pending` (L726-L731). That ordering is observable in the one failing
/// case, because a caller that supplied both still receives a valid bit count alongside the
/// error.
///
/// ★ The `Z_BUF_ERROR` is reported **only when `pending` is non-null**. C's overflow probe lives
/// inside `if (pending != Z_NULL)`, so a caller that asked only for the bit count gets `Z_OK`
/// however large the byte count is. `zlib.h` L798-L801 documents the condition: "if an int is 16
/// bits and memLevel is 9, then it is possible for the number of pending bytes to not fit in an
/// unsigned", and then `*pending` is set to the maximum value of an `unsigned` as well.
///
/// # Returns
///
/// `Z_OK`; `Z_STREAM_ERROR` if the stream state is inconsistent; `Z_BUF_ERROR` in the narrow
/// case above.
///
/// # Safety
///
/// `strm` must be null or address a live [`z_stream`] holding a state this library installed.
/// `pending` must be null or a valid, aligned, writable `unsigned`, and `bits` null or a valid,
/// aligned, writable `int`; neither may overlap the other or the [`z_stream`].
///
/// Ported from `deflatePending`, `deflate.c` L722-L734.
#[no_mangle]
pub unsafe extern "C" fn deflatePending(
    strm: z_streamp,
    pending: *mut c_uint,
    bits: *mut c_int,
) -> c_int {
    guard_code(|| {
        // `if (deflateStateCheck(strm)) return Z_STREAM_ERROR;` (L723)
        //
        // SAFETY: unsafe-site categories 3 and 4 -- recovering the state block for reading, the
        // first thing this body does. Nothing is dereferenced before it is checked:
        // `deflate_block_ref` tests `strm` for null and alignment, the `state` member for null
        // and alignment, the owner back-pointer for identity and the tag for range, and
        // answers `None` -- the documented `Z_STREAM_ERROR` -- if any of them fails. What this
        // call site supplies is what those checks cannot: a non-null `strm` addresses a live
        // `z_stream` whose `state`, if non-null, is a `DeflateBlock` this library installed,
        // and whose `zalloc`/`zfree` pair is either both null or both valid.
        // The borrow is shared, it is the only one taken for this call, and it ends when the
        // body returns; the block is a separate allocation from the two out-parameters, so
        // the writes through them below cannot alias it.
        let Some(block) = (unsafe { deflate_block_ref(strm) }) else {
            return fallback::STREAM_ERROR_CODE;
        };

        let (bytes, bit_count, status) = core_deflate_pending(&block.state().state);

        // `if (bits != Z_NULL) *bits = strm->state->bi_valid;` (L724-L725), first.
        if !bits.is_null() {
            // SAFETY: unsafe-site category 1 -- writing the caller's out-parameter. Non-null by
            // the test above, and a valid, aligned, writable `int` by this function's contract.
            // Written rather than read.
            unsafe {
                bits.write(bit_count);
            }
        }

        // `if (pending != Z_NULL) { ... }` (L726-L732). Both the write and the status live
        // inside the test, which is why an absent `pending` cannot produce `Z_BUF_ERROR`.
        if pending.is_null() {
            return ReturnCode::OK;
        }

        // SAFETY: unsafe-site category 1 -- writing the caller's out-parameter. `pending` is
        // non-null by the test above, and a valid, aligned, writable `unsigned` by this
        // function's contract; it is written rather than read, so its previous contents may be
        // indeterminate. The core has already narrowed the count to the `unsigned` this pointer
        // addresses, and reports through `status` whether that narrowing lost information.
        unsafe {
            pending.write(bytes);
        }

        status
    })
}

/// Reports how many bits of the last byte were used at the most recent byte-boundary flush --
/// `zlib.h` L804.
///
/// The value is `bi_used`, which `bi_windup` maintains as `((s->bi_valid - 1) & 7) + 1`
/// (`trees.c` L187). `zlib.h` L807-L810 gives the range as "1..8, or 0 if there has not yet been
/// a flush" and the purpose: it "helps determine the location of the last bit of a deflate
/// stream", which is what a caller needs before resuming with [`deflatePrime`].
///
/// `bits` is optional, exactly as C's `Z_NULL` test at L739 makes it.
///
/// # Returns
///
/// `Z_OK`, or `Z_STREAM_ERROR` if the stream state is inconsistent.
///
/// # Safety
///
/// `strm` must be null or address a live [`z_stream`] holding a state this library installed.
/// `bits` must be null or a valid, aligned, writable `int` that does not overlap the
/// [`z_stream`].
///
/// Ported from `deflateUsed`, `deflate.c` L737-L742.
#[no_mangle]
pub unsafe extern "C" fn deflateUsed(strm: z_streamp, bits: *mut c_int) -> c_int {
    guard_code(|| {
        // `if (deflateStateCheck(strm)) return Z_STREAM_ERROR;` (L738)
        //
        // SAFETY: unsafe-site categories 3 and 4 -- recovering the state block for reading, the
        // first thing this body does. Nothing is dereferenced before it is checked:
        // `deflate_block_ref` tests `strm` for null and alignment, the `state` member for null
        // and alignment, the owner back-pointer for identity and the tag for range, and
        // answers `None` -- the documented `Z_STREAM_ERROR` -- if any of them fails. What this
        // call site supplies is what those checks cannot: a non-null `strm` addresses a live
        // `z_stream` whose `state`, if non-null, is a `DeflateBlock` this library installed,
        // and whose `zalloc`/`zfree` pair is either both null or both valid.
        // The borrow is shared, it is the only one taken for this call, and it ends when the
        // body returns; the block is a separate allocation from `bits`, so the write through
        // it below cannot alias it.
        let Some(block) = (unsafe { deflate_block_ref(strm) }) else {
            return fallback::STREAM_ERROR_CODE;
        };

        // `if (bits != Z_NULL) *bits = strm->state->bi_used;` (L739-L740)
        if !bits.is_null() {
            let bit_count = core_deflate_used(&block.state().state);
            // SAFETY: unsafe-site category 1 -- writing the caller's out-parameter. Non-null by
            // the test above, and a valid, aligned, writable `int` by this function's contract.
            unsafe {
                bits.write(bit_count);
            }
        }

        ReturnCode::OK
    })
}

/// Inserts up to sixteen bits directly into the output bit stream -- `zlib.h` L816.
///
/// Its purpose, per `zlib.h` L820-L826, is "to start off the deflate output with the bits
/// leftover from a previous deflate stream when appending to it", so it applies to raw deflate
/// and must be used before the first `deflate` call after an initialisation or a reset.
///
/// Both arguments pass through untouched: the `0 ..= 16` range test and the pending-buffer room
/// test are the core's, together forming C's single guard at `deflate.c` L756-L758.
///
/// # Returns
///
/// `Z_OK`; `Z_BUF_ERROR` if `bits` is outside `0 ..= 16` or the internal buffer has no room for
/// the insertion; `Z_STREAM_ERROR` if the stream state is inconsistent (`zlib.h` L828-L831).
///
/// # Safety
///
/// `strm` must be null or address a live [`z_stream`] holding a state this library installed,
/// with nothing else concurrently accessing it.
///
/// Ported from `deflatePrime`, `deflate.c` L745-L771.
#[no_mangle]
pub unsafe extern "C" fn deflatePrime(strm: z_streamp, bits: c_int, value: c_int) -> c_int {
    guard_code(|| {
        // `if (deflateStateCheck(strm)) return Z_STREAM_ERROR;` (L749)
        //
        // SAFETY: unsafe-site categories 3 and 4 -- recovering the state block, the
        // first thing this body does. Nothing is dereferenced before it is checked:
        // `deflate_block_mut` tests `strm` for null and alignment, the `state` member for null
        // and alignment, the owner back-pointer for identity and the tag for range, and
        // answers `None` -- the documented `Z_STREAM_ERROR` -- if any of them fails. What this
        // call site supplies is what those checks cannot: a non-null `strm` addresses a live
        // `z_stream` whose `state`, if non-null, is a `DeflateBlock` this library installed,
        // and whose `zalloc`/`zfree` pair is either both null or both valid.
        // The borrow is the only one taken for this call, nothing else dereferences `strm`
        // while it is alive, and it ends when the body returns.
        let Some(block) = (unsafe { deflate_block_mut(strm) }) else {
            return fallback::STREAM_ERROR_CODE;
        };

        let code = core_deflate_prime(&mut block.state_mut().state, bits, value);
        sync_tag(block);
        code
    })
}

// ---------------------------------------------------------------------------
// Live reconfiguration -- `deflate.c` L774-L830
// ---------------------------------------------------------------------------

/// The `last_flush` sentinel meaning "reset, and `deflate` has not been called since".
///
/// `deflateResetKeep` installs it (`deflate.c` L672) and `deflate` overwrites it with the
/// caller's flush value on its first call (L998). `deflateParams` tests it at L792 to decide
/// whether it must flush the open block, and this module tests it for the same reason -- see
/// [`deflateParams`].
/// cbindgen:ignore
const LAST_FLUSH_AFTER_RESET: c_int = -2;

/// Changes the compression level and strategy of a live stream -- `zlib.h` L713.
///
/// The interesting part is that it can **emit bytes**: if the new parameters would switch
/// compressor or strategy and `deflate` has already run, C first flushes the open block with
/// `deflate(strm, Z_BLOCK)` (`deflate.c` L791-L799), because symbols already tallied were
/// chosen under the old settings and cannot be re-interpreted under the new ones. That is why
/// this entry point sets up the caller's buffers exactly as [`deflate`] does rather than
/// touching the state alone.
///
/// # ★ Why the buffers are read conditionally
///
/// C's `deflateParams` reads no member of `strm` itself; every access happens inside the inner
/// `deflate` call, which is reachable only when `s->last_flush != -2` -- that is, only once
/// `deflate` has been called since the last reset. And a stream on which `deflate` has been
/// called necessarily had `next_out` and `avail_out` set by its caller.
///
/// Before that point the four buffer members may not have been initialised at all: `zlib.h`
/// L236 has `deflateInit` initialise `total_in`, `total_out`, `adler` and `msg` and says
/// nothing about the buffers, and `test/example.c` leaves them alone until just before its first
/// `deflate`. Reading an indeterminate `next_out` into a Rust pointer would be undefined
/// behaviour, so this function reads the four only on the path where C reaches them, and writes
/// them back only on that same path.
///
/// # ★ The one status this wrapper rewrites
///
/// C's inner `deflate` call rejects a null `next_out`, or a null `next_in` paired with a
/// non-zero `avail_in`, with `Z_STREAM_ERROR` (L990-L993), and `deflateParams` propagates that
/// status verbatim (L795-L796). Those two tests cannot exist in the core, because a Rust slice
/// is never null: an empty output buffer arrives as an empty slice, so the core answers
/// `Z_BUF_ERROR` where C answers `Z_STREAM_ERROR`.
///
/// Reproducing C needs two facts, and only the first can be decided here:
///
/// 1. **whether the caller's pointers are in the rejected state** -- decided from the members
///    read on entry, before any slice exists; and
/// 2. **whether the inner `deflate` call happens at all** -- which is the compressor-change
///    predicate at L791, and re-deriving that here would put a second copy of a byte-identity
///    decision outside the core.
///
/// So fact 2 is never decided: it is made irrelevant, then read back. When fact 1 holds, the
/// stream handed to the core carries **two empty slices** instead of the caller's pointers.
/// That is faithful in both directions, because C refuses at L993 having read and written
/// nothing: if the parameters do reach the inner `deflate`, it now refuses one test later on
/// `avail_out == 0` (L995) -- likewise emitting nothing, consuming nothing and leaving the state
/// untouched -- and if they do not, the empty slices are never looked at and the answer is C's
/// `Z_OK` regardless.
///
/// The status is then restored from the message. `deflate` records one on every status it
/// refuses with -- that is what C's `ERR_RETURN` does, and the core reproduces it through
/// [`ReturnCode::record_msg`] -- while `deflateParams`' own `Z_BUF_ERROR` at L797 is a plain
/// return that records nothing. A recorded message therefore means, and can only mean, that the
/// inner call ran and refused; paired with fact 1 that is C's L993, so both the status and the
/// message become the ones `ERR_RETURN` sets there. The same single-status rewrite, for the same
/// underlying reason, appears in `crate::compress` for a null `dest`.
///
/// Measured against the reference across all eight reachable combinations of the two disjuncts
/// -- null `next_out` with input pending and with nothing pending, no compressor change, the
/// fresh-stream reset branch, a null `next_in` with a non-zero `avail_in` both after a flush and
/// after `Z_NO_FLUSH`, a strategy-only change, and a valid `next_out` with `avail_out == 0` --
/// this reproduces C's status, message and every `z_stream` member exactly. No residual remains.
///
/// # Returns
///
/// `Z_OK`; `Z_STREAM_ERROR` for an out-of-range `level` or `strategy` or an inconsistent stream;
/// `Z_BUF_ERROR` if the open block could not be flushed completely because the output buffer was
/// too small, in which case **nothing is changed** and `zlib.h` L735-L742 tells the caller to
/// retry with more room.
///
/// # Safety
///
/// `strm` must be null or address a live [`z_stream`] holding a state this library installed.
/// Where `deflate` has already been called on that stream, its four buffer members must be
/// initialised and its two buffers must satisfy the same contract [`deflate`] requires.
///
/// Ported from `deflateParams`, `deflate.c` L774-L816.
#[no_mangle]
pub unsafe extern "C" fn deflateParams(strm: z_streamp, level: c_int, strategy: c_int) -> c_int {
    guard_code(|| {
        // `if (deflateStateCheck(strm)) return Z_STREAM_ERROR;` (L778)
        //
        // SAFETY: unsafe-site categories 3 and 4 -- recovering the state block, the
        // first thing this body does. Nothing is dereferenced before it is checked:
        // `deflate_block_mut` tests `strm` for null and alignment, the `state` member for null
        // and alignment, the owner back-pointer for identity and the tag for range, and
        // answers `None` -- the documented `Z_STREAM_ERROR` -- if any of them fails. What this
        // call site supplies is what those checks cannot: a non-null `strm` addresses a live
        // `z_stream` whose `state`, if non-null, is a `DeflateBlock` this library installed,
        // and whose `zalloc`/`zfree` pair is either both null or both valid.
        // The borrow is the only one taken for this call and ends when the body returns; the
        // block is a separate allocation from the `z_stream`, so the raw reads and writes of
        // the caller's members below cannot alias it.
        let Some(block) = (unsafe { deflate_block_mut(strm) }) else {
            return fallback::STREAM_ERROR_CODE;
        };

        if block.state().state.last_flush() == LAST_FLUSH_AFTER_RESET {
            // C cannot reach its inner `deflate` call, so it touches none of the caller's
            // buffer members; neither does this. The four scalars still travel in and out,
            // because they are the members a reset does initialise and the core is free to
            // read them.
            //
            // SAFETY: unsafe-site category 1 -- four members read through raw places on a
            // non-null, aligned, live stream, so all four are in bounds and readable. They are
            // also initialised: a stream that owns a state has been through `deflateInit2_`,
            // whose closing reset assigns all four.
            let scalars = unsafe { StreamScalars::read(strm) };
            let mut stream = scalars.into_stream(&[], OutputRegion::empty());
            let code =
                core_deflate_params(&mut block.state_mut().state, &mut stream, level, strategy);
            sync_tag(block);

            // SAFETY: unsafe-site category 1 -- the same four members written back, plus `msg`
            // if the core recorded one, on a non-null, aligned, live stream.
            unsafe {
                publish_scalars(strm, &stream, code);
            }
            return code;
        }

        // SAFETY: unsafe-site category 1 -- eight members read through raw places on a non-null,
        // aligned, live stream, so all eight are in bounds and readable. All eight are also
        // initialised: `deflate` has already run on this stream, which establishes that its
        // caller initialised the four buffer members, and `deflateInit2_`'s closing reset
        // assigned the four scalars.
        let entry = unsafe { StreamFields::read(strm) };

        // The two disjuncts of `deflate.c` L990-L991, evaluated now because the slices built
        // below erase the distinction between "no room" and "nowhere to write".
        //
        // An overlapping pair does *not* join them: C's inner `deflate` call at L795 would run,
        // so this one runs too, over a snapshot of the input. Only a snapshot that could not be
        // allocated joins them, which is the third disjunct below -- and it lands on exactly the
        // right arm, because C's refusal for a bad pointer pair is `ERR_RETURN(strm,
        // Z_STREAM_ERROR)` and that is what an unservable pair deserves as well. See
        // `AliasScratch` and `StreamFields::buffers_overlap`.
        // SAFETY: unsafe-site category 4 -- `scratch_allocator`'s contract is this function's:
        // `strm` is live, and the three hook members are read through raw places.
        let allocator = unsafe { scratch_allocator(strm) };
        let mut scratch = if entry.buffers_overlap() {
            // SAFETY: unsafe-site category 2 -- `capture_overlapping_input`'s contract. `entry`
            // came from `StreamFields::read` on this live stream and no mutable borrow of its
            // input region exists yet.
            unsafe { capture_overlapping_input(&allocator, &entry) }
        } else {
            None
        };
        let pointers_rejected = entry.next_out.is_null()
            || (entry.avail_in != 0 && entry.next_in.is_null())
            || (entry.buffers_overlap() && scratch.is_none());

        // Held in a binding that outlives `stream`, as `scratch_view` requires.
        //
        // SAFETY: unsafe-site category 2 -- `scratch_view` fabricates `'static`; the scratch is
        // a local declared above `stream` and dropped after it, so the slice never outlives the
        // bytes it points at.
        let captured_input = unsafe { scratch_view(scratch.as_ref()) };

        let mut stream = if pointers_rejected {
            // C refuses this pointer state at L993, before it has read or written a byte
            // through either pointer, so nothing may travel through them here either. Two empty
            // slices deliver that: if the parameters reach the inner `deflate` at all it now
            // refuses on `avail_out == 0` at L995 -- C's own refusal, one test later, having
            // likewise emitted nothing, consumed nothing and left the state alone -- and the
            // rewrite below restores C's status. If the parameters do *not* reach it, the empty
            // slices are never looked at and the answer is C's `Z_OK` either way.
            entry.scalars.into_stream(&[], OutputRegion::empty())
        } else {
            // SAFETY: unsafe-site category 2 -- slice reconstruction, once, through the shared
            // helper. `entry` came from `StreamFields::read` on this live stream, and this
            // function's contract makes both buffers valid for their counts with the output
            // unaliased; the input is either disjoint from it or replaced by `captured_input`.
            unsafe { borrow_stream(&entry, captured_input) }
        };

        // The header view has to be present for this call, and only for this one of the
        // two paths. `deflateParams` flushes the current block by calling
        // `deflate(strm, Z_BLOCK)` itself (`deflate.c` L794), and that internal call is
        // an ordinary `deflate`: if the gzip header is only partly written -- a first
        // `deflate` that ran out of output space mid-name leaves the status in a header
        // stage and `s->gzindex` part-way through -- it resumes emitting it, reading
        // every field through `s->gzhead`. Without the view the core would emit the
        // *default* header instead of the caller's, mid-stream. The path above cannot
        // reach the internal call at all, because `last_flush == -2` is exactly the
        // condition L791 tests to skip it, so it reads nothing and needs nothing.
        //
        // SAFETY: unsafe-site category 6 -- `with_header_while_emitting`'s contract is
        // this function's: `head` is null or the pointer the caller passed to
        // `deflateSetHeader`, which `zlib.h` L838-L845 requires to stay live and
        // readable while the header is being emitted -- which is exactly the window in
        // which that helper reads it.
        let mut code = unsafe {
            with_header_while_emitting(block.state_mut(), |state| {
                core_deflate_params(state, &mut stream, level, strategy)
            })
        };
        sync_tag(block);

        if pointers_rejected && stream.msg.is_some() {
            // A recorded message can only have come from the inner `deflate` call at L795,
            // because `deflateParams`' own `Z_BUF_ERROR` at L797 records nothing. So the inner
            // call ran and refused -- and with the caller's pointers in this state, C's refusal
            // is `ERR_RETURN(strm, Z_STREAM_ERROR)` (L993). Both the status and the message that
            // macro records are therefore the ones to publish, whichever status the core itself
            // arrived at once `deflateParams` was done with the inner call's answer.
            code = ReturnCode::STREAM_ERROR;
            stream.msg = Some(code.msg());
        }

        // SAFETY: unsafe-site category 1 -- the write-back, with `entry` the fields read on
        // entry and `stream` the view built from them, on a non-null, aligned, live stream. On
        // the rewritten path the four buffer members are unchanged, exactly as in C: the inner
        // `deflate` refuses before it moves either cursor.
        unsafe {
            publish(strm, &entry, &stream, code);
        }

        // The snapshot, when there was one, goes back to the allocator it came from -- after
        // the last use of the view built over it, so no borrow of the block is live when it is
        // released, and before this frame returns, so a tracking allocator sees one strictly
        // nested allocate/free pair around this call. `stream` is a plain view with no `Drop`,
        // so `drop` would not shorten anything: what establishes the ordering is that
        // `publish` above is its final use and nothing below reads it.
        if let Some(scratch) = scratch.as_mut() {
            scratch.release(&allocator);
        }

        code
    })
}

/// Overrides the four match-finding tuning parameters -- `zlib.h` L751.
///
/// `zlib.h` L757-L762 describes the audience as "the most fanatic optimizer trying to squeeze out
/// the last compressed bit for their specific input data". All four assignments are
/// unconditional and C validates **nothing** (`deflate.c` L825-L828): an out-of-range value
/// simply produces a poor or a very slow search.
///
/// ★ These are the four fields the level's `configuration_table` entry normally supplies, so a
/// caller that uses this function has *deliberately* left the byte-identity envelope. The values
/// are therefore passed through exactly as given -- no validation is added, because that would
/// reject calls the reference accepts, and no clamping, because that would choose different
/// numbers from the ones the caller asked for. The core reproduces C's `(uInt)` cast, negative
/// arguments included.
///
/// # Returns
///
/// `Z_OK`, or `Z_STREAM_ERROR` if the stream state is inconsistent (`zlib.h` L764-L765).
///
/// # Safety
///
/// `strm` must be null or address a live [`z_stream`] holding a state this library installed,
/// with nothing else concurrently accessing it.
///
/// Ported from `deflateTune`, `deflate.c` L819-L830.
#[no_mangle]
pub unsafe extern "C" fn deflateTune(
    strm: z_streamp,
    good_length: c_int,
    max_lazy: c_int,
    nice_length: c_int,
    max_chain: c_int,
) -> c_int {
    guard_code(|| {
        // `if (deflateStateCheck(strm)) return Z_STREAM_ERROR;` (L823)
        //
        // SAFETY: unsafe-site categories 3 and 4 -- recovering the state block, the
        // first thing this body does. Nothing is dereferenced before it is checked:
        // `deflate_block_mut` tests `strm` for null and alignment, the `state` member for null
        // and alignment, the owner back-pointer for identity and the tag for range, and
        // answers `None` -- the documented `Z_STREAM_ERROR` -- if any of them fails. What this
        // call site supplies is what those checks cannot: a non-null `strm` addresses a live
        // `z_stream` whose `state`, if non-null, is a `DeflateBlock` this library installed,
        // and whose `zalloc`/`zfree` pair is either both null or both valid.
        // The borrow is the only one taken for this call, nothing else dereferences `strm`
        // while it is alive, and it ends when the body returns.
        let Some(block) = (unsafe { deflate_block_mut(strm) }) else {
            return fallback::STREAM_ERROR_CODE;
        };

        let code = core_deflate_tune(
            &mut block.state_mut().state,
            good_length,
            max_lazy,
            nice_length,
            max_chain,
        );
        sync_tag(block);
        code
    })
}

// ---------------------------------------------------------------------------
// Output bounds -- `deflate.c` L856-L932
// ---------------------------------------------------------------------------

/// An upper bound on the compressed size of `sourceLen` bytes, in `size_t` units -- `zlib.h`
/// L769.
///
/// ★ **This is not advisory.** `zlib.h` L770-L780 makes it the number callers allocate their
/// output buffer with, and promises that a single `Z_FINISH` call into a buffer of this size
/// returns `Z_STREAM_END`. A bound that is too small becomes a heap overflow in *caller* code;
/// one that is too large breaks tests that assert exact sizes. Every operation, width and order
/// in the arithmetic is therefore the core's verbatim port of `deflate.c` L859-L927, and this
/// wrapper adds nothing to it.
///
/// ★ **An invalid stream is not an error here.** `deflate.c` L875-L879 answers a failed
/// `deflateStateCheck` with the larger of the two conservative bounds plus eighteen bytes of
/// wrapper, and returns it as an ordinary result -- there is no status to report through, since
/// the return type is a length. A null `strm`, an uninitialised one and a foreign one therefore
/// all yield a usable, generous bound rather than zero, and reproducing that is what keeps a
/// caller which sizes its buffer before initialising its stream working.
///
/// # Safety
///
/// `strm` must be null or address a live [`z_stream`] whose three allocator members are
/// initialised. Nothing is written and no buffer is touched.
///
/// Ported from `deflateBound_z`, `deflate.c` L856-L928.
#[no_mangle]
pub unsafe extern "C" fn deflateBound_z(strm: z_streamp, sourceLen: z_size_t) -> z_size_t {
    guard(|| {
        // `deflateStateCheck(strm)` (L875) as an `Option` rather than a status: `None` selects
        // the conservative branch inside the core.
        //
        // SAFETY: unsafe-site categories 3 and 4 -- the helper's contract is this function's,
        // and the borrow it yields is shared and lives no longer than this call.
        let block = unsafe { deflate_block_mut(strm) };
        // SAFETY: unsafe-site category 6 -- as `deflate`. `deflateBound` reads the header too:
        // `deflate.c` L893-L910 walks `gzhead->extra`, `name` and `comment` to size the gzip
        // wrapper, so the numbers it returns depend on the structure's CURRENT contents.
        match block {
            // SAFETY: unsafe-site category 6 -- `with_header`'s contract, discharged by the
            // paragraph above: the header pointer the state holds is the caller's, live and
            // unchanged, and the borrow it lends the closure ends with the call.
            Some(block) => unsafe {
                with_header(block.state_mut(), |state| {
                    core_deflate_bound_z(Some(state), sourceLen)
                })
            },
            None => core_deflate_bound_z::<StreamAllocator>(None, sourceLen),
        }
    })
}

/// An upper bound on the compressed size of `sourceLen` bytes, in `uLong` units -- `zlib.h` L768.
///
/// [`deflateBound_z`] with C's narrowing step on top:
///
/// ```text
/// return (uLong)bound != bound ? (uLong)-1 : (uLong)bound;
/// ```
///
/// -- `deflate.c` L931. On LP64 the narrowing is the identity. On LLP64 Windows, where
/// `unsigned long` is four bytes and `size_t` is eight, a bound above 4 GiB cannot be expressed
/// and C reports `(uLong)-1`; `zlib.h` L783-L784 exists to point callers at the `_z` form for
/// exactly that case ("Note that a long is 32 bits on Windows").
///
/// Reporting the largest representable value rather than zero is the safe direction and is C's:
/// too small a bound is a heap overflow in the caller, whereas too large a one only fails the
/// caller's own allocation, which it must already handle.
///
/// # Safety
///
/// As [`deflateBound_z`].
///
/// Ported from `deflateBound`, `deflate.c` L929-L932.
#[no_mangle]
pub unsafe extern "C" fn deflateBound(strm: z_streamp, sourceLen: uLong) -> uLong {
    guard(|| {
        // SAFETY: unsafe-site categories 3 and 4 -- recovering the state block, where a null or
        // stateless stream is legal rather than an error: `zlib.h` L768-L774 lets a caller ask
        // for a bound before `deflateInit_`, so `None` feeds the conservative estimate instead
        // of a refusal. `deflate_block_mut` tests `strm` for null and alignment, the `state`
        // member for null and alignment, the owner back-pointer for identity and the tag for
        // range before forming any reference, so a null or foreign stream is never dereferenced.
        // What this call site supplies is what those checks cannot: a non-null `strm` addresses
        // a live `z_stream` whose `state`, if non-null, is a deflate block this library
        // installed, and whose `zalloc`/`zfree` pair is either both null or both valid. The
        // borrow is the only one taken for this call and it ends with it.
        let block = unsafe { deflate_block_mut(strm) };
        let bound = match block {
            // SAFETY: unsafe-site category 6 -- `with_header`'s contract. `deflateBound` reads
            // the caller's `gz_header` too: `deflate.c` L893-L910 walks `gzhead->extra`, `name`
            // and `comment` to size the gzip wrapper, so the number returned depends on the
            // structure's CURRENT contents, and the borrow lent to the closure ends with the
            // call.
            Some(block) => unsafe {
                with_header(block.state_mut(), |state| {
                    core_deflate_bound(Some(state), widen_uLong(sourceLen))
                })
            },
            None => core_deflate_bound::<StreamAllocator>(None, widen_uLong(sourceLen)),
        };

        // L931, with the round trip standing in for C's `(uLong)bound != bound` comparison: the
        // narrowing is information-preserving exactly when widening it again reproduces the
        // original.
        let narrowed = narrow_uLong(bound);
        if widen_uLong(narrowed) == bound {
            narrowed
        } else {
            // `(uLong)-1`, which `crate::panic_guard::fallback` names so that no call site has
            // to spell an all-ones constant.
            fallback::BOUND
        }
    })
}

// ---------------------------------------------------------------------------
// Compression -- `deflate.c` L981-L1290
// ---------------------------------------------------------------------------

/// Compresses as much as possible, and flushes according to `flush` -- `zlib.h` L254.
///
/// The stream's driver, and the only entry point that produces compressed bytes. It is
/// *resumable* at every point: a caller whose `avail_out` runs out mid-header, mid-block or
/// mid-trailer re-enters exactly where it left off, which is what lets a one-byte output buffer
/// still make progress. `zlib.h` L256-L363 documents the whole contract; the summary is that a
/// caller repeats the call, supplying more input or more room, until it has what it needs.
///
/// # The four entry guards, in C's order
///
/// The order is observable, because the first pair leaves `msg` alone and the second pair sets
/// it:
///
/// 1. `deflateStateCheck(strm) || flush > Z_BLOCK || flush < 0` -- a **plain**
///    `return Z_STREAM_ERROR` at L985-L987, deliberately not an `ERR_RETURN`, so a message an
///    earlier failure recorded survives.
/// 2. `next_out == Z_NULL`, or `next_in == Z_NULL` with a non-zero `avail_in` -- `ERR_RETURN`
///    with `Z_STREAM_ERROR` at L990-L993.
/// 3. `s->status == FINISH_STATE && flush != Z_FINISH` -- the third disjunct of the same
///    `ERR_RETURN`, and the core's, since it is a state fact rather than a pointer fact.
/// 4. `avail_out == 0` -- `ERR_RETURN` with `Z_BUF_ERROR` at L995.
///
/// ★ A null `next_in` with `avail_in == 0` is **legal**, and `zlib.h` L138-L139 invites it. It
/// becomes an empty input slice here.
///
/// ## ★ `Z_TREES` is not a deflate flush
///
/// The valid values are `Z_NO_FLUSH` (0), `Z_PARTIAL_FLUSH` (1), `Z_SYNC_FLUSH` (2),
/// `Z_FULL_FLUSH` (3), `Z_FINISH` (4) and `Z_BLOCK` (5). `Z_TREES` (6) is a *decompression*
/// request, and C's `flush > Z_BLOCK` at L985 rejects it -- so a caller that passes 6, or 99, or
/// -1, receives `Z_STREAM_ERROR` rather than a panic and rather than a silently coerced flush.
/// The test is [`zlib_rs::config::validate_deflate_flush`], which performs both halves of C's
/// range check, and it is applied here as well as inside the core so that it happens *before*
/// the pointer tests, exactly as C's single combined condition does.
///
/// # Returns
///
/// `Z_OK` if progress was made; `Z_STREAM_END` if `flush` was `Z_FINISH` and everything has been
/// written; `Z_STREAM_ERROR` for an invalid flush, an inconsistent stream or more output
/// requested after the stream finished; `Z_BUF_ERROR` if no progress was possible -- which is
/// **not fatal**, per `zlib.h` L360-L363.
///
/// # Safety
///
/// `strm` must be null or address a live [`z_stream`] holding a state this library installed,
/// with all four buffer members initialised and nothing else concurrently accessing it.
/// `avail_in` bytes must be readable at `next_in` when that count is non-zero, `avail_out` bytes
/// writable at `next_out` when that count is non-zero, and neither region may overlap the
/// [`z_stream`] itself. The two regions **may** overlap each other, exactly as they may in C:
/// this detects that and copies the input first, so no aliasing borrow is ever formed. See
/// [`AliasScratch`].
///
/// Ported from `deflate`, `deflate.c` L981-L1290.
#[no_mangle]
pub unsafe extern "C" fn deflate(strm: z_streamp, flush: c_int) -> c_int {
    guard_code(|| {
        // Guard 1, first half: `deflateStateCheck(strm)` (L985).
        //
        // SAFETY: unsafe-site categories 3 and 4 -- the helper's contract is this function's,
        // and the mutable borrow it yields is the only one taken for this call.
        let Some(block) = (unsafe { deflate_block_mut(strm) }) else {
            return fallback::STREAM_ERROR_CODE;
        };

        // Guard 1, second half: `flush > Z_BLOCK || flush < 0` (L985). A plain return, so `msg`
        // is deliberately left exactly as the caller found it.
        if validate_deflate_flush(flush).is_err() {
            return fallback::STREAM_ERROR_CODE;
        }

        // SAFETY: unsafe-site category 1 -- eight members read through raw places on a
        // non-null, aligned, live stream whose four buffer members are initialised by this
        // function's contract.
        let entry = unsafe { StreamFields::read(strm) };

        // Guard 2: the two pointer disjuncts of L990-L991. `ERR_RETURN`, so the message is set.
        if entry.next_out.is_null() || (entry.avail_in != 0 && entry.next_in.is_null()) {
            // SAFETY: unsafe-site category 1 -- one member written through a raw place on a
            // non-null, aligned, live stream, with a `'static` NUL-terminated string that
            // outlives every caller.
            unsafe {
                write_msg(strm, Some(error_message(ReturnCode::STREAM_ERROR.as_i32())));
            }
            return fallback::STREAM_ERROR_CODE;
        }

        // ★ Not a C guard, and not a refusal either. C runs happily with an overlapping input
        // and output -- `test/example.c`'s `test_large_deflate` does exactly that at L275-L277
        // and the suite asserts the resulting byte count -- so the pair has to be *served*.
        // What cannot happen is `borrow_stream` building a `&[u8]` and a `&mut [u8]` over one
        // region, so the input is copied first and the core reads the copy. `AliasScratch`
        // documents the whole argument. Only a failed snapshot allocation is refused, and it is
        // refused exactly as an invalid pointer pair is, message included.
        // SAFETY: unsafe-site category 4 -- `scratch_allocator`'s contract is this function's:
        // `strm` is live, and the three hook members are read through raw places.
        let allocator = unsafe { scratch_allocator(strm) };
        let mut scratch = if entry.buffers_overlap() {
            // SAFETY: unsafe-site category 2 -- `capture_overlapping_input`'s contract. `entry`
            // came from `StreamFields::read` on this live stream, whose input region is
            // readable for `avail_in` bytes, and no mutable borrow of it exists yet: the only
            // one this function creates comes from `borrow_stream`, below.
            let captured = unsafe { capture_overlapping_input(&allocator, &entry) };
            if captured.is_none() {
                // SAFETY: unsafe-site category 1 -- one member written through a raw place on
                // a non-null, aligned, live stream, with a `'static` NUL-terminated string.
                unsafe {
                    write_msg(strm, Some(error_message(ReturnCode::STREAM_ERROR.as_i32())));
                }
                return fallback::STREAM_ERROR_CODE;
            }
            captured
        } else {
            None
        };

        // Held in a binding that outlives `stream`, which is what `scratch_view` requires of
        // every caller.
        //
        // SAFETY: unsafe-site category 2 -- `scratch_view` fabricates `'static`, and its
        // contract is that the slice must not outlive the scratch. `scratch` is a local binding
        // declared above `stream` and dropped after it, so the slice dies first.
        let captured_input = unsafe { scratch_view(scratch.as_ref()) };

        // Guards 3 and 4, and everything after them, are the core's.
        //
        // SAFETY: unsafe-site category 2 -- slice reconstruction, once, through the shared
        // helper. `entry` came from `StreamFields::read` on this live stream, and this
        // function's contract makes both buffers valid for their counts, with the output
        // unaliased; the input is either disjoint from it or replaced by `captured_input`,
        // whose length is `avail_in` because `AliasScratch::capture` was given that count.
        // `flush` is passed through unchanged rather than pre-converted, so the core applies
        // its own canonical validation to the same value C validates.
        let mut stream = unsafe { borrow_stream(&entry, captured_input) };

        // SAFETY: unsafe-site category 6 -- `with_header_while_emitting`'s contract is
        // this function's: `head` is null or the pointer the caller passed to
        // `deflateSetHeader`, which `zlib.h` L838-L845 requires to stay live and
        // readable for the duration of the calls that emit it. Past that point the
        // helper reads nothing, so a caller which has released the structure -- as that
        // same paragraph permits -- is not read after the fact.
        let code = unsafe {
            with_header_while_emitting(block.state_mut(), |state| {
                core_deflate(state, &mut stream, flush)
            })
        };
        // The status advances through the header stages, `BUSY_STATE` and `FINISH_STATE` as the
        // stream progresses, so the C-visible tag must follow it -- and `deflateEnd`'s
        // `Z_DATA_ERROR` depends on the same status being accurate.
        sync_tag(block);

        // SAFETY: unsafe-site category 1 -- the write-back, with `entry` the fields read on
        // entry and `stream` the view built from them, on a non-null, aligned, live stream.
        unsafe {
            publish(strm, &entry, &stream, code);
        }

        // As `deflateParams`: the snapshot goes back to the stream's own allocator after the
        // final use of the view over it -- the `publish` immediately above -- and before this
        // frame returns.
        if let Some(scratch) = scratch.as_mut() {
            scratch.release(&allocator);
        }

        code
    })
}

// ---------------------------------------------------------------------------
// Teardown -- `deflate.c` L1293-L1310
// ---------------------------------------------------------------------------

/// Releases everything the compression state owns -- `zlib.h` L367.
///
/// ★ **The return value is a documented contract, not a formality.** `zlib.h` L373-L377 promises
/// `Z_DATA_ERROR` "if the stream was freed prematurely (some input or output was discarded)",
/// and `test/infcover.c` exercises exactly that. The status is captured *before* the teardown
/// (`deflate.c` L1298), because after it there is nothing left to read it from -- and a `Drop`
/// implementation could not report anything at all. Everything is still freed in that case: the
/// code is a report, not a refusal.
///
/// The four buffers are released in reverse order of allocation -- `pending_buf`, `head`, `prev`,
/// `window` (L1300-L1304) -- and then the state object itself (L1306), each through the caller's
/// own `zfree`. That order is not cosmetic: `test/infcover.c`'s tracking allocator counts a
/// departure from last-in-first-out as a `notlifo` defect, and a block returned to a different
/// allocator as a *rogue* free. `strm->state` is cleared *before* the object is released
/// (L1307's effect, brought forward by [`take_state_with`]), so no path can leave the caller holding
/// a dangling pointer -- not even one on which the release itself fails.
///
/// Calling it twice is safe and reports `Z_STREAM_ERROR` the second time, because the first call
/// left `strm->state` null.
///
/// # Returns
///
/// `Z_OK`; `Z_DATA_ERROR` if the stream was in `BUSY_STATE`; `Z_STREAM_ERROR` if the stream
/// state is inconsistent.
///
/// # Safety
///
/// `strm` must be null or address a live [`z_stream`] holding a state this library installed,
/// with nothing else concurrently accessing it. Its `zalloc`, `zfree` and `opaque` members must
/// still be the ones the matching `deflateInit_` saw -- which `zlib.h` L140-L142 requires of the
/// application anyway -- because the state can only be returned to the allocator that produced
/// it.
///
/// Ported from `deflateEnd`, `deflate.c` L1293-L1310.
#[no_mangle]
pub unsafe extern "C" fn deflateEnd(strm: z_streamp) -> c_int {
    guard_code(|| {
        // The allocator half of `deflateStateCheck` (L1296), and the triple every block must be
        // returned through.
        //
        // SAFETY: unsafe-site category 4 -- reading the caller's three allocator members
        // through raw places. The helper checks non-null and alignment itself and forms no
        // reference, so the caller's pointer stays usable below; liveness is this function's
        // contract.
        let Some(allocator) = (unsafe { StreamAllocator::from_stream_ptr(strm) }) else {
            return fallback::STREAM_ERROR_CODE;
        };

        // The rest of `deflateStateCheck`, then L1298-L1309 in C's order: the status is read,
        // the four buffers are released as `pending_buf`, `head`, `prev`, `window`, the state
        // object goes back last, and the status decides the return value. Dropping the state
        // inside `core_deflate_end` is what releases the buffers -- the core spells that order
        // out -- and `take_state_with` is the variant that holds the state block's own storage
        // until afterwards, so the caller's `zfree` sees the strictly last-in-first-out sequence
        // `test/infcover.c` checks for. `strm->state` is cleared before any of it, so no path
        // can leave the caller holding a dangling pointer.
        //
        // SAFETY: unsafe-site categories 3 and 4 -- the opaque `state` round-trip and the
        // caller's `zfree`. The helper performs the four validity checks before touching
        // anything, and `allocator` carries the same triple `commit_state` used, which this
        // function's contract requires. No borrow of the block exists here, so recovering it is
        // sound and cannot be repeated. The closure releases the buffers through a borrow of
        // the block and returns a status code, which owns no storage from this allocator --
        // `take_state_with`'s one extra obligation.
        let code = unsafe {
            take_state_with(
                strm,
                &allocator,
                StateKind::Deflate,
                |block: &mut DeflateBlock| core_deflate_end(&mut block.state_mut().state),
            )
        };

        let Some(code) = code else {
            return fallback::STREAM_ERROR_CODE;
        };

        code
    })
}

// ---------------------------------------------------------------------------
// Duplication -- `deflate.c` L1317-L1377
// ---------------------------------------------------------------------------

/// Makes `dest` a complete, independent copy of `source` -- `zlib.h` L684.
///
/// `zlib.h` L687-L693 gives the use: trying several compression strategies from a common
/// prefix, for instance when the input can be pre-processed several ways. The copy duplicates
/// the whole compression state, "which can be quite large, so this strategy is slow and can
/// consume lots of memory".
///
/// The copy's four buffers are allocated through the **source's** `zalloc`, because the byte copy
/// of the stream structure gives `dest` the source's three allocator members before anything is
/// allocated -- so the copy is owned by the same allocator as the original and
/// `deflateEnd(dest)` returns it to the same `zfree`. How much of each buffer is copied is not
/// uniform, and the core reproduces C's five different answers exactly, including the `slid` flag
/// that decides whether the whole `prev` array must come across.
///
/// # ★ `dest` and `source` must be distinct
///
/// A `dest` that is the same stream as `source`, or that overlaps it, is rejected with
/// `Z_STREAM_ERROR`. C does not test for it, and what C then does is not something a
/// memory-safe port may reproduce: L1333's `zmemcpy` onto itself is already undefined, and on
/// the aliasing path L1338 overwrites `dest->state` while `ss` still names the original, leaking
/// the source's state object and all four of its buffers. `test/infcover.c`'s `mem_done` reports
/// a leak as a defect, so producing one deliberately is not an option. No documented use of
/// `deflateCopy` passes one stream twice -- the function exists to *fork* a stream -- and
/// `zlib.h` L695 already names `Z_STREAM_ERROR` as the status for an argument it will not act on.
///
/// # ★ `dest->state` on the failure paths
///
/// On success `dest->state` addresses the new state. The two failure paths leave different
/// values there, and both are C's:
///
/// * The **state object** could not be allocated (L1336-L1337). C returns before L1338 replaces
///   `dest->state`, so the member still holds what the byte copy put there at L1333 -- the
///   *source's* state pointer. This port leaves the same value.
/// * A **buffer** could not be allocated (L1347-L1351). C calls `deflateEnd(dest)`, which clears
///   `dest->state` at L1307. This port clears it too.
///
/// Reproducing the alias is safe here for the same reason it is safe in C: it is refused, not
/// acted upon. Every entry point checks that the state block records the stream it was installed
/// on -- C's `s->strm != strm` (L544), this port's prefix check -- so `deflateEnd(dest)` on that
/// aliased pointer reports `Z_STREAM_ERROR` and frees nothing, rather than releasing the source's
/// state a second time. A caller can therefore read the value C's documentation implies without
/// any path existing on which it becomes a double free.
///
/// # Returns
///
/// `Z_OK`; `Z_MEM_ERROR` if the copy could not be allocated, in which case `source` is left
/// completely untouched and remains usable; `Z_STREAM_ERROR` if `source`'s state is inconsistent,
/// if `dest` is null, or if the two overlap (`zlib.h` L694-L696).
///
/// # Safety
///
/// `source` must be null or address a live [`z_stream`] holding a state this library installed.
/// `dest` must be null or address `size_of::<z_stream>()` writable, aligned bytes -- it need not
/// be initialised, and any state it already held is overwritten rather than released, exactly as
/// in C, so a caller must not pass a stream with a live state. Neither may be concurrently
/// accessed by anything else.
///
/// Ported from `deflateCopy`, `deflate.c` L1317-L1377.
#[no_mangle]
pub unsafe extern "C" fn deflateCopy(dest: z_streamp, source: z_streamp) -> c_int {
    guard_code(|| {
        // `dest == Z_NULL` (L1327). The alignment half has no C counterpart, for the reason
        // `deflateInit2_` gives.
        if dest.is_null() || !dest.is_aligned() {
            return fallback::STREAM_ERROR_CODE;
        }

        // The distinctness requirement, which `crates/libz-rs-sys/src/types.rs` places on the
        // two-stream entry points explicitly. It precedes every read of `source`, because an
        // overlapping pair would make the byte copy below undefined.
        if !streams_are_disjoint(dest, source) {
            return fallback::STREAM_ERROR_CODE;
        }

        // The allocator half of `deflateStateCheck(source)` (L1327), and the triple the copy
        // will be allocated through -- the source's, which the byte copy is about to install in
        // `dest` as well.
        //
        // SAFETY: unsafe-site category 4 -- reading `source`'s three allocator members through
        // raw places. The helper checks non-null and alignment itself and forms no reference,
        // so `source` stays usable for the byte copy; liveness is this function's contract.
        let Some(allocator) = (unsafe { StreamAllocator::from_stream_ptr(source) }) else {
            return fallback::STREAM_ERROR_CODE;
        };

        // The rest of `deflateStateCheck(source)`.
        //
        // SAFETY: unsafe-site categories 3 and 4 -- recovering `source`'s state block for
        // reading. `deflate_block_ref` tests `source` for null and alignment, its `state` member
        // for null and alignment, the owner back-pointer for identity and the tag for range, and
        // answers `None` -- the documented `Z_STREAM_ERROR` -- if any of them fails, so nothing
        // is dereferenced before it is checked. What this call site supplies is what those checks
        // cannot: a non-null `source` addresses a live `z_stream` whose `state`, if non-null, is
        // a `DeflateBlock` this library installed, and whose `zalloc`/`zfree` pair is either
        // both null or both valid. The borrow is shared and is over the state block, which is a
        // separate allocation from either stream, so the writes to `dest` below cannot alias it,
        // and it ends when the body returns.
        let Some(block) = (unsafe { deflate_block_ref(source) }) else {
            return fallback::STREAM_ERROR_CODE;
        };

        // `zmemcpy(dest, source, sizeof(z_stream));` (L1333). This also gives `dest` the
        // source's `state` pointer, which the next statement replaces -- C replaces it at L1338.
        //
        // SAFETY: unsafe-site category 1 -- `copy_stream`'s contract. Both pointers are non-null
        // and aligned (`dest` by the test above, `source` by the two helpers), both cover a
        // whole `z_stream`, and the regions are disjoint by the test above.
        unsafe {
            copy_stream(dest, source);
        }

        // `ds = (deflate_state *) ZALLOC(dest, 1, sizeof(deflate_state));` (L1335) -- the state
        // object first, exactly as `deflateInit2_` takes it first, and through the source's
        // hooks because the byte copy has just given `dest` the same three allocator members.
        let slot = match reserve_state::<DeflateSlot>(&allocator) {
            Ok(slot) => slot,
            // L1336-L1337: `if (ds == Z_NULL) return Z_MEM_ERROR;`, reached before L1338, so
            // `dest->state` keeps the value the byte copy put there -- the *source's* state
            // pointer. Left exactly as C leaves it; see the note on the failure paths above.
            Err(code) => return code,
        };

        // `dest->state = (struct internal_state FAR *) ds;` (L1338), which also replaces the
        // alias the byte copy created.
        //
        // SAFETY: unsafe-site categories 1 and 3 -- recording the reserved block's address in
        // one member of `dest`, through a raw place. `dest` is non-null, aligned and writable as
        // established above; `slot` came from `reserve_state` one statement ago. The member is
        // written, not read.
        unsafe {
            publish_state(dest, slot);
        }

        // L1339-L1373: the four buffers, and the five different amounts of each that C copies.
        let copy = match core_deflate_copy(&block.state().state, allocator) {
            Ok(copy) => copy,
            Err(code) => {
                // L1347-L1351: `deflateEnd(dest); return Z_MEM_ERROR;`. The core has already
                // returned every buffer it took, in `deflateEnd`'s order; the state block goes
                // back after them and `dest->state` is cleared first, which is `deflateEnd`'s
                // own effect at L1307. `source` is untouched throughout -- which is what lets a
                // caller force this failure with a limiting allocator and carry on using the
                // original.
                //
                // SAFETY: unsafe-site categories 1, 3 and 4 -- one member of `dest` written
                // through a raw place, then the reserved block returned to the allocator that
                // produced it, detached from `dest` by that write, never committed, and so
                // owing no destructor.
                unsafe {
                    write_state_null(dest);
                    discard_reserved_state(&allocator, slot);
                }
                return code;
            }
        };

        // `ds->strm = dest;` (L1341), which `commit_state` performs by recording `dest` in the
        // block's prefix -- without it the copy would fail its own owner-identity check on the
        // very next call. The header pointer comes across verbatim, because C's
        // `zmemcpy(ds, ss, sizeof(deflate_state))` (L1339) copies `gzhead` with everything else
        // and never adjusts it: the copy emits the same caller-owned `gz_header` the source
        // would.
        let tag = copy.status().as_raw();
        let head = block.state().head;

        // SAFETY: unsafe-site categories 3 and 4 -- initialising the copy's state block.
        // `slot` came from `reserve_state` on `allocator`, the same triple the four buffers
        // above were taken from, so the whole copy can only be released through the matching
        // `zfree`; the block has not been committed before; and `copy` is moved in, so the
        // buffers it holds are now owned by the block `dest` points at.
        unsafe {
            commit_state(
                slot,
                &allocator,
                dest,
                StateKind::Deflate,
                tag,
                DeflateSlot { state: copy, head },
            );
        }

        ReturnCode::OK
    })
}

#[cfg(test)]
// The workspace denies the panic-prone lints in library code, which is the right policy
// there and the wrong one in a harness: a test asserts, and an assertion that fails
// panics. Indexing is allowed too, because every expectation below is written against a
// literal, known-good index.
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]
mod tests {
    use super::{
        deflate, deflateBound, deflateEnd, deflateInit2_, deflateInit_, deflateParams,
        deflateSetDictionary, deflateSetHeader, z_stream,
    };
    use crate::types::{gz_header, gz_headerp, uInt, uLong, Bytef};
    use crate::util::ZLIB_VERSION;
    use core::ffi::{c_char, c_int, c_uint};
    use core::mem::size_of;
    use core::ptr;
    use zlib_rs::error::ReturnCode;

    /// `Z_OK` (`zlib.h` L179).
    const OK: c_int = ReturnCode::OK.as_i32();
    /// `Z_STREAM_END` (`zlib.h` L180).
    const STREAM_END: c_int = ReturnCode::STREAM_END.as_i32();
    /// `Z_STREAM_ERROR` (`zlib.h` L185).
    const STREAM_ERROR: c_int = ReturnCode::STREAM_ERROR.as_i32();
    /// `Z_DATA_ERROR` (`zlib.h` L186), which `deflateEnd` reports for a stream torn down
    /// while it still held work -- `zlib.h` L373-L377.
    const DATA_ERROR: c_int = ReturnCode::DATA_ERROR.as_i32();
    /// `Z_NO_FLUSH` (`zlib.h` L172).
    const NO_FLUSH: c_int = 0;
    /// `Z_FINISH` (`zlib.h` L176).
    const FINISH: c_int = 4;
    /// `windowBits` selecting a gzip container at the default window size
    /// (`zlib.h` L588-L590: 16 added to the window size).
    const GZIP: c_int = 15 + 16;

    /// The payload `test/example.c` L35 declares, as L69 passes it -- the terminating
    /// NUL included.
    const HELLO: &[u8] = b"hello, hello!\0";

    /// A zeroed `z_stream`, which is what a C caller declares before initialising.
    fn blank_stream() -> z_stream {
        z_stream {
            next_in: ptr::null(),
            avail_in: 0,
            total_in: 0,
            next_out: ptr::null_mut(),
            avail_out: 0,
            total_out: 0,
            msg: ptr::null(),
            state: ptr::null_mut(),
            zalloc: None,
            zfree: None,
            opaque: ptr::null_mut(),
            data_type: 0,
            adler: 0,
            reserved: 0,
        }
    }

    /// The `version` and `stream_size` arguments the `deflateInit` macros supply.
    fn version_args() -> (*const c_char, c_int) {
        (
            ZLIB_VERSION.as_ptr(),
            c_int::try_from(size_of::<z_stream>()).unwrap(),
        )
    }

    /// `deflateInit_(strm, level)`, asserting success.
    fn init(strm: &mut z_stream, level: c_int) {
        let (version, size) = version_args();
        // SAFETY: `strm` is a live, zeroed `z_stream` and the version pair is this
        // library's own, so the version check and the size check both pass.
        let ret = unsafe { deflateInit_(strm, level, version, size) };
        assert_eq!(ret, OK, "deflateInit_({level})");
    }

    /// `deflateInit2_(strm, level, ..., window_bits, ...)`, asserting success.
    fn init2(strm: &mut z_stream, level: c_int, window_bits: c_int) {
        let (version, size) = version_args();
        // SAFETY: as `init`, with `window_bits` in the range `zlib.h` L580-L590 allows.
        let ret = unsafe { deflateInit2_(strm, level, 8, window_bits, 8, 0, version, size) };
        assert_eq!(ret, OK, "deflateInit2_({level}, {window_bits})");
    }

    /// A non-null pointer that must never be dereferenced.
    ///
    /// ★ The whole point of the guard-ordering tests below. `deflate.c` returns before it
    /// reads through either `head` (L716, before L717 assigns it) or `dictionary`
    /// (L572, before L575 checksums it), so a caller whose stream is in the wrong state
    /// may legitimately pass a pointer that is stale, misaligned or wild and still be
    /// told `Z_STREAM_ERROR`. Reading through this address would fault, and under Miri
    /// it is caught as the invalid dereference it is -- which is what makes it evidence.
    ///
    /// An ordinary integer-to-pointer cast rather than
    /// `core::ptr::without_provenance_mut`, which is `strict_provenance` and so was
    /// stabilised in Rust 1.84 -- past this workspace's 1.80 floor.
    fn wild<T>() -> *mut T {
        0xdead_0001_usize as *mut T
    }

    #[test]
    fn overlapping_input_and_output_are_served_from_a_snapshot() {
        // ★ C runs the compressor over a single buffer it both reads and writes, and
        // `test/example.c` depends on it: `test_large_deflate` L275-L277 sets `next_in` back
        // to the start of the buffer `next_out` is already writing into, and
        // `test_large_inflate` L327 then asserts that all of those bytes were consumed by
        // that one call. So the pair must be SERVED, not refused -- the input is snapshotted
        // and the compressor reads the snapshot, which is what keeps the two borrows apart.
        // Consuming everything is the property the suite actually depends on.
        let mut strm = blank_stream();
        init(&mut strm, 6);

        let mut buffer = vec![0x5a_u8; 256];
        buffer[..HELLO.len()].copy_from_slice(HELLO);

        strm.next_in = buffer.as_ptr();
        strm.avail_in = c_uint::try_from(HELLO.len()).unwrap();
        // The output window starts inside the input, so the two share bytes.
        strm.next_out = buffer[4..].as_mut_ptr();
        strm.avail_out = 128;

        // SAFETY: `strm` holds a state this module installed, and both buffer members
        // address `buffer`, which is live for their counts. Overlap is permitted.
        let ret = unsafe { deflate(&mut strm, FINISH) };
        assert_eq!(
            ret, STREAM_END,
            "an overlapping pair must compress, not fail"
        );
        assert_eq!(
            strm.total_in,
            uLong::try_from(HELLO.len()).unwrap(),
            "every input byte must be consumed, which is what example.c asserts"
        );
        assert!(strm.total_out > 0, "and output must have been produced");
        assert_eq!(strm.avail_in, 0);

        // The stream is well formed and decodes back to the ORIGINAL bytes, because the
        // snapshot was taken before the first output byte landed on top of them.
        let produced = usize::try_from(strm.total_out).unwrap();
        let mut round_trip = vec![0_u8; 64];
        let mut d = blank_stream();
        let (version, size) = version_args();
        assert_eq!(
            // SAFETY: a blank stream, and this library's own version/size pair, so both the
            // version check and the size check pass.
            unsafe { crate::inflate::inflateInit_(&mut d, version, size) },
            OK
        );
        d.next_in = buffer[4..].as_ptr();
        d.avail_in = c_uint::try_from(produced).unwrap();
        d.next_out = round_trip.as_mut_ptr();
        d.avail_out = c_uint::try_from(round_trip.len()).unwrap();
        assert_eq!(
            // SAFETY: two disjoint live buffers on an initialised inflate stream.
            unsafe { crate::inflate::inflate(&mut d, FINISH) },
            STREAM_END
        );
        assert_eq!(&round_trip[..HELLO.len()], HELLO);
        // SAFETY: the stream is the initialised one.
        assert_eq!(unsafe { crate::inflate::inflateEnd(&mut d) }, OK);

        // SAFETY: as above; the state is still installed.
        assert_eq!(unsafe { deflateEnd(&mut strm) }, OK);
    }

    #[test]
    fn adjacent_input_and_output_are_accepted() {
        // Disjointness is what is required, not distance: an output buffer that begins one
        // byte past the end of the input shares nothing with it and must be accepted, or
        // the check would reject the tightly packed buffers real callers use.
        let mut strm = blank_stream();
        init(&mut strm, 6);

        let mut buffer = vec![0_u8; 512];
        buffer[..HELLO.len()].copy_from_slice(HELLO);
        let (input, output) = buffer.split_at_mut(HELLO.len());

        strm.next_in = input.as_ptr();
        strm.avail_in = c_uint::try_from(input.len()).unwrap();
        strm.next_out = output.as_mut_ptr();
        strm.avail_out = c_uint::try_from(output.len()).unwrap();

        // SAFETY: the two halves of one split are disjoint and live for their counts, and
        // `strm` holds a state this module installed.
        let ret = unsafe { deflate(&mut strm, FINISH) };
        assert_eq!(ret, STREAM_END, "adjacent buffers must compress normally");
        assert_eq!(strm.total_in, uLong::try_from(HELLO.len()).unwrap());
        assert!(strm.total_out > 0);

        // SAFETY: as above.
        assert_eq!(unsafe { deflateEnd(&mut strm) }, OK);
    }

    #[test]
    fn deflate_params_serves_an_overlapping_pair_too() {
        // `deflateParams` calls `deflate` internally to flush what the old parameters
        // produced (`deflate.c` L795), so it reads and writes the same two buffer pairs and
        // takes the same snapshot. The inner call is only reached once `last_flush` has
        // left its post-reset value and the new parameters actually select a different
        // compression function (L788), which is why the stream below is used first and the
        // level moves from 1 (`deflate_fast`) to 9 (`deflate_slow`).
        let mut strm = blank_stream();
        init(&mut strm, 1);

        let mut warmup = vec![0_u8; 256];
        strm.next_in = HELLO.as_ptr();
        strm.avail_in = c_uint::try_from(HELLO.len()).unwrap();
        strm.next_out = warmup.as_mut_ptr();
        strm.avail_out = c_uint::try_from(warmup.len()).unwrap();
        // SAFETY: two disjoint live buffers, and a state this module installed.
        assert_eq!(unsafe { deflate(&mut strm, NO_FLUSH) }, OK);

        let mut buffer = vec![0x5a_u8; 256];
        strm.next_in = buffer.as_ptr();
        strm.avail_in = 32;
        strm.next_out = buffer.as_mut_ptr();
        strm.avail_out = 32;

        // SAFETY: `strm` holds a state this module installed and both members address
        // `buffer`, live for their counts. Overlap is permitted.
        let ret = unsafe { deflateParams(&mut strm, 9, 0) };
        assert_eq!(ret, OK, "an overlapping pair must be served, not refused");

        // `Z_DATA_ERROR` rather than `Z_OK`: the stream is in `BUSY_STATE`, which
        // `zlib.h` L373-L377 makes the documented answer for a premature teardown.
        // SAFETY: as above.
        assert_eq!(unsafe { deflateEnd(&mut strm) }, DATA_ERROR);
    }

    #[test]
    fn set_header_on_a_non_gzip_stream_never_reads_the_header() {
        // `deflate.c` L715-L716 returns on `wrap != 2` *before* L717 assigns the pointer,
        // so a zlib or raw stream never causes C to read one member of the structure.
        // Building a `GzHeaderView` reads eight members and scans two NUL-terminated
        // strings, so the predicate has to come first. A wild pointer is the only way to
        // assert that it does.
        for window_bits in [15, -15] {
            let mut strm = blank_stream();
            init2(&mut strm, 6, window_bits);

            // SAFETY: `strm` holds a state this module installed. `head` is deliberately
            // not dereferenceable, and the assertion below is that it is not dereferenced.
            let ret = unsafe { deflateSetHeader(&mut strm, wild::<gz_header>()) };
            assert_eq!(
                ret, STREAM_ERROR,
                "windowBits {window_bits} is not a gzip stream"
            );

            // SAFETY: as above.
            assert_eq!(unsafe { deflateEnd(&mut strm) }, OK);
        }
    }

    #[test]
    fn set_dictionary_in_the_wrong_state_never_reads_the_dictionary() {
        // `deflate.c` L570-L572 returns before `adler32` (L575) or `read_buf` (L237) has
        // read a dictionary byte. Two states reach that return: a gzip stream, which can
        // never take a dictionary at all, and a stream that has already consumed input.
        let mut gzip = blank_stream();
        init2(&mut gzip, 6, GZIP);
        // SAFETY: `gzip` holds a state this module installed; `dictionary` is deliberately
        // not dereferenceable and the assertion is that it is not dereferenced.
        let ret = unsafe { deflateSetDictionary(&mut gzip, wild::<Bytef>().cast_const(), 8) };
        assert_eq!(ret, STREAM_ERROR, "wrap == 2 refuses a dictionary");
        // SAFETY: as above.
        assert_eq!(unsafe { deflateEnd(&mut gzip) }, OK);

        let mut used = blank_stream();
        init(&mut used, 6);
        let mut out = vec![0_u8; 256];
        used.next_in = HELLO.as_ptr();
        used.avail_in = c_uint::try_from(HELLO.len()).unwrap();
        used.next_out = out.as_mut_ptr();
        used.avail_out = c_uint::try_from(out.len()).unwrap();
        // SAFETY: two disjoint live buffers, and a state this module installed.
        assert_eq!(unsafe { deflate(&mut used, NO_FLUSH) }, OK);

        // SAFETY: as the gzip case -- the pointer must not be read, and is not.
        let ret = unsafe { deflateSetDictionary(&mut used, wild::<Bytef>().cast_const(), 8) };
        assert_eq!(ret, STREAM_ERROR, "a consumed stream refuses a dictionary");
        // `Z_DATA_ERROR`, because the stream is in `BUSY_STATE` -- the premature-teardown
        // report `zlib.h` L373-L377 documents, not a failure of the call above.
        // SAFETY: as above.
        assert_eq!(unsafe { deflateEnd(&mut used) }, DATA_ERROR);
    }

    #[test]
    fn the_header_fields_may_be_freed_once_the_header_has_been_written() {
        // ★ The lifetime contract `zlib.h` L838-L855 states: the `gz_header` and its three
        // buffers must stay available *while the header is being written*, and no longer.
        // A conforming caller therefore frees them as soon as the header is out, and a
        // library that kept borrowing them would hold dangling references from that moment
        // on -- undefined behaviour in Rust whether or not it ever reads them again, and a
        // use-after-free the moment `deflateBound` scans the two strings.
        const EXTRA: &[u8] = &[0xde, 0xad, 0xbe, 0xef];
        const NAME: &[u8] = b"probe.txt\0";
        const COMMENT: &[u8] = b"a comment\0";

        let mut strm = blank_stream();
        init2(&mut strm, 6, GZIP);

        // Heap storage, so that dropping it really returns the pages and a retained borrow
        // really dangles.
        let mut extra: Vec<u8> = EXTRA.to_vec();
        let mut name: Vec<u8> = NAME.to_vec();
        let mut comment: Vec<u8> = COMMENT.to_vec();
        let mut head = gz_header {
            text: 1,
            time: 0x1234_5678,
            xflags: 0,
            os: 3,
            extra: extra.as_mut_ptr(),
            extra_len: uInt::try_from(extra.len()).unwrap(),
            extra_max: 0,
            name: name.as_mut_ptr(),
            name_max: 0,
            comment: comment.as_mut_ptr(),
            comm_max: 0,
            hcrc: 1,
            done: 0,
        };

        // `ptr::addr_of_mut!` rather than `&raw mut`, which is Rust 1.82 syntax and this
        // workspace's floor is 1.80. Neither forms a reference, which is the point: the
        // pointer handed to C must carry the struct's own provenance.
        let head_ptr: gz_headerp = ptr::addr_of_mut!(head);
        // SAFETY: `strm` holds a gzip state this module installed, and `head` is a live,
        // aligned `gz_header` whose three buffers are live, NUL-terminated where the
        // contract requires it, and readable for the lengths given.
        assert_eq!(unsafe { deflateSetHeader(&mut strm, head_ptr) }, OK);

        // The bound while the borrows are live. `deflate.c` L895-L906 counts
        // `2 + extra_len`, then one byte per name character plus its terminator, then the
        // same for the comment.
        // SAFETY: `strm` holds a state this module installed; nothing is dereferenced but
        // the stream itself.
        let bound_before =
            unsafe { deflateBound(&mut strm, uLong::try_from(HELLO.len()).unwrap()) };

        // One call with room to spare writes the whole gzip header and leaves the status at
        // `BUSY_STATE` -- the point at which the release happens.
        let mut out = vec![0_u8; 512];
        strm.next_in = HELLO.as_ptr();
        strm.avail_in = c_uint::try_from(HELLO.len()).unwrap();
        strm.next_out = out.as_mut_ptr();
        strm.avail_out = c_uint::try_from(out.len()).unwrap();
        // SAFETY: two disjoint live buffers and a state this module installed.
        assert_eq!(unsafe { deflate(&mut strm, NO_FLUSH) }, OK);

        let header_bytes = usize::try_from(strm.total_out).unwrap();
        assert!(
            header_bytes >= 10 + 2 + EXTRA.len() + NAME.len() + COMMENT.len() + 2,
            "the whole gzip header must be out before the buffers are freed"
        );

        // The caller now does what the documentation permits: it frees all three buffers
        // and the header struct's pointers along with them.
        extra.clear();
        extra.shrink_to_fit();
        name.clear();
        name.shrink_to_fit();
        comment.clear();
        comment.shrink_to_fit();
        drop(extra);
        drop(name);
        drop(comment);
        head.extra = ptr::null_mut();
        head.name = ptr::null_mut();
        head.comment = ptr::null_mut();

        // Read back, which is both what makes those three writes meaningful and a check
        // that nothing in the library restored them: `deflate` never writes through a
        // `gz_headerp` -- `deflate.c` only ever reads `s->gzhead` -- so the struct still
        // holds exactly what the caller last put in it, `done` included.
        assert!(head.extra.is_null());
        assert!(head.name.is_null());
        assert!(head.comment.is_null());
        assert_eq!(head.done, 0, "writing a header never sets `done`");

        // Neither of these two may read a freed byte. The bound does *change*, and that is
        // the reference's own behaviour rather than a shortcoming: `deflate.c` L895-L906
        // re-reads `s->gzhead`'s three pointers on every `deflateBound` call, so a header
        // whose pointers the caller has since nulled costs nothing extra. The drop is
        // exactly the field cost C would have added -- `2 + extra_len` for the extra field,
        // and one byte per name and comment character including each terminator -- which is
        // what proves the earlier value came from those fields and that the later one read
        // the *pointers* rather than the bytes behind them.
        // SAFETY: as the earlier call.
        let bound_after = unsafe { deflateBound(&mut strm, uLong::try_from(HELLO.len()).unwrap()) };
        let field_cost = uLong::try_from(2 + EXTRA.len() + NAME.len() + COMMENT.len()).unwrap();
        assert_eq!(
            bound_before - bound_after,
            field_cost,
            "the bound must shed exactly the header fields the caller withdrew"
        );

        // SAFETY: `out` is still live and disjoint from `HELLO`, and the state is installed.
        assert_eq!(unsafe { deflate(&mut strm, FINISH) }, STREAM_END);
        let produced = usize::try_from(strm.total_out).unwrap();
        // SAFETY: as above.
        assert_eq!(unsafe { deflateEnd(&mut strm) }, OK);

        // The header that was emitted before the release is intact and complete.
        assert_eq!(&out[..3], &[0x1f, 0x8b, 0x08], "ID1, ID2, CM");
        assert_eq!(out[3], 0x01 | 0x02 | 0x04 | 0x08 | 0x10, "every FLG bit");
        let xlen = usize::from(u16::from_le_bytes([out[10], out[11]]));
        assert_eq!(xlen, EXTRA.len());
        let mut cursor = 12;
        assert_eq!(&out[cursor..cursor + EXTRA.len()], EXTRA);
        cursor += EXTRA.len();
        assert_eq!(&out[cursor..cursor + NAME.len()], NAME);
        cursor += NAME.len();
        assert_eq!(&out[cursor..cursor + COMMENT.len()], COMMENT);
        assert!(produced > cursor + COMMENT.len() + 2);
    }

    /// The `gz_header` **structure itself** may be freed once the header is written,
    /// and compression continues to `Z_STREAM_END` without reading it again.
    ///
    /// ★ The test above withdraws the three *buffers* and keeps the structure, because
    /// it goes on to call `deflateBound`, which C re-reads the structure for on every
    /// call (`deflate.c` L891-L910). This one is the other half of the same paragraph
    /// of `zlib.h` (L836-L852): a caller that is done with the header releases the
    /// whole allocation, and from that moment `deflate()` must not touch it. C honours
    /// that because every `s->gzhead` dereference in `deflate()` is guarded by a
    /// header status (L1066-L1200) and `HCRC_STATE` hands over to `BUSY_STATE` at
    /// L1195, never to return.
    ///
    /// [`with_header_while_emitting`] is what holds that here: it is the only path that
    /// forms a `GzHeaderView`, and it forms one only while a header status says the
    /// emission is still in progress. Rebuilding the view on entry to every `deflate`
    /// call instead -- seven member reads plus a NUL scan of `name` and `comment` --
    /// would read a structure the caller was entitled to release, on every call after
    /// the header was written. The allocation below is a `Box` so that such a read is a
    /// `heap-use-after-free` under `AddressSanitizer` rather than a silent one.
    #[test]
    fn the_header_structure_itself_may_be_freed_once_the_header_has_been_written() {
        const EXTRA: &[u8] = &[0x11, 0x22, 0x33];
        const NAME: &[u8] = b"freed.txt\0";
        const COMMENT: &[u8] = b"gone\0";

        let mut strm = blank_stream();
        init2(&mut strm, 6, GZIP);

        let mut extra: Vec<u8> = EXTRA.to_vec();
        let mut name: Vec<u8> = NAME.to_vec();
        let mut comment: Vec<u8> = COMMENT.to_vec();
        let head: gz_headerp = Box::into_raw(Box::new(gz_header {
            text: 1,
            time: 0x0102_0304,
            xflags: 0,
            os: 3,
            extra: extra.as_mut_ptr(),
            extra_len: uInt::try_from(extra.len()).unwrap(),
            extra_max: 0,
            name: name.as_mut_ptr(),
            name_max: 0,
            comment: comment.as_mut_ptr(),
            comm_max: 0,
            hcrc: 1,
            done: 0,
        }));
        // SAFETY: `strm` holds a gzip state this module installed, and `head` is a
        // live, aligned `gz_header` whose three buffers are live and NUL-terminated
        // where the contract requires it.
        assert_eq!(unsafe { deflateSetHeader(&mut strm, head) }, OK);

        // One call with room to spare puts the whole header out and leaves the status
        // at `BUSY_STATE` -- the point from which nothing may read the structure.
        let mut out = vec![0_u8; 512];
        strm.next_in = HELLO.as_ptr();
        strm.avail_in = c_uint::try_from(HELLO.len()).unwrap();
        strm.next_out = out.as_mut_ptr();
        strm.avail_out = c_uint::try_from(out.len()).unwrap();
        // SAFETY: two disjoint live buffers and a state this module installed.
        assert_eq!(unsafe { deflate(&mut strm, NO_FLUSH) }, OK);
        let header_bytes = usize::try_from(strm.total_out).unwrap();
        assert!(
            header_bytes >= 10 + 2 + EXTRA.len() + NAME.len() + COMMENT.len() + 2,
            "the whole gzip header must be out before the release"
        );

        // The caller releases everything: the structure and all three buffers.
        // SAFETY: `head` came from `Box::into_raw` above and is released exactly once.
        drop(unsafe { Box::from_raw(head) });
        drop(extra);
        drop(name);
        drop(comment);

        // Finishing the stream must not read one byte of any of them.
        // SAFETY: `out` is live and disjoint from `HELLO`, and the state is installed.
        assert_eq!(unsafe { deflate(&mut strm, FINISH) }, STREAM_END);
        let produced = usize::try_from(strm.total_out).unwrap();
        // SAFETY: as above.
        assert_eq!(unsafe { deflateEnd(&mut strm) }, OK);

        // The header emitted before the release is intact, which is what proves the
        // fields were read while they were still the caller's to read.
        assert_eq!(&out[..3], &[0x1f, 0x8b, 0x08], "ID1, ID2, CM");
        assert_eq!(out[3], 0x01 | 0x02 | 0x04 | 0x08 | 0x10, "every FLG bit");
        assert_eq!(&out[4..8], &0x0102_0304_u32.to_le_bytes(), "MTIME");
        let xlen = usize::from(u16::from_le_bytes([out[10], out[11]]));
        assert_eq!(xlen, EXTRA.len());
        let mut cursor = 12;
        assert_eq!(&out[cursor..cursor + EXTRA.len()], EXTRA);
        cursor += EXTRA.len();
        assert_eq!(&out[cursor..cursor + NAME.len()], NAME);
        cursor += NAME.len();
        assert_eq!(&out[cursor..cursor + COMMENT.len()], COMMENT);
        // The payload and the eight-byte gzip trailer follow the two HCRC bytes.
        assert!(produced > cursor + COMMENT.len() + 2 + 8);
    }
}

// `clippy::indexing_slicing` is denied workspace-wide and is relaxed HERE ONLY, on
// the test module: every index below is a literal into a buffer or a request log this
// module has just checked the length of, so each one is provably in range. The reason
// is spelled out in `crate::panic_guard`'s module documentation, which also records why
// there is no `allow-indexing-slicing-in-tests` key to use instead.
//
// `clippy::undocumented_unsafe_blocks` is NOT relaxed. It is denied workspace-wide and
// stays denied here, so every `unsafe` block below carries its own `// SAFETY:` naming
// the invariant that discharges it, exactly as the library half of this file does and
// as the test harnesses in `crate::gz` and `crate::infback` do. That is what makes the
// module machine-checked rather than merely reviewed: there is no configuration in
// which an undocumented block can be added here without failing the build.
#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests_backend {
    //! Backend tests for the `deflate*` exports: the allocation sequence a caller's
    //! hooks observe, the `gz_header` lifetime contract, and `deflateCopy`'s failure
    //! behaviour.
    //!
    //! # The facts every `// SAFETY:` below builds on
    //!
    //! Four properties hold across the whole module, so the individual invariants
    //! name what is specific to their call rather than restating these:
    //!
    //! - Every `z_stream` reaches an export as `&mut z_stream` derived from a local
    //!   this module declared with [`blank_stream`], so the pointer the export
    //!   receives is non-null, aligned and valid for reads and writes for at least
    //!   `size_of::<z_stream>()` bytes. A stream is never moved after its `state` is
    //!   installed, which is the rule `deflate.c`'s `deflateStateCheck` enforces.
    //!   Where a null or foreign pointer is passed on purpose, the block says so.
    //! - Every `state` a call operates on is either null — before `deflateInit2_`, or
    //!   after `deflateEnd` — or the exact value this module's own `deflateInit2_`
    //!   installed and has not yet ended. No test hands an export a state another
    //!   test owns.
    //! - Every `gz_headerp` is a `Box::into_raw` allocation made in the same test,
    //!   live until that test's matching `Box::from_raw`, and its `extra`, `name` and
    //!   `comment` members point into `Vec`s that outlive the last call reading them.
    //!   `deflateSetHeader` only records the pointer, so the *structure* has to stay
    //!   live exactly as long as the emitting calls, which each test arranges.
    //! - `zalloc` and `zfree` are [`mem_alloc`] and [`mem_free`], and `opaque` is the
    //!   single `Box::into_raw` pointer a [`Zone`] owns. Memory obtained from
    //!   `mem_alloc` is returned only to `mem_free`, which is the contract
    //!   `test/infcover.c`'s `mem_done` checks and [`MemZone::assert_clean`]
    //!   reproduces.
    //!
    //! Tests may panic — that is how they report failure — so `clippy.toml` allows
    //! panicking here, and the `#[allow]` above re-enables indexing for the reason
    //! stated beside it.

    use super::{
        deflate, deflateBound, deflateCopy, deflateEnd, deflateInit2_, deflateParams,
        deflateSetHeader, DeflateSlot,
    };
    use core::alloc::Layout;
    use core::ffi::{c_char, c_int, c_void};
    use core::mem::size_of;

    use crate::types::{gz_header, uInt, uLong, voidpf, z_stream, StateBlock};
    use crate::util::ZLIB_VERSION;
    use zlib_rs::error::ReturnCode;

    /// `Z_DEFLATED`, `zlib.h` L196.
    const Z_DEFLATED: c_int = 8;
    /// `Z_NO_FLUSH`, `zlib.h` L172.
    const Z_NO_FLUSH: c_int = 0;
    /// `Z_FINISH`, `zlib.h` L177.
    const Z_FINISH: c_int = 4;
    /// `Z_DEFAULT_STRATEGY`, `zlib.h` L206.
    const Z_DEFAULT_STRATEGY: c_int = 0;
    /// The `windowBits` that select a gzip wrapper with the largest window: 15 + 16
    /// (`zlib.h` L586-L592). The only wrap for which a `gz_header` is honoured.
    const GZIP_15: c_int = 31;
    /// `DEF_MEM_LEVEL` on a target where `MAX_MEM_LEVEL` is 9 (`zutil.h` L52).
    const DEF_MEM_LEVEL: c_int = 8;

    // -----------------------------------------------------------------------
    // Harness
    // -----------------------------------------------------------------------

    /// A zeroed `z_stream`, which is what a C caller declares before initialising.
    fn blank_stream() -> z_stream {
        z_stream {
            next_in: core::ptr::null(),
            avail_in: 0,
            total_in: 0,
            next_out: core::ptr::null_mut(),
            avail_out: 0,
            total_out: 0,
            msg: core::ptr::null(),
            state: core::ptr::null_mut(),
            zalloc: None,
            zfree: None,
            opaque: core::ptr::null_mut(),
            data_type: 0,
            adler: 0,
            reserved: 0,
        }
    }

    /// A `gz_header` with every member cleared, as `test/example.c` L166-L177 builds
    /// one before filling in the members it cares about.
    fn blank_header() -> gz_header {
        gz_header {
            text: 0,
            time: 0,
            xflags: 0,
            os: 0,
            extra: core::ptr::null_mut(),
            extra_len: 0,
            extra_max: 0,
            name: core::ptr::null_mut(),
            name_max: 0,
            comment: core::ptr::null_mut(),
            comm_max: 0,
            hcrc: 0,
            done: 0,
        }
    }

    /// The `version` and `stream_size` pair the `deflateInit2` macro supplies.
    fn version_args() -> (*const c_char, c_int) {
        (
            ZLIB_VERSION.as_ptr(),
            c_int::try_from(size_of::<z_stream>()).expect("z_stream fits in a c_int"),
        )
    }

    /// A tracking allocator in the shape of `test/infcover.c`'s `mem_zone`, with the
    /// request *order* recorded as well as the live set.
    ///
    /// The order is what `deflate.c`'s allocation sequence has to be checked against:
    /// the state object first (L440), the four buffers after it (L468-L479), and the
    /// exact reverse on the way out (L1300-L1306).
    #[derive(Default)]
    struct MemZone {
        /// Live blocks, most recent first -- C's `mem_zone::first` list.
        live: Vec<(*mut u8, usize)>,
        /// Every block ever handed out, in request order, as (address, bytes).
        requested: Vec<(*mut u8, usize)>,
        /// Every address freed, in release order.
        released: Vec<*mut u8>,
        /// Live bytes.
        total: usize,
        /// Refuse every request from this one onward, counting from one. Zero means
        /// refuse nothing. C's `mem_limit` bounds bytes; a request count is the
        /// precise way to fail one *named* allocation.
        deny_from: usize,
        /// Frees that were not of the most recent block: C's `notlifo`.
        notlifo: usize,
        /// Frees of an address the zone never handed out: C's `rogue`.
        rogue: usize,
    }

    impl MemZone {
        /// C's `mem_done` (its L200-L234): everything back, in order, nothing stray.
        fn assert_clean(&self, what: &str) {
            assert!(
                self.live.is_empty(),
                "{what}: {} blocks leaked",
                self.live.len()
            );
            assert_eq!(self.total, 0, "{what}: bytes not freed");
            assert_eq!(self.notlifo, 0, "{what}: frees not LIFO");
            assert_eq!(self.rogue, 0, "{what}: frees not recognized");
        }
    }

    /// The layout a tracked block of `len` bytes is allocated with; 16-byte aligned,
    /// as C's `malloc` guarantees, and never zero-sized, which Rust's allocator
    /// forbids and C's `malloc` does not.
    fn tracked_layout(len: usize) -> Option<Layout> {
        Layout::from_size_align(len.max(1), 16).ok()
    }

    /// C's `mem_alloc` (its L71-L109), including the `0xa5` fill at its L87.
    unsafe extern "C" fn mem_alloc(opaque: voidpf, count: uInt, size: uInt) -> voidpf {
        let zone = opaque.cast::<MemZone>();
        if zone.is_null() {
            return core::ptr::null_mut();
        }
        // SAFETY: `opaque` is the pointer a [`Zone`] obtained from `Box::into_raw`, the null branch
        // above has already returned, and the library only ever hands back the value the caller
        // installed -- so it addresses that live `MemZone`. `mem_alloc` and `mem_free` are its only
        // other users and neither re-enters the other, so this is the sole live reference for the
        // rest of the call.
        let zone = unsafe { &mut *zone };
        let len = (count as usize) * (size as usize);
        if zone.deny_from != 0 && zone.requested.len() + 1 >= zone.deny_from {
            return core::ptr::null_mut();
        }
        let Some(layout) = tracked_layout(len) else {
            return core::ptr::null_mut();
        };
        // SAFETY: `tracked_layout` rejects nothing but a failed `from_size_align`, and its
        // `len.max(1)` guarantees a non-zero size, which is `alloc`'s one requirement. A null
        // return is handled on the next line.
        let ptr = unsafe { std::alloc::alloc(layout) };
        if ptr.is_null() {
            return core::ptr::null_mut();
        }
        // ★ Never zeros. This is the byte that catches code assuming zeroed memory.
        // SAFETY: `ptr` came from `alloc(layout)` and the check above has ruled out null, so it is
        // valid for writes for exactly `layout.size()` bytes; a `u8` write needs no further
        // alignment.
        unsafe { core::ptr::write_bytes(ptr, 0xa5, layout.size()) };
        zone.live.insert(0, (ptr, len));
        zone.requested.push((ptr, len));
        zone.total += len;
        ptr.cast()
    }

    /// C's `mem_free` (its L112-L154), including the `notlifo` and `rogue` counts.
    unsafe extern "C" fn mem_free(opaque: voidpf, address: voidpf) {
        let zone = opaque.cast::<MemZone>();
        if zone.is_null() {
            return;
        }
        // SAFETY: `opaque` is the pointer a [`Zone`] obtained from `Box::into_raw` and the null
        // branch above has already returned, so it addresses that live `MemZone`; as in
        // [`mem_alloc`], no other reference to it exists while this call runs.
        let zone = unsafe { &mut *zone };
        let found = zone
            .live
            .iter()
            .position(|&(at, _)| core::ptr::eq(at.cast::<c_void>(), address));
        let Some(index) = found else {
            zone.rogue += 1;
            return;
        };
        if index != 0 {
            zone.notlifo += 1;
        }
        let (at, len) = zone.live.remove(index);
        zone.total -= len;
        zone.released.push(at);
        if let Some(layout) = tracked_layout(len) {
            // SAFETY: `at` was handed out by [`mem_alloc`], which allocated it with the layout
            // `tracked_layout` returns for this same `len`, and it has just been removed from
            // `live` -- so it is deallocated exactly once, with the layout it was allocated with.
            unsafe { std::alloc::dealloc(at, layout) };
        }
    }

    /// A [`MemZone`] on the heap, reached only through the one raw pointer it owns.
    ///
    /// ★ **A tracking zone cannot live in a local.** The pointer installed in
    /// `z_stream.opaque` is what [`mem_alloc`] and [`mem_free`] dereference, and a
    /// later write through the *local* -- `zone.deny_from = 6` -- is a write through
    /// the local's own tag, which invalidates every pointer derived from it. The next
    /// `zalloc` then dereferences a dead tag, and Miri rejects the test. A C caller has
    /// no such rule; this is a property of the harness, not of the library.
    ///
    /// So the zone is heap-allocated once, this raw pointer is the only handle, and
    /// every access -- the test's through [`Zone::with`] and the hooks' through
    /// `opaque` -- derives from it. That is also how `test/infcover.c` holds its own
    /// zone, which makes the harness faithful rather than merely acceptable.
    struct Zone(*mut MemZone);

    impl Zone {
        /// A zone that refuses nothing.
        fn new() -> Self {
            Self::with_deny_from(0)
        }

        /// A zone that refuses request `deny_from` and every later one, counting from
        /// one; zero refuses nothing.
        fn with_deny_from(deny_from: usize) -> Self {
            Self(Box::into_raw(Box::new(MemZone {
                deny_from,
                ..MemZone::default()
            })))
        }

        /// The pointer to install as `z_stream.opaque`.
        fn as_opaque(&self) -> voidpf {
            self.0.cast::<c_void>()
        }

        /// Borrows the zone through its one raw pointer, for the duration of `body`.
        fn with<R>(&self, body: impl FnOnce(&mut MemZone) -> R) -> R {
            // SAFETY: the pointer came from `Box::into_raw` in `Zone::with_deny_from`,
            // is live until `Zone::drop`, and is aligned and unique. No other reference
            // to the zone exists while `body` runs: the library forms one only inside
            // `mem_alloc` and `mem_free`, and neither can be executing while this
            // statement is.
            body(unsafe { &mut *self.0 })
        }

        /// Starts refusing at request `at`; zero refuses nothing.
        fn deny_from(&self, at: usize) {
            self.with(|zone| zone.deny_from = at);
        }

        /// C's `mem_done` (its L200-L234), through the zone's one pointer.
        fn assert_clean(&self, what: &str) {
            self.with(|zone| zone.assert_clean(what));
        }
    }

    impl Drop for Zone {
        /// Reclaims the allocation. The `mem_done` equivalent is
        /// [`Zone::assert_clean`], which each test calls where C calls it, so this
        /// deliberately asserts nothing: a panic here would replace a test's own
        /// diagnosis with a less specific one.
        fn drop(&mut self) {
            // SAFETY: the pointer came from `Box::into_raw` and this is the only place
            // it is reclaimed, so it is reclaimed exactly once.
            drop(unsafe { Box::from_raw(self.0) });
        }
    }

    /// Points a stream at `zone` as `test/infcover.c`'s `mem_setup` does.
    fn track(strm: &mut z_stream, zone: &Zone) {
        strm.zalloc = Some(mem_alloc);
        strm.zfree = Some(mem_free);
        strm.opaque = zone.as_opaque();
    }

    /// Initialises a gzip-wrapped compressor at the default settings.
    unsafe fn init_gzip(strm: &mut z_stream) -> c_int {
        let (version, size) = version_args();
        // SAFETY: `strm` arrives as `&mut z_stream`, so the pointer is non-null, aligned and valid
        // for the whole structure; `version` is `ZLIB_VERSION`'s NUL-terminated bytes and `size` is
        // `size_of::<z_stream>()`, which is the pair the `deflateInit2` macro supplies. Callers
        // pass a stream whose `state` is still null.
        unsafe {
            deflateInit2_(
                strm,
                6,
                Z_DEFLATED,
                GZIP_15,
                DEF_MEM_LEVEL,
                Z_DEFAULT_STRATEGY,
                version,
                size,
            )
        }
    }

    // -----------------------------------------------------------------------
    // M3 -- the allocation and release sequence a caller's hooks observe
    // -----------------------------------------------------------------------

    /// `deflateInit2_` asks for the state object before the four buffers, and
    /// `deflateEnd` gives it back after them.
    ///
    /// `deflate.c` L440 takes the state first and L468-L479 take `window`, `prev`,
    /// `head` and `pending_buf`; L1300-L1306 release `pending_buf`, `head`, `prev`,
    /// `window`, state. So the sequence a caller's hooks see is strictly
    /// last-in-first-out, which is exactly what `test/infcover.c`'s `mem_free`
    /// checks for -- it counts any other order as a `notlifo` defect.
    #[test]
    fn the_state_object_is_the_first_block_requested_and_the_last_returned() {
        let zone = Zone::new();
        let mut strm = blank_stream();
        track(&mut strm, &zone);

        // SAFETY: `strm` is this test's own blank stream: its `state` is still null, so nothing is
        // leaked by initialising it, and `init_gzip` takes it by `&mut` so the pointer is non-null,
        // aligned and valid for the whole structure.
        assert_eq!(unsafe { init_gzip(&mut strm) }, ReturnCode::OK.as_i32());

        let requested = zone.with(|zone| zone.requested.clone());
        assert_eq!(
            requested.len(),
            5,
            "deflate.c L440 plus L468-L479: one state object and four buffers"
        );
        assert_eq!(
            requested[0].1,
            size_of::<StateBlock<DeflateSlot>>(),
            "the first request is the state object itself"
        );
        assert!(
            requested[1..].iter().all(|&(_, len)| len == 65_536),
            "the four buffers at the default settings are 64 KiB each: {requested:?}"
        );
        let state_block = requested[0].0;

        // SAFETY: `strm` still holds the state `init_gzip` installed and has not been ended, so
        // `deflateEnd` releases each block exactly once through the same `zfree` it was allocated
        // with.
        assert_eq!(unsafe { deflateEnd(&mut strm) }, ReturnCode::OK.as_i32());
        assert!(strm.state.is_null());

        let released = zone.with(|zone| zone.released.clone());
        assert_eq!(released.len(), 5);
        assert!(
            core::ptr::eq(*released.last().expect("five frees"), state_block),
            "the state object is released last, after all four buffers"
        );
        let reverse: Vec<*mut u8> = requested.iter().rev().map(|&(at, _)| at).collect();
        assert_eq!(
            released, reverse,
            "the release order is the exact reverse of the request order"
        );
        zone.assert_clean("deflateInit2_ then deflateEnd");
    }

    /// A refused *buffer* request undoes the state object too, and reports it the
    /// way C does.
    ///
    /// `deflate.c` L508-L513 sets `strm->msg` to the out-of-memory string and then
    /// calls `deflateEnd(strm)`, which clears `strm->state`. Nothing may be left
    /// behind, and the blocks that were taken must come back last-in-first-out.
    #[test]
    fn a_refused_buffer_request_returns_the_state_object_as_well() {
        for deny_from in 2..=5 {
            let zone = Zone::with_deny_from(deny_from);
            let mut strm = blank_stream();
            track(&mut strm, &zone);

            assert_eq!(
                // SAFETY: `strm` is this test's own blank stream: its `state` is still null, so
                // nothing is leaked by initialising it, and `init_gzip` takes it by `&mut` so the
                // pointer is non-null, aligned and valid for the whole structure.
                unsafe { init_gzip(&mut strm) },
                ReturnCode::MEM_ERROR.as_i32(),
                "request {deny_from} refused"
            );
            assert!(
                strm.state.is_null(),
                "deflate.c L512's deflateEnd clears it"
            );
            assert!(!strm.msg.is_null(), "deflate.c L511 sets the message");
            let (released, state_block) =
                zone.with(|zone| (zone.released.clone(), zone.requested[0].0));
            assert_eq!(
                released.len(),
                deny_from - 1,
                "every block taken before the refusal is returned"
            );
            assert!(
                core::ptr::eq(
                    *released.last().expect("at least the state object"),
                    state_block
                ),
                "the state object is still the last one back"
            );
            zone.assert_clean("a refused buffer request");
        }
    }

    /// A refused *state* request leaves the caller's stream exactly as it was.
    ///
    /// `deflate.c` L443-L444 returns before L445, so `strm->state` is never written
    /// and no message is set.
    #[test]
    fn a_refused_state_request_writes_nothing_back() {
        let zone = Zone::with_deny_from(1);
        let mut strm = blank_stream();
        track(&mut strm, &zone);

        assert_eq!(
            // SAFETY: `strm` is this test's own blank stream: its `state` is still null, so nothing
            // is leaked by initialising it, and `init_gzip` takes it by `&mut` so the pointer is
            // non-null, aligned and valid for the whole structure.
            unsafe { init_gzip(&mut strm) },
            ReturnCode::MEM_ERROR.as_i32()
        );
        assert!(strm.state.is_null());
        assert!(zone.with(|zone| zone.requested.is_empty()));
        zone.assert_clean("a refused state request");
    }

    /// An invalid parameter set is rejected without asking the allocator for
    /// anything, because `deflate.c` validates at L419-L438 and allocates at L440.
    #[test]
    fn an_invalid_parameter_set_allocates_nothing() {
        let zone = Zone::new();
        let mut strm = blank_stream();
        track(&mut strm, &zone);
        let (version, size) = version_args();

        // `memLevel` 10 exceeds `MAX_MEM_LEVEL` (`deflate.c` L434).
        // SAFETY: as [`init_gzip`], except that `memLevel` is deliberately 10. `deflate.c` L434
        // rejects that before it allocates, so this exercises validation rather than
        // initialisation; `strm` is still a blank stream and `version`/`size` are the macro's pair.
        let ret = unsafe {
            deflateInit2_(
                &mut strm,
                6,
                Z_DEFLATED,
                GZIP_15,
                10,
                Z_DEFAULT_STRATEGY,
                version,
                size,
            )
        };
        assert_eq!(ret, ReturnCode::STREAM_ERROR.as_i32());
        assert!(
            zone.with(|zone| zone.requested.is_empty()),
            "C validates before it allocates, so nothing may be requested"
        );
        assert!(strm.state.is_null(), "deflate.c L438 leaves state alone");
        zone.assert_clean("an invalid parameter set");
    }

    // -----------------------------------------------------------------------
    // C5 -- the header is read through the caller's pointer, every time
    // -----------------------------------------------------------------------

    /// A `gz_header` edited *after* `deflateSetHeader` is the one that gets emitted.
    ///
    /// `deflate.c` L717 stores the caller's pointer -- `s->gzhead = head` -- and
    /// L1092-L1182 read every field through it when the header is written out, which
    /// is a later call. `zlib.h` L838-L845 requires the structure to stay available
    /// until `deflate` finishes emitting it, so the pointer, not a copy of what it
    /// pointed at, is the contract.
    #[test]
    fn a_header_edited_after_deflate_set_header_is_the_one_emitted() {
        let mut strm = blank_stream();
        // SAFETY: `strm` is this test's own blank stream: its `state` is still null, so nothing is
        // leaked by initialising it, and `init_gzip` takes it by `&mut` so the pointer is non-null,
        // aligned and valid for the whole structure.
        assert_eq!(unsafe { init_gzip(&mut strm) }, ReturnCode::OK.as_i32());

        // The header lives behind one raw pointer for its whole life, which is how a C
        // caller holds it: `deflateSetHeader` stores that pointer and `deflate` reads
        // through it later, so every access here goes through the same pointer rather
        // than alternating between it and the local.
        let head = Box::into_raw(Box::new(blank_header()));
        // SAFETY: `head` is the `Box::into_raw` allocation this test made and has not yet
        // reclaimed, so it is non-null, aligned and valid for writes for a whole `gz_header`. `os`
        // is a plain `c_int` member.
        unsafe { (*head).os = 3 };
        assert_eq!(
            // SAFETY: `strm` holds a gzip state this module installed, and `head` is the live,
            // aligned `gz_header` this test allocated; `deflateSetHeader` records the pointer
            // rather than copying the structure, and the structure and its buffers stay live until
            // this test reclaims them.
            unsafe { deflateSetHeader(&mut strm, head) },
            ReturnCode::OK.as_i32()
        );

        // Every edit below happens after the call that recorded the pointer.
        let mut name = *b"late.txt\0";
        // SAFETY: `head` is the `Box::into_raw` allocation this test made and has not yet
        // reclaimed, so it is non-null, aligned and valid for writes for a whole `gz_header`.
        // `name` is a local array that outlives every call below that reads it, and it is
        // NUL-terminated, which is what `deflate.c` requires of the member.
        unsafe {
            (*head).name = name.as_mut_ptr();
            (*head).time = uLong::from(0x0102_0304_u32);
            (*head).text = 1;
        }

        let input = b"hello, hello!";
        let mut out = [0_u8; 256];
        strm.next_in = input.as_ptr();
        strm.avail_in = uInt::try_from(input.len()).expect("fits");
        strm.next_out = out.as_mut_ptr();
        strm.avail_out = uInt::try_from(out.len()).expect("fits");
        assert_eq!(
            // SAFETY: `strm` holds a live state, and `next_in`/`avail_in` and
            // `next_out`/`avail_out` were set from the same slices whose lengths they carry, so
            // both extents are exact.
            unsafe { deflate(&mut strm, Z_FINISH) },
            ReturnCode::STREAM_END.as_i32()
        );
        let produced = out.len() - strm.avail_out as usize;
        let bytes = &out[..produced];

        assert_eq!(&bytes[0..3], &[0x1f, 0x8b, 0x08], "gzip magic and method");
        assert_eq!(
            bytes[3],
            0x01 | 0x08,
            "FTEXT and FNAME, from the late edits"
        );
        assert_eq!(
            &bytes[4..8],
            &[0x04, 0x03, 0x02, 0x01],
            "MTIME, little-endian, from the late edit"
        );
        assert_eq!(bytes[9], 3, "OS, set before the call");
        assert!(
            bytes[10..].starts_with(b"late.txt\0"),
            "the name assigned after deflateSetHeader is emitted"
        );

        // SAFETY: `strm` still holds the state `init_gzip` installed and has not been ended, so
        // `deflateEnd` releases each block exactly once through the same `zfree` it was allocated
        // with.
        assert_eq!(unsafe { deflateEnd(&mut strm) }, ReturnCode::OK.as_i32());
        // SAFETY: `head` came from `Box::into_raw` in this test, every stream that recorded it has
        // been ended, and this is the only place it is reclaimed -- so it is reclaimed exactly once
        // and nothing can read it afterwards.
        drop(unsafe { Box::from_raw(head) });
    }

    /// The same for `deflateBound`, which walks the name and comment strings itself.
    ///
    /// `deflate.c` L893-L910 reads `s->gzhead` through the stored pointer and counts
    /// `do { wraplen++; } while (*str++)` for each of `name` and `comment` -- so the
    /// bound must grow by the string length plus its terminator when a name appears,
    /// even though it appeared after `deflateSetHeader` was called.
    #[test]
    fn a_header_edited_after_deflate_set_header_changes_the_bound() {
        let mut strm = blank_stream();
        // SAFETY: `strm` is this test's own blank stream: its `state` is still null, so nothing is
        // leaked by initialising it, and `init_gzip` takes it by `&mut` so the pointer is non-null,
        // aligned and valid for the whole structure.
        assert_eq!(unsafe { init_gzip(&mut strm) }, ReturnCode::OK.as_i32());

        let head = Box::into_raw(Box::new(blank_header()));
        assert_eq!(
            // SAFETY: `strm` holds a gzip state this module installed, and `head` is the live,
            // aligned `gz_header` this test allocated; `deflateSetHeader` records the pointer
            // rather than copying the structure, and the structure and its buffers stay live until
            // this test reclaims them.
            unsafe { deflateSetHeader(&mut strm, head) },
            ReturnCode::OK.as_i32()
        );
        // SAFETY: `strm` holds a live gzip state whose recorded header is the `head` above, which
        // is what `deflateBound` re-reads on every call (`deflate.c` L891-L910); the structure is
        // still live.
        let empty = unsafe { deflateBound(&mut strm, 100) };

        let mut name = *b"late.txt\0";
        // SAFETY: `head` is the `Box::into_raw` allocation this test made and has not yet
        // reclaimed, so it is non-null, aligned and valid for writes for a whole `gz_header`.
        // `name` is a local that outlives the two `deflateBound` calls below.
        unsafe { (*head).name = name.as_mut_ptr() };
        // SAFETY: `strm` holds a live gzip state whose recorded header is the `head` above, which
        // is what `deflateBound` re-reads on every call (`deflate.c` L891-L910); the structure is
        // still live.
        let named = unsafe { deflateBound(&mut strm, 100) };
        assert_eq!(
            named,
            empty + 9,
            "eight name bytes and the terminator, counted live"
        );

        // SAFETY: `head` is the `Box::into_raw` allocation this test made and has not yet
        // reclaimed, so it is non-null, aligned and valid for writes for a whole `gz_header`.
        // `hcrc` is a plain `c_int` member.
        unsafe { (*head).hcrc = 1 };
        assert_eq!(
            // SAFETY: `strm` holds a live gzip state whose recorded header is the `head` above,
            // which is what `deflateBound` re-reads on every call (`deflate.c` L891-L910); the
            // structure is still live.
            unsafe { deflateBound(&mut strm, 100) },
            named + 2,
            "deflate.c L907-L908 adds two for the header CRC"
        );

        // SAFETY: `strm` still holds the state `init_gzip` installed and has not been ended, so
        // `deflateEnd` releases each block exactly once through the same `zfree` it was allocated
        // with.
        assert_eq!(unsafe { deflateEnd(&mut strm) }, ReturnCode::OK.as_i32());
        // SAFETY: `head` came from `Box::into_raw` in this test, every stream that recorded it has
        // been ended, and this is the only place it is reclaimed -- so it is reclaimed exactly once
        // and nothing can read it afterwards.
        drop(unsafe { Box::from_raw(head) });
    }

    /// A header set on the source is emitted by the copy, and still read live.
    ///
    /// `deflate.c` L1339's `zmemcpy(ds, ss, sizeof(deflate_state))` carries `gzhead`
    /// across verbatim and never adjusts it, so the copy emits the same caller-owned
    /// structure -- including edits made after the copy was taken.
    #[test]
    fn a_copy_carries_the_header_pointer_and_still_reads_it_live() {
        let mut strm = blank_stream();
        // SAFETY: `strm` is this test's own blank stream: its `state` is still null, so nothing is
        // leaked by initialising it, and `init_gzip` takes it by `&mut` so the pointer is non-null,
        // aligned and valid for the whole structure.
        assert_eq!(unsafe { init_gzip(&mut strm) }, ReturnCode::OK.as_i32());

        let head = Box::into_raw(Box::new(blank_header()));
        // SAFETY: `head` is the `Box::into_raw` allocation this test made and has not yet
        // reclaimed, so it is non-null, aligned and valid for writes for a whole `gz_header`. `os`
        // is a plain `c_int` member.
        unsafe { (*head).os = 3 };
        assert_eq!(
            // SAFETY: `strm` holds a gzip state this module installed, and `head` is the live,
            // aligned `gz_header` this test allocated; `deflateSetHeader` records the pointer
            // rather than copying the structure, and the structure and its buffers stay live until
            // this test reclaims them.
            unsafe { deflateSetHeader(&mut strm, head) },
            ReturnCode::OK.as_i32()
        );

        let mut dest = blank_stream();
        assert_eq!(
            // SAFETY: `dest` is a blank stream whose `state` is null and `strm` holds the live
            // state to copy; both arrive as `&mut`, so both pointers are non-null, aligned and
            // valid for a whole `z_stream`, and neither local moves afterwards.
            unsafe { deflateCopy(&mut dest, &mut strm) },
            ReturnCode::OK.as_i32()
        );

        // Edited after the copy was taken, and read by the copy.
        let mut name = *b"copied\0";
        // SAFETY: `head` is the `Box::into_raw` allocation this test made and has not yet
        // reclaimed, so it is non-null, aligned and valid for writes for a whole `gz_header`.
        // `name` is a local that outlives the `deflate` call below, which is the call that reads
        // it.
        unsafe { (*head).name = name.as_mut_ptr() };

        let input = b"hello, hello!";
        let mut out = [0_u8; 256];
        dest.next_in = input.as_ptr();
        dest.avail_in = uInt::try_from(input.len()).expect("fits");
        dest.next_out = out.as_mut_ptr();
        dest.avail_out = uInt::try_from(out.len()).expect("fits");
        assert_eq!(
            // SAFETY: `dest` holds a live state, and `next_in`/`avail_in` and
            // `next_out`/`avail_out` were set from the same slices whose lengths they carry, so
            // both extents are exact.
            unsafe { deflate(&mut dest, Z_FINISH) },
            ReturnCode::STREAM_END.as_i32()
        );
        let produced = out.len() - dest.avail_out as usize;
        assert!(
            out[10..produced].starts_with(b"copied\0"),
            "the copy emitted the header the source was given"
        );

        // SAFETY: `dest` holds the state `deflateCopy` installed and has not been ended, so
        // `deflateEnd` releases each of its blocks exactly once through the same `zfree` they came
        // from.
        assert_eq!(unsafe { deflateEnd(&mut dest) }, ReturnCode::OK.as_i32());
        // SAFETY: `strm` still holds the state `init_gzip` installed and has not been ended, so
        // `deflateEnd` releases each block exactly once through the same `zfree` it was allocated
        // with.
        assert_eq!(unsafe { deflateEnd(&mut strm) }, ReturnCode::OK.as_i32());
        // SAFETY: `head` came from `Box::into_raw` in this test, every stream that recorded it has
        // been ended, and this is the only place it is reclaimed -- so it is reclaimed exactly once
        // and nothing can read it afterwards.
        drop(unsafe { Box::from_raw(head) });
    }

    /// A header whose emission suspends mid-field is resumed from the caller's
    /// buffers by every later call -- including the `deflate` that `deflateParams`
    /// performs internally.
    ///
    /// This is the configuration in which storing the pointer and snapshotting it
    /// differ observably. `memLevel` 1 gives a 512-byte pending buffer, so a 4000-byte
    /// name and a 2000-byte extra field cannot be staged in one pass: `deflate.c`
    /// L1112-L1125 copies what fits, calls `flush_pending`, and returns `Z_OK` with
    /// `s->last_flush = -1` and the status still `EXTRA_STATE` or `NAME_STATE`. The
    /// next call reads `s->gzhead` again to continue -- and `deflateParams` is one such
    /// call, because L791-L794 flushes the block with `deflate(strm, Z_BLOCK)` whenever
    /// `last_flush != -2`.
    ///
    /// The assertions are on the emitted bytes rather than on a decode, so nothing here
    /// depends on the inflate side.
    #[test]
    fn a_suspended_header_is_resumed_across_deflate_params() {
        const NAME_LEN: usize = 4000;
        const EXTRA_LEN: usize = 2000;

        let mut strm = blank_stream();
        let (version, size) = version_args();
        // `memLevel` 1: the small pending buffer is what forces the suspension.
        // SAFETY: as [`init_gzip`], with `memLevel` 1 so that the pending buffer is small enough to
        // suspend the header emission this test needs; `strm` is a blank stream and
        // `version`/`size` are the macro's pair.
        let ret = unsafe {
            deflateInit2_(
                &mut strm,
                6,
                Z_DEFLATED,
                GZIP_15,
                1,
                Z_DEFAULT_STRATEGY,
                version,
                size,
            )
        };
        assert_eq!(ret, ReturnCode::OK.as_i32());

        let head = Box::into_raw(Box::new(blank_header()));
        assert_eq!(
            // SAFETY: `strm` holds a gzip state this module installed, and `head` is the live,
            // aligned `gz_header` this test allocated; `deflateSetHeader` records the pointer
            // rather than copying the structure, and the structure and its buffers stay live until
            // this test reclaims them.
            unsafe { deflateSetHeader(&mut strm, head) },
            ReturnCode::OK.as_i32()
        );
        let mut name = vec![b'n'; NAME_LEN + 1];
        name[NAME_LEN] = 0;
        let mut extra = vec![0x5a_u8; EXTRA_LEN];
        let mut comment = *b"the comment\0";
        // SAFETY: `head` is the `Box::into_raw` allocation this test made and has not yet
        // reclaimed, so it is non-null, aligned and valid for writes for a whole `gz_header`.
        // `name`, `extra` and `comment` are locals that outlive every `deflate` call below, `name`
        // and `comment` are NUL-terminated, and `extra_len` is `extra`'s own length.
        unsafe {
            (*head).text = 1;
            (*head).time = uLong::from(0x0a0b_0c0d_u32);
            (*head).os = 7;
            (*head).hcrc = 1;
            (*head).name = name.as_mut_ptr();
            (*head).extra = extra.as_mut_ptr();
            (*head).extra_len = uInt::try_from(EXTRA_LEN).expect("fits");
            (*head).comment = comment.as_mut_ptr();
        }

        let input = [b'a'; 4096];
        let mut produced: Vec<u8> = Vec::new();
        let mut fed = 0_usize;

        // Trickle three bytes at a time, which drains the pending buffer slowly and
        // leaves the emission suspended part-way through a field.
        for _ in 0..8 {
            let mut small = [0_u8; 3];
            strm.next_in = input.as_ptr();
            strm.avail_in = 16;
            strm.next_out = small.as_mut_ptr();
            strm.avail_out = 3;
            // SAFETY: `strm` holds a live state, and `next_in`/`avail_in` and
            // `next_out`/`avail_out` were set from the same slices whose lengths they carry, so
            // both extents are exact.
            let ret = unsafe { deflate(&mut strm, Z_NO_FLUSH) };
            assert_eq!(ret, ReturnCode::OK.as_i32());
            let got = 3 - strm.avail_out as usize;
            produced.extend_from_slice(&small[..got]);
        }
        fed += 16;

        // `deflateParams` with room to write: its internal `deflate(strm, Z_BLOCK)`
        // continues the suspended header, reading it through the stored pointer.
        let mut room = [0_u8; 64];
        strm.next_in = input.as_ptr();
        strm.avail_in = 0;
        strm.next_out = room.as_mut_ptr();
        strm.avail_out = 64;
        // SAFETY: `strm` holds a live state, and `deflateParams` may flush through
        // `next_out`/`avail_out`, which were set from `out` and its length just above.
        let ret = unsafe { deflateParams(&mut strm, 9, 1) };
        assert!(
            ret == ReturnCode::OK.as_i32() || ret == ReturnCode::BUF_ERROR.as_i32(),
            "deflateParams reported {ret}"
        );
        produced.extend_from_slice(&room[..64 - strm.avail_out as usize]);

        // Finish.
        let mut out = [0_u8; 8192];
        loop {
            if strm.avail_in == 0 && fed < input.len() {
                strm.next_in = input[fed..].as_ptr();
                strm.avail_in = uInt::try_from(input.len() - fed).expect("fits");
                fed = input.len();
            }
            strm.next_out = out.as_mut_ptr();
            strm.avail_out = 8192;
            // SAFETY: `strm` holds a live state, and `next_in`/`avail_in` and
            // `next_out`/`avail_out` were set from the same slices whose lengths they carry, so
            // both extents are exact.
            let ret = unsafe {
                deflate(
                    &mut strm,
                    if fed == input.len() {
                        Z_FINISH
                    } else {
                        Z_NO_FLUSH
                    },
                )
            };
            produced.extend_from_slice(&out[..8192 - strm.avail_out as usize]);
            if ret == ReturnCode::STREAM_END.as_i32() {
                break;
            }
            assert_eq!(ret, ReturnCode::OK.as_i32());
        }

        // The whole header, field by field, exactly as `deflate.c` L1092-L1182 writes it.
        assert_eq!(&produced[0..3], &[0x1f, 0x8b, 0x08]);
        assert_eq!(
            produced[3], 0x1f,
            "FTEXT, FHCRC, FEXTRA, FNAME and FCOMMENT are all set"
        );
        assert_eq!(&produced[4..8], &[0x0d, 0x0c, 0x0b, 0x0a], "MTIME");
        assert_eq!(produced[9], 7, "OS");
        assert_eq!(
            &produced[10..12],
            &[0xd0, 0x07],
            "XLEN is 2000, little-endian"
        );
        assert!(
            produced[12..12 + EXTRA_LEN].iter().all(|&b| b == 0x5a),
            "every byte of the extra field survived the suspension"
        );
        let name_at = 12 + EXTRA_LEN;
        assert!(
            produced[name_at..name_at + NAME_LEN]
                .iter()
                .all(|&b| b == b'n'),
            "every byte of the name survived the suspension"
        );
        assert_eq!(produced[name_at + NAME_LEN], 0, "the name terminator");
        let comment_at = name_at + NAME_LEN + 1;
        assert_eq!(
            &produced[comment_at..comment_at + comment.len()],
            &comment[..],
            "the comment, terminator included"
        );

        // SAFETY: `strm` still holds the state `init_gzip` installed and has not been ended, so
        // `deflateEnd` releases each block exactly once through the same `zfree` it was allocated
        // with.
        assert_eq!(unsafe { deflateEnd(&mut strm) }, ReturnCode::OK.as_i32());
        // SAFETY: `head` came from `Box::into_raw` in this test, every stream that recorded it has
        // been ended, and this is the only place it is reclaimed -- so it is reclaimed exactly once
        // and nothing can read it afterwards.
        drop(unsafe { Box::from_raw(head) });
    }

    // -----------------------------------------------------------------------
    // m2 -- what `deflateCopy` leaves in `dest->state` when it fails
    // -----------------------------------------------------------------------

    /// A refused state request leaves `dest->state` holding the source's pointer,
    /// exactly as C does -- and that alias is refused by every later call rather
    /// than acted on.
    ///
    /// `deflate.c` L1333 copies the whole `z_stream`, including `state`; L1336-L1337
    /// return `Z_MEM_ERROR` before L1338 replaces it. C's own owner-identity test
    /// (`s->strm != strm`, L544) then rejects the alias, so `deflateEnd(dest)`
    /// reports `Z_STREAM_ERROR` instead of freeing the source's state -- and this
    /// port's prefix check does the same, which is what makes reproducing the value
    /// safe here.
    #[test]
    fn a_refused_copy_leaves_dest_state_as_c_leaves_it() {
        let zone = Zone::new();
        let mut strm = blank_stream();
        track(&mut strm, &zone);
        // SAFETY: `strm` is this test's own blank stream: its `state` is still null, so nothing is
        // leaked by initialising it, and `init_gzip` takes it by `&mut` so the pointer is non-null,
        // aligned and valid for the whole structure.
        assert_eq!(unsafe { init_gzip(&mut strm) }, ReturnCode::OK.as_i32());

        // The source took five blocks; refuse the sixth, which is the copy's state
        // object -- the allocation at `deflate.c` L1335.
        zone.deny_from(6);
        let source_state = strm.state;

        let mut dest = blank_stream();
        assert_eq!(
            // SAFETY: `dest` is a blank stream whose `state` is null and `strm` holds the live
            // state to copy; both arrive as `&mut`, so both pointers are non-null, aligned and
            // valid for a whole `z_stream`, and neither local moves afterwards.
            unsafe { deflateCopy(&mut dest, &mut strm) },
            ReturnCode::MEM_ERROR.as_i32()
        );
        assert!(
            core::ptr::eq(dest.state, source_state),
            "deflate.c L1333's byte copy is what dest->state still holds"
        );
        assert_eq!(
            // SAFETY: `dest` is a live `z_stream`, so the pointer is valid for the whole structure.
            // Its `state` is the alias `deflate.c` L1333's byte copy left behind rather than a
            // state `dest` owns, and rejecting exactly that is what this call is asserting: the
            // owner-identity check answers `Z_STREAM_ERROR` and frees nothing, so the source's
            // blocks are not freed twice.
            unsafe { deflateEnd(&mut dest) },
            ReturnCode::STREAM_ERROR.as_i32(),
            "the alias fails the owner-identity check instead of being freed twice"
        );
        assert_eq!(
            zone.with(|zone| zone.released.len()),
            0,
            "nothing was released"
        );

        // The source is untouched and still usable.
        let input = b"hello, hello!";
        let mut out = [0_u8; 256];
        strm.next_in = input.as_ptr();
        strm.avail_in = uInt::try_from(input.len()).expect("fits");
        strm.next_out = out.as_mut_ptr();
        strm.avail_out = uInt::try_from(out.len()).expect("fits");
        assert_eq!(
            // SAFETY: `strm` holds a live state, and `next_in`/`avail_in` and
            // `next_out`/`avail_out` were set from the same slices whose lengths they carry, so
            // both extents are exact.
            unsafe { deflate(&mut strm, Z_FINISH) },
            ReturnCode::STREAM_END.as_i32()
        );
        // SAFETY: `strm` still holds the state `init_gzip` installed and has not been ended, so
        // `deflateEnd` releases each block exactly once through the same `zfree` it was allocated
        // with.
        assert_eq!(unsafe { deflateEnd(&mut strm) }, ReturnCode::OK.as_i32());
        zone.assert_clean("a refused copy");
    }

    /// A refused *buffer* request inside `deflateCopy` clears `dest->state`, because
    /// C reaches `deflateEnd(dest)` at L1349.
    #[test]
    fn a_copy_whose_buffers_are_refused_clears_dest_state() {
        let zone = Zone::new();
        let mut strm = blank_stream();
        track(&mut strm, &zone);
        // SAFETY: `strm` is this test's own blank stream: its `state` is still null, so nothing is
        // leaked by initialising it, and `init_gzip` takes it by `&mut` so the pointer is non-null,
        // aligned and valid for the whole structure.
        assert_eq!(unsafe { init_gzip(&mut strm) }, ReturnCode::OK.as_i32());

        // Allow the copy's state object (request six), refuse its window (seven).
        zone.deny_from(7);

        let mut dest = blank_stream();
        assert_eq!(
            // SAFETY: `dest` is a blank stream whose `state` is null and `strm` holds the live
            // state to copy; both arrive as `&mut`, so both pointers are non-null, aligned and
            // valid for a whole `z_stream`, and neither local moves afterwards.
            unsafe { deflateCopy(&mut dest, &mut strm) },
            ReturnCode::MEM_ERROR.as_i32()
        );
        assert!(
            dest.state.is_null(),
            "deflate.c L1349's deflateEnd(dest) clears it"
        );
        let (released, copy_state) = zone.with(|zone| (zone.released.clone(), zone.requested[5].0));
        assert_eq!(
            released.len(),
            1,
            "only the copy's own state object was returned"
        );
        assert!(
            core::ptr::eq(released[0], copy_state),
            "and it is the block the copy had taken"
        );

        // SAFETY: `strm` still holds the state `init_gzip` installed and has not been ended, so
        // `deflateEnd` releases each block exactly once through the same `zfree` it was allocated
        // with.
        assert_eq!(unsafe { deflateEnd(&mut strm) }, ReturnCode::OK.as_i32());
        zone.assert_clean("a copy whose buffers are refused");
    }

    /// A successful copy takes its state object first as well, and `deflateEnd` on
    /// each stream returns its own five blocks.
    #[test]
    fn a_successful_copy_takes_its_state_object_first() {
        let zone = Zone::new();
        let mut strm = blank_stream();
        track(&mut strm, &zone);
        // SAFETY: `strm` is this test's own blank stream: its `state` is still null, so nothing is
        // leaked by initialising it, and `init_gzip` takes it by `&mut` so the pointer is non-null,
        // aligned and valid for the whole structure.
        assert_eq!(unsafe { init_gzip(&mut strm) }, ReturnCode::OK.as_i32());

        let mut dest = blank_stream();
        assert_eq!(
            // SAFETY: `dest` is a blank stream whose `state` is null and `strm` holds the live
            // state to copy; both arrive as `&mut`, so both pointers are non-null, aligned and
            // valid for a whole `z_stream`, and neither local moves afterwards.
            unsafe { deflateCopy(&mut dest, &mut strm) },
            ReturnCode::OK.as_i32()
        );
        let (requests, copy_state_size) =
            zone.with(|zone| (zone.requested.len(), zone.requested[5].1));
        assert_eq!(requests, 10);
        assert_eq!(
            copy_state_size,
            size_of::<StateBlock<DeflateSlot>>(),
            "the copy's first request is its state object"
        );
        assert!(!dest.state.is_null());
        assert!(
            !core::ptr::eq(dest.state, strm.state),
            "the copy has a state of its own"
        );

        // SAFETY: `dest` holds the state `deflateCopy` installed and has not been ended, so
        // `deflateEnd` releases each of its blocks exactly once through the same `zfree` they came
        // from.
        assert_eq!(unsafe { deflateEnd(&mut dest) }, ReturnCode::OK.as_i32());
        // SAFETY: `strm` still holds the state `init_gzip` installed and has not been ended, so
        // `deflateEnd` releases each block exactly once through the same `zfree` it was allocated
        // with.
        assert_eq!(unsafe { deflateEnd(&mut strm) }, ReturnCode::OK.as_i32());
        zone.assert_clean("a successful copy");
    }

    /// Two streams sharing one tracking allocator still see a last-in-first-out
    /// sequence when they are ended in reverse order of creation.
    #[test]
    fn a_copy_and_its_source_release_last_in_first_out() {
        let zone = Zone::new();
        let mut strm = blank_stream();
        track(&mut strm, &zone);
        // SAFETY: `strm` is this test's own blank stream: its `state` is still null, so nothing is
        // leaked by initialising it, and `init_gzip` takes it by `&mut` so the pointer is non-null,
        // aligned and valid for the whole structure.
        assert_eq!(unsafe { init_gzip(&mut strm) }, ReturnCode::OK.as_i32());

        let mut dest = blank_stream();
        assert_eq!(
            // SAFETY: `dest` is a blank stream whose `state` is null and `strm` holds the live
            // state to copy; both arrive as `&mut`, so both pointers are non-null, aligned and
            // valid for a whole `z_stream`, and neither local moves afterwards.
            unsafe { deflateCopy(&mut dest, &mut strm) },
            ReturnCode::OK.as_i32()
        );
        // Feed the copy a little, so its buffers are in use rather than pristine.
        let input = b"hello, hello!";
        let mut out = [0_u8; 256];
        dest.next_in = input.as_ptr();
        dest.avail_in = uInt::try_from(input.len()).expect("fits");
        dest.next_out = out.as_mut_ptr();
        dest.avail_out = uInt::try_from(out.len()).expect("fits");
        assert_eq!(
            // SAFETY: `dest` holds a live state, and `next_in`/`avail_in` and
            // `next_out`/`avail_out` were set from the same slices whose lengths they carry, so
            // both extents are exact.
            unsafe { deflate(&mut dest, Z_NO_FLUSH) },
            ReturnCode::OK.as_i32()
        );

        // `Z_DATA_ERROR`, not `Z_OK`: the copy is still in `BUSY_STATE` with input it
        // never finished, which is exactly the "stream was freed prematurely" case
        // `zlib.h` L373-L377 documents. Everything is still released either way, which
        // is what this test is checking.
        assert_eq!(
            // SAFETY: `dest` holds the state `deflateCopy` installed and has not been ended, so
            // `deflateEnd` releases each of its blocks exactly once through the same `zfree` they
            // came from. `Z_DATA_ERROR` here reports an unfinished stream, not a failure to
            // release.
            unsafe { deflateEnd(&mut dest) },
            ReturnCode::DATA_ERROR.as_i32()
        );
        // SAFETY: `strm` still holds the state `init_gzip` installed and has not been ended, so
        // `deflateEnd` releases each block exactly once through the same `zfree` it was allocated
        // with.
        assert_eq!(unsafe { deflateEnd(&mut strm) }, ReturnCode::OK.as_i32());
        let (requested, released) =
            zone.with(|zone| (zone.requested.clone(), zone.released.clone()));
        let reverse: Vec<*mut u8> = requested.iter().rev().map(|&(at, _)| at).collect();
        assert_eq!(released, reverse);
        zone.assert_clean("a copy and its source");
    }

    // -----------------------------------------------------------------------
    // M4 -- the overlap snapshot is the caller's allocation, not Rust's
    // -----------------------------------------------------------------------

    /// The payload `test/example.c` L34 compresses, which is also what the overlap
    /// tests elsewhere in this crate use.
    const HELLO: &[u8] = b"hello, hello!\0";

    /// An overlapping `deflate` takes its snapshot through the caller's `zalloc` and
    /// gives it back through the caller's `zfree`, in strict LIFO order.
    ///
    /// `zlib.h` L140-L153 makes the three hook members the caller's resource policy for
    /// a stream, and the snapshot is a request the size of the caller's own `avail_in`
    /// -- precisely the kind of allocation that policy exists to govern. Taking it from
    /// Rust's global allocator instead would put a caller-sized request outside both the
    /// caller's accounting and `test/infcover.c`'s `mem_limit()`.
    ///
    /// So the assertions below are about *whose* memory it is, not about the bytes: the
    /// snapshot appears in the tracking zone as one further request of exactly
    /// `avail_in` bytes, it is the most recent block when it is freed -- which is what
    /// keeps `mem_free`'s `notlifo` counter at zero -- and it is gone before `deflate`
    /// returns, so no state carries it and `deflateEnd` has nothing extra to reclaim.
    #[test]
    fn an_overlapping_deflate_snapshots_through_the_callers_hooks() {
        let zone = Zone::new();
        let mut strm = blank_stream();
        track(&mut strm, &zone);
        // SAFETY: `strm` is this test's own blank stream: its `state` is still null, so nothing is
        // leaked by initialising it, and `init_gzip` takes it by `&mut` so the pointer is non-null,
        // aligned and valid for the whole structure.
        assert_eq!(unsafe { init_gzip(&mut strm) }, ReturnCode::OK.as_i32());

        let after_init = zone.with(|zone| zone.requested.len());
        assert_eq!(after_init, 5, "deflate.c L440 plus L468-L479");

        // One buffer, read and written at once: `next_in` and `next_out` both address
        // its first byte, which is the overlap `test/example.c`'s `test_large_deflate`
        // creates on purpose and which C serves rather than refuses.
        let mut shared = vec![0_u8; 256];
        shared[..HELLO.len()].copy_from_slice(HELLO);
        let avail_in = HELLO.len();
        strm.next_in = shared.as_ptr();
        strm.avail_in = uInt::try_from(avail_in).unwrap();
        strm.next_out = shared.as_mut_ptr();
        strm.avail_out = uInt::try_from(shared.len()).unwrap();

        // SAFETY: `strm` holds a live state, and `next_in`/`avail_in` and `next_out`/`avail_out`
        // were set from the same slices whose lengths they carry, so both extents are exact.
        let status = unsafe { deflate(&mut strm, Z_FINISH) };
        assert_eq!(
            status,
            ReturnCode::STREAM_END.as_i32(),
            "an overlapping pair runs, exactly as it does in C"
        );
        assert_eq!(
            usize::try_from(strm.total_in).unwrap(),
            avail_in,
            "the whole input is consumed in the one pass the snapshot makes possible"
        );

        let (requested, released) =
            zone.with(|zone| (zone.requested.clone(), zone.released.clone()));
        assert_eq!(
            requested.len(),
            after_init + 1,
            "the snapshot is one further request through the caller's zalloc: {requested:?}"
        );
        let (snapshot_at, snapshot_len) = requested[after_init];
        assert_eq!(
            snapshot_len, avail_in,
            "the request is `avail_in` bytes, the shape ZALLOC(strm, avail_in, 1) has"
        );
        assert_eq!(
            released.len(),
            1,
            "and it is returned before deflate does, through the caller's zfree"
        );
        assert!(
            core::ptr::eq(released[0], snapshot_at),
            "the block returned is the block taken"
        );
        // `mem_done`'s three defect counters, checked here rather than only at the end:
        // the five state blocks are still live at this point, so the whole-zone check
        // cannot run yet, but the two properties that are specifically the snapshot's --
        // that the free was LIFO and of an address the zone handed out -- can.
        zone.with(|zone| {
            assert_eq!(
                zone.live.len(),
                after_init,
                "only the five initialisation blocks remain live: {:?}",
                zone.live
            );
            assert_eq!(zone.notlifo, 0, "the snapshot was freed last-in-first-out");
            assert_eq!(
                zone.rogue, 0,
                "and the address freed was one the zone gave out"
            );
        });

        assert_eq!(
            // SAFETY: `strm` still holds the state `init_gzip` installed and has not been ended, so
            // `deflateEnd` releases each block exactly once through the same `zfree` it was
            // allocated with.
            unsafe { deflateEnd(&mut strm) },
            ReturnCode::OK.as_i32(),
            "the snapshot left nothing for deflateEnd to find"
        );
        zone.assert_clean("an overlapping deflate then deflateEnd");
    }

    /// A caller whose `zalloc` refuses the snapshot gets an error, not an abort.
    ///
    /// `test/infcover.c`'s `mem_limit()` exists to force exactly this, and a system
    /// library must answer it by returning: reaching past the hooks to an infallible
    /// allocator would turn a refused caller-sized request into a process abort. The
    /// status is `Z_STREAM_ERROR` because an unservable buffer pair is the same class of
    /// argument fault as the null pair C rejects at `deflate.c` L993 -- see the comment
    /// above `AliasScratch` in `src/types.rs` -- and nothing is emitted or consumed.
    #[test]
    fn a_refused_overlap_snapshot_is_reported_rather_than_fatal() {
        let zone = Zone::new();
        let mut strm = blank_stream();
        track(&mut strm, &zone);
        // SAFETY: `strm` is this test's own blank stream: its `state` is still null, so nothing is
        // leaked by initialising it, and `init_gzip` takes it by `&mut` so the pointer is non-null,
        // aligned and valid for the whole structure.
        assert_eq!(unsafe { init_gzip(&mut strm) }, ReturnCode::OK.as_i32());

        let after_init = zone.with(|zone| zone.requested.len());
        // Refuse the next request, which is the snapshot and nothing else.
        zone.deny_from(after_init + 1);

        let mut shared = vec![0_u8; 256];
        shared[..HELLO.len()].copy_from_slice(HELLO);
        strm.next_in = shared.as_ptr();
        strm.avail_in = uInt::try_from(HELLO.len()).unwrap();
        strm.next_out = shared.as_mut_ptr();
        strm.avail_out = uInt::try_from(shared.len()).unwrap();

        // SAFETY: `strm` holds a live state, and `next_in`/`avail_in` and `next_out`/`avail_out`
        // were set from the same slices whose lengths they carry, so both extents are exact.
        let status = unsafe { deflate(&mut strm, Z_FINISH) };
        assert_eq!(
            status,
            ReturnCode::STREAM_ERROR.as_i32(),
            "a refused snapshot is reported"
        );
        assert_eq!(strm.total_in, 0, "nothing was consumed");
        assert_eq!(strm.total_out, 0, "nothing was emitted");
        assert_eq!(
            zone.with(|zone| zone.requested.len()),
            after_init,
            "the refused request left no block behind"
        );
        assert!(
            zone.with(|zone| zone.released.is_empty()),
            "and nothing was freed either"
        );

        // The stream is still usable, which is what `Z_STREAM_ERROR` for an argument
        // fault means: the same call with room the zone will serve still completes.
        zone.deny_from(0);
        // SAFETY: `strm` holds a live state, and `next_in`/`avail_in` and `next_out`/`avail_out`
        // were set from the same slices whose lengths they carry, so both extents are exact.
        let status = unsafe { deflate(&mut strm, Z_FINISH) };
        assert_eq!(
            status,
            ReturnCode::STREAM_END.as_i32(),
            "the refusal cost the stream nothing"
        );

        // SAFETY: `strm` still holds the state `init_gzip` installed and has not been ended, so
        // `deflateEnd` releases each block exactly once through the same `zfree` it was allocated
        // with.
        assert_eq!(unsafe { deflateEnd(&mut strm) }, ReturnCode::OK.as_i32());
        zone.assert_clean("a refused overlap snapshot");
    }
}
