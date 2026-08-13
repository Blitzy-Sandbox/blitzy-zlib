//! The ten one-shot `compress`/`uncompress` exports: `compress.c` and `uncompr.c`.
//!
//! The C ABI half of the two utility translation units. `crates/zlib-rs/src/compress.rs`
//! and `crates/zlib-rs/src/uncompress.rs` already hold every decision these
//! functions make -- the chunking loop, the flush selector, the bidirectional
//! accounting and the status ladder -- so this module contributes exactly three
//! things and nothing else:
//!
//! 1. **Pointer validation**, reproducing C's own guards before anything is
//!    dereferenced, **in C's own order** -- which is why the `level` is judged by
//!    [`level_is_accepted`] here rather than left to the core: `compress.c` L42
//!    returns from `deflateInit` before L44-L47 ever assign `source` or `dest` into
//!    the stream, so a bad level must be refused before either buffer is borrowed.
//! 2. **Slice reconstruction** from the caller's pointer/length pairs, performed
//!    once on entry so that only safe slices travel inward. This is AAP §0.6.1
//!    unsafe-site **category 2**, and it is the only category this module touches.
//!    [`stage_one_shot_input`] runs first: two borrows over one region are undefined
//!    behaviour even unused, and overlap is an input the C signatures permit, so the
//!    input is COPIED before the borrows rather than refused or tolerated after.
//! 3. **Width mapping and write-back**: `uLong` versus `z_size_t`, and pushing
//!    the results back through the caller's out-parameters on exactly the paths
//!    C writes them.
//!
//! # KNOWN BEHAVIOUR DIVERGENCES FROM C -- this module's inventory
//!
//! Behaviour preservation is this port's governing constraint, so every observable
//! departure is an open item rather than a feature. This module has **two**, items 1
//! and 2, and both are the same shape: an argument combination C accepts and whose
//! result C then leaves undefined. Items 3 and 4 are recorded beside them because
//! earlier revisions of this module answered them with `Z_STREAM_ERROR` -- an invented
//! refusal of input C serves -- and the reason each is now *not* a divergence is the
//! load-bearing part.
//!
//! 1. **A null `destLen` or `sourceLen` is `Z_STREAM_ERROR` rather than a crash.**
//!    *(FORCED, UNRESOLVED -- unobservable.)* `compress.c` L70 and `uncompr.c` L86
//!    dereference them unchecked, so C's behaviour is undefined; no conforming caller
//!    can distinguish a reported error from a segmentation fault.
//! 2. **Overlapping `source` and `dest` produce the bytes the buffer held on entry.**
//!    *(FORCED, UNRESOLVED -- inside C's own undefined space.)* The pair is **served**,
//!    not refused: nothing in `zlib.h` L1276-L1341 requires the two to be distinct, C
//!    assigns both into a `z_stream` and runs, so a caller the reference serves has to
//!    keep working. What cannot be expressed is a `&[u8]` and a `&mut [u8]` over one
//!    region, so [`stage_one_shot_input`] copies the input first and the operation reads
//!    the copy. The difference is *which* bytes get processed once the output catches up
//!    with the unread input: C reads whatever it has just written over, this reads the
//!    entry values. C's answer there depends on the interleaving of its own reads and
//!    writes and is described nowhere, so this is a choice within the space C leaves
//!    undefined -- and it is the more defined of the two.
//!
//!    ★ The *streaming* entry points answer it the same way, through the same bounded
//!    `crate::types::OverlapStage`: `test/example.c`'s `test_large_deflate` overlaps
//!    `next_in` and `next_out` on purpose and the suite asserts the byte count that call
//!    consumes. One rule, one mechanism, and -- because the stage is a fixed buffer the
//!    operation is fed one window at a time rather than a copy of the caller's whole
//!    `avail_in` -- no allocation at all, so an overlapping call's high-water mark is the
//!    reference implementation's.
//! 3. **A length out-parameter that points inside `source` or `dest` is served by
//!    ordering.** *(No divergence.)* `*destLen = 0` (`compress.c` L36) is written before
//!    either buffer is borrowed and the closing count after every borrow has ended, which
//!    is exactly where C writes them, so no exclusive borrow and raw write are ever in
//!    flight together. `uncompress2_z` additionally reproduces C's `*destLen -= left`
//!    and `*sourceLen -= len` as read-modify-writes rather than assignments, so a slot
//!    the decoder has just written over is subtracted from in place, as in C.
//! 4. **A staging allocation that fails is `Z_MEM_ERROR`.** *(No divergence in kind.)*
//!    `zlib.h` L1284-L1286 and L1330-L1332 already document that status for these entry
//!    points, and the allocation goes through the library's own routines -- the same
//!    `zcalloc`/`zcfree` C substitutes for the null hooks these functions leave behind
//!    (`compress.c` L38-L40).
//!
//! ★ The C file is `uncompr.c`, not `uncompress.c`. No file by the latter name
//! exists in the tree; the Rust module is named for the function.
//!
//! # The exported set: ten symbols
//!
//! Membership was measured, by building the reference library and grouping
//! `nm -D --defined-only --extern-only`. This module owns ten of the 95 exported
//! functions:
//!
//! | Symbol | `zlib.h` | Derived from |
//! |---|---|---|
//! | [`compress`] | L1271 | `compress.c` L82-L85 |
//! | [`compress_z`] | L1273 | `compress.c` L77-L81 |
//! | [`compress2`] | L1288 | `compress.c` L67-L74 |
//! | [`compress2_z`] | L1291 | `compress.c` L24-L66 |
//! | [`compressBound`] | L1307 | `compress.c` L96-L99 |
//! | [`compressBound_z`] | L1308 | `compress.c` L91-L95 |
//! | [`uncompress`] | L1315 | `uncompr.c` L97-L101 |
//! | [`uncompress_z`] | L1317 | `uncompr.c` L92-L96 |
//! | [`uncompress2`] | L1335 | `uncompr.c` L83-L91 |
//! | [`uncompress2_z`] | L1337 | `uncompr.c` L29-L82 |
//!
//! Every one is `#[no_mangle]` with the `"C"` ABI and **never**
//! `extern "C-unwind"`: a panic must abort at the boundary rather than unwind
//! into a caller that was not compiled to support it. See [`crate::panic_guard`].
//!
//! The eight that take a pointer are additionally declared `unsafe`, and the two
//! bound functions are not. That is a Rust-side type marker with **no ABI effect
//! whatsoever** -- the emitted symbol, the calling convention and the C
//! prototype cbindgen generates are identical either way -- and it is chosen
//! because it is the truth: a function that dereferences a caller's raw pointer
//! has preconditions its callers must uphold, and declaring it safe would assert
//! the opposite. Each one therefore carries a `# Safety` section stating the
//! contract. The two bound functions have no pointer, no precondition and no
//! `# Safety` section, which is equally deliberate.
//!
//! Version decoration is **not** declared here and must not be. `compressBound`
//! belongs to the `ZLIB_1.2.0` node, `uncompress2` to `ZLIB_1.2.9`, the six `_z`
//! names to `ZLIB_1.3.2` (`zlib.map` L106-L113), and `compress`, `compress2` and
//! `uncompress` are undecorated base-set names. The decoration is applied by the
//! `--version-script` link argument `crates/libz-rs-sys/build.rs` passes, which
//! is the single place that knows about it.
//!
//! # ★ `sourceLen` is a pointer in exactly two of the ten
//!
//! `zlib.h` L1335 and L1337 declare `uncompress2` and `uncompress2_z` with
//! `sourceLen` as a **pointer**: in on entry, and on return "the number of source
//! bytes consumed". Every other entry point takes the source length by value.
//! Both spellings are one machine word wide on LP64, so confusing them is a
//! silent wrong-answer bug rather than a compile error -- and the consumed count
//! is exactly how a caller finds the first byte after a zlib stream inside a
//! larger container.
//!
//! # ★ Where the null `dest` splits the two families
//!
//! This is the one place where handing the safe core an empty slice is *not*
//! equivalent to what C does, and it was found by measurement rather than by
//! reading:
//!
//! * **Compression.** `compress.c` L45 assigns `stream.next_out = dest` and
//!   never checks it, so a null `dest` with a zero `*destLen` passes the entry
//!   guard and reaches `deflate`, which rejects a null `next_out` outright
//!   (`deflate.c` L1016) -- **`Z_STREAM_ERROR`**, measured, not `Z_BUF_ERROR`.
//!   An empty Rust slice is non-null by construction, so the core cannot
//!   reproduce that refusal and this module performs the translation. See
//!   [`compress2_z`].
//! * **Decompression.** `uncompr.c` L42-L43 anticipates the same problem and
//!   fixes it: when `*destLen` is zero and `dest` is null it aims `next_out` at
//!   `&stream.reserved`, with the comment "`next_out` cannot be NULL". A non-null
//!   zero-length buffer is precisely what an empty Rust slice already is, so
//!   here the core needs no help and the measured answer -- `Z_BUF_ERROR` for a
//!   stream with content, `Z_OK` for an empty one -- comes out on its own.
//!
//! # Write-back: which paths write, and which leave the caller alone
//!
//! Transcribed from the C sources per branch and then confirmed against the
//! reference build, because "write it back unconditionally" is wrong in one place
//! and "only write on success" is wrong in several.
//!
//! | Path | `*destLen` | `*sourceLen` (`uncompress2*` only) |
//! |---|---|---|
//! | entry guard rejects | **untouched** | **untouched** |
//! | `compress*`, stream init failed | `0` (`compress.c` L36 clears it first) | n/a |
//! | `compress*`, loop ran | bytes produced (L63) | n/a |
//! | `uncompress*`, stream init failed | **untouched** (`uncompr.c` L51-L52 returns first) | **untouched** |
//! | `uncompress*`, loop ran | bytes produced (L75) | bytes consumed (L74) |
//!
//! The two init-failure rows are opposites, and the asymmetry is the reference's
//! own. The safe core encodes it in its return values: `Compressed::produced` is
//! `0` there, while `Decompressed::produced` and `Decompressed::consumed` echo the
//! *entry* lengths -- so this module writes both counts back unconditionally once
//! the guard has passed and still leaves the caller's variables untouched on the
//! decompression path.
//!
//! # ★ The bound functions are a buffer-overflow surface (AAP §0.6.2.2)
//!
//! [`compressBound`] and [`compressBound_z`] are not informational. A caller
//! allocates from the number they return and then writes into that allocation, so
//! the two directions of error are not equivalent:
//!
//! * **Too small is a safety defect.** The caller's allocation is then shorter than
//!   the stream `compress` produces into it, which is a heap overflow **in caller
//!   code** -- something this library can neither detect nor contain. `zlib.h`
//!   L1307-L1308 is what makes this the caller's licence to size a buffer at all.
//! * **Too large is a parity defect, not a safety one.** `zlib.h` documents the
//!   result as an upper bound, so a caller handed a larger number allocates more
//!   than it needs and still behaves correctly. What it breaks is agreement with the
//!   reference: any test or consumer that asserts the exact figure sees a different
//!   one.
//!
//! Both therefore return exactly what C returns, including the two saturations --
//! the first requirement makes anything smaller unacceptable, the second makes
//! anything larger a divergence. The expectations in the test module were measured
//! against the reference build rather than recomputed from the formula, so they
//! check agreement rather than restating the arithmetic under test.
//!
//! Both take no stream, need no state, allocate nothing, and cannot fail.
//!
//! # Allocation
//!
//! The C one-shot functions zero `zalloc`, `zfree` and `opaque` before
//! initialising their stream (`compress.c` L38-L40, `uncompr.c` L47-L49), which
//! selects the library's own allocator. They offer a caller no way to supply one,
//! so neither does this module: the core's `GlobalAllocator` -- its
//! `zcalloc`/`zcfree` counterpart -- is wired in unconditionally, and no
//! caller-supplied hook is read or invoked anywhere here. That is why AAP §0.6.1
//! unsafe-site category 4 does not arise in this file.
//!
//! # Safety posture
//!
//! Unsafe appears only in the two slice-reconstruction helpers and in the
//! out-parameter reads and writes, all of which are category 2; every block
//! carries a `// SAFETY:` comment naming its invariant. There is no stream
//! pointer here (category 1), no opaque state round-trip (category 3), no
//! allocator hook (category 4), no C string or varargs (category 5) and no
//! `gz_header` (category 6).
//!
//! Nothing outside the test module can panic: no `unwrap`, no `expect`, no
//! panicking index, and every width conversion is an explicit saturating or
//! truncating cast rather than a fallible one. `try_into().unwrap()` is
//! prohibited by AAP §0.7.1 (f) and appears nowhere.
//!
//! # Provenance
//!
//! The C ABI facade over the one-shot compression and decompression wrappers.
//!
//! Ported from `compress.c` L24-L99 and `uncompr.c` L29-L101; declared at
//! `zlib.h` L1271-L1337.

// The exported names are C's, and `zlib.h` is immutable, so `compressBound` and
// `compressBound_z` cannot be renamed to satisfy Rust's casing convention. The
// crate root makes the same allowance for the ABI *types* through
// `#![allow(non_camel_case_types)]`; this is its function-name counterpart, and it
// is scoped to the module rather than the crate so that any future non-ABI helper
// added here is still held to the convention.
#![allow(non_snake_case)]

use core::ffi::{c_int, c_ulong};
use core::mem::{size_of, MaybeUninit};

use crate::types::init_view;
use zlib_rs::compress::{
    compress2_z_from as core_compress2_z_from, compress_bound_z as core_compress_bound_z,
};
use zlib_rs::config::{normalize_deflate_level, Z_DEFAULT_COMPRESSION};
use zlib_rs::error::ReturnCode;
use zlib_rs::read_buf::{OneShotSink, OneShotSource, OutputRegion};
use zlib_rs::uncompress::uncompress2_z_from as core_uncompress2_z_from;

use crate::panic_guard::{fallback, guard, guard_code};
use crate::types::{
    ranges_are_disjoint, uLong, uLongf, z_size_t, Bytef, OverlapStage, OVERLAP_STAGE_BYTES,
};

// ---------------------------------------------------------------------------
// Width agreements this module depends on
// ---------------------------------------------------------------------------

/// `uLong` must be no wider than a Rust length, or widening it would lose bits.
///
/// `uLong` is `unsigned long` (`zconf.h` L406): eight bytes on LP64, four on
/// LLP64 Windows, four on 32-bit. `usize` is the target's pointer width. The
/// relation holds on every target Rust and a C ABI are both available on, and
/// stating it as an assertion turns a hypothetical silent truncation in
/// [`widen_uLong`] into a build failure -- the same trade `types.rs` makes for
/// `uInt`.
/// cbindgen:ignore
const _: () = assert!(
    size_of::<uLong>() <= size_of::<usize>(),
    "uLong must be no wider than usize, or widening a caller's length would truncate"
);

/// `z_size_t` must be exactly `usize`, because the `_z` entry points pass it
/// straight through to the core without conversion.
///
/// `types.rs` asserts the same relation for its own use; repeating it here keeps
/// this module's pass-through honest even if that alias is ever revisited.
/// cbindgen:ignore
const _: () = assert!(
    size_of::<z_size_t>() == size_of::<usize>(),
    "z_size_t is size_t, whose Rust mirror is usize"
);

