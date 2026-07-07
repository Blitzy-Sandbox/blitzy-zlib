//! C-ABI (`extern "C"`) shim layer for the zlib **inflate** (decompression) API
//! and the raw-callback **`inflateBack`** API.
//!
//! This module is one half of the crate's FFI drop-in boundary (the other being
//! [`crate::ffi::deflate`]). Each exported function reproduces the exact zlib C
//! signature (verified against `zlib.h` / `zlib.map` of `1.3.2.1-motley`), so a
//! C consumer can link against the emitted `cdylib`/`staticlib` and call these
//! symbols with no source changes.
//!
//! # Safe engine, unsafe boundary
//!
//! The actual decompression logic lives in the safe-Rust engine under
//! [`crate::inflate`]; it operates on the idiomatic [`ZStream`] and never uses
//! `unsafe`. This file is where the raw C `#[repr(C)]` [`z_stream`] is bridged
//! to that engine: raw pointers are validated, the opaque state is boxed into
//! `z_stream.state`, `Result`/`ReturnCode` values are re-materialized as the
//! integer codes C expects, and the idiomatic `Option<&str>` messages are
//! mapped back to `'static` C strings. Consequently every `unsafe` operation in
//! this file carries a `// SAFETY:` justification, and — because a Rust panic
//! must never unwind across the `extern "C"` boundary into C — every fallible
//! body is wrapped in one of the `guard_*` helpers, which catch unwinding (when
//! `std` is available) and substitute a safe integer default.
//!
//! # State-handle model
//!
//! * Regular inflate path: the boxed [`InflateHandle`] (an idiomatic
//!   [`ZStream`] plus a raw pointer to the caller's registered `gz_header`) is
//!   stored in `z_stream.state`; [`inflateEnd`] reclaims and drops it (RAII
//!   replaces the manual `inflateEnd` free).
//! * `inflateBack` path: a `Box<InflateState>` is stored directly, matching the
//!   [`crate::inflate::back`] engine which allocates and owns the decode window.
//!
//! # `inflateBack` callback bridge
//!
//! zlib's `inflateBack` pulls input and pushes output through the C callbacks
//! `in_func`/`out_func`. Those are wrapped into [`CInFunc`]/[`COutFunc`] which
//! implement the engine's [`InFunc`]/[`OutFunc`] traits, so the safe engine
//! drives decompression while the raw pointer dereferences stay isolated here.

// The entire module is the FFI boundary: every exported function shares the
// same uniform safety contract documented above (callers must pass a valid
// `z_stream` and valid buffers exactly as the zlib C API requires). Documenting
// that contract once at the module level is clearer than repeating a `# Safety`
// section on each of the ~21 shims, so the per-item lint is allowed here.
#![allow(clippy::missing_safety_doc)]

use core::ffi::{CStr, c_char, c_int, c_long, c_uchar, c_uint, c_ulong, c_void};
use core::{ptr, slice};

use alloc::boxed::Box;

use crate::constants::DEF_WBITS;
use crate::error::ReturnCode;
use crate::ffi::types::{
    Bytef, CAllocator, advance_input, advance_output, guard_int, guard_ulong, gz_headerp, in_func,
    input_slice, out_func, output_slice, set_adler, set_data_type, set_msg, state_ptr_from_box,
    state_ref, state_take, uInt, z_stream, z_streamp, zstream_with_caller_alloc,
};
use crate::inflate::back::{InFunc, OutFunc};
use crate::inflate::state::InflateState;
use crate::stream::ZStream;

// `gz_header` (the `#[repr(C)]` mirror) and the header write-back helper are
// only referenced by the gzip-only `inflateGetHeader` path and the header
// write-back inside `inflate`, so gate their imports to avoid unused warnings.
#[cfg(feature = "gzip")]
use crate::ffi::types::{gz_header, write_gz_header_from_idiomatic};
#[cfg(feature = "gzip")]
use crate::gz_header::GzHeader;

// ---------------------------------------------------------------------------
// Return-code constants
//
// `constants.rs` intentionally does not define the integer return codes (they
// are modeled by `ReturnCode`); we materialize the small subset referenced as
// literals in this file from the canonical `ReturnCode::as_c_int()` const fn so
// there is a single source of truth. Codes returned only by flattening an
// engine `Result` go through `ReturnCode::as_c_int()` directly and need no
// local constant here.
// ---------------------------------------------------------------------------

/// C `Z_OK` (0).
const Z_OK: c_int = ReturnCode::Ok.as_c_int();
/// C `Z_STREAM_ERROR` (-2).
const Z_STREAM_ERROR: c_int = ReturnCode::StreamError.as_c_int();
/// C `Z_VERSION_ERROR` (-6).
const Z_VERSION_ERROR: c_int = ReturnCode::VersionError.as_c_int();

/// The value C's `inflateMark` returns when the stream state is unusable:
/// `-(1L << 16)` == `-65536`.
const INFLATE_MARK_ERR: c_long = -(1 << 16);

// ---------------------------------------------------------------------------
// Panic guard for `c_long`-returning shims (`inflateMark`)
//
// `crate::ffi::types` provides `guard_int`/`guard_ulong`/`guard_off`/`guard_ptr`
// but not a `c_long` variant, so we add a private one mirroring their exact
// `#[cfg(feature = "std")]` structure: catch unwinding under `std`, run directly
// under `no_std` (where `catch_unwind` is unavailable and builds abort on panic).
// ---------------------------------------------------------------------------

/// Runs `f`, returning its `c_long` result, or `default` if it panics.
#[cfg(feature = "std")]
fn guard_long(default: c_long, f: impl FnOnce() -> c_long + core::panic::UnwindSafe) -> c_long {
    std::panic::catch_unwind(f).unwrap_or(default)
}

/// `no_std` fallback: runs `f` directly (no unwinding to catch).
#[cfg(not(feature = "std"))]
fn guard_long(_default: c_long, f: impl FnOnce() -> c_long + core::panic::UnwindSafe) -> c_long {
    f()
}

// ---------------------------------------------------------------------------
// Message-string bridge
// ---------------------------------------------------------------------------

/// Maps an idiomatic engine message (`Option<&str>`, i.e. a non-NUL-terminated
/// Rust string literal) to a `'static` NUL-terminated C string pointer suitable
/// for `z_stream.msg`.
///
/// The engine stores one of a fixed set of `&'static str` diagnostics; every one
/// is matched to a compile-time `c"…"` literal whose pointer is valid for the
/// life of the program. Anything unrecognized (including `None`) maps to a null
/// pointer, matching zlib's convention of a null `msg` when there is no message.
fn msg_to_cstr(msg: Option<&str>) -> *const c_char {
    let s = match msg {
        Some(s) => s,
        None => return ptr::null(),
    };
    let cstr: &CStr = match s {
        // --- inflate engine diagnostics (inflate.c / infback.c) ---
        "incorrect header check" => c"incorrect header check",
        "unknown compression method" => c"unknown compression method",
        "invalid window size" => c"invalid window size",
        "unknown header flags set" => c"unknown header flags set",
        "header crc mismatch" => c"header crc mismatch",
        "incorrect data check" => c"incorrect data check",
        "incorrect length check" => c"incorrect length check",
        "invalid block type" => c"invalid block type",
        "invalid stored block lengths" => c"invalid stored block lengths",
        "too many length or distance symbols" => c"too many length or distance symbols",
        "invalid code lengths set" => c"invalid code lengths set",
        "invalid bit length repeat" => c"invalid bit length repeat",
        "invalid code -- missing end-of-block" => c"invalid code -- missing end-of-block",
        "invalid literal/lengths set" => c"invalid literal/lengths set",
        "invalid distances set" => c"invalid distances set",
        "invalid literal/length code" => c"invalid literal/length code",
        "invalid distance code" => c"invalid distance code",
        "invalid distance too far back" => c"invalid distance too far back",
        // --- generic return-code messages (zutil.c `z_errmsg`) ---
        "need dictionary" => c"need dictionary",
        "stream end" => c"stream end",
        "file error" => c"file error",
        "stream error" => c"stream error",
        "data error" => c"data error",
        "insufficient memory" => c"insufficient memory",
        "buffer error" => c"buffer error",
        "incompatible version" => c"incompatible version",
        _ => return ptr::null(),
    };
    cstr.as_ptr()
}

