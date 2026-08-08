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
//! | **2** -- slice reconstruction | `(next_in, avail_in)` and `(next_out, avail_out)`, and the dictionary buffers, all through [`crate::types::input_slice`] and [`crate::types::output_slice_mut`] |
//! | **3** -- the opaque `state` round-trip | [`crate::types::install_state`], [`crate::types::checked_state`], [`crate::types::checked_state_mut`] and [`crate::types::take_state`] |
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
//! [`core::ptr::addr_of!`] or [`core::ptr::addr_of_mut!`], never through
//! [`crate::types::stream_ref`] or [`crate::types::stream_mut`]. That is a soundness
//! requirement rather than a preference, and `crates/libz-rs-sys/src/types.rs` records the
//! evidence: creating a `&mut z_stream` from the caller's pointer *invalidates that
//! pointer*, so the later `z_stream.state` read inside
//! [`crate::types::checked_state_mut`] fails Miri's borrow-stack check even though the
//! addresses still compare equal. Reading and writing one member at a time through a raw
//! place has neither problem, and it is also the closer translation of the C it replaces,
//! where every access is spelled `strm->next_in`.
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
//! # Rules governing this module
//!
//! `review_rules` reports **"No user rules provided."** -- a complete, single-line document,
//! read in full. No file enters scope by rule and there is no project rule for this module
//! to satisfy. The binding standard is instead AAP §0.7.1 (a)-(i): unsafe containment with a
//! `// SAFETY:` comment naming an invariant on every block, contract immutability,
//! behavioural fidelity, no panics in library paths, a documented public API, the 1.80 MSRV,
//! and dependency minimalism -- this module names only `core`, three sibling modules and
//! [`zlib_rs`].
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
    deflate_set_dictionary as core_deflate_set_dictionary,
    deflate_set_header as core_deflate_set_header, deflate_tune as core_deflate_tune,
    deflate_used as core_deflate_used, DeflateReset, DeflateState, DeflateStream,
};
use zlib_rs::error::ReturnCode;

use crate::panic_guard::{fallback, guard, guard_code};
use crate::types::{
    checked_state, checked_state_mut, gz_headerp, input_slice, install_state, output_slice_mut,
    take_state, uLong, z_size_t, z_stream, z_streamp, Bytef, StateBlock, StateKind,
    StreamAllocator,
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
const _: () = assert!(
    size_of::<uLong>() <= size_of::<u64>(),
    "uLong must be no wider than u64, or widening a caller's total would truncate"
);

/// `z_size_t` must be exactly `usize`, because [`deflateBound_z`] passes it straight through
/// to the core without conversion.
const _: () = assert!(
    size_of::<z_size_t>() == size_of::<usize>(),
    "z_size_t is size_t, whose Rust mirror is usize"
);

/// The core's status and flush values are `i32`, and the ABI's are [`c_int`].
///
/// `c_int` is `i32` on every target that has both `std` and a C ABI, so the two travel
/// between the layers with no conversion. The assertion turns a hypothetical target where
/// that fails into a build error rather than a silent narrowing.
const _: () = assert!(size_of::<c_int>() == size_of::<i32>());

/// `sizeof(z_stream)` must be expressible as the [`c_int`] `stream_size` argument.
///
/// The measured value is 112, so this has enormous headroom; it exists so that
/// [`Z_STREAM_SIZE`]'s narrowing cast below is provably exact rather than merely obviously
/// so.
const _: () = assert!(size_of::<z_stream>() <= c_int::MAX as usize);

/// `sizeof(z_stream)` as the `int` the two `_`-suffixed initialisers compare against.
///
/// `deflate.c` L396 spells the test `stream_size != sizeof(z_stream)`, comparing the
/// *caller's* compile-time view of the struct against the library's. The caller's value
/// arrives as a [`c_int`], so the library's must be one too.
///
/// **Measured: 112 bytes.** `crates/libz-rs-sys/src/layout_assertions.rs` pins that number,
/// and every field offset behind it, at compile time; this constant only has to agree with
/// whatever `size_of` reports, which it does by construction.
// Neither `cast_possible_truncation` nor `cast_possible_wrap` can occur: the assertion
// immediately above bounds the value by `c_int::MAX`, so a target on which either could
// happen fails to build rather than reporting a wrong size. Written as comments rather than
// as the attribute's `reason` field, which was stabilised in Rust 1.81 and therefore fails to
// compile on the declared 1.80 floor.
#[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
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
// See `narrow_uLong`: the narrowing is the operation, not an accident.
#[inline]
#[must_use]
#[allow(clippy::cast_possible_truncation)]
const fn check_of(adler: uLong) -> u32 {
    adler as u32
}

/// Widens a core check value back to the caller's `uLong` member.
///
/// The inverse of [`check_of`], and lossless in this direction on every target: `uLong` is
/// at least four bytes wide wherever a C ABI exists.
// `cast_lossless` cannot be satisfied: no `From<u32> for c_ulong` impl exists that holds for
// every target, because `c_ulong` is a platform alias.
#[inline]
#[must_use]
#[allow(clippy::cast_lossless)]
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
///   "until `deflateEnd`", which only [`take_state`] can end.
type DeflateStateC = DeflateState<'static, StreamAllocator>;

/// The allocated block `z_stream.state` points at: the C-visible prefix plus the state.
///
/// [`StateBlock`] puts `{ z_streamp strm; int status; }` at offset 0, matching how the C
/// `deflate_state` begins (`deflate.h` L105-L106), which is what makes the owner-identity and
/// tag halves of `deflateStateCheck` (`deflate.c` L544-L555) possible at all.
type DeflateBlock = StateBlock<DeflateStateC>;

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
/// by calling `deflateEnd(strm)` (`deflate.c` L512). [`take_state`] performs the same
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
    // SAFETY: unsafe-site category 1, as above -- `write_msg`'s contract is this function's.
    unsafe {
        write_msg(strm, recorded_msg(reset.msg, ReturnCode::OK));
    }
}

