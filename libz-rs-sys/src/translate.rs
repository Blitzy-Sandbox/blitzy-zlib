//! # `translate` — the raw-pointer ⇄ slice / `int` ⇄ [`ReturnCode`] bridge
//!
//! This module is the **conversion core** of the `libz-rs-sys` FFI shim. It is
//! the single place where the C `#[repr(C)]` world
//! (`next_in`/`next_out` raw pointers, `avail_*` counts, an opaque
//! `state` pointer and integer return codes) is translated into the safe
//! [`zlib_rs`] engine, which performs all I/O through **borrowed slices**
//! (`&[u8]` / `&mut [u8]`), owns its state in a [`Box`], and reports outcomes as
//! `Result<ReturnCode, ZlibError>`.
//!
//! Together with `lib.rs` this is the **only** place in the entire workspace
//! where `unsafe` is permitted: the `zlib-rs` core is compiled under
//! `#![forbid(unsafe_code)]`, so every raw-pointer hazard is deliberately
//! concentrated and *justified* here. **Every `unsafe` block in this file
//! carries a `// SAFETY:` comment** naming the invariant it upholds at the C
//! boundary, and every `pub(crate)` `unsafe fn` documents its precondition in a
//! `# Safety` section.
//!
//! The shim is intentionally **thin**: it performs pointer/slice validation,
//! `ReturnCode` ⇄ `int` conversion, state-lifetime management and the small
//! numeric conversions required by the checksum / one-shot / `gz` symbols. It
//! contains **no compression or decompression logic** — the algorithmic work is
//! always delegated to [`zlib_rs`].
//!
//! ## Two load-bearing divergences in the core API
//!
//! 1. **Tuple-order divergence.** The core `deflate` entry point (C-style
//!    camelCase) returns `(result, consumed, produced)`, whereas `inflate`
//!    (snake_case) returns `(consumed, produced, result)`. The two are driven by
//!    [`run_deflate`] and [`run_inflate`] respectively; the divergence is
//!    isolated to the two destructuring sites and called out loudly there. A
//!    swap would silently corrupt the stream accounting.
//! 2. **Naming divergence.** `deflate` is camelCase and `inflate` is
//!    snake_case in the core; the call sites below mirror that exactly.
//!
//! ## Ownership model
//!
//! Input and output buffers are **borrowed per call** and never owned. The
//! engine state lives in a [`Box`] whose raw pointer is stashed in
//! `z_stream.state`; reconstructing that `Box` in [`drop_state`] runs the core's
//! `Drop`, which performs the work of C `deflateEnd` / `inflateEnd`.

// The FFI shim is normally built with `std` (it provides `gz` file I/O via
// `std::fs`). Under a `no_std` build the crate root (`lib.rs`) declares
// `extern crate alloc;`, so we can still obtain `Box` from `alloc`. Under `std`
// `Box` comes from the prelude and no import is needed.
#[cfg(not(feature = "std"))]
use alloc::boxed::Box;

use libc::{c_char, c_int, c_uint, c_ulong, off_t, size_t};

// C-ABI types defined by the sibling `zstream` module. We import only the
// symbols actually used here so the crate stays free of unused-import warnings
// under `-D warnings`. The `gz`-only types are imported inside the
// `#[cfg(feature = "gz-io")]` block further below.
use crate::zstream::{alloc_func, free_func, internal_state, voidpf, z_crc_t, z_stream, z_streamp};

// The gz C-ABI types (`gzFile`, `gzFile_s`) are always *defined* in `zstream.rs`
// but are referenced here only by the `gz`-feature helpers in Phase 8. Gating
// the import keeps the build free of an unused-import warning under
// `-D warnings` when `gz-io` is disabled.
#[cfg(feature = "gz-io")]
use crate::zstream::{gzFile, gzFile_s};

// Safe-core types. `ReturnCode`/`ZlibError` model the C status codes; the
// `constants` enums model the C `flush`/`strategy`/level inputs.
use zlib_rs::constants::{Flush, Level, Strategy, ZLIB_VERSION};
use zlib_rs::error::{ReturnCode, ZlibError};
use zlib_rs::stream::{Allocator, ZStream};

// `Vec` is used by the C-allocator adapter below. Under `std` it comes from the
// prelude; under `no_std` the crate root declares `extern crate alloc;`.
#[cfg(not(feature = "std"))]
use alloc::vec::Vec;

// ===========================================================================
// Phase 1 — Slice-formation helpers (raw ptr + len → borrowed slice)
// ===========================================================================
//
// The zlib streaming contract supplies `(next_in, avail_in)` and
// `(next_out, avail_out)`. A **null pointer paired with a zero length is
// legal** and must become an *empty slice*. `core::slice::from_raw_parts`
// requires a non-null, aligned data pointer even for a zero length, so the null
// case is special-cased to `&[]` / `&mut []` to avoid undefined behavior.

/// Forms a shared byte slice from a C `(ptr, len)` input pair.
///
/// A null pointer or a zero length yields an empty slice; the pointer is never
/// dereferenced in that case.
///
/// # Safety
///
/// When `ptr` is non-null and `len > 0`, the caller must guarantee — per the
/// zlib `next_in` / `avail_in` contract — that `ptr` is valid for reads of
/// `len` bytes, is properly aligned, points to `len` initialized bytes, and
/// that the referenced region stays immutable for the lifetime `'a` of the
/// returned borrow.
pub(crate) unsafe fn input_slice<'a>(ptr: *const u8, len: c_uint) -> &'a [u8] {
    if ptr.is_null() || len == 0 {
        return &[];
    }
    // SAFETY: `ptr` is non-null (checked) and the caller upholds the
    // `next_in`/`avail_in` contract: it is valid, aligned, and points to `len`
    // initialized bytes that remain immutable for `'a`.
    unsafe { core::slice::from_raw_parts(ptr, len as usize) }
}

/// Forms a unique (mutable) byte slice from a C `(ptr, len)` output pair.
///
/// A null pointer or a zero length yields an empty slice; the pointer is never
/// dereferenced in that case.
///
/// # Safety
///
/// When `ptr` is non-null and `len > 0`, the caller must guarantee — per the
/// zlib `next_out` / `avail_out` contract — that `ptr` is valid for writes of
/// `len` bytes, is properly aligned, and is not aliased by any other live
/// reference for the lifetime `'a` of the returned borrow.
pub(crate) unsafe fn output_slice<'a>(ptr: *mut u8, len: c_uint) -> &'a mut [u8] {
    if ptr.is_null() || len == 0 {
        return &mut [];
    }
    // SAFETY: `ptr` is non-null (checked) and the caller upholds the
    // `next_out`/`avail_out` contract: it is valid for writes of `len` bytes,
    // aligned, and unaliased for `'a`.
    unsafe { core::slice::from_raw_parts_mut(ptr, len as usize) }
}

/// `size_t`-length variant of [`input_slice`] for the `*_z` checksum and
/// `gzfread` entry points.
///
/// # Safety
///
/// Same contract as [`input_slice`], with the length expressed as a `size_t`.
pub(crate) unsafe fn input_slice_sz<'a>(ptr: *const u8, len: size_t) -> &'a [u8] {
    if ptr.is_null() || len == 0 {
        return &[];
    }
    // SAFETY: identical invariant to `input_slice`; `len` is already a `usize`.
    unsafe { core::slice::from_raw_parts(ptr, len) }
}

/// `size_t`-length variant of [`output_slice`] for the `gzfwrite` entry point.
///
/// # Safety
///
/// Same contract as [`output_slice`], with the length expressed as a `size_t`.
pub(crate) unsafe fn output_slice_sz<'a>(ptr: *mut u8, len: size_t) -> &'a mut [u8] {
    if ptr.is_null() || len == 0 {
        return &mut [];
    }
    // SAFETY: identical invariant to `output_slice`; `len` is already a `usize`.
    unsafe { core::slice::from_raw_parts_mut(ptr, len) }
}

// ===========================================================================
// Phase 2 — Return-code / error and parameter conversions (Result ⇄ C int)
// ===========================================================================
//
// The safe core reports `Result<ReturnCode, ZlibError>`; the C ABI returns a
// plain `c_int`. The integer values reproduced here are fixed by `zlib.h`
// (L172-L189):
//
//   Z_OK            =  0      Z_ERRNO         = -1
//   Z_STREAM_END    =  1      Z_STREAM_ERROR  = -2
//   Z_NEED_DICT     =  2      Z_DATA_ERROR    = -3
//                             Z_MEM_ERROR     = -4
//                             Z_BUF_ERROR     = -5
//                             Z_VERSION_ERROR = -6
//
// and the flush / strategy / level inputs:
//
//   Z_NO_FLUSH=0 … Z_TREES=6
//   Z_DEFAULT_STRATEGY=0 … Z_FIXED=4
//   level ∈ {-1 (default), 0 … 9}