/// Widens a caller's `uLong` length to a Rust length.
///
/// The implicit conversion C performs when `compress2` passes its `uLong`
/// `sourceLen` to `compress2_z` (`compress.c` L71), and when `uncompress2` widens
/// both of its `uLong` lengths (`uncompr.c` L86). Lossless by the assertion
/// above.
///
/// Named rather than written inline so that the justification lives in one place
/// and every call site reads the same, exactly as [`crate::types::widen`] does for
/// `uInt`.
// `cast_possible_truncation` cannot occur: `size_of::<uLong>() <= size_of::<usize>()`
// is asserted above, so a target on which this could truncate fails to build.
// `cast_lossless` has nothing to offer: there is no `From<c_ulong> for usize` impl,
// because the conversion is not lossless in general. Written as a comment rather
// than as the attribute's `reason` field, which was stabilised in Rust 1.81 and
// therefore fails to compile on the declared 1.80 floor.
#[inline]
#[must_use]
#[allow(clippy::cast_possible_truncation, clippy::cast_lossless)]
const fn widen_uLong(value: uLong) -> usize {
    value as usize
}

/// Narrows a Rust length back to a caller's `uLong`, truncating as C's cast does.
///
/// C writes `*destLen = (uLong)got` (`compress.c` L72) and
/// `*sourceLen = (uLong)used; *destLen = (uLong)got;` (`uncompr.c` L88-L89). On
/// LP64 those casts are the identity. On LLP64 Windows, where `unsigned long` is
/// four bytes and `size_t` is eight, they discard the high half -- and a length
/// above 4 GiB is unreachable through those entry points anyway, because the
/// caller could only have supplied it through the same four-byte parameter.
///
/// ★ Truncation is deliberate here and is the faithful behaviour. Saturating
/// instead would report a *different* number from the reference on a target where
/// the cast bites, which is a behavioural change; the `_z` entry points are the
/// supported way to move more than `uLong::MAX` bytes.
// `cast_possible_truncation` is the documented intent, above; it reproduces C's
// narrowing cast exactly. See `widen_uLong` on why the reason is a comment.
#[inline]
#[must_use]
#[allow(clippy::cast_possible_truncation)]
const fn narrow_uLong(value: usize) -> uLong {
    value as uLong
}

// ---------------------------------------------------------------------------
// Slice reconstruction -- AAP §0.6.1 unsafe-site category 2
// ---------------------------------------------------------------------------

/// Rebuilds a caller's source buffer as a slice from `(source, sourceLen)`.
///
/// The `z_size_t` counterpart of [`crate::types::input_slice`], which takes the
/// `uInt` widths a `z_stream` uses. The entry points here are declared with
/// `z_size_t` and `uLong` lengths instead, so the pair cannot go through that
/// helper; the rule it implements is reproduced exactly.
///
/// # ★ The zero-length case is not a formality
///
/// [`core::slice::from_raw_parts`] with a null pointer is **undefined behaviour**
/// even for a zero length -- the pointer must be non-null and aligned regardless.
/// A null `source` with a zero `sourceLen` is entirely ordinary input, though:
/// `compress.c` L31 admits it explicitly by guarding only the `sourceLen > 0`
/// case, and the reference answers `Z_OK` for it, producing the eight-byte empty
/// zlib stream (measured). So the zero case is branched on and yields a genuine
/// empty slice.
///
/// # Safety
///
/// If `len` is non-zero, `source` must be non-null and readable for `len` bytes,
/// and that region must stay valid and unwritten by anything else for `'a`. The
/// returned borrow's lifetime is unconstrained by the arguments, which is inherent
/// to an FFI boundary: this function's caller is responsible for not outliving the
/// buffer.
#[inline]
#[must_use]
unsafe fn source_slice<'a>(source: *const Bytef, len: usize) -> &'a [u8] {
    if len == 0 || source.is_null() {
        // Nothing to read, or nowhere to read it from -- and in the second case
        // the entry point's own guard has already reported the error. Returning
        // the empty slice keeps a null pointer out of `from_raw_parts`.
        return &[];
    }
    // SAFETY: unsafe-site category 2 -- slice reconstruction from a pointer/length
    // pair. `source` is non-null by the test above and trivially aligned for `u8`.
    // `len` is the caller's own count, and this function's contract makes those
    // bytes readable and stable for `'a`. The shared borrow is the right shape
    // because nothing downstream writes through it.
    unsafe { core::slice::from_raw_parts(source, len) }
}

/// Rebuilds a caller's destination buffer as **write-only** storage from
/// `(dest, *destLen)`.
///
/// The [`source_slice`] counterpart, and the `z_size_t` counterpart of
/// [`crate::types::output_region`]. The same zero-length rule applies for the
/// same reason, and it carries more weight on this side: `uncompr.c` L42-L43
/// exists precisely because a zero-length output buffer may legitimately be null,
/// and an empty Rust region is the non-null, non-dangling buffer that C has to
/// manufacture from `&stream.reserved`.
///
/// ★ A region rather than a `&mut [u8]`, because `*destLen` bytes of *room* is all
/// `zlib.h` L1281-L1289 and L1319-L1327 promise: a caller that hands over a buffer
/// straight from `malloc` -- which is what `test/example.c` L86 does -- has
/// initialised none of it, and a byte slice may not address a byte that holds no
/// value. [`crate::types::output_region`] carries the full argument, including why
/// initialising the buffer here instead is not an acceptable alternative.
///
/// # Safety
///
/// If `len` is non-zero, `dest` must be non-null and writable for `len` bytes, and
/// that region must not be aliased by anything else -- including by the source
/// slice -- for `'a`. Call this once on entry: two live mutable views over one
/// buffer would be undefined behaviour even if neither were written.
#[inline]
#[must_use]
unsafe fn dest_slice<'a>(dest: *mut Bytef, len: usize) -> OutputRegion<'a> {
    if len == 0 || dest.is_null() {
        return OutputRegion::empty();
    }
    // SAFETY: unsafe-site category 2 -- reconstruction from a pointer/length pair.
    // `dest` is non-null by the test above and trivially aligned for `u8`, which
    // `MaybeUninit<u8>` shares. `len` is the caller's own count, and this function's
    // contract makes those bytes writable, unaliased and stable for `'a`. No
    // initialisation is required for the element type, which is the point of it.
    let slots = unsafe { core::slice::from_raw_parts_mut(dest.cast::<MaybeUninit<u8>>(), len) };
    OutputRegion::write_only(slots, init_view())
}

// ---------------------------------------------------------------------------
// Staging, which is how an overlapping one-shot call is served
// ---------------------------------------------------------------------------

/// Stages the caller's input when it shares memory with the caller's output, so that an
/// overlapping call runs instead of being refused.
///
/// ★ **Overlap is an input these four entry points can receive, and C serves it.**
/// `zlib.h` L1276-L1310 and L1318-L1341 describe `source` and `dest` independently and
/// never say they must be distinct, and `compress.c` L44-L46 simply assigns both into a
/// `z_stream` -- so the reference runs, and whatever it produces is what a caller of an
/// overlapping pair gets. Refusing the pair would be a *new* error this library invented,
/// and a caller that the reference serves would stop working.
///
/// What cannot happen is a `&[u8]` and a `&mut [u8]` over one region: that is undefined
/// behaviour *whether or not either is ever touched*, and there is no later point at which
/// it could be undone. So the input is copied first, while no mutable borrow of the output
/// exists, and the operation reads the copy.
///
/// ★ **"While no mutable borrow exists" has to hold for every window, not just the first**,
/// and that is what [`one_shot_output`] is for. A bounded stage means the copy is repeated --
/// once per window -- so it is not enough to sequence the first copy ahead of a call-long
/// borrow of the destination: such a borrow would still be live when the second window was
/// taken, and a read through the caller's own pointer would invalidate it. On this arm the
/// destination is therefore borrowed a round at a time, so the two never coexist at all.
///
/// ★ **The copy is bounded**, which is the difference between this and the allocated snapshot
/// it replaces: it is a fixed stage the core is handed one window at a time, so an
/// overlapping call's peak memory no longer grows with the caller's own `sourceLen`. See
/// [`OverlapStage`] for the measurement that made that necessary and for the two respects in
/// which an overlapping call can be told from C's -- both inside the space C leaves undefined.
/// It also cannot *fail*, so `Z_MEM_ERROR` for an unobtainable snapshot is a status these
/// entry points can no longer produce.
///
/// The length out-parameters are deliberately *not* part of the overlap test. A slot inside
/// either buffer is served by ordering rather than by copying: the entry reads and the
/// `*destLen = 0` of `compress.c` L36 happen before any borrow exists, and the closing
/// write-back happens after every borrow has ended, which is exactly where C performs them.
///
/// Returns [`OneShotInput::Direct`] when the ranges are already disjoint -- the overwhelmingly
/// common case, in which nothing is copied at all and the core applies C's own `uInt`-sized
/// chunking and nothing else.
///
/// # Safety
///
/// `source` and `source_len` must satisfy [`source_slice`]'s contract: either the length is
/// zero, or that many bytes are readable at `source` and nothing mutates them for the duration
/// of the call, with no mutable borrow of any part of the region in existence. This therefore
/// has to run *before* the output is borrowed, not beside it.
#[must_use]
unsafe fn stage_one_shot_input(
    dest: *mut Bytef,
    dest_len: usize,
    source: *const Bytef,
    source_len: usize,
) -> OneShotInput {
    // Nothing is dereferenced to decide it: `ranges_are_disjoint` compares addresses.
    if ranges_are_disjoint(dest.cast_const(), dest_len, source, source_len) {
        // SAFETY: unsafe-site category 2 -- `source_slice`'s contract is this function's,
        // forwarded unweakened, and this arm is reached only when the region is disjoint from
        // the output.
        return OneShotInput::Direct(unsafe { source_slice(source, source_len) });
    }
    OneShotInput::Staged {
        origin: source,
        total: source_len,
        stage: OverlapStage::new(),
    }
}

/// What [`stage_one_shot_input`] decided, in the shape the core asks its input for.
///
/// ★ `clippy::large_enum_variant` is allowed here rather than obeyed, because the size
/// difference it reports *is* the design and its remedy is the defect. The lint would have
/// `Staged` boxed; a box is an allocation, and the whole reason this type exists is that the
/// earlier design's allocation -- one block sized from the caller's own `sourceLen`, taken per
/// call -- put a caller-controlled amount of memory on top of the reference implementation's
/// per-stream footprint, which AAP §0.8.4 caps at 15%. Boxing would shrink the enum and
/// reintroduce the allocation, and would do it from Rust's global allocator, where neither the
/// caller's `zfree` accounting nor `test/infcover.c`'s `mem_limit()` can see it. A fixed
/// [`OVERLAP_STAGE_BYTES`] of stack in a function C calls is the cost being chosen deliberately;
/// the enum is one stack frame deep and its larger arm is never touched on the direct path.
#[allow(clippy::large_enum_variant)]
enum OneShotInput {
    /// The ranges are disjoint; the core reads the caller's buffer directly.
    Direct(&'static [u8]),
    /// The ranges overlap; each window is copied into the bounded stage first.
    Staged {
        /// Base of the caller's input region.
        origin: *const Bytef,
        /// `sourceLen`, i.e. how much of it the call is to consume.
        total: usize,
        /// The bounded copy one window lives in.
        stage: OverlapStage,
    },
}

impl OneShotSource for OneShotInput {
    fn total(&self) -> usize {
        match self {
            Self::Direct(bytes) => bytes.len(),
            Self::Staged { total, .. } => *total,
        }
    }

    fn max_window(&self) -> usize {
        match self {
            // No chunking beyond the core's own, so a disjoint call is byte-for-byte the call
            // it was before bounded staging existed.
            Self::Direct(_) => usize::MAX,
            Self::Staged { .. } => OVERLAP_STAGE_BYTES,
        }
    }

    fn window(&mut self, at: usize, len: usize) -> Option<&[u8]> {
        let end = at.checked_add(len)?;
        match self {
            Self::Direct(bytes) => bytes.get(at..end),
            Self::Staged {
                origin,
                total,
                stage,
            } => {
                if end > *total {
                    return None;
                }
                // SAFETY: unsafe-site category 2 -- `OverlapStage::fill`'s contract, which is
                // `stage_one_shot_input`'s own: `total` bytes are readable at `origin` and
                // nothing mutates them for the duration of the call, so the `len` bytes at
                // `origin + at` are readable too, `end <= total` having just been established.
                //
                // ★ No mutable borrow of that region exists **at this instant**, and that is
                // the whole invariant rather than a restatement of the obvious. The output
                // borrow on this path is [`OneShotOutput::Rounds`], which the core takes only
                // *after* this call returns and drops at the end of the round, before the next
                // one; so a read here is not a foreign access to a live exclusive borrow. It
                // is the ordering `OneShotSink` exists to make expressible, and the reason a
                // call-long output region would be undefined behaviour on this arm however
                // little the two were used together.
                //
                // `len` is at most `OVERLAP_STAGE_BYTES` because `max_window` says so and the
                // core honours it, so the returned slice is exactly `len` bytes.
                Some(unsafe { stage.fill(origin.wrapping_add(at), len) })
            }
        }
    }
}

/// Builds the destination provider that matches the input decision
/// [`stage_one_shot_input`] took.
///
/// ★ **Why the destination has a provider at all, and why this is a soundness requirement.**
/// On the staged path the core copies each input window out of memory this destination also
/// covers, and Rust's aliasing rules forbid a read through the caller's own pointer while an
/// exclusive borrow of that memory is live -- whether or not either is ever used again. The
/// copy therefore has to happen with **no** borrow of the destination in existence, and a
/// single call-long [`OutputRegion`] cannot offer that. [`OneShotSink`] does: the core takes
/// a region for one round, immediately after that round's window, and drops it before the
/// next window is copied. This function is the two answers to it.
///
/// On the disjoint path -- every conforming call, and every call the differential corpus
/// makes -- the region is built exactly once, as it always was, and the core's per-round ask
/// is the reborrow it always took.
///
/// # Safety
///
/// `dest` and `dest_len` must satisfy [`dest_slice`]'s contract: either the length is zero,
/// or that many bytes are writable at `dest` and nothing else views them for the duration of
/// the call. On the [`OneShotOutput::Rounds`] arm the same obligation is discharged once per
/// round instead of once per call, which is what makes it compatible with the input copy.
#[must_use]
unsafe fn one_shot_output(
    dest: *mut Bytef,
    dest_len: usize,
    staged: &OneShotInput,
) -> OneShotOutput {
    match staged {
        // SAFETY: unsafe-site category 2 -- `dest_slice`'s contract is this function's,
        // forwarded unweakened. This arm is reached only when the input was proved disjoint
        // from the destination, so the single call-long borrow cannot alias anything the
        // core reads.
        OneShotInput::Direct(_) => OneShotOutput::Whole(unsafe { dest_slice(dest, dest_len) }),
        OneShotInput::Staged { .. } => OneShotOutput::Rounds {
            base: dest,
            total: dest_len,
        },
    }
}

/// What [`one_shot_output`] decided, in the shape the core asks its destination for.
enum OneShotOutput {
    /// The ranges are disjoint; one region over the caller's buffer serves the whole call.
    Whole(OutputRegion<'static>),
    /// The ranges overlap; a region is rebuilt from the caller's pointer for each round and
    /// dies with it, so that no borrow of the destination is live when the next input window
    /// is copied.
    Rounds {
        /// The caller's `dest`, i.e. C's `next_out` for this one-shot call.
        base: *mut Bytef,
        /// `*destLen` on entry: the caller's whole extent, and the bound every round's
        /// range is checked against.
        total: usize,
    },
}

impl OneShotSink for OneShotOutput {
    fn total(&self) -> usize {
        match self {
            Self::Whole(region) => region.len(),
            Self::Rounds { total, .. } => *total,
        }
    }