// ---------------------------------------------------------------------------
// Opaque state handle for the regular inflate path
// ---------------------------------------------------------------------------

/// Owns the idiomatic decompression state behind a raw `z_stream.state`.
///
/// Boxed and installed by [`inflateInit2_`]; borrowed via
/// [`state_ref`](crate::ffi::types::state_ref) during operation; reclaimed and
/// dropped by [`inflateEnd`]. Under the `gzip` feature it also records the raw
/// pointer to the caller's `gz_header` (registered by [`inflateGetHeader`]) so
/// the filled header can be written back after each [`inflate`] call.
struct InflateHandle {
    /// The idiomatic stream carrying the boxed `InflateState` plus observable
    /// bookkeeping (`total_in`/`total_out`/`adler`/`data_type`/`msg`).
    zs: ZStream<CAllocator>,
    /// Raw pointer to the caller's `gz_header` registered via `inflateGetHeader`,
    /// or null if none. Only present when gzip framing is compiled in.
    #[cfg(feature = "gzip")]
    head: *mut gz_header,
}

impl InflateHandle {
    /// Wraps a freshly initialized idiomatic stream, with no registered header.
    fn new(zs: ZStream<CAllocator>) -> Self {
        Self {
            zs,
            #[cfg(feature = "gzip")]
            head: ptr::null_mut(),
        }
    }
}

// ---------------------------------------------------------------------------
// C callback adapters for `inflateBack`
// ---------------------------------------------------------------------------

/// Adapts zlib's C `in_func` to the engine's [`InFunc`] trait.
///
/// On the first call it yields whatever input was already buffered in the
/// stream (`next_in[..avail_in]`); thereafter it invokes the C callback, which
/// returns a byte count and points `*buf` at that many readable input bytes.
struct CInFunc<'a> {
    in_fn: unsafe extern "C" fn(*mut c_void, *mut *const c_uchar) -> c_uint,
    in_desc: *mut c_void,
    initial: &'a [u8],
    initial_done: bool,
}

impl InFunc for CInFunc<'_> {
    fn next_input(&mut self) -> &[u8] {
        if !self.initial_done {
            self.initial_done = true;
            if !self.initial.is_empty() {
                return self.initial;
            }
        }
        let mut buf: *const c_uchar = ptr::null();
        // SAFETY: `in_fn` is a valid zlib `in_func` (checked non-null by the
        // `inflateBack` shim before this adapter is constructed). Per the zlib
        // callback contract it returns a count `n` and stores in `*buf` a pointer
        // to `n` readable bytes that remain valid at least until the next call —
        // which is exactly the lifetime over which the engine uses the slice.
        let n = unsafe { (self.in_fn)(self.in_desc, &mut buf) };
        if n == 0 || buf.is_null() {
            &[]
        } else {
            // SAFETY: the callback guaranteed `n` bytes are readable at `buf`.
            unsafe { slice::from_raw_parts(buf, n as usize) }
        }
    }
}

/// Adapts zlib's C `out_func` to the engine's [`OutFunc`] trait. The engine
/// hands it a slice of decompressed output; the callback returns `0` on success
/// (any nonzero value aborts `inflateBack`, mirroring zlib).
struct COutFunc {
    out_fn: unsafe extern "C" fn(*mut c_void, *mut c_uchar, c_uint) -> c_int,
    out_desc: *mut c_void,
}

impl OutFunc for COutFunc {
    fn write_output(&mut self, buf: &[u8]) -> Result<(), ()> {
        // SAFETY: `out_fn` is a valid zlib `out_func` (checked non-null by the
        // `inflateBack` shim). It reads `buf.len()` bytes from the supplied
        // pointer for the duration of the call and must not retain it; we pass a
        // pointer into the engine's live window slice. A zero return is success.
        let rc = unsafe {
            (self.out_fn)(
                self.out_desc,
                buf.as_ptr() as *mut c_uchar,
                buf.len() as c_uint,
            )
        };
        if rc == 0 { Ok(()) } else { Err(()) }
    }
}

// ---------------------------------------------------------------------------
// Shared init version check (identical to the deflate side)
// ---------------------------------------------------------------------------

/// Reproduces zlib's `inflateInit_`/`inflateInit2_` version guard: the caller's
/// version string must be non-null with a matching major version, and the
/// caller's reported `sizeof(z_stream)` must equal ours. Returns `true` when
/// compatible.
#[inline]
unsafe fn version_check(version: *const c_char, stream_size: c_int) -> bool {
    if version.is_null() {
        return false;
    }
    // SAFETY: `version` is non-null, so it points to at least the first byte of
    // the caller's NUL-terminated version C-string; we read only that one byte,
    // exactly as the C init macros do for ABI-compatibility checking.
    if unsafe { *version } != b'1' as c_char {
        return false;
    }
    stream_size == size_of::<z_stream>() as c_int
}

// ===========================================================================
// Phase 1 — initialization shims
//
// Only the real `_`-suffixed symbols are exported. The bare `inflateInit`,
// `inflateInit2`, and `inflateBackInit` names are C preprocessor macros (they
// inject the version string and `sizeof(z_stream)`), so they are deliberately
// NOT defined here.
// ===========================================================================

/// C `inflateInit2_` — initialize for decompression with an explicit
/// `windowBits`.
///
/// `windowBits` retains zlib's full overloading, forwarded unchanged to the
/// engine: `8..=15` selects a zlib (RFC 1950) wrapper, `-8..=-15` raw DEFLATE
/// (no wrapper), `24..=31` (i.e. `16 + 8..=15`) a gzip (RFC 1952) wrapper, and
/// `40..=47` (`32 + 8..=15`) zlib/gzip auto-detection.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn inflateInit2_(
    strm: z_streamp,
    window_bits: c_int,
    version: *const c_char,
    stream_size: c_int,
) -> c_int {
    guard_int(Z_STREAM_ERROR, move || {
        // SAFETY: reads only the first byte of `version` (if non-null); see
        // `version_check`.
        if !unsafe { version_check(version, stream_size) } {
            return Z_VERSION_ERROR;
        }
        if strm.is_null() {
            return Z_STREAM_ERROR;
        }
        // SAFETY: `strm` is non-null and, per the FFI contract, points to a valid
        // caller-owned `z_stream`.
        let sref = unsafe { &mut *strm };
        // SAFETY: `sref` is a valid `z_stream`; this copies its `zalloc`/`zfree`/
        // `opaque` fields into an allocator wrapper (it does not read `state`).
        let mut zs = unsafe { zstream_with_caller_alloc(sref) };
        match crate::inflate::inflate_init2(&mut zs, window_bits) {
            Ok(_) => {
                let adler = zs.adler;
                let handle = Box::new(InflateHandle::new(zs));
                // SAFETY: transfers ownership of the box into the opaque
                // `state` slot; reclaimed and dropped by `inflateEnd`.
                sref.state = unsafe { state_ptr_from_box(handle) };
                sref.total_in = 0;
                sref.total_out = 0;
                sref.msg = ptr::null_mut();
                set_adler(sref, adler);
                Z_OK
            }
            Err(e) => ReturnCode::from(e).as_c_int(),
        }
    })
}