/// Collapses a core `Result<ReturnCode, ZlibError>` into the single `c_int`
/// status code the C ABI expects.
///
/// Delegates to the core's `From<…> for i32` conversions so the exact
/// discriminants (`Ok=0`, `StreamEnd=1`, `NeedDict=2`, `ErrNo=-1` … =`-6`) are
/// the single source of truth.
pub(crate) fn to_c_int(r: Result<ReturnCode, ZlibError>) -> c_int {
    match r {
        Ok(code) => i32::from(code),
        Err(err) => i32::from(err),
    }
}

/// Maps a raw C flush integer (0..=6) to [`Flush`].
///
/// Returns `None` for an out-of-range value; the caller is expected to surface
/// that as `Z_STREAM_ERROR` (`-2`). (An `Option` is used rather than
/// `Result<Flush, ()>` to avoid a `clippy::result_unit_err`; the call-site
/// behavior — invalid ⇒ `Z_STREAM_ERROR` — is identical.)
pub(crate) fn flush_from_c(flush: c_int) -> Option<Flush> {
    Flush::try_from_i32(flush)
}

/// Maps a raw C strategy integer (0..=4) to [`Strategy`].
///
/// Returns `None` for an out-of-range value; the caller surfaces that as
/// `Z_STREAM_ERROR`.
pub(crate) fn strategy_from_c(s: c_int) -> Option<Strategy> {
    Strategy::try_from_i32(s)
}

/// Validates a raw C compression level and wraps it in [`Level`].
///
/// Accepts `-1` (the default) and `0..=9`; any other value yields `None`, which
/// the caller surfaces as `Z_STREAM_ERROR`.
pub(crate) fn level_from_c(level: c_int) -> Option<Level> {
    Level::new(level)
}

// ===========================================================================
// Phase 3 — Opaque-state lifetime management (`Box` ⇄ `z_stream.state`)
// ===========================================================================
//
// C stores the engine state behind the opaque `z_stream.state`
// (`*mut internal_state`). Here the state is a heap-allocated [`Box<ZStream>`]
// whose raw pointer is stashed in that field. This replaces the C
// `malloc`/`free` of the internal state and turns `deflateEnd`/`inflateEnd`
// into the `Box`'s `Drop`.
//
// The raw `Box` machinery is written once, generically over the boxed type
// `T`, then specialized to [`ZStream`]. Keeping the generic core separate lets
// it be unit-tested with a trivial dummy type (no engine initialization
// required) while the typed wrappers stay obviously-correct one-liners.

/// Stashes a freshly-boxed state `T` into `z_stream.state` via
/// [`Box::into_raw`], transferring ownership to the C struct.
///
/// # Safety
///
/// `strm` must be a valid, non-null `z_streamp` (the caller's init routine
/// validates this). Any state previously stored in `state` is overwritten
/// without being freed, so this must only be called on a freshly zeroed /
/// detached stream.
unsafe fn attach_raw<T>(strm: z_streamp, state: Box<T>) {
    // SAFETY: `strm` is a valid, non-null pointer (caller's invariant); writing
    // the `state` field is therefore in-bounds. `Box::into_raw` yields a unique,
    // properly aligned pointer that we hand to C for safekeeping.
    unsafe {
        (*strm).state = Box::into_raw(state) as *mut internal_state;
    }
}

/// Recovers a unique reference to the boxed state `T` stored in
/// `z_stream.state`.
///
/// Returns `None` when `strm` or its `state` pointer is null, so the caller can
/// emit `Z_STREAM_ERROR`.
///
/// # Safety
///
/// `state` must hold a pointer that was produced by `attach_raw::<T>` for the
/// *same* `T` and not yet released by `drop_raw`. C callers must serialize
/// access to a given stream so the returned `&mut` is genuinely unique.
unsafe fn recover_raw<'a, T>(strm: z_streamp) -> Option<&'a mut T> {
    if strm.is_null() {
        return None;
    }
    // SAFETY: `strm` is non-null (checked); reading the `state` field is
    // in-bounds for a valid `z_stream`.
    let state_ptr = unsafe { (*strm).state };
    if state_ptr.is_null() {
        return None;
    }
    // SAFETY: `state_ptr` is non-null (checked) and, per this function's
    // contract, was produced by `attach_raw::<T>` and remains valid until
    // `drop_raw`. The reborrow is unique because C serializes per-stream access.
    Some(unsafe { &mut *(state_ptr as *mut T) })
}

/// Reconstructs and drops the boxed state `T`, then nulls `z_stream.state`.
///
/// Returns `false` (caller emits `Z_STREAM_ERROR`) when `strm` or `state` is
/// already null, guarding against a double free.
///
/// # Safety
///
/// `state` must hold a pointer produced by `attach_raw::<T>` for the same `T`,
/// not yet released. After this call the `state` field is null, so a second
/// `drop_raw` is a no-op.
unsafe fn drop_raw<T>(strm: z_streamp) -> bool {
    if strm.is_null() {
        return false;
    }
    // SAFETY: `strm` is non-null (checked); reading `state` is in-bounds.
    let state_ptr = unsafe { (*strm).state };
    if state_ptr.is_null() {
        return false;
    }
    // SAFETY: `state_ptr` came from `Box::into_raw` in `attach_raw::<T>`;
    // reconstructing the `Box` re-takes ownership exactly once and its `Drop`
    // performs the `deflateEnd`/`inflateEnd` cleanup. Nulling the field
    // afterwards makes any subsequent `drop_raw` a no-op (no double free).
    unsafe {
        drop(Box::from_raw(state_ptr as *mut T));
        (*strm).state = core::ptr::null_mut();
    }
    true
}

/// Attaches an initialized [`ZStream`] to a C `z_stream`.
///
/// # Safety
///
/// See [`attach_raw`]: `strm` must be a valid, non-null, freshly-detached
/// `z_streamp`.
pub(crate) unsafe fn attach_state(strm: z_streamp, state: Box<ZStream>) {
    // SAFETY: forwarded directly to `attach_raw`; the caller upholds its
    // contract that `strm` is valid and non-null.
    unsafe { attach_raw(strm, state) }
}

/// Recovers the [`ZStream`] owned by a C `z_stream`.
///
/// A single recover helper serves both deflate and inflate: the `StreamState`
/// enum *inside* [`ZStream`] already disambiguates the two, so the C `state`
/// pointer always denotes a `Box<ZStream>`. Returns `None` (→ caller emits
/// `Z_STREAM_ERROR`) for a null `strm` or `state`.
///
/// # Safety
///
/// See [`recover_raw`]: `state` must denote a live `Box<ZStream>` set by
/// [`attach_state`].
pub(crate) unsafe fn stream_state<'a>(strm: z_streamp) -> Option<&'a mut ZStream> {
    // SAFETY: forwarded to `recover_raw::<ZStream>`; only a `Box<ZStream>` is
    // ever stored in `state`, matching the requested `T`.
    unsafe { recover_raw::<ZStream>(strm) }
}

/// Ends a stream: drops the owned [`ZStream`] (running its `Drop`, i.e. the work
/// of `deflateEnd`/`inflateEnd`) and nulls `z_stream.state`.
///
/// Returns `false` for an already-ended (null `state`) stream so the caller can
/// emit `Z_STREAM_ERROR`.
///
/// # Safety
///
/// See [`drop_raw`]: `state` must denote a live `Box<ZStream>`.
pub(crate) unsafe fn drop_state(strm: z_streamp) -> bool {
    // SAFETY: forwarded to `drop_raw::<ZStream>`; only a `Box<ZStream>` is ever
    // stored in `state`, matching the requested `T`.
    unsafe { drop_raw::<ZStream>(strm) }
}

// ===========================================================================
// Phase 5 — Null-pointer & `Z_STREAM_ERROR` guards (match C exactly)
// ===========================================================================

/// Validates that `strm` and its `state` are both non-null, mirroring the
/// defensive checks at the top of the C `deflate.c` / `inflate.c` entry points.
///
/// Returns `Err(Z_STREAM_ERROR)` (`-2`) when either pointer is null so the
/// caller can `return` the code directly.
///
/// # Safety
///
/// `strm`, if non-null, must point to a valid `z_stream` so the `state` field
/// can be read.
pub(crate) unsafe fn check_stream(strm: z_streamp) -> Result<(), c_int> {
    if strm.is_null() {
        return Err(ZlibError::StreamError.as_i32());
    }
    // SAFETY: `strm` is non-null (checked); reading `state` is in-bounds for a
    // valid `z_stream`.
    if unsafe { (*strm).state.is_null() } {
        return Err(ZlibError::StreamError.as_i32());
    }
    Ok(())
}