    fn window(&mut self, at: usize, len: usize) -> Option<OutputRegion<'_>> {
        let end = at.checked_add(len)?;
        match self {
            Self::Whole(region) => Some(region.reborrow(at, len)),
            Self::Rounds { base, total } => {
                if end > *total {
                    return None;
                }
                if len == 0 {
                    // No room asked for, so no slice is formed -- the same rule
                    // `dest_slice` applies to a zero `*destLen`, and it is what keeps a
                    // one-past-the-end pointer out of `from_raw_parts_mut`.
                    return Some(OutputRegion::empty());
                }
                // SAFETY: unsafe-site category 2 -- reconstruction from a pointer/length
                // pair, once per round. `base` is non-null with a non-zero `total` on this
                // arm, because `stage_one_shot_input` only reports an overlap for two
                // non-empty ranges, so `base.wrapping_add(at)` is in bounds by the
                // `end <= total` test above and `len` bytes there are writable by this
                // type's contract. It is the ONLY view of those bytes in existence: the
                // input arrives through `OverlapStage`, which copies before this is built
                // and after it has been dropped, and this borrow dies with the round.
                let slots = unsafe {
                    core::slice::from_raw_parts_mut(
                        base.wrapping_add(at).cast::<MaybeUninit<u8>>(),
                        len,
                    )
                };
                Some(OutputRegion::write_only(slots, init_view()))
            }
        }
    }
}

/// Reports whether `level` is one `deflateInit_` would accept.
///
/// ★ **The order this exists to fix.** `compress.c` L42 reaches `deflateInit` only
/// after `left = *destLen; *destLen = 0;`, and returns its status without ever
/// looking at the `source` or `dest` *bytes*: C assigns those pointers into the
/// stream at L44-L47, which happens after the early return. So for an invalid level
/// a conforming C caller may legitimately pass a wild non-null `dest` or `source`
/// and still receive `Z_STREAM_ERROR`. Building slices from them first -- which is
/// undefined behaviour for a wild pointer -- would turn that documented refusal into
/// a memory-safety defect, so the level is screened here, before the borrows.
///
/// Routed through the core's own [`normalize_deflate_level`] rather than
/// re-spelling the range, so the accepted set has one definition: `deflate.c` L419
/// resolves `Z_DEFAULT_COMPRESSION` to 6 and L435 rejects anything outside `0 ..= 9`.
#[must_use]
fn level_is_accepted(level: c_int) -> bool {
    normalize_deflate_level(level).is_ok()
}

// ---------------------------------------------------------------------------
// Compression -- `compress.c`
// ---------------------------------------------------------------------------

/// Compresses `source` into `dest` at `level` (`z_size_t` lengths).
///
/// Declared at `zlib.h` L1291; the C ABI form of `compress2_z`
/// (`compress.c` L24-L66). This is the primary of the four compression entry
/// points: the other three reach the core through it or through the same core
/// function.
///
/// `level` has the same meaning as in `deflateInit`: `0 ..= 9`, or
/// `Z_DEFAULT_COMPRESSION` (-1). It is **not** validated here, because C does not
/// validate it here either -- the encoder's own initialisation does, and
/// forwarding its status verbatim is what makes an invalid level report
/// `Z_STREAM_ERROR`.
///
/// # Parameters
///
/// * `dest` -- the output buffer. May be null only when `*destLen` is zero.
/// * `destLen` -- in: the room available; out: the bytes produced. Must not be
///   null.
/// * `source` -- the input buffer. May be null only when `sourceLen` is zero.
/// * `sourceLen` -- the input length, by value.
///
/// # Returns
///
/// `Z_OK`, `Z_MEM_ERROR` if the encoder could not allocate, `Z_BUF_ERROR` if
/// `dest` had no room left, or `Z_STREAM_ERROR` for an invalid `level` or a
/// rejected argument (`zlib.h` L1302-L1304).
///
/// # ★ The null-`dest` translation
///
/// The one place this wrapper's answer is not simply the core's. C assigns
/// `stream.next_out = dest` at L45 without checking it, so a null `dest` paired
/// with a zero `*destLen` passes the entry guard and reaches `deflate`, which
/// rejects a null `next_out` (`deflate.c` L1016) with `Z_STREAM_ERROR`. That was
/// measured against the reference build, not inferred.
///
/// An empty Rust slice is non-null by construction, so the core sees an ordinary
/// zero-length buffer and answers `Z_BUF_ERROR`. This wrapper therefore rewrites
/// `Z_BUF_ERROR` to `Z_STREAM_ERROR` when -- and only when -- `dest` is null.
/// Rewriting only that one status is what preserves the two paths that reach
/// `deflate` no differently for a null `dest` than for a valid one: an invalid
/// `level` still reports `Z_STREAM_ERROR` and a failed allocation still reports
/// `Z_MEM_ERROR`, because the core produces both before the loop begins.
///
/// # Safety
///
/// The pointers must satisfy the ordinary C contract: `source` readable for
/// `sourceLen` bytes when that is non-zero, `dest` writable for `*destLen` bytes
/// when that is non-zero, `destLen` a valid, aligned, writable `z_size_t`, and the
/// two buffers not overlapping each other or `destLen`. All three may be null; a
/// null is diagnosed and reported rather than dereferenced.
#[no_mangle]
pub unsafe extern "C" fn compress2_z(
    dest: *mut Bytef,
    destLen: *mut z_size_t,
    source: *const Bytef,
    sourceLen: z_size_t,
    level: c_int,
) -> c_int {
    guard_code(|| {
        // `compress.c` L31-L33, in C's order and with C's short-circuiting: the
        // `destLen == NULL` test precedes the only dereference of it, so `*destLen`
        // is never read from a null pointer.
        if (sourceLen > 0 && source.is_null()) || destLen.is_null() {
            return fallback::STREAM_ERROR_CODE;
        }

        // SAFETY: unsafe-site category 2 -- reading a caller's out-parameter.
        // `destLen` is non-null by the test above; the function's contract makes it
        // a live, aligned, correctly-typed `z_size_t` for the duration of the call.
        // This is the entry value C names `*destLen` at L35.
        let dest_len = unsafe { *destLen };

        if dest_len > 0 && dest.is_null() {
            // Still inside L31-L33, so no write has happened: on this path the
            // caller's `*destLen` keeps its entry value. Measured against the
            // reference, which leaves it untouched.
            return fallback::STREAM_ERROR_CODE;
        }

        // `compress.c` L35-L36: `left = *destLen; *destLen = 0;` -- performed *before*
        // the level is judged, exactly as C performs it, so a rejected level still
        // leaves the caller's count at zero. It is also performed before either buffer
        // is borrowed, which is what makes a `destLen` inside `source` or `dest` legal:
        // the write lands where C's lands, and no borrow exists yet to conflict with it.
        //
        // SAFETY: unsafe-site category 2 -- writing a caller's out-parameter. Same
        // non-null, aligned, live `z_size_t` read immediately above; no reference to it
        // or to either buffer exists at this point.
        unsafe { *destLen = 0 };

        // `compress.c` L42: `err = deflateInit(&stream, level); if (err != Z_OK) return
        // err;`. Screened here rather than inside the core, because C returns from that
        // line without ever reading the `source` or `dest` bytes -- see
        // `level_is_accepted`.
        if !level_is_accepted(level) {
            return fallback::STREAM_ERROR_CODE;
        }

        // An overlapping pair is *served*, by staging the input in bounded windows; see
        // `stage_one_shot_input`.
        //
        // SAFETY: unsafe-site category 2 -- `stage_one_shot_input`'s contract. `source`
        // is readable for `sourceLen` bytes by the guard above and this function's
        // contract, and no mutable borrow of it exists: the output borrow is created
        // below, after this returns.
        let mut staged = unsafe { stage_one_shot_input(dest, dest_len, source, sourceLen) };

        // Both providers are built exactly once, inside a block that ends before the count
        // is written back -- which is what lets a `destLen` inside `dest` be written safely,
        // and is where C writes it (L63, after the loop). The helpers additionally map the
        // zero-length cases to genuine empty slices rather than to a dangling pointer, and
        // the destination provider decides once, from the same address comparison the input
        // did, whether the caller's buffer is borrowed for the call or for a round.
        let report = {
            // SAFETY: unsafe-site category 2 -- `one_shot_output`'s contract. `dest` is
            // non-null whenever `dest_len` is non-zero by the guard above, and the caller's
            // contract makes those bytes writable. On the disjoint arm one region covers
            // the whole call and the input was proved not to touch it; on the overlapping
            // arm no borrow of it exists between rounds, which is what lets the input be
            // copied out of the same memory.
            //
            // ★ The provider is held in a binding that outlives the core call, because the
            // regions it hands out borrow from it.
            let mut output = unsafe { one_shot_output(dest, dest_len, &staged) };
            core_compress2_z_from(&mut output, &mut staged, level)
        };

        // `compress.c` L36 and L63 together: the count is written on every path
        // past the guard. The core reports `produced == 0` for a failed encoder
        // initialisation, which is exactly the zero L36 leaves behind, so one
        // unconditional write reproduces both branches.
        //
        // SAFETY: unsafe-site category 2 -- writing a caller's out-parameter.
        // `destLen` is the same non-null, aligned, live `z_size_t` read above, and no
        // borrow of it or of either buffer is live: the block above has ended.
        unsafe { *destLen = report.produced };

        // The null-`dest` translation documented above. `dest.is_null()` implies
        // `dest_len == 0` here, because the guard rejected every other combination.
        if report.code == ReturnCode::BUF_ERROR && dest.is_null() {
            return fallback::STREAM_ERROR_CODE;
        }

        report.code
    })
}

/// Compresses `source` into `dest` at `level` (`uLong` lengths).
///
/// Declared at `zlib.h` L1288; the C ABI form of `compress2`
/// (`compress.c` L67-L74). C's body is the width adapter:
///
/// ```text
/// z_size_t got = *destLen;
/// ret = compress2_z(dest, &got, source, sourceLen, level);
/// *destLen = (uLong)got;
/// ```
///
/// This reproduces it against the same core function rather than by calling
/// [`compress2_z`] through the ABI, so that the widening and the narrowing are
/// visible in one place and the exported symbol does not depend on another
/// exported symbol's interposition-visible address.
///
/// ★ **The null `destLen` is where this diverges from C, deliberately.** C reads
/// `*destLen` at L70 with no null check, so `compress2(dest, NULL, ...)` is
/// undefined behaviour there -- in practice a segmentation fault. This
/// implementation reports `Z_STREAM_ERROR`, which is what `compress2_z` itself
/// answers for the same argument (`compress.c` L32) and what `zlib.h` documents
/// for a rejected argument. No conforming caller can observe the difference,
/// because the only alternative is a crash.
///
/// # Returns
///
/// As [`compress2_z`].
///
/// # Safety
///
/// As [`compress2_z`], with `destLen` a valid, aligned, writable [`uLongf`].
#[no_mangle]
pub unsafe extern "C" fn compress2(
    dest: *mut Bytef,
    destLen: *mut uLongf,
    source: *const Bytef,
    sourceLen: uLong,
    level: c_int,
) -> c_int {
    guard_code(|| {
        // C's guard lives in `compress2_z`; reproducing it here in the same order
        // keeps the two exports' answers identical for identical arguments.
        if (sourceLen > 0 && source.is_null()) || destLen.is_null() {
            return fallback::STREAM_ERROR_CODE;
        }

        // L70: `z_size_t got = *destLen;`
        //
        // SAFETY: unsafe-site category 2 -- reading a caller's out-parameter.
        // `destLen` is non-null by the test above and is a live, aligned `uLongf`
        // by this function's contract.
        let dest_len = widen_uLong(unsafe { *destLen });
        let source_len = widen_uLong(sourceLen);

        if dest_len > 0 && dest.is_null() {
            return fallback::STREAM_ERROR_CODE;
        }

        // As `compress2_z`: `*destLen` is zeroed before the level is judged, so a
        // rejected level leaves the same value behind that C leaves, and before either
        // buffer is borrowed, so a `destLen` inside one of them is written where C
        // writes it.
        //
        // SAFETY: unsafe-site category 2 -- writing a caller's out-parameter, the same
        // non-null, aligned, live `uLongf` read above; no reference to it or to either
        // buffer exists at this point.
        unsafe { *destLen = 0 };
        if !level_is_accepted(level) {
            return fallback::STREAM_ERROR_CODE;
        }

        // An overlapping pair is served by staging the input in bounded windows; see
        // `stage_one_shot_input`.
        // SAFETY: unsafe-site category 2 -- `stage_one_shot_input`'s contract, discharged
        // by the guard above and this function's own: `source` is readable for
        // `source_len` bytes and no mutable borrow of it exists yet.
        let mut staged = unsafe { stage_one_shot_input(dest, dest_len, source, source_len) };

        let report = {
            // SAFETY: unsafe-site category 2 -- `one_shot_output`'s contract. `dest` is
            // non-null whenever `dest_len` is non-zero by the guard above, and the caller's
            // contract makes those bytes writable. On the disjoint arm one region covers
            // the whole call and the input was proved not to touch it; on the overlapping
            // arm no borrow of it exists between rounds, which is what lets the input be
            // copied out of the same memory.
            //
            // ★ The provider is held in a binding that outlives the core call, because the
            // regions it hands out borrow from it.
            let mut output = unsafe { one_shot_output(dest, dest_len, &staged) };
            core_compress2_z_from(&mut output, &mut staged, level)
        };

        // L72: `*destLen = (uLong)got;` -- unconditional, and narrowing exactly as
        // C's cast narrows.
        //
        // SAFETY: unsafe-site category 2 -- writing a caller's out-parameter, the
        // same non-null, aligned, live `uLongf` read above.
        unsafe { *destLen = narrow_uLong(report.produced) };

        if report.code == ReturnCode::BUF_ERROR && dest.is_null() {
            return fallback::STREAM_ERROR_CODE;
        }

        report.code
    })
}

/// Compresses `source` into `dest` at the default level (`z_size_t` lengths).
///
/// Declared at `zlib.h` L1273; the C ABI form of `compress_z`
/// (`compress.c` L77-L81), which is `compress2_z` with `Z_DEFAULT_COMPRESSION`.
/// The encoder resolves that to level 6.
///
/// # Returns
///
/// `Z_OK`, `Z_MEM_ERROR` or `Z_BUF_ERROR` (`zlib.h` L1284-L1286). `Z_STREAM_ERROR`
/// is additionally possible for a rejected argument, though not for a bad level:
/// the level is supplied here rather than by the caller.
///
/// # Safety
///
/// As [`compress2_z`].
#[no_mangle]
pub unsafe extern "C" fn compress_z(
    dest: *mut Bytef,
    destLen: *mut z_size_t,
    source: *const Bytef,
    sourceLen: z_size_t,
) -> c_int {
    // SAFETY: unsafe-site category 2, by delegation -- this block introduces no raw pointer
    // operation of its own, and the arguments reach `compress2_z` exactly as they arrived,
    // so its obligation is this function's own, unweakened: `source` is readable for
    // `sourceLen` bytes when that count is non-zero, `dest` is writable for `*destLen`
    // bytes when that count is non-zero, `destLen` is a valid, aligned, writable length,
    // and none of the three overlaps another -- with a null in any of the three positions
    // diagnosed and reported rather than dereferenced. The level literal is
    // `Z_DEFAULT_COMPRESSION` (`zlib.h` L196), spelled as `zlib_rs`'s constant so that the
    // two cannot drift apart.
    unsafe { compress2_z(dest, destLen, source, sourceLen, Z_DEFAULT_COMPRESSION) }
}

/// Compresses `source` into `dest` at the default level (`uLong` lengths).
///
/// Declared at `zlib.h` L1271; the C ABI form of `compress`
/// (`compress.c` L82-L85). This is the entry point most callers use --
/// `test/example.c` L69 among them -- and the prose at `zlib.h` L1275-L1286 is
/// written about it. L1280-L1281 states the equivalence implemented here:
/// "`compress()` is equivalent to `compress2()` with a level parameter of
/// `Z_DEFAULT_COMPRESSION`".
///
/// ★ It delegates to [`compress2`], **not** to [`compress_z`]. C's chain is
/// `compress` -> `compress2` -> `compress2_z`: the plain form takes the `uLong`
/// route and only the `_z` form takes the `z_size_t` route. Reproducing the chain
/// rather than tidying it matters because the two routes differ on LLP64 Windows,
/// where the `uLong` route narrows the reported count and the `z_size_t` route does
/// not. [`uncompress`] is asymmetric with this in the reference, equally
/// deliberately.
///
/// # Returns
///
/// As [`compress_z`].
///
/// # Safety
///
/// As [`compress2`].
#[no_mangle]
pub unsafe extern "C" fn compress(
    dest: *mut Bytef,
    destLen: *mut uLongf,
    source: *const Bytef,
    sourceLen: uLong,
) -> c_int {
    // SAFETY: unsafe-site category 2, by delegation -- no raw pointer operation of its own,
    // and the arguments reach `compress2` exactly as they arrived, so its obligation is
    // this function's own, unweakened: `source` is readable for `sourceLen` bytes when that
    // count is non-zero, `dest` is writable for `*destLen` bytes when that count is
    // non-zero, `destLen` is a valid, aligned, writable length, and none of the three
    // overlaps another -- with a null in any of the three positions diagnosed and reported
    // rather than dereferenced.
    unsafe { compress2(dest, destLen, source, sourceLen, Z_DEFAULT_COMPRESSION) }
}

/// An upper bound on the compressed size of `sourceLen` bytes (`z_size_t` width).
///
/// Declared at `zlib.h` L1308; the C ABI form of `compressBound_z`
/// (`compress.c` L91-L95). `zlib.h` L1310-L1312: it "returns an upper bound on the
/// compressed size after `compress()` or `compress2()` on `sourceLen` bytes. It
/// would be used before a `compress()` or `compress2()` call to allocate the
/// destination buffer."
///
/// Pure: no stream, no state, no allocation, and no failure mode. The arithmetic
/// -- the shifts 12, 14 and 25, the addend 13, the wrapping additions and the
/// `bound < sourceLen` overflow test -- lives in the core, transcribed operator for
/// operator, and returns [`usize::MAX`] (C's `(z_size_t)-1`) when it cannot
/// promise a size. This wrapper only crosses the ABI.
///
/// ★ A wrong value here is a heap overflow in *caller* code; see the module
/// documentation and AAP §0.6.2.2.
#[no_mangle]
pub extern "C" fn compressBound_z(sourceLen: z_size_t) -> z_size_t {
    // No pointer, so nothing to validate and nothing unsafe. The guard is still
    // applied: it costs nothing on the return path and keeps the policy uniform
    // across all ten exports rather than making a reader confirm the exception.
    guard(|| core_compress_bound_z(sourceLen))
}

/// An upper bound on the compressed size of `sourceLen` bytes (`uLong` width).
///
/// Declared at `zlib.h` L1307; the C ABI form of `compressBound`
/// (`compress.c` L96-L99):
///
/// ```text
/// z_size_t bound = compressBound_z(sourceLen);
/// return (uLong)bound != bound ? (uLong)-1 : (uLong)bound;
/// ```
///
/// ★ **This is the saturation the safe core deliberately leaves to the facade.**
/// The core computes in one length domain, `usize`, where `(usize)bound != bound`
/// is false for every input. The test only has anything to do where `uLong` is
/// narrower than `z_size_t` -- LLP64 Windows, `unsigned long` at four bytes
/// against `size_t` at eight -- and there the answer must be `(uLong)-1` rather
/// than a truncated bound, because a truncated bound is a *smaller* number and
/// would silently under-size a caller's buffer.
///
/// Expressed as a comparison against [`uLong::MAX`] rather than as a cast
/// round-trip: `bound > uLong::MAX as usize` is the same predicate as
/// `(uLong)bound != bound` for unsigned types, and it reads as the intent instead
/// of as a trick. The sentinel comes from `fallback::BOUND`, so this export and
/// `deflateBound` cannot disagree about it.
///
/// Pure: no stream, no state, no allocation, no failure mode.
#[no_mangle]
pub extern "C" fn compressBound(sourceLen: uLong) -> uLong {
    guard(|| {
        let bound = core_compress_bound_z(widen_uLong(sourceLen));

        // `(uLong)bound != bound`, i.e. "does the bound survive the narrowing?".
        // `c_ulong::MAX` widens to `usize` losslessly by the assertion at the top
        // of the module, so the comparison is exact on every target.
        if bound > widen_uLong(c_ulong::MAX) {
            fallback::BOUND
        } else {
            narrow_uLong(bound)
        }
    })
}

// ---------------------------------------------------------------------------
// Decompression -- `uncompr.c`
// ---------------------------------------------------------------------------

/// Decompresses `source` into `dest`, reporting both counts (`z_size_t` lengths).
///
/// Declared at `zlib.h` L1337; the C ABI form of `uncompress2_z`
/// (`uncompr.c` L29-L82). This is the primary of the four decompression entry
/// points.
///
/// # ★ `sourceLen` is in **and** out
///
/// `zlib.h` L1339-L1341: "Same as uncompress, except that sourceLen is a pointer,
/// where the length of the source is *sourceLen. On return, *sourceLen is the
/// number of source bytes consumed." So `source + *sourceLen` addresses the first
/// unused input byte on return, which is how a caller finds whatever follows a
/// zlib stream inside a larger container. Both out-parameters are written; see the
/// write-back table in the module documentation for the one path that writes
/// neither.
///
/// # Parameters
///
/// * `dest` -- the output buffer. May be null only when `*destLen` is zero.
/// * `destLen` -- in: the room available; out: the bytes produced. Must not be
///   null.
/// * `source` -- the input buffer. May be null only when `*sourceLen` is zero.
/// * `sourceLen` -- in: the input length; out: the bytes consumed. Must not be
///   null.
///
/// # Returns
///
/// `Z_OK`, `Z_MEM_ERROR`, `Z_BUF_ERROR` if `dest` filled up first, `Z_DATA_ERROR`
/// if the input was corrupt or is an incomplete zlib stream, or `Z_STREAM_ERROR`
/// for a rejected argument. The core owns the ladder at `uncompr.c` L78-L81 --
/// `Z_STREAM_END` becomes `Z_OK`, `Z_NEED_DICT` becomes `Z_DATA_ERROR`, and a
/// `Z_BUF_ERROR` that consumed every input byte becomes `Z_DATA_ERROR` -- and this
/// wrapper passes its answer through unchanged.
///
/// ★ Unlike [`compress2_z`], a null `dest` needs no translation here.
/// `uncompr.c` L42-L43 aims `next_out` at `&stream.reserved` for exactly that
/// case, so C never presents the decoder with a null output pointer either; an
/// empty Rust slice is already the non-null zero-length buffer C manufactures.
/// Measured: a stream with content answers `Z_BUF_ERROR`, an empty one `Z_OK`.
///
/// # Safety
///
/// `source` readable for `*sourceLen` bytes when that is non-zero, `dest` writable
/// for `*destLen` bytes when that is non-zero, `destLen` and `sourceLen` valid,
/// aligned, writable and distinct, and no overlap among the four. All four may be
/// null; a null is diagnosed and reported rather than dereferenced.
#[no_mangle]
pub unsafe extern "C" fn uncompress2_z(
    dest: *mut Bytef,
    destLen: *mut z_size_t,
    source: *const Bytef,
    sourceLen: *mut z_size_t,
) -> c_int {
    guard_code(|| {
        // `uncompr.c` L36-L38, in C's order and with C's short-circuiting: both
        // null tests precede the corresponding dereference.
        if sourceLen.is_null() {
            return fallback::STREAM_ERROR_CODE;
        }

        // SAFETY: unsafe-site category 2 -- reading a caller's out-parameter.
        // `sourceLen` is non-null by the test above and is a live, aligned
        // `z_size_t` by this function's contract. This is C's `*sourceLen` at L40.
        let source_len = unsafe { *sourceLen };

        if (source_len > 0 && source.is_null()) || destLen.is_null() {
            return fallback::STREAM_ERROR_CODE;
        }

        // SAFETY: unsafe-site category 2 -- reading a caller's out-parameter.
        // `destLen` is non-null by the test above and live and aligned by contract.
        // This is C's `*destLen` at L41.
        let dest_len = unsafe { *destLen };

        if dest_len > 0 && dest.is_null() {
            // Still inside L36-L38: no write has happened, so both of the caller's
            // out-parameters keep their entry values. Measured against the
            // reference, which leaves both untouched on every guard path.
            return fallback::STREAM_ERROR_CODE;
        }

        // An overlapping pair is served by staging the input in bounded windows; see
        // `stage_one_shot_input`.
        // Neither length slot enters that decision: both are written after every borrow
        // has ended, which is where `uncompr.c` L74-L75 writes them. `uncompress` needs
        // no level gate either: it has no level argument, and `inflateInit` cannot reject
        // anything a caller supplied.
        // SAFETY: unsafe-site category 2 -- `stage_one_shot_input`'s contract, discharged
        // by the guards above and this function's own: `source` is readable for
        // `source_len` bytes and no mutable borrow of it exists yet.
        let mut staged = unsafe { stage_one_shot_input(dest, dest_len, source, source_len) };

        // Both providers are built exactly once, inside a block that ends before either count
        // is written back. An empty output slice is additionally what discharges
        // `uncompr.c` L42-L43's "next_out cannot be NULL", with no scratch space to
        // manufacture.
        let report = {
            // SAFETY: unsafe-site category 2 -- `one_shot_output`'s contract. `dest` is
            // non-null whenever `dest_len` is non-zero by the guard above, and the caller's
            // contract makes those bytes writable. On the disjoint arm one region covers
            // the whole call and the input was proved not to touch it; on the overlapping
            // arm no borrow of it exists between rounds, which is what lets the input be
            // copied out of the same memory.
            //
            // ★ The provider is held in a binding that outlives the core call, because the
            // regions it hands out borrow from it.
            let mut output = unsafe { one_shot_output(dest, dest_len, &staged) };
            core_uncompress2_z_from(&mut output, &mut staged)
        };

        // `uncompr.c` L74-L75: `*sourceLen -= len; *destLen -= left;`.
        //
        // ★ Both are read-modify-writes in C, and are reproduced as read-modify-writes
        // rather than as assignments. For every disjoint call the two are identical --
        // the slot still holds its entry value, so subtracting the unused counts yields
        // exactly `consumed` and `produced`. They differ only for a slot that lies inside
        // `dest`, where the decoder has just written over it: C then subtracts from
        // whatever it wrote, and so does this. `saturating_sub` bounds the one case C
        // leaves to wrap-around, which no conforming call can reach.
        //
        // Both writes are unconditional past the guard, which reproduces L51-L52 as
        // well: on a failed decoder initialisation C returns before either write, and the
        // core reports the *entry* lengths for that path, so the unused counts are zero
        // and the caller's variables are left exactly as they were.
        let source_unused = source_len.saturating_sub(report.consumed);
        let dest_unused = dest_len.saturating_sub(report.produced);
        // SAFETY: unsafe-site category 2 -- reading and writing two caller
        // out-parameters. Both pointers are the non-null, aligned, live `z_size_t`s read
        // above; the contract requires them to be distinct, each is read and written
        // once, and no borrow of either buffer is live: the block above has ended.
        unsafe {
            *sourceLen = (*sourceLen).saturating_sub(source_unused);
            *destLen = (*destLen).saturating_sub(dest_unused);
        }

        report.code
    })
}

/// Decompresses `source` into `dest`, reporting both counts (`uLong` lengths).
///
/// Declared at `zlib.h` L1335; the C ABI form of `uncompress2`
/// (`uncompr.c` L83-L91). C's body is the width adapter:
///
/// ```text
/// z_size_t got = *destLen, used = *sourceLen;
/// ret = uncompress2_z(dest, &got, source, &used);
/// *sourceLen = (uLong)used;
/// *destLen = (uLong)got;
/// ```
///
/// Both writes are unconditional in C, and both are unconditional here past the
/// guard. Note the order: `*sourceLen` first, then `*destLen`. It is not
/// observable -- the contract requires the two pointers to be distinct -- but it is
/// reproduced anyway, because a reader comparing the two sources should not have to
/// wonder whether a difference was deliberate.
///
/// ★ As with [`compress2`], a null `destLen` or `sourceLen` is undefined behaviour
/// in C (L86 dereferences both unchecked) and is reported as `Z_STREAM_ERROR`
/// here.
///
/// # Returns
///
/// As [`uncompress2_z`].
///
/// # Safety
///
/// As [`uncompress2_z`], with `destLen` a [`uLongf`] and `sourceLen` a [`uLong`].
#[no_mangle]
pub unsafe extern "C" fn uncompress2(
    dest: *mut Bytef,
    destLen: *mut uLongf,
    source: *const Bytef,
    sourceLen: *mut uLong,
) -> c_int {
    guard_code(|| {
        // C's guard lives in `uncompress2_z`; reproducing it here in the same order
        // keeps the two exports' answers identical for identical arguments.
        if sourceLen.is_null() {
            return fallback::STREAM_ERROR_CODE;
        }

        // L86: `used = *sourceLen`.
        //
        // SAFETY: unsafe-site category 2 -- reading a caller's out-parameter.
        // `sourceLen` is non-null by the test above and live and aligned by
        // contract.
        let source_len = widen_uLong(unsafe { *sourceLen });

        if (source_len > 0 && source.is_null()) || destLen.is_null() {
            return fallback::STREAM_ERROR_CODE;
        }

        // L86: `got = *destLen`.
        //
        // SAFETY: unsafe-site category 2 -- reading a caller's out-parameter.
        // `destLen` is non-null by the test above and live and aligned by contract.
        let dest_len = widen_uLong(unsafe { *destLen });

        if dest_len > 0 && dest.is_null() {
            return fallback::STREAM_ERROR_CODE;
        }

        // As `uncompress2_z`: an overlapping pair is served by staging the input in bounded
        // windows, and neither length slot enters that decision because both are written
        // after every borrow has ended. `uLongf` is `uLong` (`zconf.h` L410), so both slots
        // share one width here.
        //
        // SAFETY: unsafe-site category 2 -- `stage_one_shot_input`'s contract, discharged
        // by the guards above and this function's own: `source` is readable for
        // `source_len` bytes and no mutable borrow of it exists yet.
        let mut staged = unsafe { stage_one_shot_input(dest, dest_len, source, source_len) };

        let report = {
            // SAFETY: unsafe-site category 2 -- `one_shot_output`'s contract. `dest` is
            // non-null whenever `dest_len` is non-zero by the guard above, and the caller's
            // contract makes those bytes writable. On the disjoint arm one region covers
            // the whole call and the input was proved not to touch it; on the overlapping
            // arm no borrow of it exists between rounds, which is what lets the input be
            // copied out of the same memory.
            //
            // ★ The provider is held in a binding that outlives the core call, because the
            // regions it hands out borrow from it.
            let mut output = unsafe { one_shot_output(dest, dest_len, &staged) };
            core_uncompress2_z_from(&mut output, &mut staged)
        };

        // L88-L89, in C's order, each narrowing exactly as C's cast narrows.
        //
        // ★ Assignments, not read-modify-writes, and the difference from
        // `uncompress2_z` is the reference's own: C passes *locals* to the inner call
        // (L86's `got` and `used`), so the inner `-=` applies to those locals and these
        // two statements copy the results out. `uncompress2_z` is the one that receives
        // the caller's own slots.
        //
        // SAFETY: unsafe-site category 2 -- writing two caller out-parameters, the same
        // non-null, aligned, live, distinct pointers read above, with no borrow of either
        // buffer live: the block above has ended.
        unsafe {
            *sourceLen = narrow_uLong(report.consumed);
            *destLen = narrow_uLong(report.produced);
        }

        report.code
    })
}

/// Decompresses `source` into `dest` (`z_size_t` lengths, input length by value).
///
/// Declared at `zlib.h` L1317; the C ABI form of `uncompress_z`
/// (`uncompr.c` L92-L96), which copies the by-value length into a local and calls
/// `uncompress2_z` with its address:
///
/// ```text
/// z_size_t used = sourceLen;
/// return uncompress2_z(dest, destLen, source, &used);
/// ```
///
/// The consumed count therefore has nowhere to go and C discards it. So does this
/// wrapper: a caller that needs it calls [`uncompress2_z`], which is the entire
/// reason that entry point exists.
///
/// # Returns
///
/// `Z_OK`, `Z_MEM_ERROR`, `Z_BUF_ERROR` or `Z_DATA_ERROR` (`zlib.h` L1328-L1332),
/// or `Z_STREAM_ERROR` for a rejected argument. `zlib.h` L1330-L1332 adds the
/// guarantee this wrapper preserves: "In the case where there is not enough room,
/// `uncompress()` will fill the output buffer with the uncompressed data up to that
/// point" -- so `*destLen` is meaningful on `Z_BUF_ERROR` too.
///
/// # Safety
///
/// As [`uncompress2_z`], except that `sourceLen` is a value rather than a pointer
/// and so cannot be null.
#[no_mangle]
pub unsafe extern "C" fn uncompress_z(
    dest: *mut Bytef,
    destLen: *mut z_size_t,
    source: *const Bytef,
    sourceLen: z_size_t,
) -> c_int {
    // C materialises the by-value length as a local and passes its address, so that
    // one implementation serves both spellings. Doing the same here -- rather than
    // duplicating the body -- keeps the two exports' behaviour identical by
    // construction, and the local is the only thing that receives the discarded
    // consumed count.
    let mut used = sourceLen;

    // SAFETY: unsafe-site category 2, by delegation -- no raw pointer operation of its own,
    // and the arguments reach `uncompress2_z` exactly as they arrived, so its obligation is
    // this function's own, unweakened: `source` is readable for `*sourceLen` bytes when
    // that count is non-zero, `dest` is writable for `*destLen` bytes when that count is
    // non-zero, `destLen` and `sourceLen` are valid, aligned, writable and distinct, and
    // none of the four overlaps another -- with a null in any position diagnosed and
    // reported rather than dereferenced. The one argument this function supplies, `&mut
    // used`, is a live, aligned, writable `z_size_t` on this frame; it is distinct from
    // `destLen` and cannot overlap either buffer, because it is a local this function owns.
    unsafe { uncompress2_z(dest, destLen, source, &mut used) }
}

/// Decompresses `source` into `dest` (`uLong` lengths, input length by value).
///
/// Declared at `zlib.h` L1315; the C ABI form of `uncompress`
/// (`uncompr.c` L97-L101). This is the entry point most callers use --
/// `test/example.c` L75 among them, where `test_compress` checks it with
/// `CHECK_ERR` and then compares the recovered bytes against the original -- and
/// the prose at `zlib.h` L1319-L1332 is written about it.
///
/// ★ It delegates to [`uncompress2`], **not** to [`uncompress_z`]. C's chain is
/// `uncompress` -> `uncompress2` -> `uncompress2_z`: the plain form takes the
/// `uLong` route and only the `_z` form takes the `z_size_t` route. That is
/// asymmetric with [`compress`], which reaches `compress2` rather than
/// `compress_z`; both chains are reproduced as written, because a reader who
/// assumed symmetry would otherwise conclude the narrowing happens somewhere it
/// does not.
///
/// # Returns
///
/// As [`uncompress_z`].
///
/// # Safety
///
/// As [`uncompress2`], except that `sourceLen` is a value rather than a pointer
/// and so cannot be null.
#[no_mangle]
pub unsafe extern "C" fn uncompress(
    dest: *mut Bytef,
    destLen: *mut uLongf,
    source: *const Bytef,
    sourceLen: uLong,
) -> c_int {
    // `uLong used = sourceLen;` (L99), for the same reason as in `uncompress_z`.
    let mut used = sourceLen;

    // SAFETY: unsafe-site category 2, by delegation -- no raw pointer operation of its own,
    // and the arguments reach `uncompress2` exactly as they arrived, so its obligation is
    // this function's own, unweakened: `source` is readable for `*sourceLen` bytes when
    // that count is non-zero, `dest` is writable for `*destLen` bytes when that count is
    // non-zero, `destLen` and `sourceLen` are valid, aligned, writable and distinct, and
    // none of the four overlaps another -- with a null in any position diagnosed and
    // reported rather than dereferenced. The one argument this function supplies, `&mut
    // used`, is a live, aligned, writable `uLong` on this frame, distinct from `destLen`
    // and from both buffers because this function owns it.
    unsafe { uncompress2(dest, destLen, source, &mut used) }
}

// Every `(status, *destLen, *sourceLen)` expectation below was MEASURED against
// the reference implementation -- the in-tree C sources built as `libz.a` -- by
// calling the corresponding C entry point with the same arguments and recording
// what it returned and what it left in the caller's variables. None of them is
// derived from the Rust implementation or from the formula, because the point of
// the suite is to catch a divergence that looks reasonable. The compressed byte
// strings and the `compressBound` table in particular are transcribed from the
// reference's own output.
//
// What this module owns, and therefore what these tests are scoped to: the entry
// guards, the slice reconstruction, the width mapping, the write-back paths, the
// null-`dest` translation, and the two bounds. The chunking loop, the flush
// selector, the accounting and the status ladder belong to `zlib-rs` and are tested
// there; byte-for-byte equality across the whole
// level x windowBits x memLevel x strategy x flush matrix belongs to
// `crates/zlib-rs-differential`.
//
// `clippy.toml` sets `allow-unwrap-in-tests`, `allow-expect-in-tests` and
// `allow-panic-in-tests`, so assertions here may panic; nothing above this line
// may. `clippy::indexing_slicing` is denied workspace-wide and is relaxed HERE
// ONLY, on the test module: every index below is a literal into a buffer this
// module just built, so each one is provably in range.
#[allow(clippy::indexing_slicing)]
#[cfg(test)]
mod tests {
    use super::{
        compress, compress2, compress2_z, compressBound, compressBound_z, compress_z, narrow_uLong,
        uncompress, uncompress2, uncompress2_z, uncompress_z, widen_uLong,
    };
    use crate::types::{uLong, uLongf, z_size_t};
    use core::ffi::c_int;
    use core::ptr;
    use zlib_rs::error::ReturnCode;