/// C `inflateInit_` — initialize for decompression with the default window
/// size. Equivalent to `inflateInit2_(strm, DEF_WBITS, …)` (`DEF_WBITS` == 15),
/// exactly as the C macro expands.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn inflateInit_(
    strm: z_streamp,
    version: *const c_char,
    stream_size: c_int,
) -> c_int {
    // SAFETY: forwards the raw arguments unchanged to `inflateInit2_`, which
    // performs all validation.
    unsafe { inflateInit2_(strm, DEF_WBITS, version, stream_size) }
}

/// C `inflateBackInit_` — initialize for a raw-callback `inflateBack` decode.
///
/// `windowBits` must be in `8..=15` (raw DEFLATE only). In zlib the caller
/// supplies the `1 << windowBits` byte sliding-window buffer; the realized
/// [`crate::inflate::back`] engine instead allocates and owns its own window, so
/// this shim validates `window` for ABI parity but does not use it as backing
/// storage. (Documented deviation; behavior is unaffected.)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn inflateBackInit_(
    strm: z_streamp,
    window_bits: c_int,
    window: *mut c_uchar,
    version: *const c_char,
    stream_size: c_int,
) -> c_int {
    guard_int(Z_STREAM_ERROR, move || {
        // SAFETY: reads only the first byte of `version` (if non-null).
        if !unsafe { version_check(version, stream_size) } {
            return Z_VERSION_ERROR;
        }
        if strm.is_null() || window.is_null() {
            return Z_STREAM_ERROR;
        }
        if !(8..=15).contains(&window_bits) {
            return Z_STREAM_ERROR;
        }
        // SAFETY: `strm` is non-null and a valid caller-owned `z_stream`.
        let sref = unsafe { &mut *strm };
        match crate::inflate::back::inflate_back_init(window_bits) {
            Ok(state) => {
                // SAFETY: transfers ownership of the `Box<InflateState>` into the
                // opaque `state` slot; reclaimed and dropped by `inflateBackEnd`.
                sref.state = unsafe { state_ptr_from_box(state) };
                sref.total_in = 0;
                sref.total_out = 0;
                sref.msg = ptr::null_mut();
                set_adler(sref, 0);
                Z_OK
            }
            Err(rc) => rc.as_c_int(),
        }
    })
}

// ===========================================================================
// Phase 2 — core driver
// ===========================================================================

/// C `inflate` — decompress as much as possible.
///
/// Bridges the raw `z_stream` input/output windows to the safe engine, then
/// writes the results back into the raw stream. The observable semantics match
/// zlib: `Z_STREAM_END` at end of stream, `Z_NEED_DICT` when a preset
/// dictionary is required (with `strm.adler` set to the required checksum),
/// `Z_BUF_ERROR` on no progress, and `Z_DATA_ERROR` with `strm.msg` set on
/// corrupt input.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn inflate(strm: z_streamp, flush: c_int) -> c_int {
    guard_int(Z_STREAM_ERROR, move || {
        if strm.is_null() {
            return Z_STREAM_ERROR;
        }
        // SAFETY: `strm` is non-null and a valid caller-owned `z_stream`.
        let sref = unsafe { &mut *strm };

        // Borrow the input/output windows. These helpers return slices with
        // *detached* lifetimes that point at the caller's external buffers, not
        // into the `z_stream` struct, so they remain valid while we subsequently
        // take a `&mut` borrow of the struct for the state handle and cursor
        // updates.
        // SAFETY: honors `next_in`/`avail_in` and `next_out`/`avail_out`; the
        // caller guarantees those describe valid readable/writable regions.
        let input: &[u8] = unsafe { input_slice(sref) };
        // SAFETY: as above for the output window; the region is exclusively ours
        // for the duration of the call.
        let output: &mut [u8] = unsafe { output_slice(sref) };

        // SAFETY: `state`, if non-null, is the `Box<InflateHandle>` installed by
        // `inflateInit2_`.
        let handle = match unsafe { state_ref::<InflateHandle>(sref) } {
            Some(h) => h,
            None => return Z_STREAM_ERROR,
        };

        let outcome = crate::inflate::inflate(&mut handle.zs, input, output, flush);

        // Snapshot observable fields out of the handle before its borrow ends.
        let adler = handle.zs.adler;
        let data_type = handle.zs.data_type;
        let msg = handle.zs.msg;

        // gzip header write-back: if the caller registered a `gz_header` via
        // `inflateGetHeader`, mirror whatever the engine has captured so far into
        // it, honoring the caller's `extra_max`/`name_max`/`comm_max` capacities.
        #[cfg(feature = "gzip")]
        {
            let head_ptr = handle.head;
            if !head_ptr.is_null() {
                if let Some(state) = handle.zs.inflate_state() {
                    if let Some(gh) = state.head.as_ref() {
                        // SAFETY: `head_ptr` is the caller's `gz_header`, still
                        // valid (registered via `inflateGetHeader`); the helper
                        // writes only within the recorded `*_max` capacities.
                        unsafe { write_gz_header_from_idiomatic(head_ptr, gh) };
                    }
                }
            }
        }

        // The `handle` borrow ends here (its last use was above); re-borrow the
        // raw stream to advance cursors and publish observable fields.
        // SAFETY: `outcome.consumed <= input.len() == avail_in`, so advancing by
        // that amount keeps `next_in`/`avail_in`/`total_in` consistent.
        unsafe { advance_input(sref, outcome.consumed) };
        // SAFETY: `outcome.produced <= output.len() == avail_out`, as above.
        unsafe { advance_output(sref, outcome.produced) };
        set_adler(sref, adler);
        set_data_type(sref, data_type);
        set_msg(sref, msg_to_cstr(msg));

        outcome.code.as_c_int()
    })
}

// ===========================================================================
// Phase 3 — lifecycle: end / reset
// ===========================================================================

/// C `inflateEnd` — free all state associated with the stream. The boxed
/// [`InflateHandle`] is reclaimed and dropped, whose `Drop` frees the window and
/// tables (RAII replaces zlib's manual cleanup).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn inflateEnd(strm: z_streamp) -> c_int {
    guard_int(Z_STREAM_ERROR, move || {
        if strm.is_null() {
            return Z_STREAM_ERROR;
        }
        // SAFETY: `strm` is non-null and a valid caller-owned `z_stream`.
        let sref = unsafe { &mut *strm };
        // SAFETY: `state`, if non-null, is the `Box<InflateHandle>` from
        // `inflateInit2_`; `state_take` reclaims it and nulls `state`.
        match unsafe { state_take::<InflateHandle>(sref) } {
            Some(boxed) => {
                drop(boxed);
                sref.msg = ptr::null_mut();
                Z_OK
            }
            None => Z_STREAM_ERROR,
        }
    })
}