/// Reproduces the C init-time version/size guard.
///
/// `deflateInit_`/`inflateInit_` reject a caller whose major version character
/// differs from the library's, or whose `sizeof(z_stream)` differs from this
/// build's, with `Z_VERSION_ERROR` (`-6`). A null `version` is likewise
/// rejected.
///
/// # Safety
///
/// `version`, if non-null, must point to a NUL-terminated C string of at least
/// one byte, per the zlib `*Init_` contract.
pub(crate) unsafe fn check_version(
    version: *const c_char,
    stream_size: c_int,
) -> Result<(), c_int> {
    if version.is_null() {
        return Err(ZlibError::VersionError.as_i32());
    }
    // SAFETY: `version` is non-null (checked); the zlib contract guarantees it
    // points to a readable, NUL-terminated string of at least one byte.
    let first = unsafe { *version } as u8;
    if first != ZLIB_VERSION.as_bytes()[0] {
        return Err(ZlibError::VersionError.as_i32());
    }
    if stream_size < 0 || stream_size as usize != core::mem::size_of::<z_stream>() {
        return Err(ZlibError::VersionError.as_i32());
    }
    Ok(())
}

// ===========================================================================
// Phase 4 — The per-call deflate / inflate protocol (the heart of the shim)
// ===========================================================================
//
// `run_deflate` and `run_inflate` are the drivers behind `lib.rs`'s `deflate`
// and `inflate` exports. Each encodes the same five-step protocol:
//
//   1. recover the `&mut ZStream` (else `Z_STREAM_ERROR`);
//   2. form the input / output slices from `next_in`/`avail_in`,
//      `next_out`/`avail_out`;
//   3. call the core engine;
//   4. write the results back into the C struct (advance pointers, decrement
//      `avail_*`, bump `total_*`, copy `adler`/`data_type`/`msg`);
//   5. return the converted `c_int`.
//
// The two differ ONLY in step 3 because of the core's TUPLE-ORDER DIVERGENCE,
// so they are kept as separate functions and the divergence is spelled out at
// each destructuring site. The write-back itself is identical and is factored
// into the shared `write_back` helper.

/// Shared step 4 (cursors + scalars): advance `next_in` / `next_out`, decrement
/// `avail_in` / `avail_out`, and publish `adler`, `data_type`, and `msg`.
///
/// This helper deliberately does **not** touch `total_in` / `total_out`: the two
/// engines account for the lifetime totals differently, so each engine-specific
/// write-back ([`write_back`] for deflate, [`write_back_inflate`] for inflate)
/// applies the totals itself and then calls this for the common cursor/scalar
/// work. `consumed` / `produced` are the per-call byte counts the engine reports
/// (always the per-call deltas, never cumulative — they drive the cursor
/// advance and `avail_*` decrement); `adler` and `data_type` are the post-call
/// engine scalars (already converted to their C widths by the caller); `result`
/// selects the `msg` pointer.
///
/// # Safety
///
/// `strm` must be the valid, non-null `z_streamp` whose state produced these
/// counts. The engine guarantees `consumed ≤` the pre-call `avail_in` and
/// `produced ≤` the pre-call `avail_out`, so the pointer advances and the
/// `avail_*` decrements stay within the caller's buffers.
unsafe fn write_back_common(
    strm: z_streamp,
    consumed: usize,
    produced: usize,
    adler: c_ulong,
    data_type: c_int,
    result: Result<ReturnCode, ZlibError>,
) {
    // SAFETY: `strm` is valid and non-null (caller's invariant). The `.add`
    // offsets are within-bounds because `consumed ≤ avail_in` and
    // `produced ≤ avail_out` (engine guarantees); advancement is skipped when a
    // count is zero so a null `next_in`/`next_out` is never offset.
    unsafe {
        let s = &mut *strm;

        if consumed != 0 {
            s.next_in = s.next_in.add(consumed);
        }
        s.avail_in -= consumed as c_uint;

        if produced != 0 {
            s.next_out = s.next_out.add(produced);
        }
        s.avail_out -= produced as c_uint;

        s.adler = adler;
        s.data_type = data_type;

        // Per the zlib contract `msg` is the last error message (NULL if no
        // error) and must point to a `'static` C string. We source it from the
        // shared, null-terminated `z_error_cstr` table keyed by the result code.
        s.msg = match result {
            Ok(_) => core::ptr::null(),
            Err(err) => z_error_cstr(err.as_i32()),
        };
    }
}

/// Deflate step 4: accumulate `total_in` / `total_out` by the per-call deltas,
/// then apply the shared cursor/scalar write-back.
///
/// This preserves the historical, validated deflate accounting unchanged: the
/// C `z_stream` totals are advanced by `consumed` / `produced` each call. The
/// deflate engine's own cumulative counters (mirrored onto the owned `ZStream`)
/// agree with this running sum, so the observable totals are identical to what C
/// zlib reports for a deflate stream.
///
/// # Safety
///
/// Same contract as [`write_back_common`].
unsafe fn write_back(
    strm: z_streamp,
    consumed: usize,
    produced: usize,
    adler: c_ulong,
    data_type: c_int,
    result: Result<ReturnCode, ZlibError>,
) {
    // SAFETY: `strm` is valid and non-null (caller's invariant).
    unsafe {
        let s = &mut *strm;
        s.total_in = s.total_in.wrapping_add(consumed as c_ulong);
        s.total_out = s.total_out.wrapping_add(produced as c_ulong);
    }
    // SAFETY: same `strm` invariant; the prior `&mut *strm` borrow above has
    // already ended, so this re-borrow inside the helper does not alias.
    unsafe { write_back_common(strm, consumed, produced, adler, data_type, result) };
}

/// Inflate step 4: **set** `total_in` / `total_out` to the engine's own
/// cumulative lifetime counters (which the core inflate wrapper mirrors onto the
/// owned `ZStream` from `InflateState`), then apply the shared cursor/scalar
/// write-back.
///
/// The inflate engine is the single source of truth for its lifetime totals, so
/// the C `z_stream` is published directly from those cumulative values rather
/// than re-accumulating per-call deltas onto the separate C-struct counter. This
/// matches C zlib — where `strm->total_in` / `strm->total_out` *are* the engine's
/// own counters — and makes double-counting across successive productive
/// `inflate()` calls structurally impossible: the published totals can only ever
/// equal the engine's cumulative count, never a re-summed delta. (`consumed` /
/// `produced` are still the per-call deltas used by [`write_back_common`] for the
/// cursor advance and `avail_*` decrement.)
///
/// # Safety
///
/// Same contract as [`write_back_common`].
#[allow(clippy::too_many_arguments)]
unsafe fn write_back_inflate(
    strm: z_streamp,
    consumed: usize,
    produced: usize,
    total_in: c_ulong,
    total_out: c_ulong,
    adler: c_ulong,
    data_type: c_int,
    result: Result<ReturnCode, ZlibError>,
) {
    // SAFETY: `strm` is valid and non-null (caller's invariant).
    unsafe {
        let s = &mut *strm;
        s.total_in = total_in;
        s.total_out = total_out;
    }
    // SAFETY: same `strm` invariant; the prior `&mut *strm` borrow above has
    // already ended, so this re-borrow inside the helper does not alias.
    unsafe { write_back_common(strm, consumed, produced, adler, data_type, result) };
}

/// Drives one `deflate` call: validate, slice, delegate, write back, convert.
///
/// # Safety
///
/// `strm` must be null or a valid `z_streamp` whose `next_in`/`next_out`
/// buffers honor the corresponding `avail_*` counts (the zlib streaming
/// contract).
pub(crate) unsafe fn run_deflate(strm: z_streamp, flush: c_int) -> c_int {
    // 1. recover the engine state.
    // SAFETY: `strm` is null or a valid `z_streamp` (caller's invariant);
    // `stream_state` performs the null/`state` checks internally.
    let zstream = match unsafe { stream_state(strm) } {
        Some(z) => z,
        None => return ZlibError::StreamError.as_i32(),
    };

    let flush = match flush_from_c(flush) {
        Some(f) => f,
        None => return ZlibError::StreamError.as_i32(),
    };

    // 2. form the borrowed slices from the C buffers.
    // SAFETY: per the `next_in`/`avail_in` and `next_out`/`avail_out` contract
    // the buffers are valid for the stated lengths; the borrows live only for
    // the single engine call below.
    let (input, output) = unsafe {
        (
            input_slice((*strm).next_in, (*strm).avail_in),
            output_slice((*strm).next_out, (*strm).avail_out),
        )
    };

    // 3. delegate to the core engine.
    // ┌──────────────────────────────────────────────────────────────────────┐
    // │ DEFLATE TUPLE ORDER: (result, consumed, produced).  This is the        │
    // │ OPPOSITE of `inflate` (see `run_inflate`). Do NOT reorder.             │
    // └──────────────────────────────────────────────────────────────────────┘
    let (result, consumed, produced) = zlib_rs::deflate::deflate(zstream, input, output, flush);

    // Capture the post-call engine scalars (both `Copy`) before the write-back.
    let adler = zstream.adler as c_ulong;
    let data_type = zstream.data_type as c_int;

    // 4. write the per-call results back into the C struct.
    // SAFETY: `strm` is valid (we recovered its state above); the counts honor
    // the engine's `consumed ≤ avail_in` / `produced ≤ avail_out` guarantee.
    unsafe { write_back(strm, consumed, produced, adler, data_type, result) };

    // 5. convert the outcome to the C status code.
    to_c_int(result)
}