    /// `Z_OK` (`zlib.h` L179).
    const OK: c_int = ReturnCode::OK.as_i32();
    /// `Z_STREAM_ERROR` (`zlib.h` L185).
    /// cbindgen:ignore
    const STREAM_ERROR: c_int = ReturnCode::STREAM_ERROR.as_i32();
    /// `Z_DATA_ERROR` (`zlib.h` L186).
    const DATA_ERROR: c_int = ReturnCode::DATA_ERROR.as_i32();
    /// `Z_BUF_ERROR` (`zlib.h` L188).
    const BUF_ERROR: c_int = ReturnCode::BUF_ERROR.as_i32();
    /// `Z_DEFAULT_COMPRESSION` (`zlib.h` L196), taken from the core rather than
    /// written as `-1` so that the two cannot drift apart.
    const DEFAULT_LEVEL: c_int = super::Z_DEFAULT_COMPRESSION;

    /// The payload `test/example.c` L35 declares, as L69 actually passes it:
    /// `strlen(hello) + 1`, so the terminating NUL is part of the payload.
    const HELLO: &[u8] = b"hello, hello!\0";

    /// `compress(HELLO)` at [`DEFAULT_LEVEL`]: 19 bytes, measured.
    const HELLO_DEFAULT: &[u8] = &[
        0x78, 0x9c, 0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0xd7, 0x51, 0xc8, 0x00, 0x51, 0x8a, 0x0c, 0x00,
        0x26, 0x06, 0x04, 0x96,
    ];

