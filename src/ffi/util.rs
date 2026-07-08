//! `extern "C"` shims for the zlib **one-call**, **checksum**, and
//! **version/error** APIs.
//!
//! This module is part of the crate's designated `unsafe` boundary. Each
//! function reproduces the exact C signature of its zlib counterpart from
//! `zlib.h` so the emitted `cdylib`/`staticlib` is a drop-in replacement for
//! the reference library. The shims validate raw C inputs exactly as the C
//! sources do, bridge to the fully **safe** sibling engines in
//! [`crate::util`] and [`crate::checksum`], and re-materialize zlib's integer
//! return codes at the boundary.
//!
//! The functions covered here are the ports of:
//!
//! * `compress.c` — [`compress`], [`compress2`], [`compressBound`] and their
//!   `size_t` (`_z`) variants;
//! * `uncompr.c` — [`uncompress`], [`uncompress2`] and their `_z` variants;
//! * `adler32.c` — [`adler32`], [`adler32_z`], [`adler32_combine`],
//!   [`adler32_combine64`];
//! * `crc32.c` — [`crc32`], [`crc32_z`], the four `crc32_combine*` entry
//!   points, [`crc32_combine_op`], and [`get_crc_table`];
//! * `zutil.c` — [`zlibVersion`], [`zError`], [`zlibCompileFlags`].
//!
//! ## Safe-core / unsafe-boundary split
//!
//! The compression, decompression, and checksum engines contain **zero**
//! `unsafe`. All raw-pointer handling lives here: every `unsafe` block carries
//! a `// SAFETY:` justification, and every body that dereferences caller
//! pointers or drives the engines is wrapped in
//! `crate::ffi::types::guard_int` / `crate::ffi::types::guard_ulong`
//! (`catch_unwind`) so a Rust panic can never unwind across the C ABI. The
//! remaining shims are pure arithmetic or return `'static` pointers and are
//! provably panic-free, so they run without a guard.
//!
//! ## Null-sentinel parity
//!
//! zlib assigns special meaning to a null buffer for the checksums:
//! `adler32(_, Z_NULL, _)` returns the initial value `1` and
//! `crc32(_, Z_NULL, _)` returns the initial value `0`. The safe checksum
//! functions treat an empty slice as "leave the running value unchanged", so
//! these shims special-case a null pointer to reproduce the C initial-value
//! contract exactly.

// zlib's public symbols are camelCase C identifiers (`compressBound`,
// `zlibVersion`, `crc32_combine64`, …); this keeps the module warning-free
// regardless of the exported spelling.
#![allow(non_snake_case)]
// The module-level contract (raw C pointers valid for the stated lengths,
// output pointers writable) is documented once above rather than repeating a
// `# Safety` section on all twenty-five `extern "C"` shims.
#![allow(clippy::missing_safety_doc)]

use core::ffi::{c_char, c_int};
use core::{ptr, slice};

use crate::constants::Z_DEFAULT_COMPRESSION;
use crate::error::ReturnCode;
use crate::ffi::types::*;
use crate::{checksum, util};

// ===========================================================================
// Integer return codes (materialized locally from `ReturnCode`).
//
// `constants.rs` intentionally does not expose bare `Z_OK`/`Z_STREAM_ERROR`
// integer aliases, so the boundary defines exactly the ones it emits using the
// `const fn` `ReturnCode::as_c_int`.
// ===========================================================================

/// `Z_OK` — successful completion.
const Z_OK: c_int = ReturnCode::Ok.as_c_int();
/// `Z_STREAM_ERROR` — inconsistent parameters (null pointers, invalid level).
const Z_STREAM_ERROR: c_int = ReturnCode::StreamError.as_c_int();

// ===========================================================================
// Boundary slice helpers
// ===========================================================================

/// Reconstruct an immutable byte slice from a C `(ptr, len)` pair.
///
/// Returns an empty slice when `ptr` is null **or** `len` is zero, so we never
/// hand a null pointer to [`slice::from_raw_parts`] (which is undefined
/// behavior even for a zero length).
///
/// # Safety
///
/// When `ptr` is non-null and `len > 0`, the caller must guarantee that `ptr`
/// is valid for reads of `len` bytes lying within a single allocation.
#[inline]
unsafe fn as_bytes<'a>(ptr: *const Bytef, len: usize) -> &'a [u8] {
    if ptr.is_null() || len == 0 {
        &[]
    } else {
        // SAFETY: `ptr` is non-null and, per the caller's contract, valid for
        // reads of `len` bytes within a single allocation.
        unsafe { slice::from_raw_parts(ptr, len) }
    }
}

/// Reconstruct a mutable byte slice from a C `(ptr, len)` pair.
///
/// Returns an empty slice when `ptr` is null **or** `len` is zero. The empty
/// case is backed by a dangling-but-aligned non-null pointer (exactly how
/// `<&mut [u8]>::default()` is built), so no memory is ever accessed.
///
/// # Safety
///
/// When `ptr` is non-null and `len > 0`, the caller must guarantee that `ptr`
/// is valid for writes of `len` bytes lying within a single allocation.
#[inline]
unsafe fn as_bytes_mut<'a>(ptr: *mut Bytef, len: usize) -> &'a mut [u8] {
    if ptr.is_null() || len == 0 {
        // SAFETY: the pointer is non-null and well-aligned and the length is
        // zero, so `from_raw_parts_mut` forms a valid empty slice without ever
        // reading or writing memory.
        unsafe { slice::from_raw_parts_mut(ptr::NonNull::<u8>::dangling().as_ptr(), 0) }
    } else {
        // SAFETY: `ptr` is non-null and, per the caller's contract, valid for
        // writes of `len` bytes within a single allocation.
        unsafe { slice::from_raw_parts_mut(ptr, len) }
    }
}