/// Drives one `inflate` call: validate, slice, delegate, write back, convert.
///
/// # Safety
///
/// `strm` must be null or a valid `z_streamp` whose `next_in`/`next_out`
/// buffers honor the corresponding `avail_*` counts (the zlib streaming
/// contract).
pub(crate) unsafe fn run_inflate(strm: z_streamp, flush: c_int) -> c_int {
    // 1. recover the engine state.
    // SAFETY: `strm` is null or a valid `z_streamp` (caller's invariant);
    // `stream_state` performs the null/`state` checks internally.
    let zstream = match unsafe { stream_state(strm) } {
        Some(z) => z,
        None => return ZlibError::StreamError.as_i32(),
    };

    let flush = match flush_from_c(flush) {
        Some(f) => f,
        None => return ZlibError::StreamError.as_i32(),
    };

    // 2. form the borrowed slices from the C buffers.
    // SAFETY: per the `next_in`/`avail_in` and `next_out`/`avail_out` contract
    // the buffers are valid for the stated lengths; the borrows live only for
    // the single engine call below.
    let (input, output) = unsafe {
        (
            input_slice((*strm).next_in, (*strm).avail_in),
            output_slice((*strm).next_out, (*strm).avail_out),
        )
    };

    // 3. delegate to the core engine.
    // ┌──────────────────────────────────────────────────────────────────────┐
    // │ INFLATE TUPLE ORDER: (consumed, produced, result).  This is the        │
    // │ OPPOSITE of `deflate` (see `run_deflate`). Do NOT reorder.             │
    // └──────────────────────────────────────────────────────────────────────┘
    let (consumed, produced, result) = zlib_rs::inflate::inflate(zstream, input, output, flush);

    // Capture the post-call engine scalars (all `Copy`) before the write-back.
    // `total_in` / `total_out` are the engine's CUMULATIVE lifetime counters
    // (the core inflate wrapper mirrors `InflateState`'s counters onto the owned
    // `ZStream`). We publish them verbatim via `write_back_inflate` rather than
    // re-accumulating per-call deltas onto the separate C-struct counter, so the
    // C `z_stream` totals always equal the engine's own count — matching C zlib
    // and making cross-call double-counting impossible.
    let total_in = zstream.total_in as c_ulong;
    let total_out = zstream.total_out as c_ulong;
    let adler = zstream.adler as c_ulong;
    let data_type = zstream.data_type as c_int;

    // 4. write the per-call cursor advance + cumulative totals back into the C
    //    struct.
    // SAFETY: `strm` is valid (we recovered its state above); the counts honor
    // the engine's `consumed ≤ avail_in` / `produced ≤ avail_out` guarantee.
    unsafe {
        write_back_inflate(
            strm, consumed, produced, total_in, total_out, adler, data_type, result,
        )
    };

    // 5. convert the outcome to the C status code.
    to_c_int(result)
}

// ===========================================================================
// Phase 6 — Numeric conversions for checksums & one-shot helpers
// ===========================================================================
//
// These conversions live here so `lib.rs` stays declarative. They reproduce
// the exact C semantics:
//   * `adler32(_, NULL, _)` returns the initial value 1; `crc32(_, NULL, _)`
//     returns 0 (regardless of the supplied running value);
//   * `c_ulong` carries a 32-bit checksum — the upper bits are zero on LP64;
//   * `*_combine` takes a *signed* `z_off_t` length with per-function handling
//     of a negative value.

/// FFI `adler32`: running Adler-32 over `(buf, len)`.
///
/// A null `buf` returns the initial value `1` (the C contract), ignoring the
/// supplied running value. The 32-bit result is widened to `c_ulong` with zero
/// upper bits on LP64.
///
/// # Safety
///
/// When `buf` is non-null it must be valid for reads of `len` bytes (see
/// [`input_slice`]).
pub(crate) unsafe fn adler32_ffi(adler: c_ulong, buf: *const u8, len: c_uint) -> c_ulong {
    if buf.is_null() {
        return 1;
    }
    // SAFETY: `buf` is non-null (checked); the caller guarantees `len` readable
    // bytes per the checksum contract.
    let data = unsafe { input_slice(buf, len) };
    c_ulong::from(zlib_rs::checksum::adler32(adler as u32, data))
}

/// FFI `adler32_z`: `size_t`-length variant of [`adler32_ffi`].
///
/// # Safety
///
/// When `buf` is non-null it must be valid for reads of `len` bytes.
pub(crate) unsafe fn adler32_z_ffi(adler: c_ulong, buf: *const u8, len: size_t) -> c_ulong {
    if buf.is_null() {
        return 1;
    }
    // SAFETY: `buf` is non-null (checked); the caller guarantees `len` readable
    // bytes per the checksum contract.
    let data = unsafe { input_slice_sz(buf, len) };
    c_ulong::from(zlib_rs::checksum::adler32_z(adler as u32, data))
}

/// FFI `crc32`: running CRC-32 over `(buf, len)`.
///
/// A null `buf` returns `0` (the C contract). The 32-bit result is widened to
/// `c_ulong` with zero upper bits on LP64.
///
/// # Safety
///
/// When `buf` is non-null it must be valid for reads of `len` bytes.
pub(crate) unsafe fn crc32_ffi(crc: c_ulong, buf: *const u8, len: c_uint) -> c_ulong {
    if buf.is_null() {
        return 0;
    }
    // SAFETY: `buf` is non-null (checked); the caller guarantees `len` readable
    // bytes per the checksum contract.
    let data = unsafe { input_slice(buf, len) };
    c_ulong::from(zlib_rs::checksum::crc32(crc as u32, data))
}

/// FFI `crc32_z`: `size_t`-length variant of [`crc32_ffi`].
///
/// # Safety
///
/// When `buf` is non-null it must be valid for reads of `len` bytes.
pub(crate) unsafe fn crc32_z_ffi(crc: c_ulong, buf: *const u8, len: size_t) -> c_ulong {
    if buf.is_null() {
        return 0;
    }
    // SAFETY: `buf` is non-null (checked); the caller guarantees `len` readable
    // bytes per the checksum contract.
    let data = unsafe { input_slice_sz(buf, len) };
    c_ulong::from(zlib_rs::checksum::crc32_z(crc as u32, data))
}

/// Converts a signed C `z_off_t` (== `off_t`) length to the core's unsigned
/// `u64`, returning `None` for a negative value.
///
/// The per-function negative handling lives at the `*_combine` call sites:
/// `crc32_combine` treats a negative length as `0`, while `adler32_combine`
/// returns the invalid-checksum sentinel `0xFFFF_FFFF`.
pub(crate) fn off_to_len(len2: off_t) -> Option<u64> {
    if len2 < 0 { None } else { Some(len2 as u64) }
}

/// FFI `adler32_combine`: combine two Adler-32 sums for streams of lengths
/// `len1` (implied) and `len2`.
///
/// A negative `len2` "has no meaning" and yields the C debugging sentinel
/// `0xFFFF_FFFF`.
pub(crate) fn adler32_combine_ffi(adler1: c_ulong, adler2: c_ulong, len2: off_t) -> c_ulong {
    match off_to_len(len2) {
        Some(len) => c_ulong::from(zlib_rs::checksum::adler32_combine(
            adler1 as u32,
            adler2 as u32,
            len,
        )),
        None => 0xFFFF_FFFF,
    }
}

/// FFI `crc32_combine`: combine two CRC-32 sums.
///
/// A negative `len2` yields `0` (the C "len2 must be non-negative, otherwise
/// zero is returned" rule).
pub(crate) fn crc32_combine_ffi(crc1: c_ulong, crc2: c_ulong, len2: off_t) -> c_ulong {
    match off_to_len(len2) {
        Some(len) => c_ulong::from(zlib_rs::checksum::crc32_combine(
            crc1 as u32,
            crc2 as u32,
            len,
        )),
        None => 0,
    }
}