/// C `inflateReset` — reset the stream to a freshly-initialized state, keeping
/// the current `windowBits`/wrap configuration and allocations.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn inflateReset(strm: z_streamp) -> c_int {
    guard_int(Z_STREAM_ERROR, move || {
        if strm.is_null() {
            return Z_STREAM_ERROR;
        }
        // SAFETY: `strm` is non-null and a valid caller-owned `z_stream`.
        let sref = unsafe { &mut *strm };
        // SAFETY: `state`, if non-null, is the `Box<InflateHandle>`.
        let handle = match unsafe { state_ref::<InflateHandle>(sref) } {
            Some(h) => h,
            None => return Z_STREAM_ERROR,
        };
        let res = crate::inflate::inflate_reset(&mut handle.zs);
        let adler = handle.zs.adler;
        // `handle` borrow ends here.
        match res {
            Ok(_) => {
                sref.total_in = 0;
                sref.total_out = 0;
                sref.msg = ptr::null_mut();
                set_adler(sref, adler);
                Z_OK
            }
            Err(e) => ReturnCode::from(e).as_c_int(),
        }
    })
}

/// C `inflateReset2` — like [`inflateReset`] but also re-selects the wrap mode
/// and window size from a new `windowBits` (with the same overloading as
/// [`inflateInit2_`]). Invalid `windowBits` yields `Z_STREAM_ERROR`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn inflateReset2(strm: z_streamp, window_bits: c_int) -> c_int {
    guard_int(Z_STREAM_ERROR, move || {
        if strm.is_null() {
            return Z_STREAM_ERROR;
        }
        // SAFETY: `strm` is non-null and a valid caller-owned `z_stream`.
        let sref = unsafe { &mut *strm };
        // SAFETY: `state`, if non-null, is the `Box<InflateHandle>`.
        let handle = match unsafe { state_ref::<InflateHandle>(sref) } {
            Some(h) => h,
            None => return Z_STREAM_ERROR,
        };
        let res = crate::inflate::inflate_reset2(&mut handle.zs, window_bits);
        let adler = handle.zs.adler;
        // `handle` borrow ends here.
        match res {
            Ok(_) => {
                sref.total_in = 0;
                sref.total_out = 0;
                sref.msg = ptr::null_mut();
                set_adler(sref, adler);
                Z_OK
            }
            Err(e) => ReturnCode::from(e).as_c_int(),
        }
    })
}

/// C `inflateResetKeep` — reset the stream while preserving already-processed
/// history (the sliding window contents), as used by `inflateReset`'s "keep"
/// variant. Observable byte totals are reset.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn inflateResetKeep(strm: z_streamp) -> c_int {
    guard_int(Z_STREAM_ERROR, move || {
        if strm.is_null() {
            return Z_STREAM_ERROR;
        }
        // SAFETY: `strm` is non-null and a valid caller-owned `z_stream`.
        let sref = unsafe { &mut *strm };
        // SAFETY: `state`, if non-null, is the `Box<InflateHandle>`.
        let handle = match unsafe { state_ref::<InflateHandle>(sref) } {
            Some(h) => h,
            None => return Z_STREAM_ERROR,
        };
        let res = crate::inflate::inflate_reset_keep(&mut handle.zs);
        let adler = handle.zs.adler;
        // `handle` borrow ends here.
        match res {
            Ok(_) => {
                sref.total_in = 0;
                sref.total_out = 0;
                sref.msg = ptr::null_mut();
                set_adler(sref, adler);
                Z_OK
            }
            Err(e) => ReturnCode::from(e).as_c_int(),
        }
    })
}

// ===========================================================================
// Phase 4 — dictionary / sync / prime shims
// ===========================================================================

/// C `inflateSetDictionary` — supply a preset dictionary for decompression
/// (used after `inflate` reports `Z_NEED_DICT`). Returns `Z_DATA_ERROR` if the
/// dictionary's Adler-32 does not match the value the stream expects.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn inflateSetDictionary(
    strm: z_streamp,
    dictionary: *const Bytef,
    dict_length: uInt,
) -> c_int {
    guard_int(Z_STREAM_ERROR, move || {
        if strm.is_null() {
            return Z_STREAM_ERROR;
        }
        // SAFETY: `strm` is non-null and a valid caller-owned `z_stream`.
        let sref = unsafe { &mut *strm };
        let dict: &[u8] = if dictionary.is_null() || dict_length == 0 {
            &[]
        } else {
            // SAFETY: the caller guarantees `dict_length` readable bytes at
            // `dictionary` for the duration of this call.
            unsafe { slice::from_raw_parts(dictionary, dict_length as usize) }
        };
        // SAFETY: `state`, if non-null, is the `Box<InflateHandle>`.
        let handle = match unsafe { state_ref::<InflateHandle>(sref) } {
            Some(h) => h,
            None => return Z_STREAM_ERROR,
        };
        match crate::inflate::inflate_set_dictionary(&mut handle.zs, dict) {
            Ok(rc) => rc.as_c_int(),
            Err(e) => ReturnCode::from(e).as_c_int(),
        }
    })
}

/// C `inflateGetDictionary` — copy the sliding-window history (the current
/// dictionary) into the caller's buffer. If `dictionary` is null, only the
/// length is reported (via `dict_length`), enabling the usual query-then-fetch
/// pattern.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn inflateGetDictionary(
    strm: z_streamp,
    dictionary: *mut Bytef,
    dict_length: *mut uInt,
) -> c_int {
    guard_int(Z_STREAM_ERROR, move || {
        if strm.is_null() {
            return Z_STREAM_ERROR;
        }
        // SAFETY: `strm` is non-null and a valid caller-owned `z_stream`.
        let sref = unsafe { &mut *strm };
        // SAFETY: `state`, if non-null, is the `Box<InflateHandle>`.
        let handle = match unsafe { state_ref::<InflateHandle>(sref) } {
            Some(h) => h,
            None => return Z_STREAM_ERROR,
        };
        // How many bytes the engine would copy (the window history length).
        let whave = handle
            .zs
            .inflate_state()
            .map(|s| s.whave as usize)
            .unwrap_or(0);
        // Present the caller's buffer (assumed to hold at least `whave` bytes, as
        // the zlib contract requires) as a mutable slice, or an empty slice when
        // querying the length only.
        let dict_slice: &mut [u8] = if dictionary.is_null() {
            &mut []
        } else {
            // SAFETY: per the zlib contract the caller supplied a buffer of at
            // least `whave` bytes at `dictionary`.
            unsafe { slice::from_raw_parts_mut(dictionary, whave) }
        };
        let mut len: usize = 0;
        let res = crate::inflate::inflate_get_dictionary(&handle.zs, dict_slice, &mut len);
        if !dict_length.is_null() {
            // SAFETY: `dict_length` is non-null and points to a writable `uInt`.
            unsafe { *dict_length = len as uInt };
        }
        match res {
            Ok(rc) => rc.as_c_int(),
            Err(e) => ReturnCode::from(e).as_c_int(),
        }
    })
}

/// C `inflateSync` — skip invalid compressed data until a possible full-flush
/// point is found, for error recovery. Consumes input from the raw stream and
/// advances its cursors accordingly.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn inflateSync(strm: z_streamp) -> c_int {
    guard_int(Z_STREAM_ERROR, move || {
        if strm.is_null() {
            return Z_STREAM_ERROR;
        }
        // SAFETY: `strm` is non-null and a valid caller-owned `z_stream`.
        let sref = unsafe { &mut *strm };
        // SAFETY: detached-lifetime input window (see `inflate`); valid to hold
        // across the later `&mut` re-borrow for cursor advancement.
        let input: &[u8] = unsafe { input_slice(sref) };
        // SAFETY: `state`, if non-null, is the `Box<InflateHandle>`.
        let handle = match unsafe { state_ref::<InflateHandle>(sref) } {
            Some(h) => h,
            None => return Z_STREAM_ERROR,
        };
        let (code, consumed) = crate::inflate::inflate_sync(&mut handle.zs, input);
        let adler = handle.zs.adler;
        let msg = handle.zs.msg;
        // `handle` borrow ends here.
        // SAFETY: `consumed <= input.len() == avail_in`.
        unsafe { advance_input(sref, consumed) };
        set_adler(sref, adler);
        set_msg(sref, msg_to_cstr(msg));
        code.as_c_int()
    })
}

