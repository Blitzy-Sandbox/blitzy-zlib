//! The ten one-shot `compress`/`uncompress` exports: `compress.c` and `uncompr.c`.
//!
//! The C ABI half of the two utility translation units. `crates/zlib-rs/src/compress.rs`
//! and `crates/zlib-rs/src/uncompress.rs` already hold every decision these
//! functions make -- the chunking loop, the flush selector, the bidirectional
//! accounting and the status ladder -- so this module contributes exactly three
//! things and nothing else:
//!
//! 1. **Pointer validation**, reproducing C's own guards before anything is
//!    dereferenced.
//! 2. **Slice reconstruction** from the caller's pointer/length pairs, performed
//!    once on entry so that only safe slices travel inward. This is AAP §0.6.1
//!    unsafe-site **category 2**, and it is the only category this module touches.
//! 3. **Width mapping and write-back**: `uLong` versus `z_size_t`, and pushing
//!    the results back through the caller's out-parameters on exactly the paths
//!    C writes them.
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
//! a bound one byte too small is a heap overflow **in caller code**, which this
//! library can neither detect nor contain; a bound too large breaks callers and
//! tests that assert exact sizes. Both therefore return exactly what C returns,
//! including the two saturations, and the expectations in the test module were
//! measured against the reference build rather than recomputed from the formula.
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
//! # Rules
//!
//! `review_rules` reports **"No user rules provided."** -- a complete,
//! single-line document, read in full. No file enters scope by rule and there is
//! no project rule for this module to satisfy. The binding standard is instead
//! AAP §0.7.1 (a)-(i): unsafe containment, contract immutability, behavioural
//! fidelity, no panics in library paths, a documented public API, the 1.80 MSRV,
//! and dependency minimalism -- this module names only `core`, two sibling
//! modules and [`zlib_rs`].
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
use core::mem::size_of;

use zlib_rs::compress::{
    compress2_z as core_compress2_z, compress_bound_z as core_compress_bound_z,
};
use zlib_rs::config::Z_DEFAULT_COMPRESSION;
use zlib_rs::error::ReturnCode;
use zlib_rs::uncompress::uncompress2_z as core_uncompress2_z;

use crate::panic_guard::{fallback, guard, guard_code};
use crate::types::{uLong, uLongf, z_size_t, Bytef};

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
const _: () = assert!(
    size_of::<uLong>() <= size_of::<usize>(),
    "uLong must be no wider than usize, or widening a caller's length would truncate"
);

/// `z_size_t` must be exactly `usize`, because the `_z` entry points pass it
/// straight through to the core without conversion.
///
/// `types.rs` asserts the same relation for its own use; repeating it here keeps
/// this module's pass-through honest even if that alias is ever revisited.
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