    /// `compress2(HELLO, 0)`: one stored block in a zlib container, 25 bytes,
    /// measured. The only level whose length differs from the rest.
    const HELLO_LEVEL_0: &[u8] = &[
        0x78, 0x01, 0x01, 0x0e, 0x00, 0xf1, 0xff, 0x68, 0x65, 0x6c, 0x6c, 0x6f, 0x2c, 0x20, 0x68,
        0x65, 0x6c, 0x6c, 0x6f, 0x21, 0x00, 0x26, 0x06, 0x04, 0x96,
    ];

    /// The measured `compress2(HELLO, level)` output for every level `1 ..= 9`.
    ///
    /// Only the two header bytes vary: `zlib.h`'s FLEVEL field records which of
    /// four speed/compression classes the level falls into, so levels 2-5 and
    /// levels 7-9 each share a spelling. The DEFLATE payload is identical across
    /// all nine, which is expected for an input this small.
    const HELLO_BY_LEVEL: [&[u8]; 9] = [
        &[
            0x78, 0x01, 0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0xd7, 0x51, 0xc8, 0x00, 0x51, 0x8a, 0x0c,
            0x00, 0x26, 0x06, 0x04, 0x96,
        ],
        &[
            0x78, 0x5e, 0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0xd7, 0x51, 0xc8, 0x00, 0x51, 0x8a, 0x0c,
            0x00, 0x26, 0x06, 0x04, 0x96,
        ],
        &[
            0x78, 0x5e, 0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0xd7, 0x51, 0xc8, 0x00, 0x51, 0x8a, 0x0c,
            0x00, 0x26, 0x06, 0x04, 0x96,
        ],
        &[
            0x78, 0x5e, 0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0xd7, 0x51, 0xc8, 0x00, 0x51, 0x8a, 0x0c,
            0x00, 0x26, 0x06, 0x04, 0x96,
        ],
        &[
            0x78, 0x5e, 0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0xd7, 0x51, 0xc8, 0x00, 0x51, 0x8a, 0x0c,
            0x00, 0x26, 0x06, 0x04, 0x96,
        ],
        &[
            0x78, 0x9c, 0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0xd7, 0x51, 0xc8, 0x00, 0x51, 0x8a, 0x0c,
            0x00, 0x26, 0x06, 0x04, 0x96,
        ],
        &[
            0x78, 0xda, 0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0xd7, 0x51, 0xc8, 0x00, 0x51, 0x8a, 0x0c,
            0x00, 0x26, 0x06, 0x04, 0x96,
        ],
        &[
            0x78, 0xda, 0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0xd7, 0x51, 0xc8, 0x00, 0x51, 0x8a, 0x0c,
            0x00, 0x26, 0x06, 0x04, 0x96,
        ],
        &[
            0x78, 0xda, 0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0xd7, 0x51, 0xc8, 0x00, 0x51, 0x8a, 0x0c,
            0x00, 0x26, 0x06, 0x04, 0x96,
        ],
    ];

    /// The eight-byte empty zlib stream, i.e. what the reference produces for a
    /// zero-length source at the default level. Measured.
    const EMPTY_STREAM: &[u8] = &[0x78, 0x9c, 0x03, 0x00, 0x00, 0x00, 0x00, 0x01];

    /// Compresses through the `uLong` export, returning the status and the bytes.
    ///
    /// A helper rather than a repeated block because every compression assertion
    /// needs the same three steps -- size the buffer, call, truncate to the
    /// reported count -- and repeating them would obscure which line is the
    /// assertion.
    fn compress_uLong(source: &[u8], level: c_int) -> (c_int, Vec<u8>) {
        let mut buffer = vec![0_u8; widen_uLong(compressBound(narrow_uLong(source.len())))];
        let mut dest_len = narrow_uLong(buffer.len());
        // SAFETY: `buffer` is a live, uniquely-borrowed allocation of `dest_len`
        // bytes; `source` is a live slice of its own length; `dest_len` is a local
        // this frame owns; none of the three overlaps another.
        let status = unsafe {
            compress2(
                buffer.as_mut_ptr(),
                &mut dest_len,
                source.as_ptr(),
                narrow_uLong(source.len()),
                level,
            )
        };
        buffer.truncate(widen_uLong(dest_len));
        (status, buffer)
    }

    /// Decompresses through the `uLong` export into a buffer of exactly `room`
    /// bytes, returning the status and the bytes produced.
    fn uncompress_uLong(source: &[u8], room: usize) -> (c_int, Vec<u8>) {
        let mut buffer = vec![0_u8; room];
        let mut dest_len = narrow_uLong(room);
        // SAFETY: `buffer` is a live, uniquely-borrowed allocation of exactly `dest_len`
        // bytes and is the destination; `source` is a live slice of its own length and is the
        // input; `dest_len` is a local this frame owns and nothing else borrows. None of the
        // three overlaps another, and all three outlive the call.
        let status = unsafe {
            uncompress(
                buffer.as_mut_ptr(),
                &mut dest_len,
                source.as_ptr(),
                narrow_uLong(source.len()),
            )
        };
        buffer.truncate(widen_uLong(dest_len));
        (status, buffer)
    }

    // -----------------------------------------------------------------------
    // The bound functions -- AAP §0.6.2.2
    // -----------------------------------------------------------------------

    /// The measured `compressBound` table. Every value came from the reference
    /// build; none was recomputed from the formula, because a transcription error
    /// in the formula is exactly what this table exists to catch.
    #[test]
    fn bound_matches_the_measured_reference_table() {
        // (sourceLen, bound). Every row here holds at any pointer width.
        let table: [(usize, usize); 7] = [
            (0, 13),
            (1, 14),
            (4095, 4108),
            (4096, 4110),
            (16383, 16399),
            (16384, 16402),
            (usize::MAX, usize::MAX),
        ];

        for (source_len, expected) in table {
            assert_eq!(
                compressBound_z(source_len),
                expected,
                "compressBound_z({source_len})"
            );
        }

        // The 2^32 - 1 row is measured only where `usize` is 64 bits, for two reasons:
        // its answer, `4_296_278_153`, is not representable in a 32-bit `usize` at all,
        // and on a 32-bit target `0xffff_ffff` *is* `usize::MAX`, which the last row
        // above already covers with the saturated `usize::MAX` the formula produces
        // there. Gated with `cfg` rather than a run-time `if`, because the literal would
        // otherwise be rejected by `overflowing_literals` before any test ran.
        #[cfg(target_pointer_width = "64")]
        assert_eq!(
            compressBound_z(0xffff_ffff),
            4_296_278_153,
            "compressBound_z(0xffff_ffff)"
        );
    }

    /// `compressBound` and `compressBound_z` agree wherever both widths hold the
    /// answer, which on LP64 is everywhere below the saturation point.
    #[test]
    fn bound_and_bound_z_agree_where_both_fit() {
        for source_len in [
            0_usize,
            1,
            2,
            255,
            4095,
            4096,
            16383,
            16384,
            1 << 20,
            1 << 25,
        ] {
            let wide = compressBound_z(source_len);
            assert!(
                wide <= widen_uLong(uLong::MAX),
                "fixture chosen so the bound fits in uLong"
            );
            assert_eq!(
                compressBound(narrow_uLong(source_len)),
                narrow_uLong(wide),
                "compressBound({source_len})"
            );
        }
    }

