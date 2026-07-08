//! `extern "C"` shims for the zlib **deflate** (compression) API.
//!
//! This module is the crate's designated `unsafe` boundary for compression.
//! Each function reproduces the exact C signature of its zlib counterpart so
//! the emitted `cdylib`/`staticlib` is a drop-in replacement for the reference
//! library. The shims validate raw C inputs, bridge the `#[repr(C)]`
//! [`z_stream`] to the fully **safe** engine in [`crate::deflate`], own the
//! boxed idiomatic state through the opaque `z_stream.state` handle, and
//! re-materialize zlib's integer return codes at the boundary.
//!
//! ## Safe-core / unsafe-boundary split
//!
//! The compression engine (`src/deflate/**`) contains **zero** `unsafe`. All
//! raw-pointer handling lives here: every `unsafe` block carries a `// SAFETY:`
//! justification, and every fallible body is wrapped in
//! [`crate::ffi::types::guard_int`] / [`crate::ffi::types::guard_ulong`]
//! (`catch_unwind`) so that a Rust panic can never unwind across the C ABI.
//!
//! ## State-handle model
//!
//! `z_stream.state` stores a `Box<ZStream<CAllocator>>`. Initialization boxes a
//! freshly built [`ZStream`] (honoring any caller-supplied `zalloc`/`zfree`) and
//! installs it via [`state_ptr_from_box`]. Every subsequent call reborrows the
//! handle with [`state_ref`]; `deflateEnd` reclaims it with [`state_take`] and
//! drops it, letting RAII run the engine cleanup that C performed manually in
//! `deflateEnd`. `deflateCopy` deep-clones the engine into a fresh box.

// zlib's public symbols are camelCase C identifiers; `#[unsafe(no_mangle)]`
// exempts them from `non_snake_case`, but this keeps the module warning-free
// regardless of how the exported names are spelled.
#![allow(non_snake_case)]

use core::ffi::{c_char, c_int, c_uint};
use core::{ptr, slice};

use alloc::boxed::Box;

use crate::constants::{DEF_MEM_LEVEL, MAX_WBITS, Strategy, Z_DEFAULT_STRATEGY, Z_DEFLATED};
use crate::deflate as engine;
use crate::error::{ReturnCode, ZlibError};
use crate::ffi::types::*;
use crate::stream::ZStream;

// ===========================================================================
// Integer return codes (materialized locally from `ReturnCode`).
//
// `constants.rs` intentionally does not expose bare `Z_OK`/`Z_STREAM_ERROR`
// integer aliases, so the boundary defines exactly the ones it emits using the
// `const fn` `ReturnCode::as_c_int`.
// ===========================================================================

/// `Z_OK` — successful completion.
const Z_OK: c_int = ReturnCode::Ok.as_c_int();
/// `Z_STREAM_ERROR` — inconsistent stream state or invalid parameters.
const Z_STREAM_ERROR: c_int = ReturnCode::StreamError.as_c_int();
/// `Z_BUF_ERROR` — no progress was possible (e.g. output buffer too small).
const Z_BUF_ERROR: c_int = ReturnCode::BufError.as_c_int();
/// `Z_VERSION_ERROR` — header/library version or `z_stream` size mismatch.
const Z_VERSION_ERROR: c_int = ReturnCode::VersionError.as_c_int();

// ===========================================================================
// Boundary helpers
// ===========================================================================

/// Collapse an engine `Result<ReturnCode, ZlibError>` into the C integer code.
#[inline]
fn code_of(result: Result<ReturnCode, ZlibError>) -> c_int {
    match result {
        Ok(rc) => rc.as_c_int(),
        Err(err) => err.as_return_code().as_c_int(),
    }
}

/// Map an idiomatic stream's `msg` (`Option<&'static str>`) to a NUL-terminated
/// C string pointer, or `NULL` when there is no message.
///
/// The compression engine keeps `msg` clear on the deflate path, so this
/// resolves to `NULL` in practice; the mapping is retained so the raw
/// `z_stream.msg` faithfully mirrors any message the engine may set.
#[inline]
fn msg_ptr(zs: &ZStream<CAllocator>) -> *const c_char {
    match zs.msg {
        None => ptr::null(),
        Some(text) => cmsg(text),
    }
}

/// Translate a known zlib message string into a `'static` NUL-terminated C
/// pointer. Unknown strings map to `NULL` (never fabricates a dangling buffer).
#[inline]
fn cmsg(text: &str) -> *const c_char {
    match text {
        "need dictionary" => c"need dictionary".as_ptr(),
        "stream end" => c"stream end".as_ptr(),
        "file error" => c"file error".as_ptr(),
        "stream error" => c"stream error".as_ptr(),
        "data error" => c"data error".as_ptr(),
        "insufficient memory" => c"insufficient memory".as_ptr(),
        "buffer error" => c"buffer error".as_ptr(),
        "incompatible version" => c"incompatible version".as_ptr(),
        _ => ptr::null(),
    }
}

// ===========================================================================
// Phase 1 — Initialization shims
//
// The linkable symbols are the `_`-suffixed forms carrying `(version,
// stream_size)`. The bare `deflateInit`/`deflateInit2` names are C header
// macros and are deliberately NOT defined here.
// ===========================================================================