/// C `inflateSyncPoint` — returns nonzero (`1`) if `inflate` is currently at a
/// point where a full flush was applied (a synchronization point), `0`
/// otherwise, or `Z_STREAM_ERROR` on an invalid stream.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn inflateSyncPoint(strm: z_streamp) -> c_int {
    guard_int(Z_STREAM_ERROR, move || {
        if strm.is_null() {
            return Z_STREAM_ERROR;
        }
        // SAFETY: `strm` is non-null and a valid caller-owned `z_stream`.
        let sref = unsafe { &mut *strm };
        // SAFETY: `state`, if non-null, is the `Box<InflateHandle>`.
        let handle = match unsafe { state_ref::<InflateHandle>(sref) } {
            Some(h) => h,
            None => return Z_STREAM_ERROR,
        };
        match crate::inflate::inflate_sync_point(&handle.zs) {
            Ok(true) => 1,
            Ok(false) => 0,
            Err(e) => ReturnCode::from(e).as_c_int(),
        }
    })
}

/// C `inflatePrime` — insert `bits` bits of `value` into the inflate input
/// stream ahead of the next byte, or drop bits when `bits` is negative. Used to
/// prime the bit buffer when resuming mid-stream.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn inflatePrime(strm: z_streamp, bits: c_int, value: c_int) -> c_int {
    guard_int(Z_STREAM_ERROR, move || {
        if strm.is_null() {
            return Z_STREAM_ERROR;
        }
        // SAFETY: `strm` is non-null and a valid caller-owned `z_stream`.
        let sref = unsafe { &mut *strm };
        // SAFETY: `state`, if non-null, is the `Box<InflateHandle>`.
        let handle = match unsafe { state_ref::<InflateHandle>(sref) } {
            Some(h) => h,
            None => return Z_STREAM_ERROR,
        };
        match crate::inflate::inflate_prime(&mut handle.zs, bits, value) {
            Ok(rc) => rc.as_c_int(),
            Err(e) => ReturnCode::from(e).as_c_int(),
        }
    })
}

// ===========================================================================
// Phase 4 (cont.) — copy / mark / validate / undermine / query shims
// ===========================================================================

/// C `inflateCopy` — deep-copy an active decompression stream (state, window,
/// and observable fields) from `source` into `dest`. Returns `Z_MEM_ERROR` if a
/// clone allocation fails, or `Z_STREAM_ERROR` on invalid arguments.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn inflateCopy(dest: z_streamp, source: z_streamp) -> c_int {
    guard_int(Z_STREAM_ERROR, move || {
        if dest.is_null() || source.is_null() {
            return Z_STREAM_ERROR;
        }

        // Build the destination idiomatic stream carrying the source's allocator
        // hooks. This shared read completes before we take the `&mut` borrow of
        // `source` below, avoiding aliasing.
        // SAFETY: `source` is non-null and a valid `z_stream`; reads its alloc
        // fields only.
        let mut dest_zs = unsafe { zstream_with_caller_alloc(&*source) };

        // SAFETY: `source` is non-null and a valid `z_stream`.
        let src = unsafe { &mut *source };
        // SAFETY: `source.state`, if non-null, is the `Box<InflateHandle>`.
        let src_handle = match unsafe { state_ref::<InflateHandle>(src) } {
            Some(h) => h,
            None => return Z_STREAM_ERROR,
        };

        let copy_res = crate::inflate::inflate_copy(&mut dest_zs, &src_handle.zs);
        #[cfg(feature = "gzip")]
        let src_head = src_handle.head;
        // `src_handle`/`src` borrows end here (last use above).

        match copy_res {
            Ok(_) => {
                // `handle` is mutated only under `gzip` (to carry the source's
                // gzip header); when `gzip` is disabled the `mut` is genuinely
                // unused, so suppress the lint in exactly that configuration.
                #[cfg_attr(not(feature = "gzip"), allow(unused_mut))]
                let mut handle = InflateHandle::new(dest_zs);
                #[cfg(feature = "gzip")]
                {
                    handle.head = src_head;
                }
                // SAFETY: `dest` is non-null and a valid `z_stream`.
                let dref = unsafe { &mut *dest };
                // SAFETY: transfers ownership of the cloned handle into
                // `dest.state`; reclaimed by `inflateEnd`.
                dref.state = unsafe { state_ptr_from_box(Box::new(handle)) };

                // Mirror zlib's `zmemcpy(dest, source, sizeof(z_stream))` for the
                // observable fields (the `state` pointer was just set above).
                // SAFETY: `source` is a valid `z_stream`; the earlier `&mut`
                // borrow has ended, so this shared read does not alias.
                let sref = unsafe { &*source };
                dref.next_in = sref.next_in;
                dref.avail_in = sref.avail_in;
                dref.total_in = sref.total_in;
                dref.next_out = sref.next_out;
                dref.avail_out = sref.avail_out;
                dref.total_out = sref.total_out;
                dref.msg = sref.msg;
                dref.zalloc = sref.zalloc;
                dref.zfree = sref.zfree;
                dref.opaque = sref.opaque;
                dref.data_type = sref.data_type;
                dref.adler = sref.adler;
                dref.reserved = sref.reserved;
                Z_OK
            }
            Err(e) => ReturnCode::from(e).as_c_int(),
        }
    })
}

/// C `inflateMark` — return a value describing the current decode location, for
/// random-access use. The high bits encode how many bytes were used from the
/// input, the low 16 bits how many bits beyond that. Returns the sentinel
/// `-(1 << 16)` when the stream state is unusable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn inflateMark(strm: z_streamp) -> c_long {
    guard_long(INFLATE_MARK_ERR, move || {
        if strm.is_null() {
            return INFLATE_MARK_ERR;
        }
        // SAFETY: `strm` is non-null and a valid caller-owned `z_stream`.
        let sref = unsafe { &mut *strm };
        // SAFETY: `state`, if non-null, is the `Box<InflateHandle>`.
        match unsafe { state_ref::<InflateHandle>(sref) } {
            Some(h) => crate::inflate::inflate_mark(&h.zs) as c_long,
            None => INFLATE_MARK_ERR,
        }
    })
}

/// C `inflateValidate` — enable (`check != 0`) or disable check-value
/// (CRC/Adler) verification for the stream.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn inflateValidate(strm: z_streamp, check: c_int) -> c_int {
    guard_int(Z_STREAM_ERROR, move || {
        if strm.is_null() {
            return Z_STREAM_ERROR;
        }
        // SAFETY: `strm` is non-null and a valid caller-owned `z_stream`.
        let sref = unsafe { &mut *strm };
        // SAFETY: `state`, if non-null, is the `Box<InflateHandle>`.
        let handle = match unsafe { state_ref::<InflateHandle>(sref) } {
            Some(h) => h,
            None => return Z_STREAM_ERROR,
        };
        match crate::inflate::inflate_validate(&mut handle.zs, check != 0) {
            Ok(rc) => rc.as_c_int(),
            Err(e) => ReturnCode::from(e).as_c_int(),
        }
    })
}