/// Rebuilds a caller's destination buffer as a mutable slice from
/// `(dest, *destLen)`.
///
/// The [`source_slice`] counterpart, and the `z_size_t` counterpart of
/// [`crate::types::output_slice_mut`]. The same zero-length rule applies for the
/// same reason, and it carries more weight on this side: `uncompr.c` L42-L43
/// exists precisely because a zero-length output buffer may legitimately be null,
/// and an empty Rust slice is the non-null, non-dangling buffer that C has to
/// manufacture from `&stream.reserved`.
///
/// # Safety
///
/// If `len` is non-zero, `dest` must be non-null and writable for `len` bytes, and
/// that region must not be aliased by anything else -- including by the source
/// slice -- for `'a`. Call this once on entry: two live mutable slices over one
/// buffer would be undefined behaviour even if neither were written.
#[inline]
#[must_use]
unsafe fn dest_slice<'a>(dest: *mut Bytef, len: usize) -> &'a mut [u8] {
    if len == 0 || dest.is_null() {
        return &mut [];
    }
    // SAFETY: unsafe-site category 2 -- slice reconstruction from a pointer/length
    // pair. `dest` is non-null by the test above and trivially aligned for `u8`.
    // `len` is the caller's own count, and this function's contract makes those
    // bytes writable, unaliased and stable for `'a`.
    unsafe { core::slice::from_raw_parts_mut(dest, len) }
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

        // Both slices are built exactly once, here, and only they travel into the
        // core. The helpers additionally map the zero-length cases to genuine empty
        // slices rather than to a dangling pointer.
        //
        // SAFETY: unsafe-site category 2 -- slice reconstruction. The guard above
        // established that `source` is non-null whenever `sourceLen` is non-zero,
        // and the caller's contract makes those bytes readable and stable for the
        // call.
        let input = unsafe { source_slice(source, sourceLen) };
        // SAFETY: unsafe-site category 2 -- slice reconstruction. The guard above
        // established that `dest` is non-null whenever `dest_len` is non-zero, and
        // the caller's contract makes those bytes writable and unaliased -- in
        // particular not aliasing `input` or `destLen` -- for the call.
        let output = unsafe { dest_slice(dest, dest_len) };

        let report = core_compress2_z(output, input, level);

        // `compress.c` L36 and L63 together: the count is written on every path
        // past the guard. The core reports `produced == 0` for a failed encoder
        // initialisation, which is exactly the zero L36 leaves behind, so one
        // unconditional write reproduces both branches.
        //
        // SAFETY: unsafe-site category 2 -- writing a caller's out-parameter.
        // `destLen` is the same non-null, aligned, live `z_size_t` read above, and
        // nothing else borrows it.
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

        // SAFETY: unsafe-site category 2 -- slice reconstruction, once, on entry,
        // under the guard established immediately above: `source` is non-null
        // whenever `source_len` is non-zero, and the caller's contract makes those
        // bytes readable for the call. See `compress2_z`.
        let input = unsafe { source_slice(source, source_len) };
        // SAFETY: unsafe-site category 2 -- slice reconstruction. `dest` is non-null
        // whenever `dest_len` is non-zero by the guard above, and the caller's
        // contract makes those bytes writable and unaliased for the call.
        let output = unsafe { dest_slice(dest, dest_len) };

        let report = core_compress2_z(output, input, level);

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
    // SAFETY: unsafe-site category 2, by delegation -- this block introduces no raw
    // pointer operation of its own, it forwards `compress2_z`'s obligation unchanged.
    // Every requirement of `compress2_z` is therefore a requirement of this function,
    // and the arguments reach it exactly as they arrived. The level literal is
    // `Z_DEFAULT_COMPRESSION` (`zlib.h` L196), spelled as `zlib_rs`'s constant so that
    // the two cannot drift apart.
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
    // SAFETY: unsafe-site category 2, by delegation -- no raw pointer operation of its
    // own; `compress2`'s obligation is forwarded unchanged, so every requirement of
    // `compress2` is a requirement of this function and the arguments reach it exactly
    // as they arrived.
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
/// of as a trick. The sentinel comes from [`fallback::BOUND`], so this export and
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

        // Both slices are built exactly once, here. An empty output slice is
        // additionally what discharges `uncompr.c` L42-L43's "next_out cannot be
        // NULL", with no scratch space to manufacture.
        //
        // SAFETY: unsafe-site category 2 -- slice reconstruction. The guard above
        // established that `source` is non-null whenever `source_len` is non-zero,
        // and the caller's contract makes those bytes readable for the call.
        let input = unsafe { source_slice(source, source_len) };
        // SAFETY: unsafe-site category 2 -- slice reconstruction. The guard above
        // established that `dest` is non-null whenever `dest_len` is non-zero, and
        // the caller's contract makes those bytes writable and unaliased -- not
        // aliasing `input`, `destLen` or `sourceLen` -- for the call.
        let output = unsafe { dest_slice(dest, dest_len) };

        let report = core_uncompress2_z(output, input);

        // `uncompr.c` L74-L75: `*sourceLen -= len; *destLen -= left;` -- the
        // reference's bidirectional accounting, which the core has already resolved
        // into a consumed and a produced count.
        //
        // Both writes are unconditional past the guard, which reproduces L51-L52 as
        // well: on a failed decoder initialisation C returns before either write,
        // and the core reports the *entry* lengths for that path, so writing them
        // back leaves the caller's variables exactly as they were.
        //
        // SAFETY: unsafe-site category 2 -- writing two caller out-parameters. Both
        // pointers are the non-null, aligned, live `z_size_t`s read above; the
        // contract requires them to be distinct, and each is written once.
        unsafe {
            *sourceLen = report.consumed;
            *destLen = report.produced;
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

        // SAFETY: unsafe-site category 2 -- slice reconstruction, once, on entry,
        // under the guard established immediately above: `source` is non-null
        // whenever `source_len` is non-zero, and the caller's contract makes those
        // bytes readable for the call. See `uncompress2_z`.
        let input = unsafe { source_slice(source, source_len) };
        // SAFETY: unsafe-site category 2 -- slice reconstruction. `dest` is non-null
        // whenever `dest_len` is non-zero by the guard above, and the caller's
        // contract makes those bytes writable and unaliased for the call.
        let output = unsafe { dest_slice(dest, dest_len) };

        let report = core_uncompress2_z(output, input);

        // L88-L89, in C's order, each narrowing exactly as C's cast narrows.
        //
        // SAFETY: unsafe-site category 2 -- writing two caller out-parameters, the
        // same non-null, aligned, live, distinct pointers read above.
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

    // SAFETY: unsafe-site category 2, by delegation -- no raw pointer operation of its
    // own; `uncompress2_z`'s obligation is forwarded unchanged, so every requirement of
    // it is a requirement of this function. The one argument this function supplies,
    // `&mut used`, is a live, aligned, writable `z_size_t` on this frame; it is distinct
    // from `destLen` and cannot overlap either buffer, because it is a local this
    // function owns.
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

    // SAFETY: unsafe-site category 2, by delegation -- no raw pointer operation of its
    // own; `uncompress2`'s obligation is forwarded unchanged. The one argument this
    // function supplies, `&mut used`, is a live, aligned, writable `uLong` on this
    // frame, distinct from `destLen` and from both buffers because this function owns
    // it.
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
    use crate::types::{uLong, uLongf};
    use core::ffi::c_int;
    use core::ptr;
    use zlib_rs::error::ReturnCode;

    /// `Z_OK` (`zlib.h` L179).
    const OK: c_int = ReturnCode::OK.as_i32();
    /// `Z_STREAM_ERROR` (`zlib.h` L185).
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
        // SAFETY: as `compress_uLong`, with the roles of the two buffers reversed.
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
        // (sourceLen, bound)
        let table: [(usize, usize); 8] = [
            (0, 13),
            (1, 14),
            (4095, 4108),
            (4096, 4110),
            (16383, 16399),
            (16384, 16402),
            (0xffff_ffff, 4_296_278_153),
            (usize::MAX, usize::MAX),
        ];

        for (source_len, expected) in table {
            assert_eq!(
                compressBound_z(source_len),
                expected,
                "compressBound_z({source_len})"
            );
        }
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
        // SAFETY: as above, at the `uLong` width.
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
        // SAFETY: as `uncompress2_reports_only_the_bytes_it_consumed`, at the
        // `z_size_t` width.
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
        // SAFETY: as above, at the `z_size_t` width.
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
        // SAFETY: as above, for the decompression family.
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
        // SAFETY: as above, and `sourceLen` is a local this frame owns.
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
        // SAFETY: as above, with two distinct locals for the in/out lengths.
        let status =
            unsafe { uncompress2_z(ptr::null_mut(), &mut dest_len, ptr::null(), &mut source_len) };
        assert_eq!(status, DATA_ERROR);
        assert_eq!(dest_len, 0);
        assert_eq!(source_len, 0);
    }

    /// Corrupt input is `Z_DATA_ERROR`, and the entry points do not panic on it.
    /// The exhaustive version of this property is `fuzz/fuzz_targets/`'s job; this
    /// is the local, deterministic check that the boundary forwards the status.
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