// ===========================================================================
// Phase 1 — One-call compression wrappers (port of `compress.c`)
// ===========================================================================

/// Compresses `source` into `dest` at the given `level`, writing the produced
/// length back through `dest_len`.
///
/// Port of C `compress2` (`compress.c`). Returns `Z_OK` on success,
/// `Z_STREAM_ERROR` for invalid arguments (null pointers or an invalid level),
/// `Z_BUF_ERROR` when `dest` is too small, or `Z_MEM_ERROR` on allocation
/// failure.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn compress2(
    dest: *mut Bytef,
    dest_len: *mut uLongf,
    source: *const Bytef,
    source_len: uLong,
    level: c_int,
) -> c_int {
    guard_int(Z_STREAM_ERROR, move || {
        // C reads `*destLen` only after confirming `destLen` is non-null.
        if dest_len.is_null() {
            return Z_STREAM_ERROR;
        }
        // SAFETY: `dest_len` is non-null (checked above) and points to a
        // caller-owned `uLongf`.
        let cap = unsafe { *dest_len } as usize;
        // C `compress2_z` validation: reject the null/length couplings.
        if (source_len > 0 && source.is_null()) || (cap > 0 && dest.is_null()) {
            return Z_STREAM_ERROR;
        }
        // SAFETY: the pointer/length couplings were validated above, so both
        // slices are sound (empty when the corresponding length is zero).
        let src = unsafe { as_bytes(source, source_len as usize) };
        let dst = unsafe { as_bytes_mut(dest, cap) };
        match util::compress2(dst, src, level) {
            Ok(produced) => {
                // SAFETY: `dest_len` is non-null (checked above).
                unsafe { *dest_len = produced as uLongf };
                Z_OK
            }
            Err(rc) => {
                // The safe `compress2` does not surface the partial output
                // count on error, so the reported length is reset to zero. This
                // matches C exactly for the `Z_STREAM_ERROR` (invalid level)
                // path, whose `*destLen` was set to 0 before the failing
                // `deflateInit`.
                // SAFETY: `dest_len` is non-null (checked above).
                unsafe { *dest_len = 0 };
                rc.as_c_int()
            }
        }
    })
}

/// `size_t` variant of [`compress2`] (C `compress2_z`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn compress2_z(
    dest: *mut Bytef,
    dest_len: *mut z_size_t,
    source: *const Bytef,
    source_len: z_size_t,
    level: c_int,
) -> c_int {
    guard_int(Z_STREAM_ERROR, move || {
        if dest_len.is_null() {
            return Z_STREAM_ERROR;
        }
        // SAFETY: `dest_len` is non-null (checked above).
        let cap = unsafe { *dest_len };
        if (source_len > 0 && source.is_null()) || (cap > 0 && dest.is_null()) {
            return Z_STREAM_ERROR;
        }
        // SAFETY: the pointer/length couplings were validated above.
        let src = unsafe { as_bytes(source, source_len) };
        let dst = unsafe { as_bytes_mut(dest, cap) };
        match util::compress2(dst, src, level) {
            Ok(produced) => {
                // SAFETY: `dest_len` is non-null (checked above).
                unsafe { *dest_len = produced };
                Z_OK
            }
            Err(rc) => {
                // SAFETY: `dest_len` is non-null (checked above).
                unsafe { *dest_len = 0 };
                rc.as_c_int()
            }
        }
    })
}

/// Compresses `source` into `dest` at the default level (C `compress`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn compress(
    dest: *mut Bytef,
    dest_len: *mut uLongf,
    source: *const Bytef,
    source_len: uLong,
) -> c_int {
    // SAFETY: forwards the identical pointers/lengths to `compress2`, which
    // performs all validation and panic-guarding.
    unsafe { compress2(dest, dest_len, source, source_len, Z_DEFAULT_COMPRESSION) }
}

/// `size_t` variant of [`compress`] (C `compress_z`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn compress_z(
    dest: *mut Bytef,
    dest_len: *mut z_size_t,
    source: *const Bytef,
    source_len: z_size_t,
) -> c_int {
    // SAFETY: forwards to `compress2_z` with the default level.
    unsafe { compress2_z(dest, dest_len, source, source_len, Z_DEFAULT_COMPRESSION) }
}

/// Upper bound on the compressed size of `source_len` input bytes
/// (C `compressBound`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn compressBound(source_len: uLong) -> uLong {
    // Reproduce the C truncation guard `(uLong)bound != bound ? (uLong)-1 : bound`.
    let bound = util::compress_bound(source_len as usize);
    if bound as uLong as usize != bound {
        uLong::MAX
    } else {
        bound as uLong
    }
}

/// `size_t` variant of [`compressBound`] (C `compressBound_z`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn compressBound_z(source_len: z_size_t) -> z_size_t {
    // `compress_bound` already saturates to `usize::MAX` on overflow, matching
    // C `compressBound_z`'s `bound < sourceLen ? (z_size_t)-1 : bound`.
    util::compress_bound(source_len)
}

// ===========================================================================
// Phase 2 — One-call decompression wrappers (port of `uncompr.c`)
// ===========================================================================