/// C `inflateUndermine` — request tolerance of invalid check values. The
/// realized engine does not implement the (compile-time-gated) leniency, so it
/// mirrors zlib's default build by reporting `Z_DATA_ERROR`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn inflateUndermine(strm: z_streamp, subvert: c_int) -> c_int {
    guard_int(Z_STREAM_ERROR, move || {
        if strm.is_null() {
            return Z_STREAM_ERROR;
        }
        // SAFETY: `strm` is non-null and a valid caller-owned `z_stream`.
        let sref = unsafe { &mut *strm };
        // SAFETY: `state`, if non-null, is the `Box<InflateHandle>`.
        let handle = match unsafe { state_ref::<InflateHandle>(sref) } {
            Some(h) => h,
            None => return Z_STREAM_ERROR,
        };
        match crate::inflate::inflate_undermine(&mut handle.zs, subvert) {
            Ok(rc) => rc.as_c_int(),
            Err(e) => ReturnCode::from(e).as_c_int(),
        }
    })
}

/// C `inflateCodesUsed` — return the number of inflate decode-table entries in
/// use, primarily for diagnostics. Returns `(unsigned long)-1` (`c_ulong::MAX`)
/// when the stream state is unusable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn inflateCodesUsed(strm: z_streamp) -> c_ulong {
    guard_ulong(c_ulong::MAX, move || {
        if strm.is_null() {
            return c_ulong::MAX;
        }
        // SAFETY: `strm` is non-null and a valid caller-owned `z_stream`.
        let sref = unsafe { &mut *strm };
        // SAFETY: `state`, if non-null, is the `Box<InflateHandle>`.
        match unsafe { state_ref::<InflateHandle>(sref) } {
            Some(h) => match crate::inflate::inflate_codes_used(&h.zs) {
                Some(n) => n as c_ulong,
                None => c_ulong::MAX,
            },
            None => c_ulong::MAX,
        }
    })
}

// ===========================================================================
// Phase 4 (cont.) — gzip header extraction
// ===========================================================================

/// C `inflateGetHeader` — register the caller's `gz_header` so that, while
/// inflating a gzip stream, the header fields (and optionally the extra field,
/// file name, and comment, bounded by the caller's `extra_max`/`name_max`/
/// `comm_max`) are captured into it.
///
/// The caller's `extra`/`name`/`comment` are **output** buffers here; we build a
/// fresh idiomatic [`GzHeader`] that pre-allocates capture vectors only for the
/// non-null buffers, register it with the engine, and remember the raw pointer
/// so [`inflate`] can mirror the captured fields back after each call.
///
/// Returns `Z_STREAM_ERROR` if the stream is not decoding gzip framing.
#[cfg(feature = "gzip")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn inflateGetHeader(strm: z_streamp, head: gz_headerp) -> c_int {
    guard_int(Z_STREAM_ERROR, move || {
        if strm.is_null() {
            return Z_STREAM_ERROR;
        }
        // SAFETY: `strm` is non-null and a valid caller-owned `z_stream`.
        let sref = unsafe { &mut *strm };

        // Construct the idiomatic header describing what to capture. We must not
        // use `gz_header_to_idiomatic` here: that reads `extra`/`name`/`comment`
        // as *inputs* (the `deflateSetHeader` direction), whereas here they are
        // caller output buffers. The engine captures a field only when its
        // idiomatic slot is pre-set to `Some(Vec)`, bounded by the `*_max`.
        let gh = if head.is_null() {
            GzHeader::default()
        } else {
            // SAFETY: `head` is non-null and a valid `gz_header`; we read its
            // buffer pointers (to test for null) and capacity fields.
            let raw = unsafe { &*head };
            GzHeader {
                extra: if raw.extra.is_null() {
                    None
                } else {
                    Some(alloc::vec::Vec::new())
                },
                name: if raw.name.is_null() {
                    None
                } else {
                    Some(alloc::vec::Vec::new())
                },
                comment: if raw.comment.is_null() {
                    None
                } else {
                    Some(alloc::vec::Vec::new())
                },
                extra_max: raw.extra_max,
                name_max: raw.name_max,
                comm_max: raw.comm_max,
                ..GzHeader::default()
            }
        };

        // SAFETY: `state`, if non-null, is the `Box<InflateHandle>`.
        let handle = match unsafe { state_ref::<InflateHandle>(sref) } {
            Some(h) => h,
            None => return Z_STREAM_ERROR,
        };
        match crate::inflate::inflate_get_header(&mut handle.zs, gh) {
            Ok(_) => {
                handle.head = head;
                Z_OK
            }
            Err(e) => ReturnCode::from(e).as_c_int(),
        }
    })
}

/// C `inflateGetHeader` — gzip framing compiled out. Mirrors zlib's behavior of
/// rejecting the request for a non-gzip stream with `Z_STREAM_ERROR`.
#[cfg(not(feature = "gzip"))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn inflateGetHeader(strm: z_streamp, head: gz_headerp) -> c_int {
    let _ = head;
    guard_int(Z_STREAM_ERROR, move || {
        if strm.is_null() {
            return Z_STREAM_ERROR;
        }
        Z_STREAM_ERROR
    })
}

// ===========================================================================
// Phase 5 — inflateBack (raw-callback decode)
// ===========================================================================

/// C `inflateBack` — decompress a raw DEFLATE stream, pulling input via the
/// `in` callback and pushing output via the `out` callback, using the window
/// allocated by [`inflateBackInit_`].
///
/// The C callbacks are wrapped into [`CInFunc`]/[`COutFunc`] and handed to the
/// safe engine. Any already-buffered input in the stream (`next_in`/`avail_in`)
/// is presented to the engine first, then the `in` callback is invoked for more.
///
/// Returns `Z_STREAM_END` on success, `Z_DATA_ERROR`/`Z_BUF_ERROR`/
/// `Z_MEM_ERROR` on decode failures, or `Z_STREAM_ERROR` for invalid arguments
/// (including a null `in`/`out` callback).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn inflateBack(
    strm: z_streamp,
    in_: in_func,
    in_desc: *mut c_void,
    out: out_func,
    out_desc: *mut c_void,
) -> c_int {
    guard_int(Z_STREAM_ERROR, move || {
        if strm.is_null() {
            return Z_STREAM_ERROR;
        }
        // Both callbacks are required; a null either way is a usage error.
        let in_fn = match in_ {
            Some(f) => f,
            None => return Z_STREAM_ERROR,
        };
        let out_fn = match out {
            Some(f) => f,
            None => return Z_STREAM_ERROR,
        };

        // SAFETY: `strm` is non-null and a valid caller-owned `z_stream`.
        let sref = unsafe { &mut *strm };
        // SAFETY: detached-lifetime input window (see `inflate`); it points at the
        // caller's external buffer, so it stays valid across the later `&mut`
        // re-borrow of the stream for cursor advancement.
        let initial: &[u8] = unsafe { input_slice(sref) };
        let initial_len = initial.len();

        // SAFETY: `state`, if non-null, is the `Box<InflateState>` installed by
        // `inflateBackInit_`.
        let state = match unsafe { state_ref::<InflateState>(sref) } {
            Some(s) => s,
            None => return Z_STREAM_ERROR,
        };

        let mut src = CInFunc {
            in_fn,
            in_desc,
            initial,
            initial_done: false,
        };
        let mut sink = COutFunc { out_fn, out_desc };

        let code = crate::inflate::back::inflate_back(state, &mut src, &mut sink);
        // `state`/`src` borrows end here.

        // Reflect consumption of the initially buffered input into the stream's
        // observable cursors. Bytes obtained via the `in` callback are counted by
        // the engine internally but are not part of `next_in`/`avail_in`, so
        // `total_in` accounts only for the buffered portion here (documented
        // limitation; `avail_in` is left consistent).
        // SAFETY: `initial_len <= avail_in`.
        unsafe { advance_input(sref, initial_len) };

        code.as_c_int()
    })
}