/// `int deflateInit2_(z_streamp strm, int level, int method, int windowBits,`
/// `int memLevel, int strategy, const char *version, int stream_size)`
///
/// Full-control initializer. Reproduces the C version guard
/// (`deflate.c` L392-396): a `NULL` version, a leading version byte other than
/// `'1'`, or a `stream_size` that disagrees with `sizeof(z_stream)` yields
/// `Z_VERSION_ERROR`.
///
/// # Safety
///
/// `strm` must be null or a valid, properly aligned pointer to a `z_stream`
/// that the caller owns exclusively for the duration of the call (typically a
/// zero-initialized stream). `version` must be null or point to a
/// NUL-terminated C string. On success the crate installs an owned state handle
/// into `strm.state` that must be released with [`deflateEnd`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn deflateInit2_(
    strm: z_streamp,
    level: c_int,
    method: c_int,
    window_bits: c_int,
    mem_level: c_int,
    strategy: c_int,
    version: *const c_char,
    stream_size: c_int,
) -> c_int {
    guard_int(Z_STREAM_ERROR, || {
        // SAFETY: `*version` is read only after the short-circuiting
        // `version.is_null()` check proves the pointer is non-null; the caller
        // guarantees it addresses a readable NUL-terminated C string, so the
        // first byte is valid to read.
        if version.is_null()
            || unsafe { *version } != b'1' as c_char
            || stream_size != core::mem::size_of::<z_stream>() as c_int
        {
            return Z_VERSION_ERROR;
        }
        if strm.is_null() {
            return Z_STREAM_ERROR;
        }
        // SAFETY: `strm` is non-null and, per the zlib contract, points at a
        // valid `z_stream` uniquely owned by the caller for this call.
        let s = unsafe { &mut *strm };

        // The engine takes a validated `Strategy` enum; an out-of-range value
        // is rejected exactly as C's `deflateInit2_` rejects it.
        let Some(strategy) = Strategy::from_c_int(strategy) else {
            return Z_STREAM_ERROR;
        };

        // Build the idiomatic stream, honoring any caller `zalloc`/`zfree`/
        // `opaque` (falls back to the global allocator when they are null).
        // SAFETY: `s` is a valid `&z_stream`; only its `Copy` allocator fields
        // are read.
        let mut zs = unsafe { zstream_with_caller_alloc(s) };

        if let Err(err) =
            engine::deflate_init2(&mut zs, level, method, window_bits, mem_level, strategy)
        {
            return err.as_return_code().as_c_int();
        }

        // Snapshot the engine's initial observable state before the stream is
        // moved into its box.
        let adler = zs.adler;
        let data_type = zs.data_type;
        let msg = msg_ptr(&zs);

        // Install the boxed state. C overwrites `strm->state` unconditionally;
        // callers must not re-init without `deflateEnd` (documented contract).
        // SAFETY: transfers ownership of the box into the opaque `state` handle;
        // it is reclaimed exactly once by `deflateEnd` via `state_take`.
        s.state = unsafe { state_ptr_from_box(Box::new(DeflateHandle::new(zs))) };
        s.total_in = 0;
        s.total_out = 0;
        set_msg(s, msg);
        set_data_type(s, data_type);
        set_adler(s, adler);
        Z_OK
    })
}

/// `int deflateInit_(z_streamp strm, int level, const char *version,`
/// `int stream_size)`
///
/// Convenience initializer; delegates to [`deflateInit2_`] with zlib's defaults
/// (`Z_DEFLATED`, `MAX_WBITS`, `DEF_MEM_LEVEL`, `Z_DEFAULT_STRATEGY`), exactly
/// as the C macro in `deflate.c` L379 does.
///
/// # Safety
///
/// Same contract as [`deflateInit2_`]: `strm` must be null or a valid,
/// exclusively-owned `z_stream`, and `version` must be null or a NUL-terminated
/// C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn deflateInit_(
    strm: z_streamp,
    level: c_int,
    version: *const c_char,
    stream_size: c_int,
) -> c_int {
    // SAFETY: forwards the raw inputs unchanged to `deflateInit2_`, which
    // performs the null/version validation before any dereference.
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

// ===========================================================================
// Phase 2 — Core driver
// ===========================================================================

/// `int deflate(z_streamp strm, int flush)`
///
/// Advances the compression state machine. All seven flush modes
/// (`Z_NO_FLUSH` … `Z_TREES`) are validated by the engine exactly as C does
/// (`flush < 0 || flush > Z_BLOCK` is rejected with `Z_STREAM_ERROR`). Returns
/// `Z_STREAM_END` once a `Z_FINISH` request completes and `Z_BUF_ERROR` when no
/// forward progress is possible. Input consumption and output production are
/// written back onto the raw `z_stream`, along with `adler`, `data_type`, and
/// `msg`.
///
/// # Safety
///
/// `strm` must be null or a valid `z_stream` previously initialized by
/// `deflateInit*` (its `state` handle unmodified by the caller). When
/// `avail_in` / `avail_out` are non-zero, `next_in` / `next_out` must address
/// that many valid readable / writable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn deflate(strm: z_streamp, flush: c_int) -> c_int {
    guard_int(Z_STREAM_ERROR, || {
        if strm.is_null() {
            return Z_STREAM_ERROR;
        }
        // SAFETY: `strm` is non-null and points at a valid, uniquely-owned
        // `z_stream` for the duration of this call.
        let s = unsafe { &mut *strm };

        // C `deflate` entry validation (`deflate.c` L981-L1010): a null
        // `next_out`, or a positive `avail_in` paired with a null `next_in`, is
        // a `Z_STREAM_ERROR`. This must run *before* the buffers are bridged so
        // the programmer error is surfaced rather than silently masked into an
        // empty slice by `input_slice`/`output_slice`. (The remaining C guards
        // — finish-state-with-wrong-flush and the `avail_out == 0` buffer error
        // — are already enforced inside the engine's `deflate` driver.)
        if !stream_buffers_valid(s) {
            return Z_STREAM_ERROR;
        }

        // Bridge the raw C buffers to slices. These carry detached lifetimes and
        // alias the caller's external buffers (never the `z_stream` struct), so
        // the subsequent `&mut` reborrow of `s` for `state_ref` is sound.
        // SAFETY: `next_in`/`avail_in` describe a readable input region.
        let input = unsafe { input_slice(s) };
        // SAFETY: `next_out`/`avail_out` describe a writable region disjoint
        // from `input`, per the zlib API contract.
        let output = unsafe { output_slice(s) };

        // Run the engine against the boxed state, then release the borrow before
        // writing observable fields back onto the raw stream.
        let (code, consumed, produced, adler, data_type, msg) = {
            // SAFETY: `state`, when non-null, was installed by `deflateInit*` as
            // a `Box<ZStream<CAllocator>>`.
            let Some(zs) = (unsafe { deflate_state(s) }) else {
                return Z_STREAM_ERROR;
            };
            let outcome = engine::deflate(zs, input, output, flush);
            (
                outcome.code.as_c_int(),
                outcome.consumed,
                outcome.produced,
                zs.adler,
                zs.data_type,
                msg_ptr(zs),
            )
        };

        // SAFETY: `consumed <= input.len() == avail_in` (engine invariant).
        unsafe { advance_input(s, consumed) };
        // SAFETY: `produced <= output.len() == avail_out` (engine invariant).
        unsafe { advance_output(s, produced) };
        set_adler(s, adler);
        set_data_type(s, data_type);
        set_msg(s, msg);
        code
    })
}

// ===========================================================================
// Phase 3 — Lifecycle: end / reset
// ===========================================================================