/// Shared core for the four `uncompress2*` shims.
///
/// The pointer/length couplings must already have been validated by the caller
/// (each shim does so with its own integer widths). This routine builds the
/// safe slices from the validated `(ptr, len)` pairs, drives the fully safe
/// [`crate::util::uncompress2`] engine — which already reproduces the
/// `uncompr.c` L78-L81 return-code mapping (`Z_STREAM_END`→`Ok`,
/// `Z_NEED_DICT`→`Z_DATA_ERROR`, all-input-consumed `Z_BUF_ERROR`→
/// `Z_DATA_ERROR`) — and reports `(consumed, produced, Result<(), code>)`.
///
/// The `produced` count is authoritative on **every** path that reaches the
/// decode loop (success *and* failure), so the FFI shims can publish
/// `*destLen = total_out` on all paths exactly as C's `uncompress2_z` does
/// (`uncompr.c`: `*destLen = stream.total_out;` runs before the error-mapping
/// `return`). The trailing `Result` carries only the mapped return code.
///
/// # Safety
///
/// When non-null, `dest` must be valid for writes of `cap` bytes and `source`
/// valid for reads of `avail` bytes; a null pointer is only permitted when its
/// paired length is zero.
#[inline]
unsafe fn uncompress2_engine(
    dest: *mut Bytef,
    cap: usize,
    source: *const Bytef,
    avail: usize,
) -> (usize, usize, Result<(), c_int>) {
    // SAFETY: the caller guarantees each `(ptr, len)` pair is valid (or the
    // pointer is null with a zero length, which yields an empty slice).
    let src = unsafe { as_bytes(source, avail) };
    // SAFETY: as above for the output buffer.
    let dst = unsafe { as_bytes_mut(dest, cap) };
    // `*source_len` is in/out: seed it with the declared available count so the
    // engine caps its input at `avail`, then it is overwritten with the number
    // of bytes actually consumed (C `*sourceLen -= len`).
    let mut consumed = avail;
    // `produced` is a pure out-parameter (seeded at zero, matching C's
    // `*destLen = 0;` before the decode loop). `util::uncompress2` overwrites
    // it on every path that reaches the decode loop, so it is correct on both
    // the success and error arms below.
    let mut produced = 0usize;
    let result = match util::uncompress2(dst, src, &mut consumed, &mut produced) {
        Ok(_) => Ok(()),
        Err(rc) => Err(rc.as_c_int()),
    };
    (consumed, produced, result)
}

/// Decompresses the whole zlib stream in `source` into `dest`, writing the
/// produced length back through `dest_len` and the consumed length back through
/// `source_len` (C `uncompress2`).
///
/// Returns `Z_OK` on success, `Z_STREAM_ERROR` for invalid arguments,
/// `Z_DATA_ERROR` for corrupt/truncated input (or a needed preset dictionary),
/// `Z_BUF_ERROR` when `dest` is too small, or `Z_MEM_ERROR` on allocation
/// failure.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uncompress2(
    dest: *mut Bytef,
    dest_len: *mut uLongf,
    source: *const Bytef,
    source_len: *mut uLong,
) -> c_int {
    guard_int(Z_STREAM_ERROR, move || {
        // C reads `*sourceLen`/`*destLen` only after the null-pointer checks.
        if source_len.is_null() || dest_len.is_null() {
            return Z_STREAM_ERROR;
        }
        // SAFETY: both length pointers are non-null (checked immediately above).
        let avail = unsafe { *source_len } as usize;
        // SAFETY: as above.
        let cap = unsafe { *dest_len } as usize;
        if (avail > 0 && source.is_null()) || (cap > 0 && dest.is_null()) {
            return Z_STREAM_ERROR;
        }
        // SAFETY: the pointer/length couplings were validated above.
        let (consumed, produced, result) = unsafe { uncompress2_engine(dest, cap, source, avail) };
        // C writes back BOTH counts after the decode loop, on success *and* on
        // error (`uncompr.c`: `*sourceLen -= len + stream.avail_in;` and
        // `*destLen = stream.total_out;` both run before the error-mapping
        // `return`). Publishing the produced count on the error path is the M2
        // fix: callers relying on the partial output length now observe it.
        // SAFETY: `source_len` is non-null (checked above).
        unsafe { *source_len = consumed as uLong };
        // SAFETY: `dest_len` is non-null (checked above).
        unsafe { *dest_len = produced as uLongf };
        match result {
            Ok(()) => Z_OK,
            Err(code) => code,
        }
    })
}

/// `size_t` variant of [`uncompress2`] (C `uncompress2_z`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uncompress2_z(
    dest: *mut Bytef,
    dest_len: *mut z_size_t,
    source: *const Bytef,
    source_len: *mut z_size_t,
) -> c_int {
    guard_int(Z_STREAM_ERROR, move || {
        if source_len.is_null() || dest_len.is_null() {
            return Z_STREAM_ERROR;
        }
        // SAFETY: both length pointers are non-null (checked above).
        let avail = unsafe { *source_len };
        // SAFETY: as above.
        let cap = unsafe { *dest_len };
        if (avail > 0 && source.is_null()) || (cap > 0 && dest.is_null()) {
            return Z_STREAM_ERROR;
        }
        // SAFETY: the pointer/length couplings were validated above.
        let (consumed, produced, result) = unsafe { uncompress2_engine(dest, cap, source, avail) };
        // As in [`uncompress2`], both counts are published on every path
        // (success and error), matching C `uncompress2_z`.
        // SAFETY: `source_len` is non-null (checked above).
        unsafe { *source_len = consumed };
        // SAFETY: `dest_len` is non-null (checked above).
        unsafe { *dest_len = produced };
        match result {
            Ok(()) => Z_OK,
            Err(code) => code,
        }
    })
}