/// FFI `crc32_combine_gen`: precompute the combine operator for length `len2`.
///
/// A negative `len2` yields `0` (the C "non-negative, otherwise zero" rule).
pub(crate) fn crc32_combine_gen_ffi(len2: off_t) -> c_ulong {
    match off_to_len(len2) {
        Some(len) => c_ulong::from(zlib_rs::checksum::crc32_combine_gen(len)),
        None => 0,
    }
}

/// FFI `crc32_combine_op`: combine two CRC-32 sums using a precomputed operator.
///
/// All three values are 32-bit; `op` is masked into `u32` and the result
/// widened back to `c_ulong`.
pub(crate) fn crc32_combine_op_ffi(crc1: c_ulong, crc2: c_ulong, op: c_ulong) -> c_ulong {
    c_ulong::from(zlib_rs::checksum::crc32_combine_op(
        crc1 as u32,
        crc2 as u32,
        op as u32,
    ))
}

/// FFI `get_crc_table`: exposes the core's static CRC-32 table as a
/// `*const z_crc_t`.
///
/// `z_crc_t` is `u32`, so the `&'static [u32; 256]` reinterprets without a cast.
pub(crate) fn get_crc_table_ptr() -> *const z_crc_t {
    zlib_rs::checksum::get_crc_table().as_ptr()
}

/// Reads the in/out `destLen` capacity supplied to the one-shot
/// `compress`/`uncompress` entry points.
///
/// # Safety
///
/// `dest_len` must be a valid, readable pointer to a `c_ulong`.
pub(crate) unsafe fn read_dest_len(dest_len: *mut c_ulong) -> usize {
    // SAFETY: the caller guarantees `dest_len` is a valid pointer to an
    // initialized, writable `c_ulong` (the one-shot in/out length parameter).
    unsafe { *dest_len as usize }
}

/// Writes the produced length back through the one-shot `destLen` pointer.
///
/// # Safety
///
/// `dest_len` must be a valid, writable pointer to a `c_ulong`.
pub(crate) unsafe fn write_dest_len(dest_len: *mut c_ulong, produced: usize) {
    // SAFETY: the caller guarantees `dest_len` is a valid, writable pointer to a
    // `c_ulong` (the one-shot in/out length parameter).
    unsafe {
        *dest_len = produced as c_ulong;
    }
}

// ===========================================================================
// Phase 7 — Static C strings (`zlibVersion`, `zError`, `z_stream.msg`)
// ===========================================================================
//
// The C ABI returns `*const c_char` for version / error strings, which must be
// `'static` and NUL-terminated. The core's `&'static str` values are NOT
// NUL-terminated, so this module keeps NUL-terminated mirrors.

/// NUL-terminated mirror of [`ZLIB_VERSION`], backing `zlibVersion()`.
///
/// A `const` assertion below proves it equals `ZLIB_VERSION` followed by a
/// single NUL byte.
pub(crate) const VERSION_CSTR: &[u8] = b"1.3.2.1-motley\0";

// Compile-time proof that `VERSION_CSTR == ZLIB_VERSION + "\0"`. If the core
// version string ever changes, this fails the build rather than silently
// shipping a stale version through the C ABI.
const _: () = {
    let v = ZLIB_VERSION.as_bytes();
    assert!(
        VERSION_CSTR.len() == v.len() + 1,
        "VERSION_CSTR must be ZLIB_VERSION + NUL"
    );
    assert!(
        VERSION_CSTR[v.len()] == 0,
        "VERSION_CSTR must be NUL-terminated"
    );
    let mut i = 0;
    while i < v.len() {
        assert!(
            VERSION_CSTR[i] == v[i],
            "VERSION_CSTR must match ZLIB_VERSION"
        );
        i += 1;
    }
};

/// Returns the `'static`, NUL-terminated version string pointer for
/// `zlibVersion()`.
pub(crate) fn version_ptr() -> *const c_char {
    VERSION_CSTR.as_ptr() as *const c_char
}

/// Returns a `'static`, NUL-terminated message pointer for a C status `code`,
/// backing `zError()` and the `z_stream.msg` write-back.
///
/// The text mirrors `zlib_rs::error::err_msg` exactly. Any code outside the
/// inclusive range `-6..=2` maps to the empty string, matching the C `ERR_MSG`
/// macro's trailing empty slot.
pub(crate) fn z_error_cstr(code: c_int) -> *const c_char {
    // NUL-terminated twins of the `crate::error` messages, keyed by C code.
    const M_OK: &[u8] = b"\0";
    const M_STREAM_END: &[u8] = b"stream end\0";
    const M_NEED_DICT: &[u8] = b"need dictionary\0";
    const M_ERRNO: &[u8] = b"file error\0";
    const M_STREAM_ERR: &[u8] = b"stream error\0";
    const M_DATA_ERR: &[u8] = b"data error\0";
    const M_MEM_ERR: &[u8] = b"insufficient memory\0";
    const M_BUF_ERR: &[u8] = b"buffer error\0";
    const M_VERSION_ERR: &[u8] = b"incompatible version\0";
    const M_EMPTY: &[u8] = b"\0";

    let bytes: &'static [u8] = match code {
        0 => M_OK,
        1 => M_STREAM_END,
        2 => M_NEED_DICT,
        -1 => M_ERRNO,
        -2 => M_STREAM_ERR,
        -3 => M_DATA_ERR,
        -4 => M_MEM_ERR,
        -5 => M_BUF_ERR,
        -6 => M_VERSION_ERR,
        _ => M_EMPTY,
    };
    bytes.as_ptr() as *const c_char
}

// ===========================================================================
// Phase 8 — gz support conversions (gated `#[cfg(feature = "gz-io")]`)
// ===========================================================================
//
// These helpers back the `gz*` functions in `lib.rs`. The gz C-ABI *types*
// (`gzFile`, `gzFile_s`) are always defined in `zstream.rs`; only the gz
// *functions* and their conversions are feature-gated, because they require
// `std` file I/O. The real gz state is the core's `GzState`; the shim only ever
// hands C a `*mut gzFile_s` whose leading `have`/`next`/`pos` fields are
// layout-compatible. The `Box` machinery below is generic over the concrete
// state type `T`, exactly mirroring Phase 3, so this module need not name
// `GzState` (its concrete shape is supplied by `lib.rs`).

/// Adopts an OS file descriptor into an owned [`std::fs::File`] for `gzdopen`.
///
/// The core `gzdopen` takes an owned `File`; the C `gzdopen(int fd, …)` adopts
/// `fd`, so ownership is transferred here.
///
/// # Safety
///
/// The caller transfers ownership of `fd`: it must be a valid, open descriptor
/// that is not owned (and will not be closed) elsewhere.
#[cfg(all(feature = "gz-io", unix))]
pub(crate) unsafe fn file_from_fd(fd: c_int) -> std::fs::File {
    use std::os::unix::io::FromRawFd;
    // SAFETY: the C `gzdopen` contract transfers ownership of `fd` to us; the
    // caller guarantees it is a valid, open descriptor owned by no one else.
    unsafe { std::fs::File::from_raw_fd(fd) }
}

/// Windows counterpart of [`file_from_fd`]: maps a CRT file descriptor to its
/// underlying OS `HANDLE` via `_get_osfhandle`, then adopts it.
///
/// # Safety
///
/// The caller transfers ownership of `fd`: it must be a valid, open CRT
/// descriptor owned by no one else.
#[cfg(all(feature = "gz-io", windows))]
pub(crate) unsafe fn file_from_fd(fd: c_int) -> std::fs::File {
    use std::os::windows::io::{FromRawHandle, RawHandle};
    // The MSVC C runtime maps a CRT fd to an OS HANDLE; `std` cannot do this, so
    // the CRT entry point is declared and called directly.
    unsafe extern "C" {
        fn _get_osfhandle(fd: c_int) -> isize;
    }
    // SAFETY: the caller transfers ownership of the CRT `fd`; `_get_osfhandle`
    // returns its backing OS HANDLE, which we adopt into an owned `File`.
    let handle = unsafe { _get_osfhandle(fd) } as RawHandle;
    // SAFETY: `handle` is the OS HANDLE backing `fd`, whose ownership the caller
    // transferred to us.
    unsafe { std::fs::File::from_raw_handle(handle) }
}

/// Stashes a boxed gz state `T` and returns the C `gzFile` view of it.
///
/// `T`'s first three fields must be layout-compatible with `gzFile_s`
/// (`have`, `next`, `pos`); `lib.rs` upholds this when it defines the concrete
/// state type.
#[cfg(feature = "gz-io")]
pub(crate) fn gzfile_from_state<T>(state: Box<T>) -> gzFile {
    Box::into_raw(state) as *mut gzFile_s
}