/// `int deflateEnd(z_streamp strm)`
///
/// Reclaims the boxed engine state and drops it (RAII replaces C's manual
/// `zcfree` cleanup). Mirrors C's return contract: `Z_DATA_ERROR` if the stream
/// was freed while still in a busy (non-finished) state, otherwise `Z_OK`.
///
/// # Safety
///
/// `strm` must be null or a valid `z_stream` initialized by `deflateInit*`. On
/// return the `state` handle has been reclaimed and `strm.state` is null, so it
/// must not be reused without re-initialization.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn deflateEnd(strm: z_streamp) -> c_int {
    guard_int(Z_STREAM_ERROR, || {
        if strm.is_null() {
            return Z_STREAM_ERROR;
        }
        // SAFETY: `strm` is non-null and valid for this call.
        let s = unsafe { &mut *strm };
        // SAFETY: `deflate_take` validates the handle's `HandleKind` tag BEFORE
        // reconstituting the box, so it returns the `Box<DeflateHandle>` only for
        // a genuine deflate handle (nulling `s.state` for exactly-once reclaim)
        // and `None` for a cross-type stream (e.g. one from `inflateInit*`) —
        // yielding `Z_STREAM_ERROR` here WITHOUT a layout-mismatched free
        // (FINDING-6). It matches C's `deflateEnd`, which rejects a non-deflate
        // stream with `Z_STREAM_ERROR`.
        let Some(mut boxed) = (unsafe { deflate_take(s) }) else {
            return Z_STREAM_ERROR;
        };
        // The engine computes the `Z_DATA_ERROR`-if-busy result and clears its
        // inner state; dropping `boxed` afterward frees the `ZStream`.
        code_of(engine::deflate_end(&mut boxed.zs))
    })
}

/// `int deflateReset(z_streamp strm)`
///
/// Resets the stream to the just-initialized state, preserving the compression
/// parameters. Observable counters, `adler`, `data_type`, and `msg` are
/// re-seeded on the raw stream.
///
/// # Safety
///
/// `strm` must be null or a valid `z_stream` initialized by `deflateInit*`
/// (its `state` handle unmodified by the caller).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn deflateReset(strm: z_streamp) -> c_int {
    guard_int(Z_STREAM_ERROR, || {
        if strm.is_null() {
            return Z_STREAM_ERROR;
        }
        // SAFETY: `strm` is non-null and valid for this call.
        let s = unsafe { &mut *strm };
        let (code, adler, data_type, msg) = {
            // SAFETY: installed engine state (see `deflate`).
            let Some(zs) = (unsafe { deflate_state(s) }) else {
                return Z_STREAM_ERROR;
            };
            let code = code_of(engine::deflate_reset(zs));
            (code, zs.adler, zs.data_type, msg_ptr(zs))
        };
        s.total_in = 0;
        s.total_out = 0;
        set_adler(s, adler);
        set_data_type(s, data_type);
        set_msg(s, msg);
        code
    })
}

/// `int deflateResetKeep(z_streamp strm)` (`ZLIB_1.2.5.2`)
///
/// Like [`deflateReset`] but keeps allocations and does not re-emit the wrapper
/// header. Mirrors the same observable fields C's `deflateResetKeep` touches.
///
/// # Safety
///
/// `strm` must be null or a valid `z_stream` initialized by `deflateInit*`
/// (its `state` handle unmodified by the caller).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn deflateResetKeep(strm: z_streamp) -> c_int {
    guard_int(Z_STREAM_ERROR, || {
        if strm.is_null() {
            return Z_STREAM_ERROR;
        }
        // SAFETY: `strm` is non-null and valid for this call.
        let s = unsafe { &mut *strm };
        let (code, adler, data_type, msg) = {
            // SAFETY: installed engine state (see `deflate`).
            let Some(zs) = (unsafe { deflate_state(s) }) else {
                return Z_STREAM_ERROR;
            };
            let code = code_of(engine::deflate_reset_keep(zs));
            (code, zs.adler, zs.data_type, msg_ptr(zs))
        };
        s.total_in = 0;
        s.total_out = 0;
        set_adler(s, adler);
        set_data_type(s, data_type);
        set_msg(s, msg);
        code
    })
}

// ===========================================================================
// Phase 4 — Parameter / tuning shims
// ===========================================================================

/// `int deflateParams(z_streamp strm, int level, int strategy)`
///
/// Dynamically changes the compression level and/or strategy. This may need to
/// flush pending output, so the input/output buffers are bridged around the
/// engine call; a partial flush surfaces as `Z_BUF_ERROR` just as in C.
///
/// # Safety
///
/// `strm` must be null or a valid `z_stream` initialized by `deflateInit*`.
/// When `avail_out` is non-zero, `next_out` must address that many writable
/// bytes (pending output may be flushed here).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn deflateParams(strm: z_streamp, level: c_int, strategy: c_int) -> c_int {
    guard_int(Z_STREAM_ERROR, || {
        if strm.is_null() {
            return Z_STREAM_ERROR;
        }
        // SAFETY: `strm` is non-null and valid for this call.
        let s = unsafe { &mut *strm };

        // An out-of-range strategy is rejected exactly as C rejects it.
        let Some(strategy) = Strategy::from_c_int(strategy) else {
            return Z_STREAM_ERROR;
        };

        // `deflateParams` may flush pending output through an internal
        // `deflate(strm, Z_BLOCK)` (see `engine::deflate_params`), and that is
        // precisely where C validates the raw buffers. Because the engine call
        // receives already-bridged slices — with a null-and-nonempty buffer
        // masked to empty by `input_slice`/`output_slice` — that masked case
        // would otherwise degrade to `Z_BUF_ERROR` instead of C's
        // `Z_STREAM_ERROR`. Surface the same entry check eagerly here (the zlib
        // manual permits `Z_STREAM_ERROR` for an "inconsistent stream state"):
        // reject a null `next_out`, or a positive `avail_in` with a null
        // `next_in`, before any bridging.
        if !stream_buffers_valid(s) {
            return Z_STREAM_ERROR;
        }

        // SAFETY: `next_in`/`avail_in` describe a readable input region.
        let input = unsafe { input_slice(s) };
        // SAFETY: `next_out`/`avail_out` describe a writable region disjoint
        // from `input`.
        let output = unsafe { output_slice(s) };

        let (code, consumed, produced, adler, data_type, msg) = {
            // SAFETY: installed engine state (see `deflate`).
            let Some(zs) = (unsafe { deflate_state(s) }) else {
                return Z_STREAM_ERROR;
            };
            let outcome = engine::deflate_params(zs, input, output, level, strategy);
            (
                outcome.code.as_c_int(),
                outcome.consumed,
                outcome.produced,
                zs.adler,
                zs.data_type,
                msg_ptr(zs),
            )
        };

        // SAFETY: `consumed <= avail_in` (engine invariant).
        unsafe { advance_input(s, consumed) };
        // SAFETY: `produced <= avail_out` (engine invariant).
        unsafe { advance_output(s, produced) };
        set_adler(s, adler);
        set_data_type(s, data_type);
        set_msg(s, msg);
        code
    })
}