/// Decompresses the whole zlib stream in `source` into `dest` (C `uncompress`).
///
/// Convenience form that discards the consumed-length report.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uncompress(
    dest: *mut Bytef,
    dest_len: *mut uLongf,
    source: *const Bytef,
    source_len: uLong,
) -> c_int {
    // C seeds `used = sourceLen`, calls `uncompress2`, and discards `used`.
    let mut used: uLong = source_len;
    // SAFETY: forwards to `uncompress2`, which performs all validation and
    // panic-guarding. `&mut used` is a valid non-null pointer.
    unsafe { uncompress2(dest, dest_len, source, &mut used) }
}

/// `size_t` variant of [`uncompress`] (C `uncompress_z`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uncompress_z(
    dest: *mut Bytef,
    dest_len: *mut z_size_t,
    source: *const Bytef,
    source_len: z_size_t,
) -> c_int {
    let mut used: z_size_t = source_len;
    // SAFETY: forwards to `uncompress2_z`, which performs all validation and
    // panic-guarding. `&mut used` is a valid non-null pointer.
    unsafe { uncompress2_z(dest, dest_len, source, &mut used) }
}

// ===========================================================================
// Phase 3 — Adler-32 (port of `adler32.c`)
// ===========================================================================

/// Widen a `z_off_t` (C `long`) to the `i64` used by the safe combine API.
///
/// C `long` is 32-bit on LLP64 targets (e.g. 64-bit Windows) and 64-bit on
/// LP64 targets (e.g. 64-bit Linux). The `as` cast is the identity on LP64 —
/// hence the localized `allow` for [`clippy::unnecessary_cast`] — and a
/// lossless widening on LLP64; it is correct on both. Confining the conversion
/// (and its lint exemption) here keeps every `*_combine`/`*_combine_gen` shim
/// portable and lint-clean.
#[inline]
#[allow(clippy::unnecessary_cast)]
fn off_to_i64(len: z_off_t) -> i64 {
    len as i64
}

/// Adler-32 checksum of `buf`, continuing from a running value `adler`
/// (C `adler32`).
///
/// Reproduces the C null sentinel: `adler32(_, Z_NULL, _)` returns the initial
/// value `1`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn adler32(adler: uLong, buf: *const Bytef, len: uInt) -> uLong {
    guard_ulong(1, move || {
        if buf.is_null() {
            // C `adler32(_, Z_NULL, _)` == 1 (the Adler-32 initial value).
            return 1;
        }
        // SAFETY: `buf` is non-null (checked above) and, per the caller's
        // contract, valid for reads of `len` bytes (empty when `len == 0`).
        let s = unsafe { as_bytes(buf, len as usize) };
        checksum::adler32(adler as u32, s) as uLong
    })
}

/// `size_t`-length variant of [`adler32`] (C `adler32_z`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn adler32_z(adler: uLong, buf: *const Bytef, len: z_size_t) -> uLong {
    guard_ulong(1, move || {
        if buf.is_null() {
            return 1;
        }
        // SAFETY: `buf` is non-null (checked above) and valid for `len` bytes.
        let s = unsafe { as_bytes(buf, len) };
        checksum::adler32(adler as u32, s) as uLong
    })
}

/// Combine two Adler-32 checksums as if their input streams were concatenated,
/// where `len2` is the byte length of the second stream (C `adler32_combine`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn adler32_combine(adler1: uLong, adler2: uLong, len2: z_off_t) -> uLong {
    // Pure arithmetic over the values — no pointers, provably panic-free, so no
    // guard is required.
    checksum::adler32_combine(adler1 as u32, adler2 as u32, off_to_i64(len2)) as uLong
}

/// 64-bit-offset variant of [`adler32_combine`] (C `adler32_combine64`).
///
/// Bridges to the same single-`i64` safe implementation; only the C parameter
/// width differs (`z_off64_t` is always `i64`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn adler32_combine64(adler1: uLong, adler2: uLong, len2: z_off64_t) -> uLong {
    checksum::adler32_combine(adler1 as u32, adler2 as u32, len2) as uLong
}

// ===========================================================================
// Phase 4 — CRC-32 (port of `crc32.c`)
// ===========================================================================

/// CRC-32 checksum of `buf`, continuing from a running value `crc`
/// (C `crc32`).
///
/// Reproduces the C null sentinel: `crc32(_, Z_NULL, _)` returns the initial
/// value `0`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn crc32(crc: uLong, buf: *const Bytef, len: uInt) -> uLong {
    guard_ulong(0, move || {
        if buf.is_null() {
            // C `crc32(_, Z_NULL, _)` == 0 (the CRC-32 initial value).
            return 0;
        }
        // SAFETY: `buf` is non-null (checked above) and, per the caller's
        // contract, valid for reads of `len` bytes (empty when `len == 0`).
        let s = unsafe { as_bytes(buf, len as usize) };
        checksum::crc32(crc as u32, s) as uLong
    })
}

/// `size_t`-length variant of [`crc32`] (C `crc32_z`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn crc32_z(crc: uLong, buf: *const Bytef, len: z_size_t) -> uLong {
    guard_ulong(0, move || {
        if buf.is_null() {
            return 0;
        }
        // SAFETY: `buf` is non-null (checked above) and valid for `len` bytes.
        let s = unsafe { as_bytes(buf, len) };
        checksum::crc32(crc as u32, s) as uLong
    })
}