    /// The two saturations, which are the whole reason the pair exists.
    #[test]
    fn bound_saturates_at_both_widths() {
        // `compress.c` L94: the additions wrap, and a wrapped sum lands below
        // `sourceLen`, so the sentinel is `(z_size_t)-1`.
        assert_eq!(compressBound_z(usize::MAX), usize::MAX);
        assert_eq!(compressBound_z(usize::MAX - 1), usize::MAX);

        // `compress.c` L98: the `uLong` form additionally reports `(uLong)-1` when
        // the bound does not survive the narrowing. On LP64 the two widths are
        // equal, so this is the same input; on LLP64 it is the narrowing that
        // triggers. Either way the answer is the sentinel.
        assert_eq!(compressBound(uLong::MAX), uLong::MAX);
    }

    /// The bound is genuinely an upper bound: a buffer of exactly that size always
    /// holds the stream. This is the property callers depend on, and it is checked
    /// against real output rather than argued from the arithmetic.
    #[test]
    fn bound_is_large_enough_for_every_fixture() {
        let window_crossing = vec![b'q'; 40_000];
        let fixtures: [&[u8]; 5] = [b"", b"a", HELLO, &window_crossing, EMPTY_STREAM];

        for source in fixtures {
            for level in [DEFAULT_LEVEL, 0, 1, 6, 9] {
                let room = compressBound_z(source.len());
                let mut buffer = vec![0_u8; room];
                let mut dest_len = buffer.len();
                // SAFETY: a live, uniquely-borrowed destination of `dest_len`
                // bytes, a live source, and a local length; nothing overlaps.
                let status = unsafe {
                    compress2_z(
                        buffer.as_mut_ptr(),
                        &mut dest_len,
                        source.as_ptr(),
                        source.len(),
                        level,
                    )
                };
                assert_eq!(status, OK, "len {} level {level}", source.len());
                assert!(
                    dest_len <= room,
                    "produced {dest_len} exceeded bound {room}"
                );
            }
        }
    }

    // -----------------------------------------------------------------------
    // Compression: the exported bytes and the reported count
    // -----------------------------------------------------------------------

    /// The default-level output is byte-for-byte the reference's, through both the
    /// `uLong` and the `z_size_t` spelling, and both report the same count.
    #[test]
    fn compress_reproduces_the_reference_bytes() {
        let (status, bytes) = compress_uLong(HELLO, DEFAULT_LEVEL);
        assert_eq!(status, OK);
        assert_eq!(bytes, HELLO_DEFAULT);

        let mut buffer = vec![0_u8; 64];
        let mut dest_len = buffer.len();
        // SAFETY: a live, uniquely-borrowed destination, a live source, a local
        // length; nothing overlaps.
        let status = unsafe {
            compress_z(
                buffer.as_mut_ptr(),
                &mut dest_len,
                HELLO.as_ptr(),
                HELLO.len(),
            )
        };
        assert_eq!(status, OK);
        assert_eq!(&buffer[..dest_len], HELLO_DEFAULT);

        // And the plain `uLong` form, which `test/example.c` L69 calls.
        let mut wide_buffer = vec![0_u8; 64];
        let mut wide_len = narrow_uLong(wide_buffer.len());
        // SAFETY: `wide_buffer` is a live, uniquely-borrowed allocation of exactly `wide_len`
        // bytes; `HELLO` is a `'static` byte string, so its pointer is non-null and readable
        // for its whole length; `wide_len` is a local this frame owns. Nothing overlaps and
        // all three outlive the call.
        let status = unsafe {
            compress(
                wide_buffer.as_mut_ptr(),
                &mut wide_len,
                HELLO.as_ptr(),
                narrow_uLong(HELLO.len()),
            )
        };
        assert_eq!(status, OK);
        assert_eq!(&wide_buffer[..widen_uLong(wide_len)], HELLO_DEFAULT);
    }

    /// Every level `-1` and `0 ..= 9` matches the reference byte for byte.
    #[test]
    fn compress2_reproduces_the_reference_bytes_at_every_level() {
        let (status, bytes) = compress_uLong(HELLO, DEFAULT_LEVEL);
        assert_eq!(status, OK);
        assert_eq!(bytes, HELLO_DEFAULT, "level -1");

        let (status, bytes) = compress_uLong(HELLO, 0);
        assert_eq!(status, OK);
        assert_eq!(bytes, HELLO_LEVEL_0, "level 0");

        // Zipped rather than indexed so that the level and its expectation are
        // paired by construction and no cast is needed to bridge them.
        for (level, expected) in (1..=9_i32).zip(HELLO_BY_LEVEL) {
            let (status, bytes) = compress_uLong(HELLO, level);
            assert_eq!(status, OK, "level {level}");
            assert_eq!(bytes, expected, "level {level}");
        }
    }

    /// An empty source produces the eight-byte empty zlib stream, and a null
    /// `source` with a zero length is the same input rather than an error --
    /// `compress.c` L31 admits it and the reference answers `Z_OK` (measured).
    #[test]
    fn compress_accepts_an_empty_and_a_null_source() {
        let (status, bytes) = compress_uLong(b"", DEFAULT_LEVEL);
        assert_eq!(status, OK);
        assert_eq!(bytes, EMPTY_STREAM);

        let mut buffer = vec![0_u8; 64];
        let mut dest_len = narrow_uLong(buffer.len());
        // SAFETY: `source` is null with a zero length, which the helper maps to an
        // empty slice rather than to `from_raw_parts(null, 0)`. The destination is a
        // live, uniquely-borrowed allocation of `dest_len` bytes.
        let status = unsafe { compress(buffer.as_mut_ptr(), &mut dest_len, ptr::null(), 0) };
        assert_eq!(status, OK, "measured: the reference returns Z_OK");
        assert_eq!(widen_uLong(dest_len), EMPTY_STREAM.len());
        assert_eq!(&buffer[..widen_uLong(dest_len)], EMPTY_STREAM);
    }

    /// An invalid level is the encoder's rejection, forwarded, and it clears the
    /// caller's count -- `compress.c` L36 writes the zero before initialising.
    #[test]
    fn compress2_forwards_an_invalid_level_and_clears_the_count() {
        for level in [-2_i32, 10, 42] {
            let mut buffer = vec![0_u8; 64];
            let mut dest_len = narrow_uLong(buffer.len());
            // SAFETY: a live, uniquely-borrowed destination and a live source.
            let status = unsafe {
                compress2(
                    buffer.as_mut_ptr(),
                    &mut dest_len,
                    HELLO.as_ptr(),
                    narrow_uLong(HELLO.len()),
                    level,
                )
            };
            assert_eq!(status, STREAM_ERROR, "level {level}");
            assert_eq!(dest_len, 0, "level {level}: measured *destLen == 0");
        }
    }

    /// A destination too small reports `Z_BUF_ERROR` together with every byte that
    /// did fit, because `compress.c` L63 assigns the count before L65 translates
    /// the status.
    #[test]
    fn compress_reports_the_partial_count_on_a_short_destination() {
        let mut buffer = vec![0_u8; HELLO_DEFAULT.len() - 1];
        let room = buffer.len();
        let mut dest_len = narrow_uLong(room);
        // SAFETY: a live, uniquely-borrowed destination of `room` bytes and a live
        // source.
        let status = unsafe {
            compress(
                buffer.as_mut_ptr(),
                &mut dest_len,
                HELLO.as_ptr(),
                narrow_uLong(HELLO.len()),
            )
        };
        assert_eq!(status, BUF_ERROR);
        assert_eq!(widen_uLong(dest_len), room);
        assert_eq!(&buffer[..], &HELLO_DEFAULT[..room]);
    }

    // -----------------------------------------------------------------------
    // The argument gate: overlap, and the order the level is judged in
    // -----------------------------------------------------------------------

    /// Overlapping `source` and `dest` are **served**, in every spelling.
    ///
    /// ★ C serves them: it assigns both pointers into a `z_stream` (`compress.c`
    /// L44-L47, `uncompr.c` L45-L46) and lets the encoder read and write one buffer,
    /// producing whatever it produces. Nothing in `zlib.h` L1276-L1341 says the two must
    /// be distinct, so refusing the pair would be an error this library invented and a
    /// caller the reference serves would stop working.
    ///
    /// What this implementation cannot do is form a `&[u8]` and a `&mut [u8]` over one
    /// region, so the operation is fed the input through `OverlapStage` -- a fixed 1 KiB
    /// buffer refilled a window at a time, which `stage_one_shot_input` documents in full.
    /// The staging is bounded rather than input-sized precisely so that an overlapping
    /// call costs no allocation and no memory above the reference implementation's, and it
    /// is invisible at the API: the observable consequence is the one asserted here, that
    /// the call runs, reports a status from the operation rather than an argument refusal,
    /// and writes the count.
    ///
    /// The last two cases below are the ones that matter for the bound: an input several
    /// stage windows long, in both directions, so that the refill loop is exercised rather
    /// than only its first iteration.
    #[test]
    fn overlapping_source_and_destination_are_served() {
        // The payload is written into the shared region first, so the overlapping call has
        // something real to compress and the round trip below can check it.
        let mut shared = vec![0_u8; 256];
        shared[..HELLO.len()].copy_from_slice(HELLO);
        let base = shared.as_mut_ptr();

        // Exactly the same region: compress `HELLO` from the front of the buffer into the
        // front of the buffer.
        let mut dest_len = narrow_uLong(256);
        // SAFETY: `base` is a live 256-byte allocation, and the call takes its own copy of
        // the input before it borrows the output, so no two views of it coexist.
        let status = unsafe {
            compress2(
                base,
                &mut dest_len,
                base,
                narrow_uLong(HELLO.len()),
                DEFAULT_LEVEL,
            )
        };
        assert_eq!(status, OK, "an overlapping pair must run");
        assert_eq!(
            widen_uLong(dest_len),
            HELLO_DEFAULT.len(),
            "the count must be the produced length"
        );
        assert_eq!(
            &shared[..HELLO_DEFAULT.len()],
            HELLO_DEFAULT,
            "the bytes the entry values would produce"
        );

        // Partially overlapping: compress from [32,32+len) into [0,256).
        let mut shared = vec![0_u8; 256];
        shared[32..32 + HELLO.len()].copy_from_slice(HELLO);
        let base = shared.as_mut_ptr();
        // SAFETY: the offset is inside the same live allocation and nothing is
        // dereferenced through it here.
        let middle = unsafe { base.add(32) };
        let mut dest_len = narrow_uLong(256);
        // SAFETY: as above.
        let status = unsafe {
            compress2(
                base,
                &mut dest_len,
                middle,
                narrow_uLong(HELLO.len()),
                DEFAULT_LEVEL,
            )
        };
        assert_eq!(status, OK);
        assert_eq!(&shared[..HELLO_DEFAULT.len()], HELLO_DEFAULT);

        // Adjacent but disjoint still takes the direct path and copies nothing.
        let mut shared = vec![0_u8; 256];
        let base = shared.as_mut_ptr();
        let mut dest_len = narrow_uLong(128);
        // SAFETY: `[0,128)` and `[128,142)` lie inside the same live allocation and
        // share no byte, so the two borrows the call forms cannot alias.
        let status = unsafe {
            let tail = base.add(128);
            ptr::copy_nonoverlapping(HELLO.as_ptr(), tail, HELLO.len());
            compress2(
                base,
                &mut dest_len,
                tail,
                narrow_uLong(HELLO.len()),
                DEFAULT_LEVEL,
            )
        };
        assert_eq!(status, OK, "disjoint ranges must still be accepted");

        // The decoder side, `z_size_t` spelling: inflate a stream that lies in the
        // destination buffer, in place.
        let mut shared = vec![0_u8; 256];
        shared[..HELLO_DEFAULT.len()].copy_from_slice(HELLO_DEFAULT);
        let base = shared.as_mut_ptr();
        let mut got = 256_usize;
        let mut used = HELLO_DEFAULT.len();
        // SAFETY: one live allocation, aliased on purpose; the call copies the input
        // before borrowing the output.
        let status = unsafe { uncompress2_z(base, &mut got, base, &mut used) };
        assert_eq!(status, OK, "an in-place inflate must run");
        assert_eq!(got, HELLO.len(), "*destLen is the produced length");
        assert_eq!(
            used,
            HELLO_DEFAULT.len(),
            "*sourceLen is the consumed length"
        );
        assert_eq!(&shared[..HELLO.len()], HELLO);

        // And the `uLong` spelling, where a too-short input is a data error rather than an
        // argument error -- the point being that the answer comes from the decoder.
        let mut shared = vec![0_u8; 256];
        shared[..HELLO_DEFAULT.len()].copy_from_slice(HELLO_DEFAULT);
        let base = shared.as_mut_ptr();
        let mut got = narrow_uLong(256);
        // SAFETY: as above.
        let status = unsafe { uncompress(base, &mut got, base, narrow_uLong(4)) };
        assert_ne!(
            status, STREAM_ERROR,
            "the status must come from the decoder, not from an argument gate"
        );

        // ★ An input several stage windows long, which is what actually exercises the refill
        // loop rather than only its first iteration -- and which is where an input-sized
        // snapshot and a bounded stage stop being interchangeable. The payload is built from a
        // repeating unit so that it compresses to something much smaller than itself, which
        // makes the overlapping destination genuinely collide with the unread input.
        let payload: Vec<u8> = HELLO.repeat(600);
        assert!(
            payload.len() > 4 * crate::types::OVERLAP_STAGE_BYTES,
            "the fixture must span several stage windows, not one"
        );

        // The reference answer: the same payload compressed out of a disjoint source, so the
        // only difference between the two calls is whether the input was staged.
        let mut disjoint = vec![0_u8; payload.len() * 2];
        let mut disjoint_len = narrow_uLong(disjoint.len());
        // SAFETY: `disjoint` and `payload` are two distinct live allocations, and the lengths
        // passed are their own.
        let status = unsafe {
            compress2(
                disjoint.as_mut_ptr(),
                &mut disjoint_len,
                payload.as_ptr(),
                narrow_uLong(payload.len()),
                DEFAULT_LEVEL,
            )
        };
        assert_eq!(status, OK, "the disjoint reference call must run");
        let expected = disjoint[..widen_uLong(disjoint_len)].to_vec();

        // The same compression, in place, over one buffer.
        let mut shared = vec![0_u8; payload.len() * 2];
        shared[..payload.len()].copy_from_slice(&payload);
        let base = shared.as_mut_ptr();
        let mut dest_len = narrow_uLong(shared.len());
        // SAFETY: one live allocation, aliased on purpose; the operation reads the staged
        // window and never holds a shared and an exclusive view of the region together.
        let status = unsafe {
            compress2(
                base,
                &mut dest_len,
                base,
                narrow_uLong(payload.len()),
                DEFAULT_LEVEL,
            )
        };
        assert_eq!(status, OK, "a multi-window overlapping call must run");
        assert_eq!(
            &shared[..widen_uLong(dest_len)],
            &expected[..],
            "staging the input a window at a time must not change a byte of the output"
        );

        // The `Z_NO_FLUSH` feeding above is what makes that byte-for-byte equality hold, and
        // it holds for every level whose compressor buffers its input. Level 0 is the one
        // exception -- `deflate_stored` sizes each stored block from the `avail_in` it is
        // handed, so a window-sized feed produces window-sized blocks -- and it is documented
        // as such above `OverlapStage`. It is inside the space C leaves undefined, because C's
        // own overlapping level-0 path `memcpy`s a region onto itself. The round trip is what
        // has to hold there, and it does:
        let mut shared0 = vec![0_u8; payload.len() * 2];
        shared0[..payload.len()].copy_from_slice(&payload);
        let base0 = shared0.as_mut_ptr();
        let mut dest_len0 = narrow_uLong(shared0.len());
        // SAFETY: as above.
        let status =
            unsafe { compress2(base0, &mut dest_len0, base0, narrow_uLong(payload.len()), 0) };
        assert_eq!(status, OK, "level 0 must run over an overlapping pair too");
        let stored = shared0[..widen_uLong(dest_len0)].to_vec();

        // And the decoder side, in place, over a stream several stage windows long: the bytes
        // that come back are the bytes that went in.
        for stream in [&expected, &stored] {
            let mut shared = vec![0_u8; payload.len() + stream.len() + 64];
            shared[..stream.len()].copy_from_slice(stream);
            let base = shared.as_mut_ptr();
            let mut got = shared.len();
            let mut used = stream.len();
            // SAFETY: one live allocation, aliased on purpose, whose extent is the length
            // reported for the destination.
            let status = unsafe { uncompress2_z(base, &mut got, base, &mut used) };
            assert_eq!(status, OK, "a multi-window in-place inflate must run");
            assert_eq!(got, payload.len(), "*destLen is the produced length");
            assert_eq!(used, stream.len(), "*sourceLen is the consumed length");
            assert_eq!(
                &shared[..payload.len()],
                &payload[..],
                "and the round trip is exact"
            );
        }
    }