/// `int deflateTune(z_streamp strm, int good_length, int max_lazy,`
/// `int nice_length, int max_chain)` (`ZLIB_1.2.2.3`)
///
/// Fine-tunes the internal match-finding thresholds for advanced use.
///
/// # Safety
///
/// `strm` must be null or a valid `z_stream` initialized by `deflateInit*`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn deflateTune(
    strm: z_streamp,
    good_length: c_int,
    max_lazy: c_int,
    nice_length: c_int,
    max_chain: c_int,
) -> c_int {
    guard_int(Z_STREAM_ERROR, || {
        if strm.is_null() {
            return Z_STREAM_ERROR;
        }
        // SAFETY: `strm` is non-null and valid for this call.
        let s = unsafe { &mut *strm };
        // SAFETY: installed engine state (see `deflate`).
        let Some(zs) = (unsafe { deflate_state(s) }) else {
            return Z_STREAM_ERROR;
        };
        code_of(engine::deflate_tune(
            zs,
            good_length,
            max_lazy,
            nice_length,
            max_chain,
        ))
    })
}

/// `uLong deflateBound(z_streamp strm, uLong sourceLen)`
///
/// Returns an upper bound on the compressed size of `sourceLen` input bytes.
/// Must never panic, even for a `NULL` stream or a stream without installed
/// state; in those cases the conservative worst-case bound (matching C's
/// uninitialized-stream path) is returned.
///
/// # Safety
///
/// `strm` must be null or a valid, properly aligned pointer to a `z_stream`
/// (initialized or freshly zeroed).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn deflateBound(strm: z_streamp, source_len: uLong) -> uLong {
    // Conservative bound computed from a throwaway stateless stream; used both
    // as the panic-guard default and when no engine state is installed.
    let stateless = engine::deflate_bound(&ZStream::new(), source_len as usize) as uLong;
    guard_ulong(stateless, || {
        if !strm.is_null() {
            // SAFETY: `strm` is non-null and valid for this call.
            let s = unsafe { &mut *strm };
            // SAFETY: `state`, when non-null, is a `Box<ZStream<CAllocator>>`.
            if let Some(zs) = unsafe { deflate_state(s) } {
                return engine::deflate_bound(zs, source_len as usize) as uLong;
            }
        }
        stateless
    })
}

/// `z_size_t deflateBound_z(z_streamp strm, z_size_t sourceLen)`
/// (`ZLIB_1.3.2`)
///
/// `size_t`-typed variant of [`deflateBound`]. The underlying computation is
/// pure saturating arithmetic and cannot panic, so no `catch_unwind` guard is
/// required.
///
/// # Safety
///
/// `strm` must be null or a valid, properly aligned pointer to a `z_stream`
/// (initialized or freshly zeroed).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn deflateBound_z(strm: z_streamp, source_len: z_size_t) -> z_size_t {
    if !strm.is_null() {
        // SAFETY: `strm` is non-null and valid for this call.
        let s = unsafe { &mut *strm };
        // SAFETY: `state`, when non-null, is a `Box<ZStream<CAllocator>>`.
        if let Some(zs) = unsafe { deflate_state(s) } {
            return engine::deflate_bound_z(zs, source_len);
        }
    }
    engine::deflate_bound_z(&ZStream::new(), source_len)
}

/// `int deflatePending(z_streamp strm, unsigned *pending, int *bits)`
/// (`ZLIB_1.2.5.1`)
///
/// Reports the number of bytes and bits of pending output not yet emitted.
/// Either output pointer may be `NULL`. Mirrors C's ordering (writes `*bits`
/// first) and its truncation guard: if the pending byte count does not fit in a
/// C `unsigned`, `*pending` is set to `UINT_MAX` and `Z_BUF_ERROR` is returned.
///
/// # Safety
///
/// `strm` must be null or a valid `z_stream` initialized by `deflateInit*`.
/// `pending` and `bits` must each be null or a valid, writable pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn deflatePending(
    strm: z_streamp,
    pending: *mut c_uint,
    bits: *mut c_int,
) -> c_int {
    guard_int(Z_STREAM_ERROR, || {
        if strm.is_null() {
            return Z_STREAM_ERROR;
        }
        // SAFETY: `strm` is non-null and valid for this call.
        let s = unsafe { &mut *strm };
        let (pending_val, bits_val) = {
            // SAFETY: installed engine state (see `deflate`).
            let Some(zs) = (unsafe { deflate_state(s) }) else {
                return Z_STREAM_ERROR;
            };
            match engine::deflate_pending(zs) {
                Ok(pair) => pair,
                Err(err) => return err.as_return_code().as_c_int(),
            }
        };

        // C writes `*bits` before `*pending`.
        if !bits.is_null() {
            // SAFETY: caller passed a valid, writable `*mut c_int`.
            unsafe { *bits = bits_val };
        }
        if !pending.is_null() {
            let truncated = pending_val as c_uint;
            if truncated as usize != pending_val {
                // SAFETY: caller passed a valid, writable `*mut c_uint`.
                unsafe { *pending = c_uint::MAX };
                return Z_BUF_ERROR;
            }
            // SAFETY: caller passed a valid, writable `*mut c_uint`.
            unsafe { *pending = truncated };
        }
        Z_OK
    })
}

/// `int deflateUsed(z_streamp strm, int *bits)` (`ZLIB_1.3.1.2`)
///
/// Reports the number of bits (0-8) of the last output byte that are still in
/// use. The output pointer may be `NULL`.
///
/// # Safety
///
/// `strm` must be null or a valid `z_stream` initialized by `deflateInit*`.
/// `bits` must be null or a valid, writable pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn deflateUsed(strm: z_streamp, bits: *mut c_int) -> c_int {
    guard_int(Z_STREAM_ERROR, || {
        if strm.is_null() {
            return Z_STREAM_ERROR;
        }
        // SAFETY: `strm` is non-null and valid for this call.
        let s = unsafe { &mut *strm };
        let bits_val = {
            // SAFETY: installed engine state (see `deflate`).
            let Some(zs) = (unsafe { deflate_state(s) }) else {
                return Z_STREAM_ERROR;
            };
            match engine::deflate_used(zs) {
                Ok(v) => v,
                Err(err) => return err.as_return_code().as_c_int(),
            }
        };
        if !bits.is_null() {
            // SAFETY: caller passed a valid, writable `*mut c_int`.
            unsafe { *bits = bits_val };
        }
        Z_OK
    })
}