/// Combine two CRC-32 checksums as if their input streams were concatenated,
/// where `len2` is the byte length of the second stream (C `crc32_combine`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn crc32_combine(crc1: uLong, crc2: uLong, len2: z_off_t) -> uLong {
    checksum::crc32_combine(crc1 as u32, crc2 as u32, off_to_i64(len2)) as uLong
}

/// 64-bit-offset variant of [`crc32_combine`] (C `crc32_combine64`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn crc32_combine64(crc1: uLong, crc2: uLong, len2: z_off64_t) -> uLong {
    checksum::crc32_combine(crc1 as u32, crc2 as u32, len2) as uLong
}

/// Pre-compute the combine operator for a second stream of length `len2`,
/// for repeated use with [`crc32_combine_op`] (C `crc32_combine_gen`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn crc32_combine_gen(len2: z_off_t) -> uLong {
    checksum::crc32_combine_gen(off_to_i64(len2)) as uLong
}

/// 64-bit-offset variant of [`crc32_combine_gen`] (C `crc32_combine_gen64`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn crc32_combine_gen64(len2: z_off64_t) -> uLong {
    checksum::crc32_combine_gen(len2) as uLong
}

/// Combine two CRC-32 checksums using a pre-computed operator `op` from
/// [`crc32_combine_gen`] (C `crc32_combine_op`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn crc32_combine_op(crc1: uLong, crc2: uLong, op: uLong) -> uLong {
    checksum::crc32_combine_op(crc1 as u32, crc2 as u32, op as u32) as uLong
}

/// Return a pointer to the 256-entry CRC-32 lookup table (C `get_crc_table`).
///
/// The table is a `&'static [z_crc_t; 256]`, so the returned pointer is valid
/// for the lifetime of the program; C consumers only ever read through it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn get_crc_table() -> *const z_crc_t {
    // `z_crc_t` is `c_uint` (== `u32`) and the table's element type is `u32`,
    // so the slice pointer is already the correct type — no cast needed.
    checksum::get_crc_table().as_ptr()
}

// ===========================================================================
// Phase 5 — Version / error / compile flags (port of `zutil.c`)
// ===========================================================================

/// Return the zlib version string this crate is compatible with
/// (C `zlibVersion`).
///
/// The pointer refers to a `'static` NUL-terminated C string and must not be
/// freed by the caller. Its contents match [`crate::ZLIB_VERSION`]
/// (`"1.3.2.1-motley"`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn zlibVersion() -> *const c_char {
    // A C-string literal is a `&'static CStr`; hand C the pointer to its bytes.
    c"1.3.2.1-motley".as_ptr()
}

/// Return the static error message for the return code `err` (C `zError`).
///
/// The pointer refers to a `'static` NUL-terminated C string (never freed by
/// the caller). The wording matches zlib's `z_errmsg` table in `zutil.c`
/// (indexed `2 - err`), and out-of-range codes yield the empty string —
/// exactly C's `ERR_MSG(err)` falling through to `z_errmsg[9] == ""`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn zError(err: c_int) -> *const c_char {
    // `ReturnCode::from_c_int` returns `Some` for exactly the in-range codes
    // `-6..=2`; every other value maps to the empty string, mirroring the C
    // `(err < -6 || err > 2) ? 9 : 2 - err` index computation.
    match ReturnCode::from_c_int(err) {
        Some(ReturnCode::NeedDict) => c"need dictionary".as_ptr(),
        Some(ReturnCode::StreamEnd) => c"stream end".as_ptr(),
        Some(ReturnCode::Ok) => c"".as_ptr(),
        Some(ReturnCode::ErrNo) => c"file error".as_ptr(),
        Some(ReturnCode::StreamError) => c"stream error".as_ptr(),
        Some(ReturnCode::DataError) => c"data error".as_ptr(),
        Some(ReturnCode::MemError) => c"insufficient memory".as_ptr(),
        Some(ReturnCode::BufError) => c"buffer error".as_ptr(),
        Some(ReturnCode::VersionError) => c"incompatible version".as_ptr(),
        None => c"".as_ptr(),
    }
}

/// Return the flags describing the compile-time configuration of the library
/// (C `zlibCompileFlags`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn zlibCompileFlags() -> uLong {
    // Widening `u32` -> `uLong` (never truncating); `From` keeps it portable
    // and lint-clean regardless of whether `c_ulong` is 32- or 64-bit.
    uLong::from(util::zlib_compile_flags())
}