    /// No window the core is handed for a staged input can exceed the stage.
    ///
    /// ★ The bound itself, checked directly rather than inferred from an allocator ledger. The
    /// streaming entry points can be measured through the caller's `zalloc` -- `benches/`'s
    /// `deflate_memory` group emits a `-overlap` row per configuration and the gate requires its
    /// high-water mark to equal the disjoint row's exactly -- but the one-shot wrappers cannot be:
    /// `compress.c` L38 initialises its stream with the hook members zeroed, so the allocation
    /// they make is the library's own and no caller hook can see it. What is left to check is the
    /// property the bound actually rests on, and it is checkable at the source: whatever
    /// `sourceLen` a caller supplies, the largest window [`OneShotSource`] will offer the core is
    /// [`OVERLAP_STAGE_BYTES`], so the staging cost is a constant of this crate rather than a
    /// function of the caller's argument.
    ///
    /// The disjoint arm is asserted in the same test and for the same reason: it must report no
    /// bound at all, because a disjoint call has to be the call it was before bounded staging
    /// existed -- the core's own `MAX_CHUNK` and nothing else.
    #[test]
    fn a_staged_window_is_bounded_by_the_stage_and_a_direct_one_is_not() {
        use zlib_rs::read_buf::OneShotSource;

        // Far past the stage, so a supplier that reported its own `sourceLen` would be caught.
        let payload = vec![0x5a_u8; 8 * super::OVERLAP_STAGE_BYTES + 7];
        let mut shared = vec![0_u8; payload.len() * 2];
        shared[..payload.len()].copy_from_slice(&payload);
        let base = shared.as_mut_ptr();

        // Overlapping: same base for both, so `stage_one_shot_input` takes the staged arm.
        // SAFETY: `base` addresses a live allocation of `shared.len()` bytes and the two lengths
        // passed are inside it; nothing is dereferenced to decide the arm, which
        // `ranges_are_disjoint` settles by comparing addresses.
        let mut staged =
            unsafe { super::stage_one_shot_input(base, shared.len(), base, payload.len()) };
        assert_eq!(
            staged.total(),
            payload.len(),
            "the total is the caller's whole sourceLen: the staging changes how it is delivered,              not how much"
        );
        assert_eq!(
            staged.max_window(),
            super::OVERLAP_STAGE_BYTES,
            "and no window may exceed the stage, whatever sourceLen was"
        );
        // Every window the core can ask for, at the two extremes and across a boundary.
        for at in [0, super::OVERLAP_STAGE_BYTES, payload.len() - 1] {
            let len = super::OVERLAP_STAGE_BYTES.min(payload.len() - at);
            let window = staged
                .window(at, len)
                .expect("an in-range window is served");
            assert_eq!(window.len(), len);
            assert_eq!(
                window,
                &payload[at..at + len],
                "and it carries the caller's own bytes, taken before the output could land on them"
            );
        }
        assert!(
            staged.window(payload.len(), 1).is_none(),
            "a window past the total is refused rather than clamped, so a mis-stepped loop              cannot read a byte the caller never offered"
        );

        // Disjoint: the tail of the buffer as the source, the head as the destination.
        let mut shared = vec![0_u8; payload.len() * 2];
        shared[payload.len()..].copy_from_slice(&payload);
        let base = shared.as_mut_ptr();
        // SAFETY: `[0, payload.len())` and `[payload.len(), 2 * payload.len())` lie inside the
        // same live allocation and share no byte.
        let direct = unsafe {
            let tail = base.add(payload.len());
            super::stage_one_shot_input(base, payload.len(), tail, payload.len())
        };
        assert_eq!(direct.total(), payload.len());
        assert_eq!(
            direct.max_window(),
            usize::MAX,
            "a disjoint call reports no bound, so it is chunked exactly as it was before the              stage existed"
        );
    }

    /// A length out-parameter that points inside one of the buffers is **served**.
    ///
    /// C writes through both pointers and produces whatever that produces; nothing in
    /// `zlib.h` forbids the aliasing. What this implementation does is order the writes
    /// where C orders them: `*destLen = 0` (`compress.c` L36) happens before either
    /// buffer is borrowed, and the closing write happens after every borrow has ended, so
    /// no exclusive borrow and raw write are ever in flight together.
    #[test]
    fn a_length_out_parameter_inside_a_buffer_is_served() {
        // `z_size_t`-aligned backing storage, so the slot pointer below is a pointer a
        // caller could really have produced. The first word doubles as the length
        // out-parameter and advertises the whole buffer as the destination.
        let mut buffer = vec![0_usize; 32];
        let words = buffer.len();
        buffer[0] = words * size_of::<usize>();
        let slot: *mut z_size_t = buffer.as_mut_ptr();
        // SAFETY: `buffer` is 32 words; the byte view and the offset both stay inside
        // it, and nothing is dereferenced through the raw view here.
        let (dest, source) = unsafe {
            let bytes = buffer.as_mut_ptr().cast::<u8>();
            (bytes, bytes.add(128).cast_const())
        };

        // SAFETY: `dest` covers the same words as `slot`, deliberately. The call writes
        // the slot before it borrows `dest` and again after that borrow has ended.
        let status = unsafe { compress2_z(dest, slot, source, 16, DEFAULT_LEVEL) };
        assert_eq!(status, OK, "a slot inside dest must not be refused");
        // SAFETY: the slot is the first word of a live 32-word allocation.
        let produced = unsafe { *slot };
        assert!(
            produced > 0 && produced <= words * size_of::<usize>(),
            "the produced count must be written back, got {produced}"
        );
    }

    /// An invalid level is refused **before** either buffer is borrowed.
    ///
    /// `compress.c` reaches `deflateInit` at L42, having written `*destLen = 0` at L36
    /// and *not yet* having assigned `source` or `dest` into the stream (L44-L47), so C
    /// returns `Z_STREAM_ERROR` for a bad level without ever reading either buffer. The
    /// two assertions below are the observable shadow of that ordering: the count is
    /// zeroed, which the *null-argument* gate ahead of L36 never does, so a caller can
    /// tell which gate answered -- and the implementation cannot have built a slice from
    /// an argument it had not yet judged.
    #[test]
    fn an_invalid_level_is_refused_after_the_count_is_cleared() {
        for level in [-2_i32, 10, i32::MIN, i32::MAX] {
            let mut buffer = vec![0_u8; 64];
            let mut dest_len = buffer.len();
            // SAFETY: a live, uniquely-borrowed destination and a live, disjoint source.
            let status = unsafe {
                compress2_z(
                    buffer.as_mut_ptr(),
                    &mut dest_len,
                    HELLO.as_ptr(),
                    HELLO.len(),
                    level,
                )
            };
            assert_eq!(status, STREAM_ERROR, "level {level}");
            assert_eq!(dest_len, 0, "level {level}: measured *destLen == 0");
        }

        // Every accepted level still round-trips, so the gate is not over-reaching:
        // `Z_DEFAULT_COMPRESSION` and `0 ..= 9` are exactly the accepted set.
        for level in [DEFAULT_LEVEL, 0, 1, 9] {
            let mut buffer = vec![0_u8; 64];
            let mut dest_len = buffer.len();
            // SAFETY: as above.
            let status = unsafe {
                compress2_z(
                    buffer.as_mut_ptr(),
                    &mut dest_len,
                    HELLO.as_ptr(),
                    HELLO.len(),
                    level,
                )
            };
            assert_eq!(status, OK, "level {level}");
            assert!(dest_len > 0, "level {level}");
        }
    }

    // -----------------------------------------------------------------------
    // Round trips, in both spellings
    // -----------------------------------------------------------------------

    /// What the encoder emits the decoder accepts, for every fixture class the
    /// corpus names: empty, one byte, highly repetitive, incompressible, text,
    /// binary, and data crossing the 32 KiB window boundary.
    #[test]
    fn round_trip_every_fixture_class_at_every_level() {
        let repetitive = vec![b'a'; 70_000];
        let window_crossing: Vec<u8> = (0..40_000_u32).map(|i| (i % 251) as u8).collect();
        let incompressible: Vec<u8> = (0..8192_u32)
            .map(|i| i.wrapping_mul(2_654_435_761).to_le_bytes()[0])
            .collect();
        let binary: Vec<u8> = (0..=255_u8).collect();

        let fixtures: [&[u8]; 7] = [
            b"",
            b"x",
            &repetitive,
            &incompressible,
            b"The quick brown fox jumps over the lazy dog. The quick brown fox jumps again.",
            &binary,
            &window_crossing,
        ];

        for source in fixtures {
            for level in [DEFAULT_LEVEL, 0, 1, 6, 9] {
                let (status, stream) = compress_uLong(source, level);
                assert_eq!(status, OK, "len {} level {level}", source.len());

                let (status, plain) = uncompress_uLong(&stream, source.len());
                assert_eq!(status, OK, "len {} level {level}", source.len());
                assert_eq!(plain, source, "len {} level {level}", source.len());

                // And the `z_size_t` spelling, which must agree exactly.
                let mut buffer = vec![0_u8; source.len()];
                let mut dest_len = buffer.len();
                // SAFETY: a live, uniquely-borrowed destination sized from the
                // known plaintext length, and a live source.
                let status = unsafe {
                    uncompress_z(
                        buffer.as_mut_ptr(),
                        &mut dest_len,
                        stream.as_ptr(),
                        stream.len(),
                    )
                };
                assert_eq!(status, OK);
                assert_eq!(dest_len, source.len());
                assert_eq!(&buffer[..dest_len], source);
            }
        }
    }

    // -----------------------------------------------------------------------
    // `uncompress2`: the second out-parameter
    // -----------------------------------------------------------------------

    /// ★ Trailing data is not consumed, and `*sourceLen` says so. Measured:
    /// status `Z_OK`, `*destLen == 14`, `*sourceLen == 19` for the 19-byte stream
    /// followed by four unrelated bytes.
    #[test]
    fn uncompress2_reports_only_the_bytes_it_consumed() {
        let mut input = HELLO_DEFAULT.to_vec();
        input.extend_from_slice(b"tail");

        let mut buffer = vec![0_u8; 64];
        let mut dest_len = narrow_uLong(buffer.len());
        let mut source_len = narrow_uLong(input.len());
        // SAFETY: a live, uniquely-borrowed destination, a live source, and two
        // distinct locals for the in/out lengths; none of the four overlaps.
        let status = unsafe {
            uncompress2(
                buffer.as_mut_ptr(),
                &mut dest_len,
                input.as_ptr(),
                &mut source_len,
            )
        };

        assert_eq!(status, OK);
        assert_eq!(widen_uLong(dest_len), HELLO.len());
        assert_eq!(&buffer[..widen_uLong(dest_len)], HELLO);
        assert_eq!(
            widen_uLong(source_len),
            HELLO_DEFAULT.len(),
            "the tail must not be reported as consumed"
        );
        assert_eq!(&input[widen_uLong(source_len)..], b"tail");
    }

    /// The `z_size_t` spelling reports the same two counts.
    #[test]
    fn uncompress2_z_agrees_with_uncompress2() {
        let mut input = HELLO_DEFAULT.to_vec();
        input.extend_from_slice(b"tail");

        let mut buffer = vec![0_u8; 64];
        let mut dest_len = buffer.len();
        let mut source_len = input.len();
        // SAFETY: `buffer` is a live, uniquely-borrowed allocation of exactly `dest_len`
        // bytes; `input` is a live `Vec` whose pointer is readable for `source_len` bytes;
        // `dest_len` and `source_len` are two distinct locals this frame owns. None of the
        // four overlaps another, and all of them outlive the call.
        let status = unsafe {
            uncompress2_z(
                buffer.as_mut_ptr(),
                &mut dest_len,
                input.as_ptr(),
                &mut source_len,
            )
        };

        assert_eq!(status, OK);
        assert_eq!(dest_len, HELLO.len());
        assert_eq!(source_len, HELLO_DEFAULT.len());
    }

    /// A truncated stream is corruption, not a short buffer: measured
    /// `Z_DATA_ERROR` with `*destLen == 14` and `*sourceLen == 16` for a 19-byte
    /// stream cut by three. The count is real -- the payload decoded, only the
    /// trailer is missing.
    #[test]
    fn uncompress2_reports_a_truncated_stream_as_a_data_error() {
        let input = &HELLO_DEFAULT[..HELLO_DEFAULT.len() - 3];

        let mut buffer = vec![0_u8; 64];
        let mut dest_len = narrow_uLong(buffer.len());
        let mut source_len = narrow_uLong(input.len());
        // SAFETY: a live destination, a live source, two distinct locals.
        let status = unsafe {
            uncompress2(
                buffer.as_mut_ptr(),
                &mut dest_len,
                input.as_ptr(),
                &mut source_len,
            )
        };

        assert_eq!(status, DATA_ERROR);
        assert_eq!(widen_uLong(dest_len), HELLO.len());
        assert_eq!(
            widen_uLong(source_len),
            input.len(),
            "all input was consumed"
        );
    }

    /// A destination one byte short is a short buffer, not corruption: measured
    /// `Z_BUF_ERROR` with `*destLen == 13` and `*sourceLen == 14`. This is the
    /// discrimination the core's ladder makes on the residual input, and the pair
    /// of tests around it is what proves the two cases are not confused.
    #[test]
    fn uncompress2_reports_a_short_destination_as_a_buffer_error() {
        let mut buffer = vec![0_u8; HELLO.len() - 1];
        let room = buffer.len();
        let mut dest_len = narrow_uLong(room);
        let mut source_len = narrow_uLong(HELLO_DEFAULT.len());
        // SAFETY: a live destination of `room` bytes, a live source, two locals.
        let status = unsafe {
            uncompress2(
                buffer.as_mut_ptr(),
                &mut dest_len,
                HELLO_DEFAULT.as_ptr(),
                &mut source_len,
            )
        };

        assert_eq!(status, BUF_ERROR);
        assert_eq!(widen_uLong(dest_len), room);
        assert_eq!(source_len, 14, "measured");
        assert_eq!(&buffer[..], &HELLO[..room]);
    }