/// `int deflatePrime(z_streamp strm, int bits, int value)` (`ZLIB_1.2.0.8`)
///
/// Inserts `bits` bits of `value` into the output bit stream ahead of the next
/// compressed data — used to align or splice streams.
///
/// # Safety
///
/// `strm` must be null or a valid `z_stream` initialized by `deflateInit*`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn deflatePrime(strm: z_streamp, bits: c_int, value: c_int) -> c_int {
    guard_int(Z_STREAM_ERROR, || {
        if strm.is_null() {
            return Z_STREAM_ERROR;
        }
        // SAFETY: `strm` is non-null and valid for this call.
        let s = unsafe { &mut *strm };
        // SAFETY: installed engine state (see `deflate`).
        let Some(zs) = (unsafe { deflate_state(s) }) else {
            return Z_STREAM_ERROR;
        };
        code_of(engine::deflate_prime(zs, bits, value))
    })
}

// ===========================================================================
// Phase 5 — Dictionary & header shims
// ===========================================================================

/// `int deflateSetDictionary(z_streamp strm, const Bytef *dictionary,`
/// `uInt dictLength)`
///
/// Initializes the compression dictionary from the given byte sequence. The
/// dictionary is folded into the running Adler-32, which is written back to the
/// raw stream.
///
/// # Safety
///
/// `strm` must be null or a valid `z_stream` initialized by `deflateInit*`.
/// `dictionary` must be null or address at least `dict_length` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn deflateSetDictionary(
    strm: z_streamp,
    dictionary: *const Bytef,
    dict_length: uInt,
) -> c_int {
    guard_int(Z_STREAM_ERROR, || {
        if strm.is_null() {
            return Z_STREAM_ERROR;
        }
        // SAFETY: `strm` is non-null and valid for this call.
        let s = unsafe { &mut *strm };

        // A null dictionary is a usage error: reference C's
        // `deflateSetDictionary` returns `Z_STREAM_ERROR` for a `Z_NULL`
        // dictionary regardless of length (FINDING-4), rather than silently
        // treating it as empty — which would mask a caller bug, since the
        // intended dictionary would never actually be set. Match C exactly.
        if dictionary.is_null() {
            return Z_STREAM_ERROR;
        }
        // A non-null dictionary of length 0 is a valid empty dictionary; view
        // `dict_length` bytes at `dictionary` otherwise.
        let dict: &[u8] = if dict_length == 0 {
            &[]
        } else {
            // SAFETY: the caller guarantees `dict_length` readable bytes at
            // `dictionary` (`Bytef` is `u8`).
            unsafe { slice::from_raw_parts(dictionary, dict_length as usize) }
        };

        let (code, adler) = {
            // SAFETY: installed engine state (see `deflate`).
            let Some(zs) = (unsafe { deflate_state(s) }) else {
                return Z_STREAM_ERROR;
            };
            let code = code_of(engine::deflate_set_dictionary(zs, dict));
            (code, zs.adler)
        };
        set_adler(s, adler);
        code
    })
}

/// `int deflateGetDictionary(z_streamp strm, Bytef *dictionary,`
/// `uInt *dictLength)` (`ZLIB_1.2.9`)
///
/// Retrieves the sliding-window contents currently used as the dictionary.
/// Both output pointers may be `NULL`: passing a null `dictionary` performs a
/// length-only query. Implemented with the engine's two-call pattern (query the
/// length, then copy into a caller buffer of that length).
///
/// # Safety
///
/// `strm` must be null or a valid `z_stream` initialized by `deflateInit*`.
/// If `dictionary` is non-null it must address at least the returned dictionary
/// length in writable bytes (never more than 32 KiB). `dict_length` must be
/// null or a valid, writable pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn deflateGetDictionary(
    strm: z_streamp,
    dictionary: *mut Bytef,
    dict_length: *mut uInt,
) -> c_int {
    guard_int(Z_STREAM_ERROR, || {
        if strm.is_null() {
            return Z_STREAM_ERROR;
        }
        // SAFETY: `strm` is non-null and valid for this call.
        let s = unsafe { &mut *strm };
        // SAFETY: installed engine state (see `deflate`).
        let Some(zs) = (unsafe { deflate_state(s) }) else {
            return Z_STREAM_ERROR;
        };

        // First, query the available dictionary length.
        let len = match engine::deflate_get_dictionary(zs, None) {
            Ok(l) => l,
            Err(err) => return err.as_return_code().as_c_int(),
        };

        // Then copy the bytes out if the caller supplied a destination buffer.
        if !dictionary.is_null() && len != 0 {
            // SAFETY: the zlib contract requires `dictionary` to address at
            // least `len` writable bytes (the window is at most 32 KiB).
            let dst = unsafe { slice::from_raw_parts_mut(dictionary, len) };
            if let Err(err) = engine::deflate_get_dictionary(zs, Some(dst)) {
                return err.as_return_code().as_c_int();
            }
        }
        if !dict_length.is_null() {
            // SAFETY: caller passed a valid, writable `*mut uInt`.
            unsafe { *dict_length = len as uInt };
        }
        Z_OK
    })
}

/// `int deflateSetHeader(z_streamp strm, gz_headerp head)` (`ZLIB_1.2.2`)
///
/// Supplies the gzip header for a gzip-wrapped stream. Returns `Z_STREAM_ERROR`
/// unless the stream is in gzip mode (`wrap == 2`), mirroring C. C stores the
/// caller's pointer; this shim deep-copies the header fields into an owned
/// [`GzHeader`], so no dangling pointer can be read during later `deflate`
/// calls. A `NULL` header clears any previously set header (restoring the
/// default), exactly as passing `Z_NULL` does in C.
///
/// # Safety
///
/// `strm` must be null or a valid `z_stream` initialized by `deflateInit*`.
/// `head` must be null or point to a valid `gz_header` whose field-declared
/// buffers remain valid for the duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn deflateSetHeader(strm: z_streamp, head: gz_headerp) -> c_int {
    guard_int(Z_STREAM_ERROR, || {
        if strm.is_null() {
            return Z_STREAM_ERROR;
        }

        #[cfg(feature = "gzip")]
        {
            // SAFETY: `strm` is non-null and valid for this call.
            let s = unsafe { &mut *strm };
            // Validate installed state before touching `head` (C order:
            // `deflateStateCheck` precedes the `wrap` test).
            // SAFETY: installed engine state (see `deflate`).
            let Some(zs) = (unsafe { deflate_state(s) }) else {
                return Z_STREAM_ERROR;
            };
            // SAFETY: `head` is null or points at a valid `gz_header` whose
            // field-declared buffers remain valid for this call.
            let header = unsafe { gz_header_to_idiomatic(head) };
            code_of(engine::deflate_set_header(zs, header))
        }
        #[cfg(not(feature = "gzip"))]
        {
            // Without gzip framing no stream can ever be `wrap == 2`, so C's
            // contract (`wrap != 2 -> Z_STREAM_ERROR`) is unconditional here.
            let _ = head;
            Z_STREAM_ERROR
        }
    })
}