// ===========================================================================
// Phase 6 — Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use core::ffi::CStr;

    // Local integer return codes for assertions (the module keeps only the two
    // it emits; tests need a couple more for clarity).
    const Z_DATA_ERROR: c_int = ReturnCode::DataError.as_c_int();

    /// Read a shim-returned C string back into a Rust byte slice for comparison.
    ///
    /// # Safety
    /// `p` must point to a valid `'static` NUL-terminated string, which every
    /// `zError`/`zlibVersion` return value does.
    unsafe fn cstr_bytes<'a>(p: *const c_char) -> &'a [u8] {
        // SAFETY: the callee returns a pointer to a `'static` C-string literal.
        unsafe { CStr::from_ptr(p) }.to_bytes()
    }

    // ---------------------------------------------------------------- checksum

    #[test]
    fn adler32_null_buffer_returns_one() {
        // C `adler32(_, Z_NULL, _)` == 1.
        let v = unsafe { adler32(1, ptr::null(), 0) };
        assert_eq!(v, 1);
        // A non-1 running value is ignored for the null sentinel, matching C.
        let v = unsafe { adler32(0xDEAD_BEEF, ptr::null(), 123) };
        assert_eq!(v, 1);
    }

    #[test]
    fn adler32_empty_buffer_returns_initial() {
        // Non-null but zero length leaves the running value unchanged.
        let data = b"";
        let v = unsafe { adler32(1, data.as_ptr(), 0) };
        assert_eq!(v, 1);
    }

    #[test]
    fn adler32_known_vector() {
        // Adler-32 of "Wikipedia" is 0x11E60398 (a classic reference vector).
        let data = b"Wikipedia";
        let v = unsafe { adler32(1, data.as_ptr(), data.len() as uInt) };
        assert_eq!(v, 0x11E6_0398);
    }

    #[test]
    fn adler32_z_matches_adler32() {
        let data = b"the quick brown fox";
        let a = unsafe { adler32(1, data.as_ptr(), data.len() as uInt) };
        let b = unsafe { adler32_z(1, data.as_ptr(), data.len()) };
        assert_eq!(a, b);
    }

    #[test]
    fn adler32_combine_matches_single_pass() {
        let whole = b"abcdefghijklmnopqrstuvwxyz";
        let (first, second) = whole.split_at(10);
        let a1 = unsafe { adler32(1, first.as_ptr(), first.len() as uInt) };
        let a2 = unsafe { adler32(1, second.as_ptr(), second.len() as uInt) };
        let combined = unsafe { adler32_combine(a1, a2, second.len() as z_off_t) };
        let direct = unsafe { adler32(1, whole.as_ptr(), whole.len() as uInt) };
        assert_eq!(combined, direct);
        // The 64-bit-offset symbol must yield the identical result.
        let combined64 = unsafe { adler32_combine64(a1, a2, second.len() as z_off64_t) };
        assert_eq!(combined64, direct);
    }

    #[test]
    fn crc32_null_buffer_returns_zero() {
        // C `crc32(_, Z_NULL, _)` == 0.
        let v = unsafe { crc32(0, ptr::null(), 0) };
        assert_eq!(v, 0);
        let v = unsafe { crc32(0xFFFF_FFFF, ptr::null(), 99) };
        assert_eq!(v, 0);
    }

    #[test]
    fn crc32_known_vector() {
        // The canonical check value: CRC-32 of "123456789" is 0xCBF43926.
        let data = b"123456789";
        let v = unsafe { crc32(0, data.as_ptr(), data.len() as uInt) };
        assert_eq!(v, 0xCBF4_3926);
    }

    #[test]
    fn crc32_z_matches_crc32() {
        let data = b"the quick brown fox";
        let a = unsafe { crc32(0, data.as_ptr(), data.len() as uInt) };
        let b = unsafe { crc32_z(0, data.as_ptr(), data.len()) };
        assert_eq!(a, b);
    }

    #[test]
    fn crc32_combine_matches_single_pass() {
        let whole = b"The quick brown fox jumps over the lazy dog";
        let (first, second) = whole.split_at(19);
        let c1 = unsafe { crc32(0, first.as_ptr(), first.len() as uInt) };
        let c2 = unsafe { crc32(0, second.as_ptr(), second.len() as uInt) };
        let combined = unsafe { crc32_combine(c1, c2, second.len() as z_off_t) };
        let direct = unsafe { crc32(0, whole.as_ptr(), whole.len() as uInt) };
        assert_eq!(combined, direct);
        // 64-bit-offset symbol parity.
        let combined64 = unsafe { crc32_combine64(c1, c2, second.len() as z_off64_t) };
        assert_eq!(combined64, direct);
    }

    #[test]
    fn crc32_combine_gen_and_op_match_combine() {
        let whole = b"The quick brown fox jumps over the lazy dog";
        let (first, second) = whole.split_at(19);
        let c1 = unsafe { crc32(0, first.as_ptr(), first.len() as uInt) };
        let c2 = unsafe { crc32(0, second.as_ptr(), second.len() as uInt) };
        let op = unsafe { crc32_combine_gen(second.len() as z_off_t) };
        let via_op = unsafe { crc32_combine_op(c1, c2, op) };
        let via_combine = unsafe { crc32_combine(c1, c2, second.len() as z_off_t) };
        assert_eq!(via_op, via_combine);
        // The gen64 symbol must produce the same operator.
        let op64 = unsafe { crc32_combine_gen64(second.len() as z_off64_t) };
        assert_eq!(op64, op);
    }

    #[test]
    fn get_crc_table_first_entries_are_standard() {
        // The IEEE 802.3 CRC-32 table's first two entries are well known.
        let table = unsafe { get_crc_table() };
        assert!(!table.is_null());
        // SAFETY: `get_crc_table` returns a pointer to a `[z_crc_t; 256]`.
        let entry0 = unsafe { *table };
        let entry1 = unsafe { *table.add(1) };
        assert_eq!(entry0, 0x0000_0000);
        assert_eq!(entry1, 0x7707_3096);
    }

    // ------------------------------------------------------------- compression

    #[test]
    fn compress_bound_matches_c_formula() {
        // C `compressBound(0)` == 13; `compressBound(100000)` == 100043.
        assert_eq!(unsafe { compressBound(0) }, 13);
        assert_eq!(unsafe { compressBound(100_000) }, 100_043);
        // `_z` variant agrees for representable inputs.
        assert_eq!(unsafe { compressBound_z(0) }, 13);
        assert_eq!(unsafe { compressBound_z(100_000) }, 100_043);
    }

    #[test]
    fn compress2_rejects_null_arguments() {
        let mut dest = [0u8; 64];
        let mut dest_len: uLongf = dest.len() as uLongf;
        let src = b"payload";

        // Null dest_len -> Z_STREAM_ERROR.
        let rc = unsafe {
            compress2(
                dest.as_mut_ptr(),
                ptr::null_mut(),
                src.as_ptr(),
                src.len() as uLong,
                Z_DEFAULT_COMPRESSION,
            )
        };
        assert_eq!(rc, Z_STREAM_ERROR);

        // Null source with positive source_len -> Z_STREAM_ERROR.
        let rc = unsafe {
            compress2(
                dest.as_mut_ptr(),
                &mut dest_len,
                ptr::null(),
                7,
                Z_DEFAULT_COMPRESSION,
            )
        };
        assert_eq!(rc, Z_STREAM_ERROR);

        // Null dest with positive *dest_len -> Z_STREAM_ERROR.
        let mut dest_len2: uLongf = 64;
        let rc = unsafe {
            compress2(
                ptr::null_mut(),
                &mut dest_len2,
                src.as_ptr(),
                src.len() as uLong,
                Z_DEFAULT_COMPRESSION,
            )
        };
        assert_eq!(rc, Z_STREAM_ERROR);
    }

    #[test]
    fn compress2_then_uncompress2_round_trip() {
        let plain = b"the quick brown fox jumps over the lazy dog, repeatedly!";

        // --- compress2 ---
        let bound = unsafe { compressBound(plain.len() as uLong) } as usize;
        let mut comp = vec![0u8; bound];
        let mut comp_len: uLongf = comp.len() as uLongf;
        let rc = unsafe {
            compress2(
                comp.as_mut_ptr(),
                &mut comp_len,
                plain.as_ptr(),
                plain.len() as uLong,
                6,
            )
        };
        assert_eq!(rc, Z_OK);
        assert!((comp_len as usize) <= bound);
        assert!(comp_len > 0);

        // --- uncompress2 ---
        let mut plain_out = vec![0u8; plain.len()];
        let mut out_len: uLongf = plain_out.len() as uLongf;
        let mut src_len: uLong = comp_len; // available compressed bytes
        let rc = unsafe {
            uncompress2(
                plain_out.as_mut_ptr(),
                &mut out_len,
                comp.as_ptr(),
                &mut src_len,
            )
        };
        assert_eq!(rc, Z_OK);
        assert_eq!(out_len as usize, plain.len());
        assert_eq!(&plain_out[..out_len as usize], plain);
        // The whole compressed stream was consumed.
        assert_eq!(src_len, comp_len);
    }

    #[test]
    fn compress_and_uncompress_default_level_round_trip() {
        let plain = b"default-level round trip through the convenience wrappers";

        let bound = unsafe { compressBound(plain.len() as uLong) } as usize;
        let mut comp = vec![0u8; bound];
        let mut comp_len: uLongf = comp.len() as uLongf;
        let rc = unsafe {
            compress(
                comp.as_mut_ptr(),
                &mut comp_len,
                plain.as_ptr(),
                plain.len() as uLong,
            )
        };
        assert_eq!(rc, Z_OK);

        let mut plain_out = vec![0u8; plain.len()];
        let mut out_len: uLongf = plain_out.len() as uLongf;
        let rc = unsafe {
            uncompress(
                plain_out.as_mut_ptr(),
                &mut out_len,
                comp.as_ptr(),
                comp_len,
            )
        };
        assert_eq!(rc, Z_OK);
        assert_eq!(&plain_out[..out_len as usize], plain);
    }

    #[test]
    fn compress2_z_uncompress2_z_round_trip() {
        let plain = b"size_t variant round trip payload for the _z symbols";

        let bound = unsafe { compressBound_z(plain.len()) };
        let mut comp = vec![0u8; bound];
        let mut comp_len: z_size_t = comp.len();
        let rc = unsafe {
            compress2_z(
                comp.as_mut_ptr(),
                &mut comp_len,
                plain.as_ptr(),
                plain.len(),
                9,
            )
        };
        assert_eq!(rc, Z_OK);

        let mut plain_out = vec![0u8; plain.len()];
        let mut out_len: z_size_t = plain_out.len();
        let mut src_len: z_size_t = comp_len;
        let rc = unsafe {
            uncompress2_z(
                plain_out.as_mut_ptr(),
                &mut out_len,
                comp.as_ptr(),
                &mut src_len,
            )
        };
        assert_eq!(rc, Z_OK);
        assert_eq!(out_len, plain.len());
        assert_eq!(&plain_out[..out_len], plain);
        assert_eq!(src_len, comp_len);
    }

    #[test]
    fn uncompress2_rejects_null_arguments() {
        let mut dest = [0u8; 32];
        let mut dest_len: uLongf = dest.len() as uLongf;
        let mut src_len: uLong = 8;
        let src = [0u8; 8];

        // Null source_len -> Z_STREAM_ERROR.
        let rc = unsafe {
            uncompress2(
                dest.as_mut_ptr(),
                &mut dest_len,
                src.as_ptr(),
                ptr::null_mut(),
            )
        };
        assert_eq!(rc, Z_STREAM_ERROR);

        // Null source with positive *source_len -> Z_STREAM_ERROR.
        let rc =
            unsafe { uncompress2(dest.as_mut_ptr(), &mut dest_len, ptr::null(), &mut src_len) };
        assert_eq!(rc, Z_STREAM_ERROR);
    }

    #[test]
    fn uncompress2_corrupt_input_is_data_error() {
        // A well-formed zlib header followed by damaged deflate data.
        let mut bad = [0x78u8, 0x9c, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff];
        let mut out = [0u8; 32];
        let mut out_len: uLongf = out.len() as uLongf;
        let mut src_len: uLong = bad.len() as uLong;
        let rc = unsafe {
            uncompress2(
                out.as_mut_ptr(),
                &mut out_len,
                bad.as_mut_ptr(),
                &mut src_len,
            )
        };
        assert_eq!(rc, Z_DATA_ERROR);
        // The corrupt byte 0xFF decodes to BFINAL=1, BTYPE=11 (an invalid,
        // reserved block type), so decoding fails at the block-type check
        // before emitting any output. Zero is therefore the *true* produced
        // count here, not the old force-to-zero behavior; the general
        // partial-output contract is verified by
        // `uncompress2_reports_partial_output_on_error` below.
        assert_eq!(out_len, 0);
    }

    #[test]
    fn uncompress2_reports_partial_output_on_error() {
        // Regression guard for the M2 fix: on an error path that produces
        // output before failing, `*destLen` must report the produced count
        // (C `uncompress2_z` writes `*destLen = stream.total_out;` before the
        // error-mapping `return`), not the old force-to-zero value.
        //
        // Build a fully valid zlib stream, then truncate its 4-byte Adler-32
        // trailer. All deflate data is present, so inflate decodes the entire
        // payload, but the stream never reaches `Z_STREAM_END` (the checksum is
        // missing and input is exhausted). With output capacity still
        // remaining, `uncompress2` maps this to `Z_DATA_ERROR` while having
        // produced the full plaintext.
        let plain: Vec<u8> = (0..200u32).map(|i| (i * 37 + 11) as u8).collect();

        let bound = unsafe { compressBound(plain.len() as uLong) } as usize;
        let mut comp = vec![0u8; bound];
        let mut comp_len: uLongf = comp.len() as uLongf;
        let rc = unsafe {
            compress(
                comp.as_mut_ptr(),
                &mut comp_len,
                plain.as_ptr(),
                plain.len() as uLong,
            )
        };
        assert_eq!(rc, Z_OK);
        let comp_len = comp_len as usize;
        assert!(
            comp_len > 4,
            "compressed stream includes the Adler-32 trailer"
        );

        // Drop the trailing 4-byte Adler-32 checksum.
        let truncated = &comp[..comp_len - 4];

        // Oversize the output buffer so leftover output capacity forces the
        // truncated-stream branch to `Z_DATA_ERROR` rather than `Z_BUF_ERROR`.
        let mut out = vec![0u8; plain.len() + 64];
        let mut out_len: uLongf = out.len() as uLongf;
        let mut src_len: uLong = truncated.len() as uLong;
        let rc = unsafe {
            uncompress2(
                out.as_mut_ptr(),
                &mut out_len,
                truncated.as_ptr(),
                &mut src_len,
            )
        };
        assert_eq!(rc, Z_DATA_ERROR, "a truncated stream is a data error");
        // The M2 assertion: the produced count is surfaced on the error path.
        assert_eq!(
            out_len as usize,
            plain.len(),
            "the full decoded length is reported despite the error"
        );
        assert_eq!(
            &out[..out_len as usize],
            &plain[..],
            "output is the payload"
        );
    }

    // ------------------------------------------------------------- version/err

    #[test]
    fn zlib_version_matches_crate_constant() {
        let p = unsafe { zlibVersion() };
        assert!(!p.is_null());
        let bytes = unsafe { cstr_bytes(p) };
        assert_eq!(bytes, b"1.3.2.1-motley");
        // Must agree with the crate-level constant.
        assert_eq!(bytes, crate::ZLIB_VERSION.as_bytes());
    }

    #[test]
    fn z_error_messages_match_z_errmsg_table() {
        let cases: [(c_int, &[u8]); 9] = [
            (ReturnCode::NeedDict.as_c_int(), b"need dictionary"),
            (ReturnCode::StreamEnd.as_c_int(), b"stream end"),
            (ReturnCode::Ok.as_c_int(), b""),
            (ReturnCode::ErrNo.as_c_int(), b"file error"),
            (ReturnCode::StreamError.as_c_int(), b"stream error"),
            (ReturnCode::DataError.as_c_int(), b"data error"),
            (ReturnCode::MemError.as_c_int(), b"insufficient memory"),
            (ReturnCode::BufError.as_c_int(), b"buffer error"),
            (ReturnCode::VersionError.as_c_int(), b"incompatible version"),
        ];
        for (code, expected) in cases {
            let bytes = unsafe { cstr_bytes(zError(code)) };
            assert_eq!(bytes, expected, "mismatch for code {code}");
        }
        // Out-of-range codes yield the empty string (C `z_errmsg[9]`).
        assert_eq!(unsafe { cstr_bytes(zError(42)) }, b"");
        assert_eq!(unsafe { cstr_bytes(zError(-100)) }, b"");
    }

    #[test]
    fn zlib_compile_flags_is_stable() {
        // The flags are a compile-time constant; two reads must agree and the
        // value must be non-zero (the size-code bits are always populated).
        let a = unsafe { zlibCompileFlags() };
        let b = unsafe { zlibCompileFlags() };
        assert_eq!(a, b);
        assert_ne!(a, 0);
    }
}