/// Recovers a unique reference to the boxed gz state `T` from a `gzFile`.
///
/// Returns `None` for a null handle so the caller can report an error.
///
/// # Safety
///
/// `file` must be a handle produced by [`gzfile_from_state`] for the same `T`
/// and not yet released by [`drop_gzfile`]; C callers serialize per-file access.
#[cfg(feature = "gz-io")]
pub(crate) unsafe fn gz_state_mut<'a, T>(file: gzFile) -> Option<&'a mut T> {
    if file.is_null() {
        return None;
    }
    // SAFETY: `file` is non-null (checked) and, per this function's contract,
    // was produced by `gzfile_from_state::<T>` and remains valid; the reborrow
    // is unique because C serializes per-file access.
    Some(unsafe { &mut *(file as *mut T) })
}

/// Reconstructs and drops the boxed gz state `T` behind a `gzFile`.
///
/// Returns `false` for a null handle (guards against a double free).
///
/// # Safety
///
/// `file` must be a handle produced by [`gzfile_from_state`] for the same `T`,
/// not yet released.
#[cfg(feature = "gz-io")]
pub(crate) unsafe fn drop_gzfile<T>(file: gzFile) -> bool {
    if file.is_null() {
        return false;
    }
    // SAFETY: `file` came from `Box::into_raw` in `gzfile_from_state::<T>`;
    // reconstructing the `Box` re-takes ownership exactly once and runs the
    // state's `Drop` (closing the file and releasing buffers).
    unsafe {
        drop(Box::from_raw(file as *mut T));
    }
    true
}

/// Converts a core `i64` stream position to a C `z_off_t` (== `off_t`).
///
/// An out-of-range value (only possible where `off_t` is narrower than `i64`)
/// saturates to `-1`, the zlib `z_off_t` error sentinel used by
/// `gzseek`/`gztell`/`gzoffset`.
///
/// The `try_from` is genuinely fallible on targets where `off_t` is 32-bit
/// (e.g. 32-bit platforms without LFS). On 64-bit Unix `off_t == i64`, where
/// the conversion is infallible — hence the `allow`, which keeps the code
/// portable without a platform-specific `cfg` fork.
#[cfg(feature = "gz-io")]
#[allow(clippy::unnecessary_fallible_conversions)]
pub(crate) fn i64_to_off_t(pos: i64) -> off_t {
    off_t::try_from(pos).unwrap_or(-1)
}

/// Converts a C `z_off_t` (== `off_t`) to the core's `i64` position width.
///
/// `i64::from` is a real widening where `off_t` is 32-bit; on 64-bit Unix
/// (`off_t == i64`) it is the identity conversion — hence the `allow`, which
/// keeps the helper portable without a platform-specific `cfg` fork.
#[cfg(feature = "gz-io")]
#[allow(clippy::useless_conversion)]
pub(crate) fn off_t_to_i64(off: off_t) -> i64 {
    i64::from(off)
}