// ===========================================================================
// Phase 6 — Copy
// ===========================================================================

/// `int deflateCopy(z_streamp dest, z_streamp source)`
///
/// Duplicates a compression stream, including its sliding window, pending
/// buffer, and hash tables (the engine's `Clone` performs the deep copy). All
/// observable `z_stream` fields are mirrored from `source` into `dest`, matching
/// C's full-struct copy. Returns `Z_MEM_ERROR` if the clone allocation fails.
///
/// # Safety
///
/// `dest` and `source` must each be null or a valid, exclusively-owned
/// `z_stream`, and must not alias one another. `source` must have been
/// initialized by `deflateInit*`. On success `dest` receives its own owned
/// state handle that must be released with [`deflateEnd`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn deflateCopy(dest: z_streamp, source: z_streamp) -> c_int {
    guard_int(Z_STREAM_ERROR, || {
        if dest.is_null() || source.is_null() {
            return Z_STREAM_ERROR;
        }
        // SAFETY: both pointers are non-null and, per the zlib contract, refer
        // to distinct (non-aliasing) valid streams for this call.
        let d = unsafe { &mut *dest };
        // SAFETY: see above; `source` is distinct from `dest`.
        let src = unsafe { &mut *source };

        // Deep-copy the source engine into a fresh stream carrying the same
        // allocator, then install it into `dest`.
        let new_zs = {
            // SAFETY: `source.state`, when non-null, is a
            // `Box<ZStream<CAllocator>>`.
            let Some(src_zs) = (unsafe { deflate_state(src) }) else {
                return Z_STREAM_ERROR;
            };
            let mut new_zs = ZStream::with_allocator(*src_zs.allocator());
            if let Err(err) = engine::deflate_copy(&mut new_zs, src_zs) {
                return err.as_return_code().as_c_int();
            }
            new_zs
        };

        // Install the cloned state into `dest`.
        // SAFETY: transfers ownership into `dest.state`; reclaimed once by
        // `deflateEnd`.
        d.state = unsafe { state_ptr_from_box(Box::new(DeflateHandle::new(new_zs))) };

        // Mirror the full observable `z_stream` (C copies the whole struct).
        d.next_in = src.next_in;
        d.avail_in = src.avail_in;
        d.total_in = src.total_in;
        d.next_out = src.next_out;
        d.avail_out = src.avail_out;
        d.total_out = src.total_out;
        d.msg = src.msg;
        d.data_type = src.data_type;
        d.adler = src.adler;
        d.zalloc = src.zalloc;
        d.zfree = src.zfree;
        d.opaque = src.opaque;
        d.reserved = src.reserved;
        Z_OK
    })
}