    /// A zero `*sourceLen` is an empty stream, which is corrupt: measured
    /// `Z_DATA_ERROR` with both counts zero.
    #[test]
    fn uncompress2_treats_no_input_as_a_data_error() {
        let mut buffer = vec![0_u8; 64];
        let mut dest_len = narrow_uLong(buffer.len());
        let mut source_len: uLong = 0;
        // SAFETY: a live destination, a live source pointer that is never read
        // because the length is zero, and two locals.
        let status = unsafe {
            uncompress2(
                buffer.as_mut_ptr(),
                &mut dest_len,
                HELLO_DEFAULT.as_ptr(),
                &mut source_len,
            )
        };

        assert_eq!(status, DATA_ERROR);
        assert_eq!(dest_len, 0);
        assert_eq!(source_len, 0);
    }

    // -----------------------------------------------------------------------
    // The null and zero matrix -- every entry measured against the reference
    // -----------------------------------------------------------------------

    /// ★ A null `dest` with a zero `*destLen` is `Z_STREAM_ERROR` for the
    /// compression family, because C's `deflate` refuses a null `next_out`. This
    /// is the one place an empty Rust slice is not equivalent to what C does, and
    /// this test is what holds the translation in place.
    #[test]
    fn compress_rejects_a_null_destination() {
        let mut dest_len: uLongf = 0;
        // SAFETY: `dest` is null with a zero length, which the helper maps to an
        // empty slice; `destLen` is a local this frame owns; `source` is live.
        let status = unsafe {
            compress(
                ptr::null_mut(),
                &mut dest_len,
                HELLO.as_ptr(),
                narrow_uLong(HELLO.len()),
            )
        };
        assert_eq!(status, STREAM_ERROR, "measured: not Z_BUF_ERROR");
        assert_eq!(dest_len, 0);

        // The same through the `z_size_t` spelling ...
        let mut dest_len = 0_usize;
        // SAFETY: the destination pointer is null with a zero length, which the guard rejects
        // before any slice is built, so it is never dereferenced; `HELLO` is a `'static` byte
        // string readable for its whole length; `dest_len` is a local this frame owns.
        let status = unsafe {
            compress2_z(
                ptr::null_mut(),
                &mut dest_len,
                HELLO.as_ptr(),
                HELLO.len(),
                6,
            )
        };
        assert_eq!(status, STREAM_ERROR);
        assert_eq!(dest_len, 0);

        // ... and for an empty source too, which still needs an eight-byte stream
        // and so still has nowhere to put it.
        let mut dest_len = 0_usize;
        // SAFETY: both pointers are null with zero lengths, both mapped to empty
        // slices; `destLen` is a local.
        let status = unsafe { compress2_z(ptr::null_mut(), &mut dest_len, ptr::null(), 0, 6) };
        assert_eq!(status, STREAM_ERROR);
        assert_eq!(dest_len, 0);
    }

    /// A *valid* destination of zero length is `Z_BUF_ERROR`, not
    /// `Z_STREAM_ERROR`. The contrast with the test above is the whole point: the
    /// translation keys on the pointer, never on the length.
    #[test]
    fn compress_reports_a_valid_empty_destination_as_a_buffer_error() {
        let mut buffer = [0_u8; 1];
        let mut dest_len: uLongf = 0;
        // SAFETY: `dest` is a live, uniquely-borrowed allocation; the zero length
        // means nothing is written through it. `destLen` is a local.
        let status = unsafe {
            compress(
                buffer.as_mut_ptr(),
                &mut dest_len,
                HELLO.as_ptr(),
                narrow_uLong(HELLO.len()),
            )
        };
        assert_eq!(status, BUF_ERROR, "measured: not Z_STREAM_ERROR");
        assert_eq!(dest_len, 0);
        assert_eq!(buffer, [0_u8; 1], "nothing was written");
    }

    /// A null `dest` with a *non-zero* `*destLen` is rejected by the entry guard,
    /// and the guard leaves the caller's count untouched. Measured: the reference
    /// returns `Z_STREAM_ERROR` with `*destLen` still 100.
    #[test]
    fn a_null_destination_with_room_is_rejected_without_a_write() {
        let mut dest_len: uLongf = 100;
        // SAFETY: `dest` is null and is never dereferenced, because the guard
        // rejects the call before any slice is built. `destLen` is a local.
        let status = unsafe {
            compress(
                ptr::null_mut(),
                &mut dest_len,
                HELLO.as_ptr(),
                narrow_uLong(HELLO.len()),
            )
        };
        assert_eq!(status, STREAM_ERROR);
        assert_eq!(dest_len, 100, "the guard must not write");

        let mut dest_len: uLongf = 100;
        // SAFETY: the destination pointer is null, and the guard rejects the call before any
        // slice is built, so it is never dereferenced -- which is what the following assertion
        // on `dest_len` proves. `HELLO_DEFAULT` is a `'static` byte string readable for its
        // whole length, and `dest_len` is a local this frame owns.
        let status = unsafe {
            uncompress(
                ptr::null_mut(),
                &mut dest_len,
                HELLO_DEFAULT.as_ptr(),
                narrow_uLong(HELLO_DEFAULT.len()),
            )
        };
        assert_eq!(status, STREAM_ERROR);
        assert_eq!(dest_len, 100, "the guard must not write");
    }

    /// A null `source` with a non-zero length is rejected by the entry guard, and
    /// the guard leaves the caller's count untouched. Measured: `Z_STREAM_ERROR`
    /// with `*destLen` still at its entry value.
    #[test]
    fn a_null_source_with_a_length_is_rejected_without_a_write() {
        let mut buffer = vec![0_u8; 64];
        let entry = narrow_uLong(buffer.len());

        let mut dest_len = entry;
        // SAFETY: `source` is null and is never dereferenced: the guard rejects the
        // call before any slice is built. The destination is live.
        let status = unsafe { compress(buffer.as_mut_ptr(), &mut dest_len, ptr::null(), 5) };
        assert_eq!(status, STREAM_ERROR);
        assert_eq!(dest_len, entry, "the guard must not write");

        let mut dest_len = entry;
        let mut source_len: uLong = 5;
        // SAFETY: the source pointer is null, and the guard rejects the call before any slice
        // is built, so it is never dereferenced. `buffer` is a live, uniquely-borrowed
        // destination, and `dest_len` and `source_len` are two distinct locals this frame
        // owns; nothing overlaps.
        let status = unsafe {
            uncompress2(
                buffer.as_mut_ptr(),
                &mut dest_len,
                ptr::null(),
                &mut source_len,
            )
        };
        assert_eq!(status, STREAM_ERROR);
        assert_eq!(dest_len, entry, "the guard must not write");
        assert_eq!(source_len, 5, "the guard must not write");
    }

    /// A null length out-parameter is `Z_STREAM_ERROR` rather than a crash, in
    /// every one of the eight entry points that takes one. C dereferences it
    /// unchecked in five of them, so this is where the port is defined and the
    /// reference is not.
    #[test]
    fn a_null_length_out_parameter_is_rejected_everywhere() {
        let mut buffer = vec![0_u8; 64];
        let dest = buffer.as_mut_ptr();
        let source = HELLO_DEFAULT.as_ptr();
        let source_len_wide = narrow_uLong(HELLO_DEFAULT.len());
        let source_len = HELLO_DEFAULT.len();

        // SAFETY: every call below passes a null `destLen` (and, in the last two, a
        // null `sourceLen`). Each is tested for null before any read or write, so no
        // null is ever dereferenced. `dest` and `source` are live.
        unsafe {
            assert_eq!(
                compress(dest, ptr::null_mut(), source, source_len_wide),
                STREAM_ERROR
            );
            assert_eq!(
                compress_z(dest, ptr::null_mut(), source, source_len),
                STREAM_ERROR
            );
            assert_eq!(
                compress2(dest, ptr::null_mut(), source, source_len_wide, 6),
                STREAM_ERROR
            );
            assert_eq!(
                compress2_z(dest, ptr::null_mut(), source, source_len, 6),
                STREAM_ERROR
            );
            assert_eq!(
                uncompress(dest, ptr::null_mut(), source, source_len_wide),
                STREAM_ERROR
            );
            assert_eq!(
                uncompress_z(dest, ptr::null_mut(), source, source_len),
                STREAM_ERROR
            );

            let mut used_wide = source_len_wide;
            assert_eq!(
                uncompress2(dest, ptr::null_mut(), source, &mut used_wide),
                STREAM_ERROR
            );
            let mut used = source_len;
            assert_eq!(
                uncompress2_z(dest, ptr::null_mut(), source, &mut used),
                STREAM_ERROR
            );

            // And the second out-parameter, which only `uncompress2*` has.
            let mut dest_len_wide = narrow_uLong(buffer.len());
            assert_eq!(
                uncompress2(dest, &mut dest_len_wide, source, ptr::null_mut()),
                STREAM_ERROR
            );
            let mut dest_len = buffer.len();
            assert_eq!(
                uncompress2_z(dest, &mut dest_len, source, ptr::null_mut()),
                STREAM_ERROR
            );
        }
    }

    /// ★ A null `dest` needs no translation on the decompression side, because
    /// `uncompr.c` L42-L43 already aims `next_out` at non-null scratch space.
    /// Measured: `Z_BUF_ERROR` for a stream with content, and both counts zero.
    #[test]
    fn uncompress_accepts_a_null_destination_of_zero_length() {
        let mut dest_len: uLongf = 0;
        // SAFETY: `dest` is null with a zero length, mapped to an empty slice --
        // the Rust equivalent of C's `&stream.reserved` redirection. `destLen` is a
        // local; `source` is live.
        let status = unsafe {
            uncompress(
                ptr::null_mut(),
                &mut dest_len,
                HELLO_DEFAULT.as_ptr(),
                narrow_uLong(HELLO_DEFAULT.len()),
            )
        };
        assert_eq!(status, BUF_ERROR, "measured: not Z_STREAM_ERROR");
        assert_eq!(dest_len, 0);

        // A *valid* zero-length destination answers identically, which is what
        // makes the redirection unobservable.
        let mut buffer = [0_u8; 1];
        let mut dest_len: uLongf = 0;
        // SAFETY: a live destination whose zero length means nothing is written.
        let status = unsafe {
            uncompress(
                buffer.as_mut_ptr(),
                &mut dest_len,
                HELLO_DEFAULT.as_ptr(),
                narrow_uLong(HELLO_DEFAULT.len()),
            )
        };
        assert_eq!(status, BUF_ERROR);
        assert_eq!(dest_len, 0);
    }

    /// Every pointer null and every length zero: measured `Z_DATA_ERROR` with both
    /// counts zero, because no input at all is an incomplete stream.
    #[test]
    fn uncompress_with_everything_empty_is_a_data_error() {
        let mut dest_len: uLongf = 0;
        // SAFETY: both buffer pointers are null with zero lengths, both mapped to
        // empty slices; `destLen` is a local.
        let status = unsafe { uncompress(ptr::null_mut(), &mut dest_len, ptr::null(), 0) };
        assert_eq!(status, DATA_ERROR);
        assert_eq!(dest_len, 0);

        let mut dest_len = 0_usize;
        let mut source_len = 0_usize;
        // SAFETY: both buffer pointers are null with zero lengths, which the helpers map to
        // empty slices rather than passing to `from_raw_parts`, so neither is dereferenced.
        // `dest_len` and `source_len` are two distinct locals this frame owns.
        let status =
            unsafe { uncompress2_z(ptr::null_mut(), &mut dest_len, ptr::null(), &mut source_len) };
        assert_eq!(status, DATA_ERROR);
        assert_eq!(dest_len, 0);
        assert_eq!(source_len, 0);
    }

    /// Corrupt input is `Z_DATA_ERROR`, and the entry points do not panic on it.
    ///
    /// This is the local, deterministic check that the boundary forwards the status.
    /// The exhaustive form -- arbitrary bytes rather than five chosen ones -- is
    /// where the coverage is uneven, and it is worth stating narrowly rather than
    /// with the sweeping claim that used to stand here ("`fuzz/` declares a manifest
    /// but holds none"), which was simply wrong: `fuzz/fuzz_targets/` holds five
    /// targets.
    ///
    /// What they do and do not reach, measured:
    ///
    /// * The **compress** family IS driven with arbitrary input --
    ///   `fuzz_deflate.rs` calls `compress`, `compress2` and `compress_z` across a
    ///   structured configuration space.
    /// * The **uncompress** family is NOT: no fuzz target calls `uncompress`,
    ///   `uncompress2` or either `_z` form, so this deterministic test and the
    ///   differential suites are what cover them. `fuzz_inflate.rs` does drive the
    ///   streaming decoder with arbitrary bytes, which exercises the same underlying
    ///   inflate machinery through a different entry point.
    /// * The **overlapping source and destination** path is covered by
    ///   `tests/alias_overlap.rs` under Miri rather than by fuzzing, because the
    ///   property there is aliasing rather than input validity and a fuzzer cannot
    ///   observe a borrow-stack violation.
    ///
    /// What holds the uncompress gap up meanwhile is structural: the decoder is safe
    /// Rust, so a malformed stream is a status code rather than a memory-safety
    /// event.
    #[test]
    fn uncompress_rejects_corrupt_input_without_panicking() {
        let corrupt: [&[u8]; 5] = [
            &[0x00],
            &[0xff, 0xff, 0xff, 0xff],
            &[0x78, 0x9c],
            &[0x78, 0x9c, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff],
            &[0x1f, 0x8b, 0x08, 0x00],
        ];

        for source in corrupt {
            let (status, _) = uncompress_uLong(source, 64);
            assert_ne!(status, OK, "corrupt input must not report success");
            assert!(
                status == DATA_ERROR || status == BUF_ERROR,
                "unexpected status {status} for {source:?}"
            );
        }
    }

    // -----------------------------------------------------------------------
    // Interoperability with the reference's own bytes
    // -----------------------------------------------------------------------

    /// A stream the C implementation produced decompresses here, which is the
    /// C-to-Rust half of the bidirectional interoperability requirement. The
    /// fixtures are the reference's measured output, so this is a genuine
    /// cross-implementation check rather than a round trip in disguise.
    #[test]
    fn reference_streams_decompress_here() {
        let mut streams: Vec<&[u8]> = vec![HELLO_DEFAULT, HELLO_LEVEL_0];
        streams.extend_from_slice(&HELLO_BY_LEVEL);

        for stream in streams {
            let (status, plain) = uncompress_uLong(stream, 64);
            assert_eq!(status, OK, "{stream:?}");
            assert_eq!(plain, HELLO, "{stream:?}");
        }

        let (status, plain) = uncompress_uLong(EMPTY_STREAM, 64);
        assert_eq!(status, OK);
        assert!(plain.is_empty());
    }

    // -----------------------------------------------------------------------
    // The width helpers
    // -----------------------------------------------------------------------

    /// Widening is lossless and narrowing round-trips for every value a `uLong`
    /// can hold, which is what the module's compile-time assertion promises.
    #[test]
    fn the_width_helpers_round_trip() {
        for value in [
            uLong::MIN,
            1,
            13,
            4096,
            uLong::MAX / 2,
            uLong::MAX - 1,
            uLong::MAX,
        ] {
            assert_eq!(narrow_uLong(widen_uLong(value)), value);
        }

        // And narrowing truncates rather than saturating, exactly as C's cast does.
        // On LP64 the two widths are equal so this is the identity; the assertion is
        // written so that it states the intent on either kind of target.
        let widest = widen_uLong(uLong::MAX);
        assert_eq!(narrow_uLong(widest), uLong::MAX);
    }
}