/// C `inflateBackEnd` — free the state allocated by [`inflateBackInit_`]. The
/// boxed `InflateState` is reclaimed and passed to the engine's terminator
/// (which validates it), then dropped (RAII frees the window).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn inflateBackEnd(strm: z_streamp) -> c_int {
    guard_int(Z_STREAM_ERROR, move || {
        if strm.is_null() {
            return Z_STREAM_ERROR;
        }
        // SAFETY: `strm` is non-null and a valid caller-owned `z_stream`.
        let sref = unsafe { &mut *strm };
        // SAFETY: `state`, if non-null, is the `Box<InflateState>` from
        // `inflateBackInit_`; `state_take` reclaims it and nulls `state`.
        match unsafe { state_take::<InflateState>(sref) } {
            Some(boxed) => crate::inflate::back::inflate_back_end(boxed).as_c_int(),
            None => Z_STREAM_ERROR,
        }
    })
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use alloc::vec::Vec;
    use core::ffi::CStr;

    /// A version string whose first byte is `'1'` (all `version_check` inspects).
    const VERSION: &CStr = c"1.3.2.1-motley";
    /// C `Z_NO_FLUSH`.
    const Z_NO_FLUSH: c_int = 0;
    /// C `Z_STREAM_END`.
    const Z_STREAM_END: c_int = ReturnCode::StreamEnd.as_c_int();

    /// The plaintext that every compressed test vector decodes to.
    const MSG: &[u8] = b"hello, hello, hello, world!";

    // zlib-wrapped (RFC 1950) stream for `MSG`.
    const ZLIB_STREAM: &[u8] = &[
        120, 156, 203, 72, 205, 201, 201, 215, 81, 200, 64, 161, 202, 243, 139, 114, 82, 20, 1,
        133, 250, 9, 106,
    ];
    // Raw DEFLATE (RFC 1951) stream for `MSG`.
    const RAW_STREAM: &[u8] = &[
        203, 72, 205, 201, 201, 215, 81, 200, 64, 161, 202, 243, 139, 114, 82, 20, 1,
    ];
    // gzip-wrapped (RFC 1952) stream for `MSG` (no name/comment).
    const GZIP_STREAM: &[u8] = &[
        31, 139, 8, 0, 0, 0, 0, 0, 0, 3, 203, 72, 205, 201, 201, 215, 81, 200, 64, 161, 202, 243,
        139, 114, 82, 20, 1, 131, 137, 31, 110, 27, 0, 0, 0,
    ];
    // gzip stream for `MSG` carrying FNAME="name.txt" and FCOMMENT="a comment".
    #[cfg(feature = "gzip")]
    const GZIP_NAMED: &[u8] = &[
        31, 139, 8, 24, 0, 0, 0, 0, 0, 3, 110, 97, 109, 101, 46, 116, 120, 116, 0, 97, 32, 99, 111,
        109, 109, 101, 110, 116, 0, 203, 72, 205, 201, 201, 215, 81, 200, 64, 161, 202, 243, 139,
        114, 82, 20, 1, 131, 137, 31, 110, 27, 0, 0, 0,
    ];

    /// An all-zero/null `z_stream`, as a C caller would `memset` before init.
    fn zeroed_stream() -> z_stream {
        z_stream {
            next_in: ptr::null(),
            avail_in: 0,
            total_in: 0,
            next_out: ptr::null_mut(),
            avail_out: 0,
            total_out: 0,
            msg: ptr::null_mut(),
            state: ptr::null_mut(),
            zalloc: None,
            zfree: None,
            opaque: ptr::null_mut(),
            data_type: 0,
            adler: 0,
            reserved: 0,
        }
    }

    /// Initialize with `window_bits`, decompress `input` in one shot, and return
    /// `(return_code, decompressed_bytes)`.
    fn inflate_once(window_bits: c_int, input: &[u8]) -> (c_int, Vec<u8>) {
        let mut strm = zeroed_stream();
        let rc = unsafe {
            inflateInit2_(
                &mut strm,
                window_bits,
                VERSION.as_ptr(),
                size_of::<z_stream>() as c_int,
            )
        };
        assert_eq!(rc, Z_OK, "inflateInit2_ failed");

        let mut out = vec![0u8; 512];
        strm.next_in = input.as_ptr();
        strm.avail_in = input.len() as c_uint;
        strm.next_out = out.as_mut_ptr();
        strm.avail_out = out.len() as c_uint;

        let rc = unsafe { inflate(&mut strm, Z_NO_FLUSH) };
        let produced = out.len() - strm.avail_out as usize;
        out.truncate(produced);

        assert_eq!(unsafe { inflateEnd(&mut strm) }, Z_OK, "inflateEnd failed");
        (rc, out)
    }

    #[test]
    fn round_trip_zlib() {
        let (rc, out) = inflate_once(15, ZLIB_STREAM);
        assert_eq!(rc, Z_STREAM_END);
        assert_eq!(out, MSG);
    }

    #[test]
    fn round_trip_raw() {
        // Raw DEFLATE selected by a negative windowBits.
        let (rc, out) = inflate_once(-15, RAW_STREAM);
        assert_eq!(rc, Z_STREAM_END);
        assert_eq!(out, MSG);
    }

    #[test]
    #[cfg(feature = "gzip")]
    fn round_trip_gzip() {
        // gzip framing selected by 16 + windowBits.
        let (rc, out) = inflate_once(15 + 16, GZIP_STREAM);
        assert_eq!(rc, Z_STREAM_END);
        assert_eq!(out, MSG);
    }

    #[test]
    #[cfg(feature = "gzip")]
    fn auto_detect_decodes_zlib_and_gzip() {
        // 32 + windowBits enables zlib/gzip auto-detection.
        let (rc_z, out_z) = inflate_once(15 + 32, ZLIB_STREAM);
        assert_eq!(rc_z, Z_STREAM_END);
        assert_eq!(out_z, MSG);

        let (rc_g, out_g) = inflate_once(15 + 32, GZIP_STREAM);
        assert_eq!(rc_g, Z_STREAM_END);
        assert_eq!(out_g, MSG);
    }

    #[test]
    fn version_and_size_mismatch_rejected() {
        let mut strm = zeroed_stream();
        // Wrong reported sizeof(z_stream).
        let rc = unsafe {
            inflateInit2_(
                &mut strm,
                15,
                VERSION.as_ptr(),
                (size_of::<z_stream>() - 1) as c_int,
            )
        };
        assert_eq!(rc, Z_VERSION_ERROR);

        // Null version string.
        let rc =
            unsafe { inflateInit2_(&mut strm, 15, ptr::null(), size_of::<z_stream>() as c_int) };
        assert_eq!(rc, Z_VERSION_ERROR);
    }

    #[test]
    fn null_stream_is_stream_error() {
        assert_eq!(
            unsafe { inflate(ptr::null_mut(), Z_NO_FLUSH) },
            Z_STREAM_ERROR
        );
        assert_eq!(unsafe { inflateEnd(ptr::null_mut()) }, Z_STREAM_ERROR);
        assert_eq!(unsafe { inflateReset(ptr::null_mut()) }, Z_STREAM_ERROR);
    }

    #[test]
    fn mark_and_codes_used_null_sentinels() {
        // inflateMark returns -(1 << 16) on a null/invalid stream.
        assert_eq!(unsafe { inflateMark(ptr::null_mut()) }, -(1 << 16));
        // inflateCodesUsed returns (unsigned long)-1 on a null/invalid stream.
        assert_eq!(unsafe { inflateCodesUsed(ptr::null_mut()) }, c_ulong::MAX);
    }

    #[test]
    fn reset_then_reinflate() {
        let mut strm = zeroed_stream();
        assert_eq!(
            unsafe {
                inflateInit2_(
                    &mut strm,
                    15,
                    VERSION.as_ptr(),
                    size_of::<z_stream>() as c_int,
                )
            },
            Z_OK
        );
        // First decode.
        let mut out = vec![0u8; 512];
        strm.next_in = ZLIB_STREAM.as_ptr();
        strm.avail_in = ZLIB_STREAM.len() as c_uint;
        strm.next_out = out.as_mut_ptr();
        strm.avail_out = out.len() as c_uint;
        assert_eq!(unsafe { inflate(&mut strm, Z_NO_FLUSH) }, Z_STREAM_END);

        // Reset and decode the same stream again.
        assert_eq!(unsafe { inflateReset(&mut strm) }, Z_OK);
        assert_eq!(strm.total_out, 0);
        let mut out2 = vec![0u8; 512];
        strm.next_in = ZLIB_STREAM.as_ptr();
        strm.avail_in = ZLIB_STREAM.len() as c_uint;
        strm.next_out = out2.as_mut_ptr();
        strm.avail_out = out2.len() as c_uint;
        assert_eq!(unsafe { inflate(&mut strm, Z_NO_FLUSH) }, Z_STREAM_END);
        let produced = out2.len() - strm.avail_out as usize;
        assert_eq!(&out2[..produced], MSG);

        assert_eq!(unsafe { inflateEnd(&mut strm) }, Z_OK);
    }

    // -- inflateGetHeader ---------------------------------------------------

    #[cfg(feature = "gzip")]
    fn zeroed_gz_header() -> gz_header {
        gz_header {
            text: 0,
            time: 0,
            xflags: 0,
            os: 0,
            extra: ptr::null_mut(),
            extra_len: 0,
            extra_max: 0,
            name: ptr::null_mut(),
            name_max: 0,
            comment: ptr::null_mut(),
            comm_max: 0,
            hcrc: 0,
            done: 0,
        }
    }

    #[test]
    #[cfg(feature = "gzip")]
    fn get_header_fills_name_and_comment() {
        let mut strm = zeroed_stream();
        assert_eq!(
            unsafe {
                inflateInit2_(
                    &mut strm,
                    15 + 16,
                    VERSION.as_ptr(),
                    size_of::<z_stream>() as c_int,
                )
            },
            Z_OK
        );

        let mut namebuf = [0u8; 64];
        let mut commbuf = [0u8; 64];
        let mut head = zeroed_gz_header();
        head.name = namebuf.as_mut_ptr();
        head.name_max = namebuf.len() as c_uint;
        head.comment = commbuf.as_mut_ptr();
        head.comm_max = commbuf.len() as c_uint;

        assert_eq!(unsafe { inflateGetHeader(&mut strm, &mut head) }, Z_OK);

        let mut out = vec![0u8; 256];
        strm.next_in = GZIP_NAMED.as_ptr();
        strm.avail_in = GZIP_NAMED.len() as c_uint;
        strm.next_out = out.as_mut_ptr();
        strm.avail_out = out.len() as c_uint;
        assert_eq!(unsafe { inflate(&mut strm, Z_NO_FLUSH) }, Z_STREAM_END);

        // The name/comment C strings were written NUL-terminated into the buffers.
        let name = CStr::from_bytes_until_nul(&namebuf).unwrap().to_bytes();
        let comment = CStr::from_bytes_until_nul(&commbuf).unwrap().to_bytes();
        assert_eq!(name, b"name.txt");
        assert_eq!(comment, b"a comment");

        assert_eq!(unsafe { inflateEnd(&mut strm) }, Z_OK);
    }

    // -- inflateBack --------------------------------------------------------

    /// Input source for `inflateBack`: hands over `data` once, then signals EOF.
    struct BackIn {
        data: &'static [u8],
        given: bool,
    }

    unsafe extern "C" fn back_in(desc: *mut c_void, buf: *mut *const c_uchar) -> c_uint {
        // SAFETY: `desc` is the `*mut BackIn` handed to `inflateBack` below.
        let st = unsafe { &mut *(desc as *mut BackIn) };
        if st.given {
            return 0;
        }
        st.given = true;
        // SAFETY: `buf` is a valid out-pointer per the `in_func` contract.
        unsafe { *buf = st.data.as_ptr() };
        st.data.len() as c_uint
    }

    /// Output sink for `inflateBack`: accumulates all produced bytes.
    struct BackOut {
        collected: Vec<u8>,
    }

    unsafe extern "C" fn back_out(desc: *mut c_void, buf: *mut c_uchar, len: c_uint) -> c_int {
        // SAFETY: `desc` is the `*mut BackOut` handed to `inflateBack` below.
        let st = unsafe { &mut *(desc as *mut BackOut) };
        // SAFETY: `buf` points to `len` readable bytes per the `out_func` contract.
        let slice = unsafe { slice::from_raw_parts(buf, len as usize) };
        st.collected.extend_from_slice(slice);
        0
    }

    #[test]
    fn inflate_back_end_to_end() {
        let mut strm = zeroed_stream();
        let mut window = [0u8; 1 << 15];
        assert_eq!(
            unsafe {
                inflateBackInit_(
                    &mut strm,
                    15,
                    window.as_mut_ptr(),
                    VERSION.as_ptr(),
                    size_of::<z_stream>() as c_int,
                )
            },
            Z_OK
        );

        // All input arrives via the `in` callback (nothing pre-buffered).
        let mut in_state = BackIn {
            data: RAW_STREAM,
            given: false,
        };
        let mut out_state = BackOut {
            collected: Vec::new(),
        };

        let rc = unsafe {
            inflateBack(
                &mut strm,
                Some(back_in),
                &mut in_state as *mut BackIn as *mut c_void,
                Some(back_out),
                &mut out_state as *mut BackOut as *mut c_void,
            )
        };
        assert_eq!(rc, Z_STREAM_END);
        assert_eq!(out_state.collected, MSG);

        assert_eq!(unsafe { inflateBackEnd(&mut strm) }, Z_OK);
    }

    #[test]
    fn inflate_back_null_callbacks_rejected() {
        let mut strm = zeroed_stream();
        let mut window = [0u8; 1 << 15];
        assert_eq!(
            unsafe {
                inflateBackInit_(
                    &mut strm,
                    15,
                    window.as_mut_ptr(),
                    VERSION.as_ptr(),
                    size_of::<z_stream>() as c_int,
                )
            },
            Z_OK
        );
        let rc = unsafe { inflateBack(&mut strm, None, ptr::null_mut(), None, ptr::null_mut()) };
        assert_eq!(rc, Z_STREAM_ERROR);
        assert_eq!(unsafe { inflateBackEnd(&mut strm) }, Z_OK);
    }
}