// ===========================================================================
// Phase 8 — Tests
//
// Exercised with the default feature set (`std` + `gzip` + `simd`), so the std
// prelude and the `flate2` dev-dependency (pure-Rust `miniz_oxide` backend) are
// available for byte-identity interop checks.
// ===========================================================================
#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::{Z_DEFAULT_COMPRESSION, Z_FINISH, Z_NO_FLUSH};
    use core::mem::size_of;
    use std::io::Read;
    use std::vec::Vec;

    /// `Z_STREAM_END` — only referenced by the tests.
    const Z_STREAM_END: c_int = ReturnCode::StreamEnd.as_c_int();

    /// A valid version pointer (only the leading byte is checked by zlib).
    fn ver() -> *const c_char {
        c"1.3.2.1-motley".as_ptr()
    }

    /// A `memset(0)` `z_stream`, matching how a C caller presents a fresh stream.
    fn zeroed_stream() -> z_stream {
        // SAFETY: `z_stream` is a `#[repr(C)]` aggregate of raw pointers, nullable
        // `Option<fn>` callbacks, and integers; the all-zero bit pattern is the
        // valid "uninitialized" state (null pointers, `None` callbacks, zero
        // counters) that C code produces with `memset`.
        unsafe { core::mem::zeroed() }
    }

    /// zlib-decompress `data` (RFC 1950 wrapper) and return the bytes.
    fn zlib_inflate(data: &[u8]) -> Vec<u8> {
        let mut dec = flate2::read::ZlibDecoder::new(data);
        let mut out = Vec::new();
        dec.read_to_end(&mut out).expect("zlib inflate failed");
        out
    }

    #[test]
    fn init_compress_end_roundtrip() {
        let mut strm = zeroed_stream();
        let rc = unsafe {
            deflateInit_(
                &mut strm,
                Z_DEFAULT_COMPRESSION,
                ver(),
                size_of::<z_stream>() as c_int,
            )
        };
        assert_eq!(rc, Z_OK);

        let input = b"Hello, hello, hello! zlib-rs FFI deflate round-trip. ".repeat(8);
        let mut output = std::vec![0u8; 1024];
        strm.next_in = input.as_ptr();
        strm.avail_in = input.len() as c_uint;
        strm.next_out = output.as_mut_ptr();
        strm.avail_out = output.len() as c_uint;

        loop {
            let rc = unsafe { deflate(&mut strm, Z_FINISH) };
            if rc == Z_STREAM_END {
                break;
            }
            assert_eq!(rc, Z_OK, "unexpected deflate return code");
            assert!(strm.avail_out > 0, "ran out of output space unexpectedly");
        }

        let produced = strm.total_out as usize;
        assert!(produced > 0, "expected some compressed output");
        assert_eq!(strm.total_in as usize, input.len(), "all input consumed");

        assert_eq!(unsafe { deflateEnd(&mut strm) }, Z_OK);
        assert!(
            strm.state.is_null(),
            "state must be reclaimed by deflateEnd"
        );

        assert_eq!(zlib_inflate(&output[..produced]), input);
    }

    #[test]
    fn streaming_small_output_buffer() {
        let mut strm = zeroed_stream();
        assert_eq!(
            unsafe { deflateInit_(&mut strm, 9, ver(), size_of::<z_stream>() as c_int) },
            Z_OK
        );

        let input = b"streaming with a tiny output buffer forces many deflate calls. ".repeat(20);
        strm.next_in = input.as_ptr();
        strm.avail_in = input.len() as c_uint;

        let mut compressed = Vec::new();
        let mut chunk = [0u8; 16];
        loop {
            strm.next_out = chunk.as_mut_ptr();
            strm.avail_out = chunk.len() as c_uint;
            let rc = unsafe { deflate(&mut strm, Z_FINISH) };
            let produced = chunk.len() - strm.avail_out as usize;
            compressed.extend_from_slice(&chunk[..produced]);
            if rc == Z_STREAM_END {
                break;
            }
            assert_eq!(rc, Z_OK, "unexpected return code during streaming");
        }
        assert_eq!(unsafe { deflateEnd(&mut strm) }, Z_OK);

        assert_eq!(zlib_inflate(&compressed), input);
    }

    #[test]
    fn version_mismatch_rejected() {
        let mut strm = zeroed_stream();
        // Wrong leading version byte.
        assert_eq!(
            unsafe {
                deflateInit_(
                    &mut strm,
                    6,
                    c"9.9".as_ptr(),
                    size_of::<z_stream>() as c_int,
                )
            },
            Z_VERSION_ERROR
        );
        // Null version pointer.
        assert_eq!(
            unsafe { deflateInit_(&mut strm, 6, ptr::null(), size_of::<z_stream>() as c_int) },
            Z_VERSION_ERROR
        );
        // Wrong stream size.
        assert_eq!(
            unsafe { deflateInit_(&mut strm, 6, ver(), size_of::<z_stream>() as c_int - 1) },
            Z_VERSION_ERROR
        );
        assert!(strm.state.is_null(), "no state after a rejected init");
    }

    #[test]
    fn null_and_stateless_streams_are_stream_error() {
        assert_eq!(
            unsafe { deflate(ptr::null_mut(), Z_NO_FLUSH) },
            Z_STREAM_ERROR
        );
        assert_eq!(unsafe { deflateEnd(ptr::null_mut()) }, Z_STREAM_ERROR);
        assert_eq!(unsafe { deflateReset(ptr::null_mut()) }, Z_STREAM_ERROR);

        // Non-null stream but no installed state.
        let mut strm = zeroed_stream();
        assert_eq!(unsafe { deflate(&mut strm, Z_NO_FLUSH) }, Z_STREAM_ERROR);
        assert_eq!(unsafe { deflateEnd(&mut strm) }, Z_STREAM_ERROR);
    }

    #[test]
    fn deflate_rejects_invalid_raw_buffers() {
        // C `deflate` entry validation (`deflate.c` L981-L1010): a null
        // `next_out`, or a positive `avail_in` paired with a null `next_in`, is
        // `Z_STREAM_ERROR`. An *initialized* stream is used so the rejection is
        // attributable to buffer validation rather than the state check.
        let mut strm = zeroed_stream();
        assert_eq!(
            unsafe { deflateInit_(&mut strm, 6, ver(), size_of::<z_stream>() as c_int) },
            Z_OK
        );

        let input = b"payload for null-buffer validation";
        let mut output = std::vec![0u8; 128];

        // Null `next_out` with nonzero `avail_out` -> Z_STREAM_ERROR (C rejects a
        // null output pointer unconditionally).
        strm.next_in = input.as_ptr();
        strm.avail_in = input.len() as c_uint;
        strm.next_out = ptr::null_mut();
        strm.avail_out = output.len() as c_uint;
        assert_eq!(unsafe { deflate(&mut strm, Z_NO_FLUSH) }, Z_STREAM_ERROR);

        // Nonzero `avail_in` with null `next_in` -> Z_STREAM_ERROR.
        strm.next_in = ptr::null();
        strm.avail_in = input.len() as c_uint;
        strm.next_out = output.as_mut_ptr();
        strm.avail_out = output.len() as c_uint;
        assert_eq!(unsafe { deflate(&mut strm, Z_NO_FLUSH) }, Z_STREAM_ERROR);

        // A fully valid buffer pair is accepted (proves the guard is not
        // over-broad): the guard lets a normal call through.
        strm.next_in = input.as_ptr();
        strm.avail_in = input.len() as c_uint;
        strm.next_out = output.as_mut_ptr();
        strm.avail_out = output.len() as c_uint;
        assert_eq!(unsafe { deflate(&mut strm, Z_FINISH) }, Z_STREAM_END);

        assert_eq!(unsafe { deflateEnd(&mut strm) }, Z_OK);
    }

    #[test]
    fn deflate_params_rejects_invalid_raw_buffers() {
        // `deflateParams` surfaces the same entry validation as `deflate` (it may
        // flush pending output through an internal `deflate(strm, Z_BLOCK)`); a
        // null `next_out` is rejected with `Z_STREAM_ERROR`.
        let mut strm = zeroed_stream();
        assert_eq!(
            unsafe { deflateInit_(&mut strm, 6, ver(), size_of::<z_stream>() as c_int) },
            Z_OK
        );
        let input = b"payload";
        strm.next_in = input.as_ptr();
        strm.avail_in = input.len() as c_uint;
        strm.next_out = ptr::null_mut();
        strm.avail_out = 128;
        assert_eq!(
            unsafe { deflateParams(&mut strm, 9, Z_DEFAULT_STRATEGY) },
            Z_STREAM_ERROR
        );
        assert_eq!(unsafe { deflateEnd(&mut strm) }, Z_OK);
    }

    #[test]
    fn bound_without_state_is_finite() {
        let b = unsafe { deflateBound(ptr::null_mut(), 1024) };
        assert!(b >= 1024, "bound must cover the source length");
        let bz = unsafe { deflateBound_z(ptr::null_mut(), 1024) };
        assert!(bz >= 1024);

        let mut strm = zeroed_stream();
        assert_eq!(
            unsafe { deflateInit_(&mut strm, 6, ver(), size_of::<z_stream>() as c_int) },
            Z_OK
        );
        let b2 = unsafe { deflateBound(&mut strm, 1024) };
        assert!(b2 >= 1024);
        assert_eq!(unsafe { deflateEnd(&mut strm) }, Z_OK);
    }

    #[test]
    fn pending_and_used_accept_null_pointers() {
        let mut strm = zeroed_stream();
        assert_eq!(
            unsafe { deflateInit_(&mut strm, 6, ver(), size_of::<z_stream>() as c_int) },
            Z_OK
        );

        // Both output pointers null: still succeeds.
        assert_eq!(
            unsafe { deflatePending(&mut strm, ptr::null_mut(), ptr::null_mut()) },
            Z_OK
        );
        let mut pending: c_uint = 0xDEAD;
        let mut bits: c_int = 99;
        assert_eq!(
            unsafe { deflatePending(&mut strm, &mut pending, &mut bits) },
            Z_OK
        );
        assert!((0..8).contains(&bits), "pending bit count out of range");

        assert_eq!(unsafe { deflateUsed(&mut strm, ptr::null_mut()) }, Z_OK);
        let mut used: c_int = -1;
        assert_eq!(unsafe { deflateUsed(&mut strm, &mut used) }, Z_OK);
        assert!((0..=8).contains(&used), "used bit count out of range");

        assert_eq!(unsafe { deflateEnd(&mut strm) }, Z_OK);
    }

    #[test]
    fn reset_clears_counters() {
        let mut strm = zeroed_stream();
        assert_eq!(
            unsafe { deflateInit_(&mut strm, 6, ver(), size_of::<z_stream>() as c_int) },
            Z_OK
        );

        let input = b"reset test payload ".repeat(4);
        let mut out = std::vec![0u8; 512];
        strm.next_in = input.as_ptr();
        strm.avail_in = input.len() as c_uint;
        strm.next_out = out.as_mut_ptr();
        strm.avail_out = out.len() as c_uint;
        loop {
            let rc = unsafe { deflate(&mut strm, Z_FINISH) };
            if rc == Z_STREAM_END {
                break;
            }
            assert_eq!(rc, Z_OK);
        }
        assert!(strm.total_out > 0);

        assert_eq!(unsafe { deflateReset(&mut strm) }, Z_OK);
        assert_eq!(strm.total_in, 0);
        assert_eq!(strm.total_out, 0);
        assert_eq!(unsafe { deflateEnd(&mut strm) }, Z_OK);
    }

    #[test]
    fn set_dictionary_updates_adler() {
        let mut strm = zeroed_stream();
        assert_eq!(
            unsafe { deflateInit_(&mut strm, 6, ver(), size_of::<z_stream>() as c_int) },
            Z_OK
        );
        let dict = b"the quick brown fox jumps over the lazy dog";
        let rc = unsafe { deflateSetDictionary(&mut strm, dict.as_ptr(), dict.len() as c_uint) };
        assert_eq!(rc, Z_OK);
        // For a zlib-wrapped stream the dictionary is folded into the Adler-32,
        // which the shim writes back onto the raw stream.
        assert_ne!(strm.adler, 1, "dictionary should change the Adler-32 seed");
        assert_eq!(unsafe { deflateEnd(&mut strm) }, Z_OK);
    }

    #[test]
    fn copy_produces_independent_finishable_stream() {
        let mut src = zeroed_stream();
        assert_eq!(
            unsafe { deflateInit_(&mut src, 6, ver(), size_of::<z_stream>() as c_int) },
            Z_OK
        );

        // Copy a fresh stream, then finish the copy on its own buffers.
        let mut dst = zeroed_stream();
        assert_eq!(unsafe { deflateCopy(&mut dst, &mut src) }, Z_OK);
        assert!(!dst.state.is_null());
        assert!(!src.state.is_null());

        let input = b"independent copy finishes correctly ".repeat(4);
        let mut out = std::vec![0u8; 1024];
        dst.next_in = input.as_ptr();
        dst.avail_in = input.len() as c_uint;
        dst.next_out = out.as_mut_ptr();
        dst.avail_out = out.len() as c_uint;
        loop {
            let rc = unsafe { deflate(&mut dst, Z_FINISH) };
            if rc == Z_STREAM_END {
                break;
            }
            assert_eq!(rc, Z_OK);
        }
        let n = dst.total_out as usize;
        assert!(n > 0);
        assert_eq!(zlib_inflate(&out[..n]), input);

        // Both streams free independently (no shared state / double free).
        assert_eq!(unsafe { deflateEnd(&mut dst) }, Z_OK);
        assert_eq!(unsafe { deflateEnd(&mut src) }, Z_OK);
    }

    #[test]
    fn set_header_on_zlib_stream_is_stream_error() {
        let mut strm = zeroed_stream();
        assert_eq!(
            unsafe { deflateInit_(&mut strm, 6, ver(), size_of::<z_stream>() as c_int) },
            Z_OK
        );
        // A zlib-wrapped stream is `wrap == 1`, not gzip, so setting a gzip
        // header must fail with `Z_STREAM_ERROR` (matches C for either feature
        // configuration).
        assert_eq!(
            unsafe { deflateSetHeader(&mut strm, ptr::null_mut()) },
            Z_STREAM_ERROR
        );
        assert_eq!(unsafe { deflateEnd(&mut strm) }, Z_OK);
    }

    #[cfg(feature = "gzip")]
    #[test]
    fn gzip_roundtrip_with_header() {
        let mut strm = zeroed_stream();
        // windowBits 31 selects the gzip wrapper with a 15-bit window.
        let rc = unsafe {
            deflateInit2_(
                &mut strm,
                6,
                Z_DEFLATED,
                31,
                DEF_MEM_LEVEL,
                Z_DEFAULT_STRATEGY,
                ver(),
                size_of::<z_stream>() as c_int,
            )
        };
        assert_eq!(rc, Z_OK);
        // Setting a (default) gzip header succeeds for a gzip stream.
        assert_eq!(
            unsafe { deflateSetHeader(&mut strm, ptr::null_mut()) },
            Z_OK
        );

        let input = b"gzip framed round trip via the FFI shims ".repeat(6);
        let mut out = std::vec![0u8; 1024];
        strm.next_in = input.as_ptr();
        strm.avail_in = input.len() as c_uint;
        strm.next_out = out.as_mut_ptr();
        strm.avail_out = out.len() as c_uint;
        loop {
            let rc = unsafe { deflate(&mut strm, Z_FINISH) };
            if rc == Z_STREAM_END {
                break;
            }
            assert_eq!(rc, Z_OK);
        }
        let n = strm.total_out as usize;
        assert_eq!(unsafe { deflateEnd(&mut strm) }, Z_OK);

        // Decode with a gzip reader to confirm RFC 1952 framing.
        let mut dec = flate2::read::GzDecoder::new(&out[..n]);
        let mut restored = Vec::new();
        dec.read_to_end(&mut restored).expect("gzip inflate failed");
        assert_eq!(restored, input);
    }
}