/// `gzprintf` formatting bridge: renders pre-captured [`core::fmt::Arguments`]
/// to bytes and forwards them to a caller-supplied `sink`, returning the
/// `sink`'s byte count (or `Z_STREAM_ERROR` on a formatting failure).
///
/// The C-variadic capture happens in `lib.rs`; this helper performs only the
/// `Arguments` → bytes rendering and delegates the actual write to the core via
/// `sink` (so the core's exact gz-write signature need not be named here).
#[cfg(feature = "gz-io")]
pub(crate) fn gz_write_fmt<F>(args: core::fmt::Arguments<'_>, mut sink: F) -> c_int
where
    F: FnMut(&[u8]) -> c_int,
{
    use core::fmt::Write as _;
    let mut rendered = std::string::String::new();
    if rendered.write_fmt(args).is_err() {
        return ZlibError::StreamError.as_i32();
    }
    sink(rendered.as_bytes())
}

// ===========================================================================
// Phase 9 — C-allocator adapter (`zalloc`/`zfree`/`opaque` → core `Allocator`)
// ===========================================================================
//
// The C `z_stream` lets a caller override allocation with two raw function
// pointers and an opaque cookie: `alloc_func zalloc(opaque, items, size)` and
// `free_func zfree(opaque, address)` (`zlib.h` lines 85-86). The safe core
// instead vends owned `Vec` buffers through the [`Allocator`] trait. This
// adapter bridges the two so that a C caller's custom or memory-limiting
// allocator is genuinely honored end-to-end and its failures surface as
// `Z_MEM_ERROR`, exactly like stock zlib.
//
// ## Why a *shadow* allocation
//
// The core's buffers are owned `Vec`s, and reconstituting a `Vec` from a
// C-`malloc`'d block (`Vec::from_raw_parts`) would be unsound — the global
// allocator, not the caller's `zfree`, frees a `Vec`. So for each request the
// adapter performs a **shadow allocation** through the caller's `zalloc` purely
// for accounting/limit/failure parity, and returns a *separate* owned `Vec` for
// the actual storage. The C-side accounting is therefore byte-for-byte what
// stock zlib would request (the shadow block stays outstanding until teardown),
// so a memory-limiting allocator fails at exactly the same point as C; the
// extra owned `Vec` is invisible to the caller's allocator. Every shadow block
// is released through `zfree` when the adapter is dropped (RAII — the analogue
// of `deflateEnd`/`inflateEnd` freeing each `zalloc`'d block).
//
// All `unsafe` here is the unavoidable invocation of caller-supplied C function
// pointers; each call carries a `// SAFETY:` note.

/// Adapts a C `z_stream`'s `zalloc`/`zfree`/`opaque` callbacks to the core
/// [`Allocator`] trait. Installed on the core `ZStream` (deflate) or the
/// `InflateState` (inflate) so the safe engine allocates through the caller's
/// hooks.
pub(crate) struct CAllocator {
    /// Caller `zalloc` (guaranteed `Some` for an installed adapter).
    zalloc: alloc_func,
    /// Caller `zfree`; if `None`, shadow blocks are simply leaked back to the
    /// caller's allocator (matching zlib, which requires both or neither).
    zfree: free_func,
    /// Caller opaque cookie passed verbatim to every `zalloc`/`zfree` call.
    opaque: voidpf,
    /// Outstanding shadow blocks obtained from `zalloc`, freed on `Drop`.
    blocks: core::cell::RefCell<Vec<voidpf>>,
}

impl CAllocator {
    /// Builds an adapter from a stream's callbacks, or `None` when the caller
    /// did not install a custom `zalloc` (the default-allocator case, in which
    /// the core simply uses the global allocator).
    ///
    /// Mirrors zlib's rule that a custom allocator is in effect only when
    /// `zalloc` is non-null; `zfree` is expected to accompany it.
    pub(crate) fn from_callbacks(
        zalloc: alloc_func,
        zfree: free_func,
        opaque: voidpf,
    ) -> Option<Self> {
        zalloc.map(|f| CAllocator {
            zalloc: Some(f),
            zfree,
            opaque,
            blocks: core::cell::RefCell::new(Vec::new()),
        })
    }

    /// Shadow-allocates `count * elem_size` bytes through the caller's
    /// `zalloc`, recording the block for teardown. Returns the raw pointer, or
    /// `None` if `zalloc` reports failure (null) or the request cannot be
    /// expressed in the C `uInt` item/size ABI.
    fn shadow_alloc(&self, count: usize, elem_size: c_uint) -> Option<voidpf> {
        // The C ABI passes `items`/`size` as `uInt` (c_uint). Refuse requests
        // that cannot be represented rather than silently truncating.
        let items = c_uint::try_from(count).ok()?;
        let zalloc = self.zalloc?;
        // SAFETY: `zalloc` is the caller-provided `alloc_func` from their
        // `z_stream`; `opaque` is their matching cookie. We pass `items`/`size`
        // within the documented `uInt` ABI. A null return means allocation
        // failure, handled below.
        let ptr = unsafe { zalloc(self.opaque, items, elem_size) };
        if ptr.is_null() {
            return None;
        }
        self.blocks.borrow_mut().push(ptr);
        Some(ptr)
    }

    /// Backs out the most recently recorded shadow block (used when the owned
    /// `Vec` allocation fails *after* a successful shadow allocation, so the
    /// caller's allocator is left balanced).
    fn unwind_last_block(&self, ptr: voidpf) {
        let mut blocks = self.blocks.borrow_mut();
        if blocks.last() == Some(&ptr) {
            blocks.pop();
        }
        if let Some(zfree) = self.zfree {
            // SAFETY: `ptr` was just returned by the caller's `zalloc` with the
            // same `opaque`; freeing it exactly once here balances that call.
            unsafe { zfree(self.opaque, ptr) };
        }
    }
}

impl Allocator for CAllocator {
    fn allocate_bytes(&self, len: usize) -> Option<Vec<u8>> {
        let block = self.shadow_alloc(len, 1)?;
        let mut storage = Vec::new();
        if storage.try_reserve_exact(len).is_err() {
            self.unwind_last_block(block);
            return None;
        }
        storage.resize(len, 0u8);
        Some(storage)
    }

    fn allocate_u16(&self, len: usize) -> Option<Vec<u16>> {
        // 16-bit elements: mirror zlib's `ZALLOC(strm, len, sizeof(Pos))`.
        let block = self.shadow_alloc(len, 2)?;
        let mut storage = Vec::new();
        if storage.try_reserve_exact(len).is_err() {
            self.unwind_last_block(block);
            return None;
        }
        storage.resize(len, 0u16);
        Some(storage)
    }

    // `deallocate_*` drops the owned `Vec` (freeing the storage). The matching
    // shadow block stays outstanding until `Drop`, mirroring zlib's "free every
    // `zalloc`'d block at `inflateEnd`/`deflateEnd`" lifetime — the core never
    // reallocates these buffers, so blocks do not accumulate unbounded.
    fn deallocate_bytes(&self, buffer: Vec<u8>) {
        drop(buffer);
    }

    fn deallocate_u16(&self, buffer: Vec<u16>) {
        drop(buffer);
    }
}

impl Drop for CAllocator {
    fn drop(&mut self) {
        if let Some(zfree) = self.zfree {
            for ptr in self.blocks.borrow().iter() {
                // SAFETY: each `ptr` was produced by the caller's `zalloc` with
                // this `opaque` and is freed exactly once here (the adapter is
                // dropped once, at `deflateEnd`/`inflateEnd`). This is the RAII
                // analogue of zlib freeing every `zalloc`'d block at teardown.
                unsafe { zfree(self.opaque, *ptr) };
            }
        }
    }
}

// ===========================================================================
// Tests
// ===========================================================================
//
// These unit tests are deliberately **engine-independent**: they exercise the
// pointer/slice, conversion, state-`Box`, guard, checksum, and string logic
// without constructing a real deflate/inflate `ZStream` (whose initialization
// lives in the core engine). The generic `Box` machinery is validated with a
// trivial dummy state, which proves the unsafe pointer lifecycle in isolation.
// The full `run_deflate`/`run_inflate` round-trip is validated separately, at
// the crate level, against the real engine.
#[cfg(test)]
mod tests {
    use super::*;

    // ---- Phase 1: slice helpers -------------------------------------------

    #[test]
    fn input_slice_null_or_zero_is_empty() {
        // SAFETY: a null pointer / zero length must never be dereferenced; the
        // helper returns an empty slice in both cases.
        let a = unsafe { input_slice(core::ptr::null(), 0) };
        assert!(a.is_empty());
        let data = [1u8, 2, 3, 4];
        // SAFETY: `data` is valid for reads of 4 bytes for the borrow's lifetime.
        let b = unsafe { input_slice(data.as_ptr(), 0) };
        assert!(b.is_empty());
        // SAFETY: null with a (bogus) non-zero len must still short-circuit to
        // empty without dereferencing.
        let c = unsafe { input_slice(core::ptr::null(), 8) };
        assert!(c.is_empty());
    }

    #[test]
    fn input_output_slice_round_trip() {
        let src = [10u8, 20, 30, 40, 50];
        // SAFETY: `src` is valid for reads of `src.len()` bytes.
        let view = unsafe { input_slice(src.as_ptr(), src.len() as c_uint) };
        assert_eq!(view, &src);

        let mut dst = [0u8; 5];
        let dst_ptr = dst.as_mut_ptr();
        // SAFETY: `dst` is valid for writes of 5 bytes and not aliased here.
        let out = unsafe { output_slice(dst_ptr, 5) };
        out.copy_from_slice(&src);
        assert_eq!(dst, src);

        // size_t variants
        // SAFETY: `src` valid for reads of `src.len()` bytes.
        let view_sz = unsafe { input_slice_sz(src.as_ptr(), src.len()) };
        assert_eq!(view_sz, &src);
    }

    #[test]
    fn output_slice_null_is_empty() {
        // SAFETY: null / zero must yield an empty mutable slice, never a deref.
        let out = unsafe { output_slice(core::ptr::null_mut(), 0) };
        assert!(out.is_empty());
    }

    // ---- Phase 2: conversions ---------------------------------------------

    #[test]
    fn to_c_int_maps_codes() {
        assert_eq!(to_c_int(Ok(ReturnCode::Ok)), 0);
        assert_eq!(to_c_int(Ok(ReturnCode::StreamEnd)), 1);
        assert_eq!(to_c_int(Ok(ReturnCode::NeedDict)), 2);
        assert_eq!(to_c_int(Err(ZlibError::ErrNo)), -1);
        assert_eq!(to_c_int(Err(ZlibError::StreamError)), -2);
        assert_eq!(to_c_int(Err(ZlibError::DataError)), -3);
        assert_eq!(to_c_int(Err(ZlibError::MemError)), -4);
        assert_eq!(to_c_int(Err(ZlibError::BufError)), -5);
        assert_eq!(to_c_int(Err(ZlibError::VersionError)), -6);
    }

    #[test]
    fn flush_strategy_level_conversions() {
        assert_eq!(flush_from_c(0), Some(Flush::NoFlush));
        assert_eq!(flush_from_c(4), Some(Flush::Finish));
        assert_eq!(flush_from_c(6), Some(Flush::Trees));
        assert_eq!(flush_from_c(7), None);
        assert_eq!(flush_from_c(-1), None);

        assert_eq!(strategy_from_c(0), Some(Strategy::Default));
        assert_eq!(strategy_from_c(4), Some(Strategy::Fixed));
        assert_eq!(strategy_from_c(5), None);

        assert!(level_from_c(-1).is_some());
        assert!(level_from_c(0).is_some());
        assert!(level_from_c(9).is_some());
        assert!(level_from_c(10).is_none());
        assert!(level_from_c(-2).is_none());
    }

    // ---- Phase 3: generic state-Box machinery (dummy type) ----------------

    struct DummyState {
        tag: u64,
    }

    #[test]
    fn state_box_lifecycle() {
        let mut zs = z_stream::default();
        let p: z_streamp = &mut zs;

        // No state attached yet -> recover returns None, drop returns false.
        // SAFETY: `p` is a valid pointer to a zeroed `z_stream`.
        assert!(unsafe { recover_raw::<DummyState>(p) }.is_none());
        // SAFETY: `p` valid; `state` is null so this is the double-free guard.
        assert!(!unsafe { drop_raw::<DummyState>(p) });

        // Attach, recover, mutate.
        // SAFETY: `p` is valid and freshly detached (state null).
        unsafe { attach_raw(p, Box::new(DummyState { tag: 7 })) };
        // SAFETY: `state` now holds a `Box<DummyState>` we just attached.
        let st = unsafe { recover_raw::<DummyState>(p) }.expect("state present");
        assert_eq!(st.tag, 7);
        st.tag = 42;

        // Recover again sees the mutation (same allocation).
        // SAFETY: same live `Box<DummyState>`.
        let st2 = unsafe { recover_raw::<DummyState>(p) }.expect("state present");
        assert_eq!(st2.tag, 42);

        // Drop releases it and nulls `state`; a second drop is a no-op.
        // SAFETY: `state` holds the live `Box<DummyState>` from `attach_raw`.
        assert!(unsafe { drop_raw::<DummyState>(p) });
        // SAFETY: `state` is now null.
        assert!(unsafe { recover_raw::<DummyState>(p) }.is_none());
        // SAFETY: `state` is null -> double-free guard returns false.
        assert!(!unsafe { drop_raw::<DummyState>(p) });
    }

    #[test]
    fn recover_and_drop_reject_null_stream() {
        let null: z_streamp = core::ptr::null_mut();
        // SAFETY: a null `strm` must short-circuit without any dereference.
        assert!(unsafe { recover_raw::<DummyState>(null) }.is_none());
        // SAFETY: null `strm` -> false, no dereference.
        assert!(!unsafe { drop_raw::<DummyState>(null) });
    }

    // ---- Phase 5: guards ---------------------------------------------------

    #[test]
    fn check_stream_guards_null() {
        // null strm -> Z_STREAM_ERROR
        let null: z_streamp = core::ptr::null_mut();
        // SAFETY: null `strm` is handled without dereference.
        assert_eq!(unsafe { check_stream(null) }, Err(-2));

        // non-null strm but null state -> Z_STREAM_ERROR
        let mut zs = z_stream::default();
        let p: z_streamp = &mut zs;
        // SAFETY: `p` valid; `state` is null.
        assert_eq!(unsafe { check_stream(p) }, Err(-2));

        // attach a dummy state -> Ok
        // SAFETY: `p` valid and detached.
        unsafe { attach_raw(p, Box::new(DummyState { tag: 0 })) };
        // SAFETY: `p` valid with non-null state.
        assert_eq!(unsafe { check_stream(p) }, Ok(()));
        // clean up to avoid a leak in the test.
        // SAFETY: `state` holds the live `Box<DummyState>`.
        assert!(unsafe { drop_raw::<DummyState>(p) });
    }

    #[test]
    fn check_version_matches_and_rejects() {
        let good = b"1.3.2.1-motley\0";
        let size = core::mem::size_of::<z_stream>() as c_int;
        // SAFETY: `good` is a valid NUL-terminated string.
        assert_eq!(
            unsafe { check_version(good.as_ptr() as *const c_char, size) },
            Ok(())
        );

        // Only the first version byte matters; a matching major still passes.
        let same_major = b"1.9.9\0";
        // SAFETY: valid NUL-terminated string.
        assert_eq!(
            unsafe { check_version(same_major.as_ptr() as *const c_char, size) },
            Ok(())
        );

        // Wrong major version -> Z_VERSION_ERROR (-6).
        let bad_major = b"2.0\0";
        // SAFETY: valid NUL-terminated string.
        assert_eq!(
            unsafe { check_version(bad_major.as_ptr() as *const c_char, size) },
            Err(-6)
        );

        // Wrong struct size -> Z_VERSION_ERROR (-6).
        // SAFETY: valid string; size deliberately wrong.
        assert_eq!(
            unsafe { check_version(good.as_ptr() as *const c_char, size - 1) },
            Err(-6)
        );

        // Null version -> Z_VERSION_ERROR (-6).
        // SAFETY: null is handled without dereference.
        assert_eq!(unsafe { check_version(core::ptr::null(), size) }, Err(-6));
    }

    // ---- Phase 6: checksums & one-shot ------------------------------------

    #[test]
    fn checksum_null_buf_rule() {
        // adler32(_, NULL, _) == 1 ; crc32(_, NULL, _) == 0, regardless of the
        // running value supplied.
        // SAFETY: null buf must short-circuit to the initial value.
        assert_eq!(unsafe { adler32_ffi(0xDEAD, core::ptr::null(), 5) }, 1);
        // SAFETY: null buf short-circuits.
        assert_eq!(unsafe { crc32_ffi(0xDEAD, core::ptr::null(), 5) }, 0);
        // SAFETY: null buf short-circuits (size_t variants).
        assert_eq!(unsafe { adler32_z_ffi(0xDEAD, core::ptr::null(), 5) }, 1);
        // SAFETY: null buf short-circuits.
        assert_eq!(unsafe { crc32_z_ffi(0xDEAD, core::ptr::null(), 5) }, 0);
    }

    #[test]
    fn checksum_known_answers() {
        let data = b"123456789";
        // Canonical CRC-32 (IEEE) check value for "123456789".
        // SAFETY: `data` valid for reads of 9 bytes.
        let crc = unsafe { crc32_ffi(0, data.as_ptr(), data.len() as c_uint) };
        assert_eq!(crc, 0xCBF4_3926);

        // adler32_ffi must agree with the core over the same bytes.
        // SAFETY: `data` valid for reads of 9 bytes.
        let adl = unsafe { adler32_ffi(1, data.as_ptr(), data.len() as c_uint) };
        assert_eq!(adl as u32, zlib_rs::checksum::adler32(1, data));
    }

    #[test]
    fn off_to_len_and_combine_negatives() {
        assert_eq!(off_to_len(-1), None);
        assert_eq!(off_to_len(0), Some(0));
        assert_eq!(off_to_len(123), Some(123));

        // Negative length sentinels.
        assert_eq!(adler32_combine_ffi(1, 1, -1), 0xFFFF_FFFF);
        assert_eq!(crc32_combine_ffi(0, 0, -1), 0);
        assert_eq!(crc32_combine_gen_ffi(-1), 0);
    }

    #[test]
    fn crc_table_ptr_is_valid() {
        let table = get_crc_table_ptr();
        assert!(!table.is_null());
        // The IEEE CRC-32 table's first entry is 0.
        // SAFETY: `table` points to a `'static [u32; 256]`; index 0 is in-bounds.
        assert_eq!(unsafe { *table }, 0);
    }

    #[test]
    fn dest_len_read_write() {
        let mut len: c_ulong = 4096;
        let p = core::ptr::addr_of_mut!(len);
        // SAFETY: `p` is a valid pointer to a writable `c_ulong`.
        assert_eq!(unsafe { read_dest_len(p) }, 4096);
        // SAFETY: `p` is a valid, writable pointer.
        unsafe { write_dest_len(p, 123) };
        assert_eq!(len, 123);
    }

    // ---- Phase 7: static C strings ----------------------------------------

    #[test]
    fn version_cstr_is_terminated_and_matches() {
        assert_eq!(*VERSION_CSTR.last().unwrap(), 0);
        assert_eq!(
            &VERSION_CSTR[..VERSION_CSTR.len() - 1],
            ZLIB_VERSION.as_bytes()
        );
        let p = version_ptr();
        assert!(!p.is_null());
        // SAFETY: `p` points to the `'static` NUL-terminated `VERSION_CSTR`.
        assert_eq!(unsafe { *p } as u8, b'1');
    }

    #[test]
    fn z_error_cstr_matches_err_msg() {
        // Every code in -6..=2 must be non-null, NUL-terminated, and equal to
        // the core's `err_msg` text.
        for code in -6..=2 {
            let p = z_error_cstr(code);
            assert!(!p.is_null());
            // SAFETY: `p` points to a `'static` NUL-terminated table entry.
            let bytes = unsafe { core::ffi::CStr::from_ptr(p) }.to_bytes();
            assert_eq!(bytes, zlib_rs::error::err_msg(code).as_bytes());
        }
        // Out-of-range codes map to the empty string.
        let p = z_error_cstr(42);
        // SAFETY: valid `'static` NUL-terminated pointer.
        assert!(
            unsafe { core::ffi::CStr::from_ptr(p) }
                .to_bytes()
                .is_empty()
        );
    }

    // ---- Phase 8: gz conversions (feature-gated) --------------------------

    #[cfg(feature = "gz-io")]
    #[test]
    fn off_t_round_trip() {
        assert_eq!(off_t_to_i64(i64_to_off_t(0)), 0);
        assert_eq!(off_t_to_i64(i64_to_off_t(123_456)), 123_456);
        assert_eq!(off_t_to_i64(i64_to_off_t(-1)), -1);
    }

    #[cfg(feature = "gz-io")]
    struct DummyGz {
        // first three fields mirror `gzFile_s` layout; unused in this test but
        // present to document the layout contract.
        _have: c_uint,
        _next: *mut u8,
        _pos: i64,
        tag: u64,
    }

    #[cfg(feature = "gz-io")]
    #[test]
    fn gz_state_box_lifecycle() {
        let boxed = Box::new(DummyGz {
            _have: 0,
            _next: core::ptr::null_mut(),
            _pos: 0,
            tag: 5,
        });
        let file = gzfile_from_state(boxed);
        assert!(!file.is_null());
        // SAFETY: `file` was produced by `gzfile_from_state::<DummyGz>`.
        let st = unsafe { gz_state_mut::<DummyGz>(file) }.expect("present");
        assert_eq!(st.tag, 5);
        st.tag = 11;
        // SAFETY: same live `Box<DummyGz>`.
        assert_eq!(unsafe { gz_state_mut::<DummyGz>(file) }.unwrap().tag, 11);
        // SAFETY: `file` denotes the live `Box<DummyGz>`.
        assert!(unsafe { drop_gzfile::<DummyGz>(file) });

        // Null handle is rejected.
        let null: gzFile = core::ptr::null_mut();
        // SAFETY: null handle handled without dereference.
        assert!(unsafe { gz_state_mut::<DummyGz>(null) }.is_none());
        // SAFETY: null handle -> false.
        assert!(!unsafe { drop_gzfile::<DummyGz>(null) });
    }

    #[cfg(feature = "gz-io")]
    #[test]
    fn gz_write_fmt_renders_and_counts() {
        let mut captured: std::vec::Vec<u8> = std::vec::Vec::new();
        let n = gz_write_fmt(format_args!("x={}", 42), |bytes| {
            captured.extend_from_slice(bytes);
            bytes.len() as c_int
        });
        assert_eq!(n, 4);
        assert_eq!(captured, b"x=42");
    }
}