/// Turns the message the core recorded into the `'static` C string to publish in `strm->msg`.
///
/// The deflate core records a message only through [`ReturnCode::record_msg`], which stores
/// `err_msg(code)` -- the `z_errmsg` entry belonging to the status it is about to return
/// (`zutil.h` L65-L68, the `ERR_MSG`/`ERR_RETURN` pair). [`error_message`] is the
/// NUL-terminated mirror of that same table, so the string to publish is the one belonging to
/// `code`, and no second copy of the table or of its index arithmetic is needed here.
///
/// [`None`] in means [`None`] out, which is what leaves `strm->msg` untouched: C's plain
/// `return Z_STREAM_ERROR` at `deflate.c` L985 is deliberately not an `ERR_RETURN`, so a
/// caller's previous message survives it.
///
/// The debug assertion is the self-check that keeps the two tables from drifting: it fires
/// only if the core recorded a message that does not belong to the status it returned, which
/// would mean the correspondence documented above had been broken. It is a `debug_assert!`
/// rather than a hard failure for the reason the whole crate is careful about: aborting a C
/// caller's process over a diagnostic string would be a far worse outcome than publishing the
/// status's own message.
#[inline]
#[must_use]
fn recorded_msg(message: Option<&'static str>, code: ReturnCode) -> Option<&'static CStr> {
    let message = message?;
    let published = error_message(code.as_i32());
    debug_assert!(
        published.to_bytes() == message.as_bytes(),
        "the deflate core recorded a message that does not belong to the status it returned"
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
/// [`crate::types::checked_state_mut`], and it is not the same test C performs. C overwrites a
/// null `zalloc`/`zfree` pair with `zcalloc`/`zcfree` during initialisation
/// (`deflate.c` L401-L414), so after `deflateInit_` neither is ever null and the test only
/// ever rejects a *foreign* or *corrupted* stream. This port leaves both members `Z_NULL` when
/// the caller did, because [`StreamAllocator::internal`] needs no function pointers at all --
/// so testing "both null" would reject every default-initialised stream. What
/// [`StreamAllocator::from_stream_ptr`] rejects instead is a *half-supplied* pair, which is
/// the genuinely inconsistent case, and which C itself refuses with `Z_STREAM_ERROR` under
/// `Z_SOLO` at L403 and L412.
///
/// [`None`] means the entry point must return `Z_STREAM_ERROR`.
///
/// # Safety
///
/// If `strm` is non-null it must address a live [`z_stream`], and if that stream's `state` is
/// non-null it must address a [`DeflateBlock`] produced by [`install_state`]. No other borrow
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
    unsafe { checked_state_mut::<DeflateStateC>(strm, StateKind::Deflate) }
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
    // SAFETY: unsafe-site category 4 -- as `deflate_block_mut`.
    unsafe { StreamAllocator::from_stream_ptr(strm) }?;
    // SAFETY: unsafe-site category 3 -- as `deflate_block_mut`, with a shared borrow.
    unsafe { checked_state::<DeflateStateC>(strm, StateKind::Deflate) }
}

/// Copies the state's `status` into the C-visible tag at offset 8 of the block.
///
/// The tag and the Rust `Status` are separate storage, so they stay in step only because
/// every entry point that can change the status syncs them. Keeping them in step is what
/// makes the tag half of `deflateStateCheck` (`deflate.c` L546-L555) meaningful on the next
/// call rather than merely non-rejecting.
fn sync_tag(block: &mut DeflateBlock) {
    let tag = block.state().status().as_raw();
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

    // `my_version[0]`, where `my_version` is `ZLIB_VERSION`. `unwrap_or` names an
    // unreachable case -- the constant is a non-empty literal -- while keeping the function
    // free of any panicking path; the sentinel it would fall back to is a NUL, which no
    // caller's first character can be, so a hypothetical empty constant would reject every
    // caller rather than accept every caller.
    let library_major = crate::util::ZLIB_VERSION
        .to_bytes()
        .first()
        .copied()
        .unwrap_or(0);

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
/// `entry` must have come from [`StreamFields::read`] on a live stream, and its two buffers
/// must satisfy the ordinary C contract: `avail_in` bytes readable at `next_in` when that
/// count is non-zero, `avail_out` bytes writable at `next_out` when that count is non-zero,
/// and neither region aliased by anything else -- including by the other, or by the
/// [`z_stream`] itself -- for the duration of the call. Call this **once** per entry point:
/// two live mutable slices over one output buffer would be undefined behaviour even if
/// neither were written.
#[must_use]
unsafe fn borrow_stream(entry: &StreamFields) -> DeflateStream<'static, 'static> {
    // SAFETY: unsafe-site category 2 -- slice reconstruction, performed once. The helper
    // branches on a zero length and on a null pointer, so a `z_stream` with `avail_in == 0`
    // and `next_in == Z_NULL` -- which `zlib.h` L138-L139 invites and `deflate.c` L990
    // explicitly permits -- yields a genuine empty slice rather than a dangling one. For a
    // non-zero count this function's contract makes the bytes readable and stable.
    let input = unsafe { input_slice(entry.next_in, entry.avail_in) };

    // SAFETY: unsafe-site category 2 -- as above, for the output. The same zero-length rule
    // applies; a null `next_out` has already been rejected by the entry point on the paths
    // where C rejects it, and yields an empty slice on the paths where C does not, so no
    // dangling slice can be formed. The region is writable, unaliased and stable by this
    // function's contract, and this is the only mutable slice over it.
    let output = unsafe { output_slice_mut(entry.next_out, entry.avail_out) };

    entry.scalars.into_stream(input, output)
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
        output: &'static mut [u8],
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

    // SAFETY: unsafe-site category 1 -- `publish_scalars`'s contract is this function's.
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
    // than performed here. `deflateInit2_`'s contract is a superset of this function's -- it
    // additionally validates the four constants supplied here, which are compile-time literals
    // -- so a caller satisfying this one satisfies that.
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
///    [`version_is_compatible`].
/// 2. `strm == Z_NULL` (L398).
/// 3. `strm->msg = Z_NULL` (L400) -- before anything can fail, so that a caller reading `msg`
///    after a rejection sees the library's answer rather than a stale one.
/// 4. The allocator (L401-L414). C substitutes `zcalloc`/`zcfree` for a null pair *in the
///    caller's structure*; this port leaves the caller's members exactly as they were and
///    keeps the substitution internal to [`StreamAllocator`], for the reason
///    [`deflate_block_mut`] documents.
/// 5. Parameter validation and the state allocation (L419-L530), both in the core.
/// 6. Recording the state in `strm->state` (L445).
/// 7. `return deflateReset(strm)` (L532), whose effect on the caller's five stream members is
///    part of this function's observable contract.
///
/// ★ Step 7 is not optional and is the easiest thing in this module to leave out.
/// [`zlib_rs::deflate::deflate_init2`] calls the core reset internally but discards the
/// [`DeflateReset`] it produces, because it has no `z_stream` to apply it to. If this function
/// did not call [`zlib_rs::deflate::deflate_reset`] again and publish the result, a freshly
/// initialised zlib stream would report `adler == 0` instead of `1`, which `test/example.c`
/// observes. The second call is sound because the core documents the reset as idempotent on a
/// state that has just been initialised.
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

        // 3. `strm->msg = Z_NULL;` (L400).
        //
        // SAFETY: unsafe-site category 1 -- one member written through a raw place. `strm` is
        // non-null and aligned by the test above and live by this function's contract; the
        // member is written, not read, so its previous contents may be indeterminate -- which
        // they are for the stack-allocated `z_stream` of `test/example.c`.
        unsafe {
            write_msg(strm, None);
        }

        // 4. L401-L414.
        //
        // SAFETY: unsafe-site category 4 -- reading the caller's three allocator members
        // through raw places. Non-null and aligned above, live and initialised in those three
        // members by this function's contract; nothing is dereferenced.
        let Some(allocator) = (unsafe { StreamAllocator::from_stream_ptr(strm) }) else {
            // A half-supplied `(zalloc, zfree)` pair, which C refuses with the same status
            // under `Z_SOLO` at L403 and L412.
            return fallback::STREAM_ERROR_CODE;
        };

        // 5a. The five arguments as the caller wrote them. `method` and `strategy` are typed
        //     here and rejected if they name no documented value (L434-L437); the three
        //     integers are bounded by the core's own validation, inside `deflate_init2`.
        let config = match DeflateConfig::from_raw(level, method, windowBits, memLevel, strategy) {
            Ok(config) => config,
            Err(code) => return code,
        };

        // 5b. L419-L530: validation, the state object and its four buffers.
        let state = match core_deflate_init2(config, allocator) {
            Ok(state) => state,
            Err(code) => {
                if code == ReturnCode::MEM_ERROR {
                    // L509-L513: C sets the out-of-memory message and then calls
                    // `deflateEnd(strm)`, whose observable effect on the caller's structure is
                    // `strm->state = Z_NULL`. The core has already returned every block it
                    // took, in `deflateEnd`'s order, so only those two writes remain.
                    //
                    // SAFETY: unsafe-site category 1 -- two members written through raw
                    // places. Non-null, aligned and live as established above; both are
                    // written rather than read.
                    unsafe {
                        write_msg(strm, Some(error_message(ReturnCode::MEM_ERROR.as_i32())));
                        write_state_null(strm);
                    }
                }
                // The parameter-rejection path returns here having touched nothing but `msg`,
                // exactly as C's L438 does -- in particular `strm->state` is left alone, so a
                // caller that re-initialises a live stream with an invalid parameter set keeps
                // the stream it had.
                return code;
            }
        };

        // 6. `strm->state = (struct internal_state FAR *)s;` (L445), with the C-visible prefix
        //    seeded so that the block passes its own owner-identity and tag checks from the
        //    moment it exists. `s->status = INIT_STATE` at L447 exists in C for precisely that
        //    reason -- "to pass state test in deflateReset()".
        let tag = state.status().as_raw();

        // SAFETY: unsafe-site categories 3 and 4 -- allocating the state object through the
        // caller's `zalloc` and recording its address in `strm->state`. `strm` is non-null,
        // aligned and live; `allocator` carries the caller's own triple, so the block can only
        // be released through the matching `zfree`. No state has been installed by this call
        // yet, and `state` is moved in, so on failure it is dropped here and every buffer it
        // holds is returned to the same allocator.
        if let Err(code) = unsafe { install_state(strm, &allocator, tag, state) } {
            // L443-L444: `if (s == Z_NULL) return Z_MEM_ERROR;` -- no message and no write to
            // `strm->state`, because C has not reached L445 either. A caller re-initialising a
            // live stream therefore still holds the state it had, exactly as in C.
            return code;
        }

        // 7. `return deflateReset(strm);` (L532).
        //
        // SAFETY: unsafe-site categories 3 and 4 -- the helper's contract is this function's,
        // and the state it recovers is the one `install_state` recorded in `strm->state` one
        // statement ago. That state was moved into the stream, so no borrow of it exists here,
        // and the `state` member is the only thing pointing at it.
        let Some(block) = (unsafe { deflate_block_mut(strm) }) else {
            // Unreachable: the block was installed one statement ago with a tag drawn from its
            // own status and an owner recorded as `strm`, so all four checks pass. Reported
            // rather than asserted, because an abort at an FFI boundary is a worse outcome than
            // a status a caller can act on.
            return fallback::STREAM_ERROR_CODE;
        };
        let reset = core_deflate_reset(block.state_mut());
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
        // SAFETY: unsafe-site categories 3 and 4 -- the helper's contract is this function's.
        let Some(block) = (unsafe { deflate_block_mut(strm) }) else {
            return fallback::STREAM_ERROR_CODE;
        };
        if dictionary.is_null() {
            return fallback::STREAM_ERROR_CODE;
        }

        // SAFETY: unsafe-site category 1 -- four members read through raw places. A stream that
        // owns a state has been through `deflateInit2_`, whose closing reset assigns all four.
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
        let code =
            core_deflate_set_dictionary(block.state_mut(), &mut check, &mut total_in, dictionary);
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
        // SAFETY: unsafe-site categories 3 and 4 -- the helper's contract is this function's.
        let Some(block) = (unsafe { deflate_block_ref(strm) }) else {
            return fallback::STREAM_ERROR_CODE;
        };
        let state = block.state();

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
            let target = unsafe { output_slice_mut(dictionary, len) };
            let code = core_deflate_get_dictionary(state, Some(target), None);
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
        // SAFETY: unsafe-site categories 3 and 4 -- the helper's contract is this function's.
        let Some(block) = (unsafe { deflate_block_mut(strm) }) else {
            return fallback::STREAM_ERROR_CODE;
        };

        let reset = core_deflate_reset_keep(block.state_mut());
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
        // SAFETY: unsafe-site categories 3 and 4 -- the helper's contract is this function's.
        let Some(block) = (unsafe { deflate_block_mut(strm) }) else {
            return fallback::STREAM_ERROR_CODE;
        };

        let reset = core_deflate_reset(block.state_mut());
        sync_tag(block);

        // SAFETY: unsafe-site category 1 -- as `deflateResetKeep`.
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
// is a comment rather than the attribute's `reason` field.
#[inline]
#[must_use]
#[allow(clippy::cast_possible_truncation)]
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
/// # Safety
///
/// `text` must be non-null and address a NUL-terminated string that stays alive and unmodified
/// until the header has been written -- which is to say, until deflation reaches the end of the
/// gzip header. `zlib.h` L838-L845 places exactly that obligation on the caller.
#[must_use]
unsafe fn field_with_nul(text: *mut Bytef) -> &'static [u8] {
    // SAFETY: unsafe-site categories 5 and 6 -- reading a NUL-terminated field of the caller's
    // `gz_header`. `text` is non-null by this function's contract and trivially aligned for
    // `c_char`, and the contract makes the bytes up to and including the terminator readable
    // and stable for as long as the borrow is held. `CStr::from_ptr` performs exactly the scan
    // C's emission loop performs, and nothing is written through the pointer. The `'static`
    // lifetime is the FFI statement that the extent is the caller's responsibility, which
    // `zlib.h` L838-L845 assigns to it explicitly.
    let field: &'static CStr = unsafe { CStr::from_ptr(text.cast::<c_char>()) };
    field.to_bytes_with_nul()
}

/// Converts a caller's `gz_header` into the borrowed view the core reads it through.
///
/// Only the seven members the compressor reads are taken. `xflags` is *computed* from the level
/// and strategy rather than read (`deflate.c` L1102-L1104), and `extra_max`, `name_max`,
/// `comm_max` and `done` are documented as being for *reading* a header rather than writing one
/// (`zlib.h` L125-L132) -- so a caller writing a gzip stream need never have initialised any of
/// the five, and reading them would be reading indeterminate memory.
///
/// ★ **What is snapshotted and what is not.** The four scalars -- `text`, `time`, `os` and
/// `hcrc` -- and the three field *lengths* are captured now. The field *contents* are not: the
/// three slices borrow the caller's buffers, so the bytes are read later, during `deflate()`,
/// exactly as C reads them. `zlib.h` L838-L855 requires the `gz_header` and every buffer it
/// points at to stay alive and unmodified until the header has been written, which is precisely
/// the condition under which the snapshot and C's late read cannot differ.
///
/// # Safety
///
/// `head` must be non-null, aligned, and address a live [`crate::types::gz_header`] whose seven
/// members below are initialised. If `extra` is non-null it must be readable for `extra_len`
/// bytes; if `name` or `comment` is non-null it must be NUL-terminated. All of it must stay
/// alive and unmodified until the gzip header has been written.
#[must_use]
unsafe fn borrow_gz_header(head: gz_headerp) -> GzHeaderView<'static> {
    // SAFETY: unsafe-site category 6 -- reading seven members of the caller's `gz_header`.
    // `head` is non-null, aligned and live in those seven members by this function's contract,
    // so each is in bounds and readable. `addr_of!` plus `read` forms raw places rather than a
    // `&gz_header`, which is what keeps the five members a writer need not initialise -- and
    // the tail padding -- entirely untouched. Nothing is written.
    let (text, time, os, extra, extra_len, name, comment, hcrc) = unsafe {
        (
            ptr::addr_of!((*head).text).read(),
            ptr::addr_of!((*head).time).read(),
            ptr::addr_of!((*head).os).read(),
            ptr::addr_of!((*head).extra).read(),
            ptr::addr_of!((*head).extra_len).read(),
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
        // SAFETY: unsafe-site categories 2 and 6 -- slice reconstruction over a counted field
        // of the caller's `gz_header`. Non-null by the test above, trivially aligned for `u8`,
        // and readable for `extra_len` bytes by this function's contract; the helper
        // additionally branches on a zero length. Nothing writes through the shared borrow.
        Some(unsafe { input_slice(extra.cast_const(), extra_len) })
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
        // SAFETY: unsafe-site categories 5 and 6 -- as `name` immediately above.
        Some(unsafe { field_with_nul(comment) })
    };

    GzHeaderView {
        // C tests `s->gzhead->text ? 1 : 0` when building `FLG` (L1092), which is exactly the
        // `int`-to-`bool` conversion here.
        text: text != 0,
        time: mtime_of(time),
        os,
        extra,
        name,
        comment,
        hcrc: hcrc != 0,
    }
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
/// and `comment` buffers stay alive and unmodified until the header has been written, and that
/// `name` and `comment` be NUL-terminated and `extra` be readable for `extra_len` bytes. This
/// port keeps that contract unchanged: [`borrow_gz_header`] captures the four scalars and the
/// three lengths, and borrows the three buffers, so the field contents are still read late.
/// Freeing or shortening any of the three before deflation reaches the end of the gzip header is
/// undefined behaviour here for the same reason it is in C.
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
/// `head` must be null or satisfy [`borrow_gz_header`]'s contract, including the lifetime
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
        // SAFETY: unsafe-site categories 3 and 4 -- the helper's contract is this function's.
        let Some(block) = (unsafe { deflate_block_mut(strm) }) else {
            return fallback::STREAM_ERROR_CODE;
        };

        let view = if head.is_null() {
            None
        } else {
            // SAFETY: unsafe-site categories 5 and 6 -- `borrow_gz_header`'s contract is this
            // function's, and `head` is non-null by the test above.
            Some(unsafe { borrow_gz_header(head) })
        };

        core_deflate_set_header(block.state_mut(), view)
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
        // SAFETY: unsafe-site categories 3 and 4 -- the helper's contract is this function's.
        let Some(block) = (unsafe { deflate_block_ref(strm) }) else {
            return fallback::STREAM_ERROR_CODE;
        };

        let (bytes, bit_count, status) = core_deflate_pending(block.state());

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

        // SAFETY: unsafe-site category 1 -- writing the caller's out-parameter, under the same
        // conditions as `bits` above. The core has already narrowed the count to the
        // `unsigned` this pointer addresses, and reports through `status` whether that
        // narrowing lost information.
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
        // SAFETY: unsafe-site categories 3 and 4 -- the helper's contract is this function's.
        let Some(block) = (unsafe { deflate_block_ref(strm) }) else {
            return fallback::STREAM_ERROR_CODE;
        };

        // `if (bits != Z_NULL) *bits = strm->state->bi_used;` (L739-L740)
        if !bits.is_null() {
            let bit_count = core_deflate_used(block.state());
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
        // SAFETY: unsafe-site categories 3 and 4 -- the helper's contract is this function's.
        let Some(block) = (unsafe { deflate_block_mut(strm) }) else {
            return fallback::STREAM_ERROR_CODE;
        };

        let code = core_deflate_prime(block.state_mut(), bits, value);
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
        // SAFETY: unsafe-site categories 3 and 4 -- the helper's contract is this function's.
        let Some(block) = (unsafe { deflate_block_mut(strm) }) else {
            return fallback::STREAM_ERROR_CODE;
        };

        if block.state().last_flush() == LAST_FLUSH_AFTER_RESET {
            // C cannot reach its inner `deflate` call, so it touches none of the caller's
            // buffer members; neither does this. The four scalars still travel in and out,
            // because they are the members a reset does initialise and the core is free to
            // read them.
            //
            // SAFETY: unsafe-site category 1 -- four members read through raw places. A stream
            // that owns a state has been through `deflateInit2_`, whose closing reset assigns
            // all four.
            let scalars = unsafe { StreamScalars::read(strm) };
            let mut stream = scalars.into_stream(&[], &mut []);
            let code = core_deflate_params(block.state_mut(), &mut stream, level, strategy);
            sync_tag(block);

            // SAFETY: unsafe-site category 1 -- the same four members written back, plus `msg`
            // if the core recorded one, on a non-null, aligned, live stream.
            unsafe {
                publish_scalars(strm, &stream, code);
            }
            return code;
        }

        // SAFETY: unsafe-site category 1 -- eight members read through raw places. `deflate` has
        // already run on this stream, which establishes that its caller initialised the four
        // buffer members; the four scalars come from the reset, as above.
        let entry = unsafe { StreamFields::read(strm) };

        // The two disjuncts of `deflate.c` L990-L991, evaluated now because the slices built
        // below erase the distinction between "no room" and "nowhere to write".
        let pointers_rejected =
            entry.next_out.is_null() || (entry.avail_in != 0 && entry.next_in.is_null());

        let mut stream = if pointers_rejected {
            // C refuses this pointer state at L993, before it has read or written a byte
            // through either pointer, so nothing may travel through them here either. Two empty
            // slices deliver that: if the parameters reach the inner `deflate` at all it now
            // refuses on `avail_out == 0` at L995 -- C's own refusal, one test later, having
            // likewise emitted nothing, consumed nothing and left the state alone -- and the
            // rewrite below restores C's status. If the parameters do *not* reach it, the empty
            // slices are never looked at and the answer is C's `Z_OK` either way.
            entry.scalars.into_stream(&[], &mut [])
        } else {
            // SAFETY: unsafe-site category 2 -- slice reconstruction, once, through the shared
            // helper. `entry` came from `StreamFields::read` on this live stream, and this
            // function's contract makes both buffers valid for their counts and unaliased.
            unsafe { borrow_stream(&entry) }
        };

        let mut code = core_deflate_params(block.state_mut(), &mut stream, level, strategy);
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
        // SAFETY: unsafe-site categories 3 and 4 -- the helper's contract is this function's.
        let Some(block) = (unsafe { deflate_block_mut(strm) }) else {
            return fallback::STREAM_ERROR_CODE;
        };

        let code = core_deflate_tune(
            block.state_mut(),
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
        let block = unsafe { deflate_block_ref(strm) };
        core_deflate_bound_z(block.map(StateBlock::state), sourceLen)
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
        // SAFETY: unsafe-site categories 3 and 4 -- as `deflateBound_z`.
        let block = unsafe { deflate_block_ref(strm) };
        let bound = core_deflate_bound(block.map(StateBlock::state), widen_uLong(sourceLen));

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
/// writable at `next_out` when that count is non-zero, and neither region may overlap the other
/// or the [`z_stream`] itself.
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

        // Guards 3 and 4, and everything after them, are the core's.
        //
        // SAFETY: unsafe-site category 2 -- slice reconstruction, once, through the shared
        // helper. `entry` came from `StreamFields::read` on this live stream, and this
        // function's contract makes both buffers valid for their counts and unaliased. `flush`
        // is passed through unchanged rather than pre-converted, so the core applies its own
        // canonical validation to the same value C validates.
        let mut stream = unsafe { borrow_stream(&entry) };

        let code = core_deflate(block.state_mut(), &mut stream, flush);
        // The status advances through the header stages, `BUSY_STATE` and `FINISH_STATE` as the
        // stream progresses, so the C-visible tag must follow it -- and `deflateEnd`'s
        // `Z_DATA_ERROR` depends on the same status being accurate.
        sync_tag(block);

        // SAFETY: unsafe-site category 1 -- the write-back, with `entry` the fields read on
        // entry and `stream` the view built from them, on a non-null, aligned, live stream.
        unsafe {
            publish(strm, &entry, &stream, code);
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
/// (L1307's effect, brought forward by [`take_state`]), so no path can leave the caller holding
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

        // The rest of `deflateStateCheck`, then `strm->state = Z_NULL` and the release of the
        // state object (L1305-L1307).
        //
        // SAFETY: unsafe-site categories 3 and 4 -- the opaque `state` round-trip and the
        // caller's `zfree`. The helper performs the four validity checks before touching
        // anything, and `allocator` carries the same triple `install_state` used, which this
        // function's contract requires. No borrow of the block exists here, so displacing it is
        // sound and cannot be repeated: the member is cleared first.
        let Some(block) =
            (unsafe { take_state::<DeflateStateC>(strm, &allocator, StateKind::Deflate) })
        else {
            return fallback::STREAM_ERROR_CODE;
        };

        // L1298-L1309: the status is read, the four buffers are released in C's order, and the
        // status decides the return value. Dropping `block`'s state is what releases them, and
        // the core spells the order out.
        core_deflate_end(block.into_state())
    })
}

// ---------------------------------------------------------------------------
// Duplication -- `deflate.c` L1317-L1377
// ---------------------------------------------------------------------------

/// Reports whether two `z_stream` pointers address regions that do not overlap at all.
///
/// `deflateCopy` copies `sizeof(z_stream)` bytes from one to the other (`deflate.c` L1333), which
/// is defined only for non-overlapping regions -- in C as much as in Rust, since `memcpy`'s own
/// contract requires it. Two `z_stream`-aligned pointers can still overlap partially, at any
/// multiple of the alignment below the struct size, so equality is not a sufficient test and the
/// full range comparison is used.
///
/// The addresses are compared as integers because that is the only way to express "these two
/// ranges are disjoint"; nothing is dereferenced, and the values are not turned back into
/// pointers, so no provenance is involved.
fn streams_are_disjoint(dest: z_streamp, source: z_streamp) -> bool {
    let dest_addr = dest as usize;
    let source_addr = source as usize;
    dest_addr.abs_diff(source_addr) >= size_of::<z_stream>()
}

/// Copies all `sizeof(z_stream)` bytes of one stream over another.
///
/// `zmemcpy(dest, source, sizeof(z_stream))` (`deflate.c` L1333) exactly: a **byte** copy, not a
/// typed one. The distinction matters. `reserved` (`zlib.h` L109) is a member the library never
/// writes and a caller need never initialise, so reading the struct as a value would be reading
/// indeterminate memory; a byte copy carries whatever is there without interpreting it, which is
/// what C does and what makes `dest` a complete copy including any `reserved` the caller had set.
///
/// # Safety
///
/// Both pointers must be non-null, aligned, and address `size_of::<z_stream>()` bytes -- `source`
/// readable, `dest` writable -- and the two regions must not overlap, which
/// [`streams_are_disjoint`] establishes. Neither struct need be fully initialised.
unsafe fn copy_stream(dest: z_streamp, source: z_streamp) {
    // SAFETY: unsafe-site category 1 -- copying the caller's stream structure byte for byte.
    // Both pointers are non-null, aligned and cover `size_of::<z_stream>()` readable or writable
    // bytes by this function's contract, and the regions are disjoint, which is
    // `copy_nonoverlapping`'s remaining requirement. The element type is `u8`, so the copy
    // reinterprets nothing and requires no member to be initialised; and it is a raw-pointer
    // copy, so no reference to either stream is formed and the caller's own pointers keep their
    // provenance for the `install_state` call that follows.
    unsafe {
        ptr::copy_nonoverlapping(
            source.cast::<u8>(),
            dest.cast::<u8>(),
            size_of::<z_stream>(),
        );
    }
}

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
/// On success `dest->state` addresses the new state. On either allocation failure this function
/// leaves it `Z_NULL`. C's answer is less uniform: the buffer-failure path calls
/// `deflateEnd(dest)` (L1349) and so also ends at `Z_NULL`, but the state-object path returns at
/// L1336 with `dest->state` still holding the *source's* state pointer, copied in at L1333. Both
/// values behave identically to every subsequent call, because C's own owner-identity test
/// rejects the alias -- `s->strm != strm` (L544) -- so no conforming caller can tell them apart.
/// `Z_NULL` is chosen because it cannot become a double free if a caller ends both streams,
/// which is precisely the class of defect this port exists to remove.
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
        // SAFETY: unsafe-site categories 3 and 4 -- the helper's contract is this function's.
        // The borrow is shared and is over the state block, which is a separate allocation from
        // either stream, so the writes to `dest` below cannot alias it.
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

        // Clear the alias the copy just created, so that no failure below can leave `dest`
        // pointing at the source's state. See the note on the failure paths above.
        //
        // SAFETY: unsafe-site category 1 -- one member written through a raw place on a
        // non-null, aligned, writable stream. Written rather than read.
        unsafe {
            write_state_null(dest);
        }

        // L1339-L1373: the state object, its four buffers, and the five different amounts of
        // each that C copies.
        let copy = match core_deflate_copy(block.state(), allocator) {
            Ok(copy) => copy,
            // L1347-L1351. The core has already returned every block it took, in `deflateEnd`'s
            // order, and `source` is untouched -- which is what lets `test/infcover.c` force
            // this failure with `mem_limit` and carry on using the original.
            Err(code) => return code,
        };

        // `dest->state = (struct internal_state FAR *) ds;` (L1338) and `ds->strm = dest;`
        // (L1341), the second of which `install_state` performs by recording `dest` in the
        // block's prefix -- without it the copy would fail its own owner-identity check on the
        // very next call.
        let tag = copy.status().as_raw();

        // SAFETY: unsafe-site categories 3 and 4 -- allocating the copy's state object through
        // the source's `zalloc` and recording its address in `dest->state`. `dest` is non-null,
        // aligned and writable; `allocator` is the same triple the buffers above were taken
        // from, so the whole copy can only be released through the matching `zfree`.
        // `dest->state` was just cleared, so nothing is overwritten and nothing leaks; and
        // `copy` is moved in, so on failure it is dropped here and every buffer it holds is
        // returned to that same allocator.
        if let Err(code) = unsafe { install_state(dest, &allocator, tag, copy) } {
            // L1336's status, reached without the aliasing `dest->state` C leaves behind.
            return code;
        }

        ReturnCode::OK
    })
}
